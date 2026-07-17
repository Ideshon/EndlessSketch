use crate::coords::{CameraAddress, DEPTH_RATIO, TILE_PIXELS};
use crate::model::EditOperation;
use crate::tile_cache::TileKey;
use egui::{Pos2, Rect};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

const MAX_ANCHOR_TILE_DISTANCE: i64 = 64;

#[derive(Debug, Clone, Copy)]
pub struct ProjectionFrame {
    generation: u64,
    reference_center_x: f64,
    reference_center_y: f64,
    screen_center: Pos2,
    screen_scale: f64,
}

#[derive(Default)]
pub struct ProjectedGeometryCache {
    anchor: Option<CameraAddress>,
    generation: u64,
    points: HashMap<Uuid, Vec<(f64, f64)>>,
}

impl ProjectedGeometryCache {
    pub fn begin_frame(
        &mut self,
        camera: &CameraAddress,
        viewport: Rect,
    ) -> Option<ProjectionFrame> {
        let should_reset = self.anchor.as_ref().is_none_or(|anchor| {
            let Some((center_x, center_y)) =
                anchor.canvas_to_screen(&camera.center_point(), 0.0, 0.0)
            else {
                return true;
            };
            anchor_center_distance_exceeds(center_x, center_y)
                || projection_screen_scale(anchor.depth, camera.depth, camera.zoom).is_none()
        });
        if should_reset {
            self.reset_anchor(camera);
        }

        let anchor = self.anchor.as_ref()?;
        let current_center = camera.center_point();
        let (reference_center_x, reference_center_y) =
            anchor.canvas_to_screen(&current_center, 0.0, 0.0)?;
        let screen_scale = projection_screen_scale(anchor.depth, camera.depth, camera.zoom)?;
        Some(ProjectionFrame {
            generation: self.generation,
            reference_center_x,
            reference_center_y,
            screen_center: viewport.center(),
            screen_scale,
        })
    }

    pub fn project_operation(
        &mut self,
        operation: &EditOperation,
        frame: ProjectionFrame,
    ) -> Vec<Pos2> {
        if frame.generation != self.generation {
            return Vec::new();
        }
        let Some(anchor) = self.anchor.as_ref() else {
            return Vec::new();
        };
        let reference_points = self.points.entry(operation.id).or_insert_with(|| {
            operation
                .points
                .iter()
                .filter_map(|point| anchor.canvas_to_screen(point, 0.0, 0.0))
                .collect()
        });
        reference_points
            .iter()
            .filter_map(|(x, y)| {
                let screen_x = f64::from(frame.screen_center.x)
                    + (x - frame.reference_center_x) * frame.screen_scale;
                let screen_y = f64::from(frame.screen_center.y)
                    + (y - frame.reference_center_y) * frame.screen_scale;
                (screen_x.is_finite() && screen_y.is_finite())
                    .then(|| Pos2::new(screen_x as f32, screen_y as f32))
            })
            .collect()
    }

    pub fn clear(&mut self) {
        self.anchor = None;
        self.points.clear();
        self.generation = self.generation.saturating_add(1);
    }

    fn reset_anchor(&mut self, camera: &CameraAddress) {
        let mut anchor = camera.clone();
        anchor.zoom = 1.0;
        self.anchor = Some(anchor);
        self.points.clear();
        self.generation = self.generation.saturating_add(1);
    }

    #[cfg(test)]
    fn cached_operation_count(&self) -> usize {
        self.points.len()
    }
}

#[derive(Default)]
pub struct VisibleOperationCache {
    revision: Option<u64>,
    tiles: Vec<TileKey>,
    indices: Vec<usize>,
}

impl VisibleOperationCache {
    pub fn get_or_update(
        &mut self,
        revision: u64,
        tiles: &[TileKey],
        query: impl FnOnce() -> Vec<usize>,
    ) -> &[usize] {
        if self.revision != Some(revision) || self.tiles != tiles {
            self.revision = Some(revision);
            self.tiles = tiles.to_vec();
            self.indices = query();
        }
        &self.indices
    }

    pub fn clear(&mut self) {
        self.revision = None;
        self.tiles.clear();
        self.indices.clear();
    }
}

