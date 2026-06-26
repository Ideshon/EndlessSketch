use crate::coords::{CameraAddress, DEPTH_RATIO};
use crate::model::{Color, EditKind, EditOperation};
use crate::tile_cache::{TILE_BLEED, TILE_SIZE, TileKey, lod_scale};
use image::{Rgba, RgbaImage};

pub fn render_tile(operations: &[EditOperation], key: &TileKey, background: Color) -> RgbaImage {
    let resolution_scale = lod_scale(key.lod);
    let content_size = TILE_SIZE * resolution_scale;
    let mut image = RgbaImage::from_pixel(
        content_size + TILE_BLEED * 2,
        content_size + TILE_BLEED * 2,
        rgba(background),
    );
    let camera = CameraAddress {
        depth: key.depth,
        tile_x: key.x.clone(),
        tile_y: key.y.clone(),
        local_x: 0.5,
        local_y: 0.5,
        zoom: resolution_scale as f64,
    };

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
        let width = operation_width(operation, key.depth, resolution_scale as f64);
        let color = operation.visible_color(background);
        if operation.kind == EditKind::Fill {
            if points.len() < 3 {
                continue;
            }
            fill_polygon(&mut image, &points, color);
        } else {
            if points.len() < 2 {
                continue;
            }
            for segment in points.windows(2) {
                draw_segment(&mut image, segment[0], segment[1], width, color);
            }
        }
    }
    image
}

pub fn operation_width(operation: &EditOperation, target_depth: i64, target_zoom: f64) -> f32 {
    let delta = target_depth
        .saturating_sub(operation.native_depth)
        .clamp(-40, 40) as i32;
    let depth_scale = (DEPTH_RATIO as f64).powi(delta);
    (operation.width_px as f64 * target_zoom * depth_scale / operation.native_zoom.max(1e-12))
        .clamp(0.35, 100_000.0) as f32
}

fn draw_segment(
    image: &mut RgbaImage,
    start: (f32, f32),
    end: (f32, f32),
    width: f32,
    color: Color,
) {
    let radius = width * 0.5;
    let min_x = (start.0.min(end.0) - radius - 1.0).floor().max(0.0) as u32;
    let max_x = (start.0.max(end.0) + radius + 1.0)
        .ceil()
        .min(image.width() as f32 - 1.0) as u32;
    let min_y = (start.1.min(end.1) - radius - 1.0).floor().max(0.0) as u32;
    let max_y = (start.1.max(end.1) + radius + 1.0)
        .ceil()
        .min(image.height() as f32 - 1.0) as u32;

    if min_x > max_x || min_y > max_y {
        return;
    }
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let distance = distance_to_segment((x as f32 + 0.5, y as f32 + 0.5), start, end);
            let coverage = (radius + 0.5 - distance).clamp(0.0, 1.0);
            if coverage > 0.0 {
                blend_pixel(image.get_pixel_mut(x, y), color, coverage);
            }
        }
    }
}

fn fill_polygon(image: &mut RgbaImage, points: &[(f32, f32)], color: Color) {
    let min_x = points
        .iter()
        .map(|point| point.0)
        .fold(f32::INFINITY, f32::min)
        .floor()
        .max(0.0) as u32;
    let max_x = points
        .iter()
        .map(|point| point.0)
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil()
        .min(image.width() as f32 - 1.0) as u32;
    let min_y = points
        .iter()
        .map(|point| point.1)
        .fold(f32::INFINITY, f32::min)
        .floor()
        .max(0.0) as u32;
    let max_y = points
        .iter()
        .map(|point| point.1)
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil()
        .min(image.height() as f32 - 1.0) as u32;

    if min_x > max_x || min_y > max_y {
        return;
    }
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let coverage = polygon_coverage(x, y, points);
            if coverage > 0.0 {
                blend_pixel(image.get_pixel_mut(x, y), color, coverage);
            }
        }
    }
}

