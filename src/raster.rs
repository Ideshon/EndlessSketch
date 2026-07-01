use crate::coords::{CameraAddress, DEPTH_RATIO};
use crate::model::{Color, EditOperation};
use crate::smoothing::{
    GeometryClipRect, STROKE_SMOOTHING_PASSES, clip_polygon_to_rect, clip_polyline_to_rect,
    simplify_render_points, smooth_closed_points, smooth_stroke_points,
};
use crate::tile_cache::{TILE_BLEED, TILE_SIZE, TileKey, tile_resolution};
use image::{Rgba, RgbaImage};

const MAX_EDGE_QUALITY: u8 = 2;
const MAX_SMOOTHING_PASSES: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RasterOptions {
    edge_quality: u8,
    smoothing_passes: u8,
}

impl Default for RasterOptions {
    fn default() -> Self {
        Self {
            edge_quality: 1,
            smoothing_passes: STROKE_SMOOTHING_PASSES as u8,
        }
    }
}

impl RasterOptions {
    pub fn new(edge_quality: u8, smoothing_passes: u8) -> Self {
        Self {
            edge_quality: edge_quality.min(MAX_EDGE_QUALITY),
            smoothing_passes: smoothing_passes.min(MAX_SMOOTHING_PASSES),
        }
    }

    pub fn cache_tag(self) -> String {
        format!("e{}_s{}", self.edge_quality, self.smoothing_passes)
    }

    fn fill_sample_grid(self) -> i64 {
        match self.edge_quality {
            0 => 1,
            1 => 4,
            _ => 8,
        }
    }