#[derive(Default)]
pub struct VisibleRenderOperationCache {
    revision: Option<u64>,
    tiles: Vec<TileKey>,
    operations: Arc<[EditOperation]>,
}

impl VisibleRenderOperationCache {
    pub fn get_or_update(
        &mut self,
        revision: u64,
        tiles: &[TileKey],
        query: impl FnOnce() -> Vec<EditOperation>,
    ) -> Arc<[EditOperation]> {
        if self.revision != Some(revision) || self.tiles != tiles {
            self.revision = Some(revision);
            self.tiles = tiles.to_vec();
            self.operations = Arc::<[EditOperation]>::from(query());
        }
        Arc::clone(&self.operations)
    }

    pub fn clear(&mut self) {
        self.revision = None;
        self.tiles.clear();
        self.operations = Arc::<[EditOperation]>::default();
    }
}

fn anchor_center_distance_exceeds(center_x: f64, center_y: f64) -> bool {
    if !center_x.is_finite() || !center_y.is_finite() {
        return true;
    }
    let max_distance = MAX_ANCHOR_TILE_DISTANCE as f64 * TILE_PIXELS;
    center_x.abs() > max_distance || center_y.abs() > max_distance
}

fn projection_screen_scale(anchor_depth: i64, camera_depth: i64, camera_zoom: f64) -> Option<f64> {
    if !camera_zoom.is_finite() || camera_zoom <= 0.0 {
        return None;
    }
    let depth_delta = camera_depth.checked_sub(anchor_depth)?;
    let levels = depth_delta.unsigned_abs();
    let depth_scale = (DEPTH_RATIO as f64).powi(i32::try_from(levels).ok()?);
    if !depth_scale.is_finite() || depth_scale <= 0.0 {
        return None;
    }
    let scale = if depth_delta >= 0 {
        camera_zoom * depth_scale
    } else {
        camera_zoom / depth_scale
    };
    (scale.is_finite() && scale > 0.0).then_some(scale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::{CanvasPoint, DEPTH_RATIO};
    use crate::model::{Color, EditKind};
    use num_bigint::BigInt;

    fn operation() -> EditOperation {
        EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![
                CanvasPoint::new(-1, 0.into(), 0.into(), 0.25, 0.75),
                CanvasPoint::new(0, 0.into(), 0.into(), 0.5, 0.5),
                CanvasPoint::new(1, 4.into(), 4.into(), 0.75, 0.25),
            ],
            Color::BLACK,
            8.0,
        )
    }

    fn assert_matches_direct_projection(
        cache: &mut ProjectedGeometryCache,
        camera: &CameraAddress,
        viewport: Rect,
        operation: &EditOperation,
    ) {
        let frame = cache
            .begin_frame(camera, viewport)
            .expect("projection frame");
        let cached = cache.project_operation(operation, frame);
        let direct: Vec<_> = operation
            .points
            .iter()
            .filter_map(|point| {
                camera
                    .canvas_to_screen(
                        point,
                        f64::from(viewport.width()),
                        f64::from(viewport.height()),
                    )
                    .map(|(x, y)| Pos2::new(viewport.left() + x as f32, viewport.top() + y as f32))
            })
            .collect();

        assert_eq!(cached.len(), direct.len());
        for (cached, direct) in cached.iter().zip(direct) {
            assert!(cached.distance(direct) < 0.01, "{cached:?} != {direct:?}");
        }
    }

    #[test]
    fn cached_projection_matches_direct_projection_across_pan_and_zoom() {
        let operation = operation();
        let viewport = Rect::from_min_max(Pos2::new(10.0, 20.0), Pos2::new(1290.0, 740.0));
        let mut cache = ProjectedGeometryCache::default();
        let mut camera = CameraAddress::default();

        assert_matches_direct_projection(&mut cache, &camera, viewport, &operation);
        camera.pan_content_by(143.0, -89.0);
        camera.zoom = 3.25;
        assert_matches_direct_projection(&mut cache, &camera, viewport, &operation);
        assert_eq!(cache.cached_operation_count(), 1);
    }

    #[test]
    fn projected_geometry_survives_representable_depth_change_and_resets_after_large_pan() {
        let operation = operation();
        let viewport = Rect::from_min_max(Pos2::ZERO, Pos2::new(1280.0, 720.0));
        let mut cache = ProjectedGeometryCache::default();
        let camera = CameraAddress::default();
        let frame = cache
            .begin_frame(&camera, viewport)
            .expect("projection frame");
        cache.project_operation(&operation, frame);
        assert_eq!(cache.cached_operation_count(), 1);

        let mut deeper = camera.clone();
        deeper.depth += 1;
        cache
            .begin_frame(&deeper, viewport)
            .expect("depth-change frame");
        assert_eq!(cache.cached_operation_count(), 1);
        assert_matches_direct_projection(&mut cache, &deeper, viewport, &operation);
        assert_eq!(cache.cached_operation_count(), 1);

        let mut moderately_distant = deeper.clone();
        moderately_distant.tile_x += 10 * DEPTH_RATIO;
        cache
            .begin_frame(&moderately_distant, viewport)
            .expect("moderately distant frame");
        assert_eq!(cache.cached_operation_count(), 1);
        assert_matches_direct_projection(&mut cache, &moderately_distant, viewport, &operation);
        assert_eq!(cache.cached_operation_count(), 1);

        let mut distant = deeper.clone();
        distant.tile_x += (MAX_ANCHOR_TILE_DISTANCE + 1) * DEPTH_RATIO;
        let frame = cache
            .begin_frame(&distant, viewport)
            .expect("distant frame");
        assert_eq!(cache.cached_operation_count(), 0);
        cache.project_operation(&operation, frame);
        assert_eq!(cache.cached_operation_count(), 1);

        distant.tile_y += (MAX_ANCHOR_TILE_DISTANCE + 1) * DEPTH_RATIO;
        cache
            .begin_frame(&distant, viewport)
            .expect("distant y frame");
        assert_eq!(cache.cached_operation_count(), 0);
    }

    #[test]
    fn visible_operation_cache_keys_on_revision_and_tile_set() {
        let mut cache = VisibleOperationCache::default();
        let first = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: 0,
        };
        let second = TileKey {
            depth: 0,
            x: 1.into(),
            y: 0.into(),
            lod: 0,
        };
        let mut queries = 0;

        assert_eq!(
            cache.get_or_update(1, std::slice::from_ref(&first), || {
                queries += 1;
                vec![1, 2]
            }),
            &[1, 2]
        );
        cache.get_or_update(1, std::slice::from_ref(&first), || {
            queries += 1;
            vec![9]
        });
        cache.get_or_update(1, &[first.clone(), second], || {
            queries += 1;
            vec![2, 3]
        });
        cache.get_or_update(2, std::slice::from_ref(&first), || {
            queries += 1;
            vec![4]
        });

        assert_eq!(queries, 3);
        assert_eq!(cache.indices, vec![4]);
    }

    #[test]
    fn visible_render_operation_cache_reuses_shared_slice_on_hits() {
        let mut cache = VisibleRenderOperationCache::default();
        let tile = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: 0,
        };
        let operation = operation();
        let mut queries = 0;

        let first = cache.get_or_update(1, std::slice::from_ref(&tile), || {
            queries += 1;
            vec![operation.clone()]
        });
        let second = cache.get_or_update(1, std::slice::from_ref(&tile), || {
            queries += 1;
            Vec::new()
        });

        assert_eq!(queries, 1);
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(second.len(), 1);

        let third = cache.get_or_update(2, std::slice::from_ref(&tile), || {
            queries += 1;
            Vec::new()
        });
        assert_eq!(queries, 2);
        assert!(!Arc::ptr_eq(&second, &third));
        assert!(third.is_empty());
    }

    #[test]
    fn projected_center_survives_minus_to_plus_hundred_depth_resets() {
        let viewport = Rect::from_min_max(Pos2::ZERO, Pos2::new(1280.0, 720.0));
        let mut camera = CameraAddress::default();
        let center = camera.center_point();
        let operation = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![center.clone(), center],
            Color::BLACK,
            8.0,
        );
        let mut cache = ProjectedGeometryCache::default();

        for _ in 0..100 {
            camera.zoom_at(1.0 / DEPTH_RATIO as f64, 640.0, 360.0, 1280.0, 720.0);
        }
        assert_eq!(camera.depth, -100);
        assert_matches_direct_projection(&mut cache, &camera, viewport, &operation);

        for _ in 0..200 {
            camera.zoom_at(DEPTH_RATIO as f64, 640.0, 360.0, 1280.0, 720.0);
        }
        assert_eq!(camera.depth, 100);
        assert_matches_direct_projection(&mut cache, &camera, viewport, &operation);
        assert_eq!(cache.cached_operation_count(), 1);
    }

    #[test]
    fn projection_cache_resets_safely_across_two_thousand_levels() {
        let viewport = Rect::from_min_max(Pos2::ZERO, Pos2::new(1280.0, 720.0));
        let mut cache = ProjectedGeometryCache::default();

        for depth in [-1000, 1000] {
            let camera = CameraAddress {
                depth,
                ..CameraAddress::default()
            };
            let center = camera.center_point();
            let operation = EditOperation::draft(
                EditKind::Paint,
                depth,
                1.0,
                vec![center.clone(), center],
                Color::BLACK,
                8.0,
            );
            assert_matches_direct_projection(&mut cache, &camera, viewport, &operation);
            assert_eq!(cache.cached_operation_count(), 1);
        }
    }

    #[test]
    fn projection_cache_recomputes_after_cross_depth_zoom_round_trip() {
        let viewport = Rect::from_min_max(Pos2::ZERO, Pos2::new(1344.0, 900.0));
        let mut camera = CameraAddress {
            depth: -4,
            tile_x: 0.into(),
            tile_y: 0.into(),
            local_x: 0.42,
            local_y: 0.58,
            zoom: 2.75,
        };
        let mut cache = ProjectedGeometryCache::default();
        let points: Vec<_> = (0..96)
            .map(|index| {
                let angle = std::f64::consts::TAU * index as f64 / 96.0;
                CanvasPoint::new(
                    -4,
                    0.into(),
                    0.into(),
                    0.42 + angle.cos() * 0.018,
                    0.58 + angle.sin() * 0.018,
                )
            })
            .collect();
        let operation = EditOperation::draft(EditKind::Paint, -4, 2.75, points, Color::BLACK, 8.0);

        assert_matches_direct_projection(&mut cache, &camera, viewport, &operation);
        while camera.depth > -20 {
            camera.zoom_at(
                1.0 / DEPTH_RATIO as f64,
                287.0,
                241.0,
                f64::from(viewport.width()),
                f64::from(viewport.height()),
            );
            assert_matches_direct_projection(&mut cache, &camera, viewport, &operation);
        }
        while camera.depth < -4 {
            camera.zoom_at(
                DEPTH_RATIO as f64,
                1030.0,
                664.0,
                f64::from(viewport.width()),
                f64::from(viewport.height()),
            );
            assert_matches_direct_projection(&mut cache, &camera, viewport, &operation);
        }

        assert_eq!(camera.depth, -4);
        assert_matches_direct_projection(&mut cache, &camera, viewport, &operation);
        assert_eq!(cache.cached_operation_count(), 1);
    }

    #[test]
    fn projection_cache_resets_at_thousand_digit_lateral_offsets() {
        let viewport = Rect::from_min_max(Pos2::ZERO, Pos2::new(1280.0, 720.0));
        let mut cache = ProjectedGeometryCache::default();
        let origin_camera = CameraAddress::default();
        let origin_operation = operation();
        assert_matches_direct_projection(&mut cache, &origin_camera, viewport, &origin_operation);

        let huge = BigInt::from(10u8).pow(1000);
        let distant_camera = CameraAddress {
            tile_x: huge.clone(),
            tile_y: -huge,
            ..CameraAddress::default()
        };
        let center = distant_camera.center_point();
        let distant_operation = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![center.clone(), center],
            Color::BLACK,
            8.0,
        );
        assert_matches_direct_projection(&mut cache, &distant_camera, viewport, &distant_operation);
        assert_eq!(cache.cached_operation_count(), 1);
    }
}