fn polygon_coverage(x: u32, y: u32, polygon: &[(f32, f32)]) -> f32 {
    const SAMPLE_GRID: u32 = 4;
    let mut covered = 0;
    for sample_y in 0..SAMPLE_GRID {
        for sample_x in 0..SAMPLE_GRID {
            let offset_x = (sample_x as f32 + 0.5) / SAMPLE_GRID as f32;
            let offset_y = (sample_y as f32 + 0.5) / SAMPLE_GRID as f32;
            if point_in_polygon((x as f32 + offset_x, y as f32 + offset_y), polygon) {
                covered += 1;
            }
        }
    }
    covered as f32 / (SAMPLE_GRID * SAMPLE_GRID) as f32
}

fn point_in_polygon(point: (f32, f32), polygon: &[(f32, f32)]) -> bool {
    let mut inside = false;
    let mut previous = polygon.len() - 1;
    for current in 0..polygon.len() {
        let a = polygon[current];
        let b = polygon[previous];
        if ((a.1 > point.1) != (b.1 > point.1))
            && point.0 < (b.0 - a.0) * (point.1 - a.1) / (b.1 - a.1) + a.0
        {
            inside = !inside;
        }
        previous = current;
    }
    inside
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

fn blend_pixel(destination: &mut Rgba<u8>, source: Color, coverage: f32) {
    let source_alpha = source.a as f32 / 255.0 * coverage;
    let destination_alpha = destination[3] as f32 / 255.0;
    let output_alpha = source_alpha + destination_alpha * (1.0 - source_alpha);
    if output_alpha <= f32::EPSILON {
        *destination = Rgba([0, 0, 0, 0]);
        return;
    }
    for channel in 0..3 {
        let source_value = [source.r, source.g, source.b][channel] as f32 / 255.0;
        let destination_value = destination[channel] as f32 / 255.0;
        let output = (source_value * source_alpha
            + destination_value * destination_alpha * (1.0 - source_alpha))
            / output_alpha;
        destination[channel] = (output * 255.0).round() as u8;
    }
    destination[3] = (output_alpha * 255.0).round() as u8;
}

fn rgba(color: Color) -> Rgba<u8> {
    Rgba([color.r, color.g, color.b, color.a])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::CanvasPoint;
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
            lod: 0,
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
            lod: 0,
        };
        let image = render_tile(&[erase, newer], &key, Color::WHITE);
        let center = image.get_pixel(TILE_BLEED + 256, TILE_BLEED + 256);
        assert!(center[0] < 30, "pixel={center:?}");
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
                lod: 0,
            },
            Color::WHITE,
        );
        let child = render_tile(
            std::slice::from_ref(&stroke),
            &TileKey {
                depth: 1,
                x: 4.into(),
                y: 4.into(),
                lod: 0,
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
                lod: 0,
            },
            Color::WHITE,
        );
        let child = render_tile(
            std::slice::from_ref(&fill),
            &TileKey {
                depth: 1,
                x: 4.into(),
                y: 4.into(),
                lod: 0,
            },
            Color::WHITE,
        );

        assert!(dark_pixel_count(&parent) > 0);
        assert!(dark_pixel_count(&child) > 0);
    }

    #[test]
    fn high_zoom_lod_has_four_times_the_linear_resolution() {
        let stroke = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![point(0, 0.45, 0.55), point(0, 0.55, 0.55)],
            Color::BLACK,
            8.0,
        );
        let image = render_tile(
            &[stroke],
            &TileKey {
                depth: 0,
                x: 0.into(),
                y: 0.into(),
                lod: 2,
            },
            Color::WHITE,
        );
        assert_eq!(image.width(), TILE_SIZE * 4 + TILE_BLEED * 2);
        assert!(dark_pixel_count(&image) > 0);
    }

    fn dark_pixel_count(image: &RgbaImage) -> usize {
        image
            .pixels()
            .filter(|pixel| pixel[0] < 80 && pixel[1] < 80 && pixel[2] < 80)
            .count()
    }
}