    fn stroke_edge_mode(self) -> StrokeEdgeMode {
        match self.edge_quality {
            0 => StrokeEdgeMode::Binary,
            1 => StrokeEdgeMode::Analytic,
            _ => StrokeEdgeMode::Supersampled(2),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrokeEdgeMode {
    Binary,
    Analytic,
    Supersampled(u8),
}

pub fn render_tile(operations: &[EditOperation], key: &TileKey, background: Color) -> RgbaImage {
    render_tile_with_options(operations, key, background, RasterOptions::default())
}

pub fn render_tile_with_options(
    operations: &[EditOperation],
    key: &TileKey,
    background: Color,
    options: RasterOptions,
) -> RgbaImage {
    let content_size = tile_resolution(key.lod);
    let mut image = RgbaImage::from_pixel(
        content_size + TILE_BLEED * 2,
        content_size + TILE_BLEED * 2,
        rgba(background),
    );
    render_operations_onto_tile_with_options(&mut image, operations, key, background, options);
    image
}

pub fn render_operations_onto_tile(
    image: &mut RgbaImage,
    operations: &[EditOperation],
    key: &TileKey,
    background: Color,
) {
    render_operations_onto_tile_with_options(
        image,
        operations,
        key,
        background,
        RasterOptions::default(),
    );
}

pub fn render_operations_onto_tile_with_options(
    image: &mut RgbaImage,
    operations: &[EditOperation],
    key: &TileKey,
    background: Color,
    options: RasterOptions,
) {
    let content_size = tile_resolution(key.lod);
    let resolution_scale = content_size as f64 / TILE_SIZE as f64;
    let camera = CameraAddress {
        depth: key.depth,
        tile_x: key.x.clone(),
        tile_y: key.y.clone(),
        local_x: 0.5,
        local_y: 0.5,
        zoom: resolution_scale,
    };
    let mut fill_coverage = None;

    for operation in operations {
        let points: Vec<(f32, f32)> = operation
            .points
            .iter()
            .filter_map(|point| {
                camera
                    .canvas_to_screen(point, content_size as f64, content_size as f64)
                    .map(|(x, y)| (x as f32 + TILE_BLEED as f32, y as f32 + TILE_BLEED as f32))
            })
            .collect();
        let width = operation_width(operation, key.depth, resolution_scale);
        let color = operation.opaque_visible_color(background);
        if operation.kind.is_area() {
            if points.len() < 3 {
                continue;
            }
            let points = simplify_render_points(&points, true);
            let image_width = image.width();
            let image_height = image.height();
            let coverage =
                fill_coverage.get_or_insert_with(|| FillCoverage::new(image_width, image_height));
            let smoothed_points =
                smooth_closed_points(&points, usize::from(options.smoothing_passes));
            let clipped_points = clip_polygon_to_rect(
                &smoothed_points,
                GeometryClipRect::new(0.0, 0.0, image_width as f32, image_height as f32),
            );
            if clipped_points.len() < 3 {
                continue;
            }
            fill_polygon(
                image,
                &clipped_points,
                color,
                coverage,
                options.fill_sample_grid(),
            );
        } else {
            if points.len() < 2 {
                continue;
            }
            let points = simplify_render_points(&points, false);
            let smoothed_points =
                smooth_stroke_points(&points, usize::from(options.smoothing_passes));
            let margin = width * 0.5 + 1.0;
            let clip_rect = GeometryClipRect::new(
                -margin,
                -margin,
                image.width() as f32 + margin,
                image.height() as f32 + margin,
            );
            for run in clip_polyline_to_rect(&smoothed_points, clip_rect) {
                draw_opaque_stroke(image, &run, width, color, options.stroke_edge_mode());
            }
        }
    }
}

pub fn operation_width(operation: &EditOperation, target_depth: i64, target_zoom: f64) -> f32 {
    let delta = target_depth
        .saturating_sub(operation.native_depth)
        .clamp(-40, 40) as i32;
    let depth_scale = (DEPTH_RATIO as f64).powi(delta);
    (operation.width_px as f64 * target_zoom * depth_scale / operation.native_zoom.max(1e-12))
        .clamp(0.35, 100_000.0) as f32
}

fn draw_opaque_stroke(
    image: &mut RgbaImage,
    points: &[(f32, f32)],
    width: f32,
    color: Color,
    edge_mode: StrokeEdgeMode,
) {
    debug_assert!(color.is_opaque());
    let radius = width * 0.5;
    for segment in points.windows(2) {
        let Some((min_x, max_x, min_y, max_y)) = stroke_segment_bounds(
            image.width(),
            image.height(),
            segment[0],
            segment[1],
            radius,
        ) else {
            continue;
        };
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let pixel_coverage =
                    stroke_pixel_coverage(x, y, segment[0], segment[1], radius, edge_mode);
                if pixel_coverage > 0.0 {
                    blend_opaque_pixel(image.get_pixel_mut(x, y), color, pixel_coverage);
                }
            }
        }
    }
}

fn stroke_pixel_coverage(
    x: u32,
    y: u32,
    start: (f32, f32),
    end: (f32, f32),
    radius: f32,
    edge_mode: StrokeEdgeMode,
) -> f32 {
    match edge_mode {
        StrokeEdgeMode::Binary => {
            let sample = (x as f32 + 0.5, y as f32 + 0.5);
            if distance_to_segment(sample, start, end) <= radius {
                1.0
            } else {
                0.0
            }
        }
        StrokeEdgeMode::Analytic => {
            let sample = (x as f32 + 0.5, y as f32 + 0.5);
            (radius + 0.5 - distance_to_segment(sample, start, end)).clamp(0.0, 1.0)
        }
        StrokeEdgeMode::Supersampled(grid) => {
            let grid = u32::from(grid.max(1));
            let mut covered = 0;
            for sample_y in 0..grid {
                for sample_x in 0..grid {
                    let sample = (
                        x as f32 + (sample_x as f32 + 0.5) / grid as f32,
                        y as f32 + (sample_y as f32 + 0.5) / grid as f32,
                    );
                    covered += u32::from(distance_to_segment(sample, start, end) <= radius);
                }
            }
            covered as f32 / (grid * grid) as f32
        }
    }
}

fn stroke_segment_bounds(
    image_width: u32,
    image_height: u32,
    start: (f32, f32),
    end: (f32, f32),
    radius: f32,
) -> Option<(u32, u32, u32, u32)> {
    if image_width == 0
        || image_height == 0
        || !start.0.is_finite()
        || !start.1.is_finite()
        || !end.0.is_finite()
        || !end.1.is_finite()
        || !radius.is_finite()
    {
        return None;
    }

    let min_x = (start.0.min(end.0) - radius - 1.0).floor();
    let max_x = (start.0.max(end.0) + radius + 1.0).ceil();
    let min_y = (start.1.min(end.1) - radius - 1.0).floor();
    let max_y = (start.1.max(end.1) + radius + 1.0).ceil();
    let image_max_x = image_width as f32 - 1.0;
    let image_max_y = image_height as f32 - 1.0;
    if max_x < 0.0 || max_y < 0.0 || min_x > image_max_x || min_y > image_max_y {
        return None;
    }

    Some((
        min_x.max(0.0) as u32,
        max_x.min(image_max_x) as u32,
        min_y.max(0.0) as u32,
        max_y.min(image_max_y) as u32,
    ))
}

struct FillCoverage {
    image_width: usize,
    values: Vec<u8>,
    touched: Vec<usize>,
}

impl FillCoverage {
    fn new(image_width: u32, image_height: u32) -> Self {
        Self {
            image_width: image_width as usize,
            values: vec![0; image_width as usize * image_height as usize],
            touched: Vec::new(),
        }
    }
}

fn fill_polygon(
    image: &mut RgbaImage,
    points: &[(f32, f32)],
    color: Color,
    coverage: &mut FillCoverage,
    sample_grid: i64,
) {
    let min_y = points
        .iter()
        .map(|point| point.1)
        .fold(f32::INFINITY, f32::min);
    let max_y = points
        .iter()
        .map(|point| point.1)
        .fold(f32::NEG_INFINITY, f32::max);
    if !min_y.is_finite() || !max_y.is_finite() {
        return;
    }

    let total_sample_rows = i64::from(image.height()) * sample_grid;
    let first_sample_row = sample_index_at_or_after(min_y, sample_grid).clamp(0, total_sample_rows);
    let end_sample_row = sample_index_at_or_after(max_y, sample_grid).clamp(0, total_sample_rows);
    let mut intersections = Vec::with_capacity(points.len());
    for sample_row in first_sample_row..end_sample_row {
        let sample_y = (sample_row as f32 + 0.5) / sample_grid as f32;
        intersections.clear();
        let mut previous = points.len() - 1;
        for current in 0..points.len() {
            let a = points[current];
            let b = points[previous];
            if ((a.1 > sample_y) != (b.1 > sample_y)) && (b.1 - a.1).abs() > f32::EPSILON {
                let x = a.0 + (sample_y - a.1) * (b.0 - a.0) / (b.1 - a.1);
                if x.is_finite() {
                    intersections.push(x);
                }
            }
            previous = current;
        }
        intersections.sort_by(|a, b| a.total_cmp(b));

        let pixel_y = (sample_row / sample_grid) as usize;
        for span in intersections.chunks_exact(2) {
            accumulate_fill_span(
                coverage,
                pixel_y,
                span[0],
                span[1],
                image.width(),
                sample_grid,
            );
        }
    }

    let max_coverage = (sample_grid * sample_grid) as f32;
    for index in coverage.touched.drain(..) {
        let pixel_coverage = coverage.values[index] as f32 / max_coverage;
        coverage.values[index] = 0;
        let x = (index % coverage.image_width) as u32;
        let y = (index / coverage.image_width) as u32;
        blend_opaque_pixel(image.get_pixel_mut(x, y), color, pixel_coverage);
    }
}

fn sample_index_at_or_after(coordinate: f32, sample_grid: i64) -> i64 {
    (coordinate * sample_grid as f32 - 0.5).ceil() as i64
}

fn accumulate_fill_span(
    coverage: &mut FillCoverage,
    pixel_y: usize,
    left: f32,
    right: f32,
    image_width: u32,
    sample_grid: i64,
) {
    let total_samples = i64::from(image_width) * sample_grid;
    let first_sample = sample_index_at_or_after(left, sample_grid).clamp(0, total_samples);
    let end_sample = sample_index_at_or_after(right, sample_grid).clamp(0, total_samples);
    if first_sample >= end_sample {
        return;
    }

    let first_pixel = first_sample / sample_grid;
    let last_pixel = (end_sample - 1) / sample_grid;
    for pixel_x in first_pixel..=last_pixel {
        let pixel_sample_start = pixel_x * sample_grid;
        let covered_samples = end_sample
            .min(pixel_sample_start + sample_grid)
            .saturating_sub(first_sample.max(pixel_sample_start))
            as u8;
        if covered_samples == 0 {
            continue;
        }
        let index = pixel_y * coverage.image_width + pixel_x as usize;
        if coverage.values[index] == 0 {
            coverage.touched.push(index);
        }
        coverage.values[index] += covered_samples;
    }
}

fn distance_to_segment(point: (f32, f32), start: (f32, f32), end: (f32, f32)) -> f32 {
    let dx = end.0 - start.0;
    let dy = end.1 - start.1;
    let length_squared = dx * dx + dy * dy;
    if length_squared <= f32::EPSILON {
        return ((point.0 - start.0).powi(2) + (point.1 - start.1).powi(2)).sqrt();
    }
    let t =
        (((point.0 - start.0) * dx + (point.1 - start.1) * dy) / length_squared).clamp(0.0, 1.0);
    let closest = (start.0 + t * dx, start.1 + t * dy);
    ((point.0 - closest.0).powi(2) + (point.1 - closest.1).powi(2)).sqrt()
}

fn blend_opaque_pixel(destination: &mut Rgba<u8>, source: Color, coverage: f32) {
    debug_assert!(source.is_opaque());
    if coverage >= 1.0 {
        *destination = rgba(source);
        return;
    }
    let coverage = coverage.clamp(0.0, 1.0);
    for channel in 0..3 {
        let source_value = [source.r, source.g, source.b][channel] as f32;
        let destination_value = destination[channel] as f32;
        destination[channel] =
            (destination_value + (source_value - destination_value) * coverage).round() as u8;
    }
    destination[3] = u8::MAX;
}

fn rgba(color: Color) -> Rgba<u8> {
    Rgba([color.r, color.g, color.b, color.a])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::CanvasPoint;
    use crate::model::EditKind;
    use crate::tile_cache::{TILE_RESOLUTIONS, tile_lod_for_resolution};
    use num_bigint::BigInt;

    fn point(depth: i64, x: f64, y: f64) -> CanvasPoint {
        CanvasPoint::new(depth, BigInt::from(0), BigInt::from(0), x, y)
    }

    #[test]
    fn later_opaque_stroke_removes_older_pixels_across_depths() {
        let old = EditOperation::draft(
            EditKind::Paint,
            1,
            1.0,
            vec![point(1, 3.2, 4.0), point(1, 4.8, 4.0)],
            Color::BLACK,
            32.0,
        );
        let covering = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![point(0, 0.5, 0.35), point(0, 0.5, 0.65)],
            Color::rgba(220, 30, 30, 255),
            48.0,
        );
        let key = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: tile_lod_for_resolution(512),
        };
        let image = render_tile(&[old, covering], &key, Color::WHITE);
        let center = image.get_pixel(TILE_BLEED + 256, TILE_BLEED + 256);
        assert!(center[0] > 180 && center[1] < 80, "pixel={center:?}");
        let old_visible = image.get_pixel(TILE_BLEED + 220, TILE_BLEED + 256);
        assert!(old_visible[0] < 80, "pixel={old_visible:?}");
    }

    #[test]
    fn later_stroke_is_not_affected_by_older_erase() {
        let erase = EditOperation::draft(
            EditKind::Erase,
            0,
            1.0,
            vec![point(0, 0.4, 0.5), point(0, 0.6, 0.5)],
            Color::WHITE,
            64.0,
        );
        let newer = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![point(0, 0.5, 0.4), point(0, 0.5, 0.6)],
            Color::BLACK,
            16.0,
        );
        let key = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: tile_lod_for_resolution(512),
        };
        let image = render_tile(&[erase, newer], &key, Color::WHITE);
        let center = image.get_pixel(TILE_BLEED + 256, TILE_BLEED + 256);
        assert!(center[0] < 30, "pixel={center:?}");
    }

    #[test]
    fn erase_area_clears_older_content_inside_its_polygon() {
        let base = EditOperation::draft(
            EditKind::Fill,
            0,
            1.0,
            vec![
                point(0, 0.1, 0.1),
                point(0, 0.9, 0.1),
                point(0, 0.9, 0.9),
                point(0, 0.1, 0.9),
            ],
            Color::BLACK,
            0.0,
        );
        let erase = EditOperation::draft(
            EditKind::EraseArea,
            0,
            1.0,
            vec![
                point(0, 0.4, 0.4),
                point(0, 0.6, 0.4),
                point(0, 0.6, 0.6),
                point(0, 0.4, 0.6),
            ],
            Color::WHITE,
            0.0,
        );
        let key = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: tile_lod_for_resolution(512),
        };

        let image = render_tile(&[base, erase], &key, Color::WHITE);
        let erased = image.get_pixel(TILE_BLEED + 256, TILE_BLEED + 256);
        let retained = image.get_pixel(TILE_BLEED + 160, TILE_BLEED + 256);

        assert!(erased[0] > 240, "pixel={erased:?}");
        assert!(retained[0] < 30, "pixel={retained:?}");
    }

    #[test]
    fn incremental_opaque_paint_matches_full_tile_rebuild() {
        let base = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![point(0, 0.2, 0.5), point(0, 0.8, 0.5)],
            Color::BLACK,
            24.0,
        );
        let newer = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![point(0, 0.5, 0.2), point(0, 0.5, 0.8)],
            Color::rgba(220, 30, 30, 255),
            18.0,
        );
        let key = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: tile_lod_for_resolution(128),
        };

        let mut incremental = render_tile(std::slice::from_ref(&base), &key, Color::WHITE);
        render_operations_onto_tile(
            &mut incremental,
            std::slice::from_ref(&newer),
            &key,
            Color::WHITE,
        );
        let full = render_tile(&[base, newer], &key, Color::WHITE);

        assert_eq!(incremental, full);
    }

    #[test]
    fn stroke_remains_visible_across_one_depth_transition() {
        let stroke = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![point(0, 0.45, 0.55), point(0, 0.55, 0.55)],
            Color::BLACK,
            8.0,
        );
        let parent = render_tile(
            std::slice::from_ref(&stroke),
            &TileKey {
                depth: 0,
                x: 0.into(),
                y: 0.into(),
                lod: tile_lod_for_resolution(512),
            },
            Color::WHITE,
        );
        let child = render_tile(
            std::slice::from_ref(&stroke),
            &TileKey {
                depth: 1,
                x: 4.into(),
                y: 4.into(),
                lod: tile_lod_for_resolution(512),
            },
            Color::WHITE,
        );

        assert!(dark_pixel_count(&parent) > 0);
        assert!(dark_pixel_count(&child) > 0);
    }

    #[test]
    fn fill_remains_visible_across_one_depth_transition() {
        let fill = EditOperation::draft(
            EditKind::Fill,
            0,
            1.0,
            vec![
                point(0, 0.25, 0.25),
                point(0, 0.75, 0.25),
                point(0, 0.75, 0.75),
                point(0, 0.25, 0.75),
                point(0, 0.25, 0.25),
            ],
            Color::BLACK,
            8.0,
        );
        let parent = render_tile(
            std::slice::from_ref(&fill),
            &TileKey {
                depth: 0,
                x: 0.into(),
                y: 0.into(),
                lod: tile_lod_for_resolution(512),
            },
            Color::WHITE,
        );
        let child = render_tile(
            std::slice::from_ref(&fill),
            &TileKey {
                depth: 1,
                x: 4.into(),
                y: 4.into(),
                lod: tile_lod_for_resolution(512),
            },
            Color::WHITE,
        );

        assert!(dark_pixel_count(&parent) > 0);
        assert!(dark_pixel_count(&child) > 0);
    }

    #[test]
    fn selected_tile_resolutions_have_exact_bitmap_dimensions() {
        for resolution in TILE_RESOLUTIONS {
            let image = render_tile(
                &[],
                &TileKey {
                    depth: 0,
                    x: 0.into(),
                    y: 0.into(),
                    lod: tile_lod_for_resolution(resolution),
                },
                Color::WHITE,
            );
            assert_eq!(image.width(), resolution + TILE_BLEED * 2);
            assert_eq!(image.height(), resolution + TILE_BLEED * 2);
        }
    }

    #[test]
    fn stroke_width_is_finite_and_bounded_at_hundred_level_deltas() {
        let stroke = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![point(0, 0.25, 0.5), point(0, 0.75, 0.5)],
            Color::BLACK,
            8.0,
        );

        for depth in [-1000, -100] {
            assert_eq!(operation_width(&stroke, depth, 1.0), 0.35);
        }
        for depth in [100, 1000] {
            assert_eq!(operation_width(&stroke, depth, 1.0), 100_000.0);
        }
    }

    #[test]
    fn opaque_blend_uses_coverage_without_alpha_compositing() {
        let mut pixel = Rgba([0, 0, 0, 255]);

        blend_opaque_pixel(&mut pixel, Color::rgba(240, 220, 40, 255), 0.5);

        assert_eq!(pixel, Rgba([120, 110, 20, 255]));
    }

    #[test]
    fn stored_transparent_stroke_is_flattened_and_rasterized_opaque() {
        let stroke = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![point(0, 0.25, 0.5), point(0, 0.5, 0.5), point(0, 0.75, 0.5)],
            Color::rgba(0, 0, 0, 128),
            12.0,
        );
        let stored_alpha = stroke.color.a;
        let image = render_tile(
            std::slice::from_ref(&stroke),
            &TileKey {
                depth: 0,
                x: 0.into(),
                y: 0.into(),
                lod: tile_lod_for_resolution(512),
            },
            Color::WHITE,
        );
        let y = TILE_BLEED + TILE_SIZE / 2;
        let segment_body = image.get_pixel(TILE_BLEED + TILE_SIZE * 3 / 8, y);
        let joint = image.get_pixel(TILE_BLEED + TILE_SIZE / 2, y);

        assert_eq!(stored_alpha, 128);
        assert_eq!(stroke.color.a, 128);
        assert_eq!(segment_body, &Rgba([125, 125, 124, 255]));
        assert_eq!(joint, segment_body);
    }

    #[test]
    fn fill_scanline_span_preserves_four_sample_horizontal_coverage() {
        let mut coverage = FillCoverage::new(4, 1);

        accumulate_fill_span(&mut coverage, 0, 0.25, 2.75, 4, 4);

        assert_eq!(coverage.values, vec![3, 4, 3, 0]);
        assert_eq!(coverage.touched, vec![0, 1, 2]);
    }

    #[test]
    fn edge_quality_modes_produce_distinct_stroke_tiles() {
        let stroke = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![point(0, 0.2, 0.23), point(0, 0.8, 0.71)],
            Color::BLACK,
            3.0,
        );
        let key = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: tile_lod_for_resolution(64),
        };

        let performance = render_tile_with_options(
            std::slice::from_ref(&stroke),
            &key,
            Color::WHITE,
            RasterOptions::new(0, 0),
        );
        let balanced = render_tile_with_options(
            std::slice::from_ref(&stroke),
            &key,
            Color::WHITE,
            RasterOptions::new(1, 0),
        );
        let quality =
            render_tile_with_options(&[stroke], &key, Color::WHITE, RasterOptions::new(2, 0));

        assert_ne!(performance, balanced);
        assert_ne!(balanced, quality);
    }

    fn dark_pixel_count(image: &RgbaImage) -> usize {
        image
            .pixels()
            .filter(|pixel| pixel[0] < 80 && pixel[1] < 80 && pixel[2] < 80)
            .count()
    }
}
