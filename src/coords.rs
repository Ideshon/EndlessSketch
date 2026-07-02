use num_bigint::BigInt;
use num_traits::{Euclid, FromPrimitive, ToPrimitive, Zero};
use serde::{Deserialize, Serialize};

pub const DEPTH_RATIO: i64 = 8;
pub const TILE_PIXELS: f64 = 512.0;
const MIN_ZOOM: f64 = 1.0;
const MAX_ZOOM: f64 = DEPTH_RATIO as f64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanvasPoint {
    pub depth: i64,
    pub tile_x: BigInt,
    pub tile_y: BigInt,
    pub local_x: f64,
    pub local_y: f64,
}

impl CanvasPoint {
    pub fn new(depth: i64, tile_x: BigInt, tile_y: BigInt, local_x: f64, local_y: f64) -> Self {
        let mut point = Self {
            depth,
            tile_x,
            tile_y,
            local_x,
            local_y,
        };
        normalize_axis(&mut point.tile_x, &mut point.local_x);
        normalize_axis(&mut point.tile_y, &mut point.local_y);
        point
    }

    pub fn translated_by_screen_delta(
        &self,
        camera_depth: i64,
        camera_zoom: f64,
        delta_x: f64,
        delta_y: f64,
    ) -> Option<Self> {
        if !camera_zoom.is_finite()
            || camera_zoom <= 0.0
            || !delta_x.is_finite()
            || !delta_y.is_finite()
        {
            return None;
        }
        let screen_scale = TILE_PIXELS * camera_zoom;
        let depth_delta = self.depth.checked_sub(camera_depth)?;
        let levels = depth_delta.unsigned_abs();
        let depth_scale = pow_ratio_f64(i64::try_from(levels).ok()?)?;
        let convert_delta = |delta: f64| {
            let camera_delta = delta / screen_scale;
            if depth_delta >= 0 {
                camera_delta * depth_scale
            } else {
                camera_delta / depth_scale
            }
        };
        let (tile_x, local_x) =
            translated_axis(&self.tile_x, self.local_x, convert_delta(delta_x))?;
        let (tile_y, local_y) =
            translated_axis(&self.tile_y, self.local_y, convert_delta(delta_y))?;
        let translated = Self::new(self.depth, tile_x, tile_y, local_x, local_y);
        if (delta_x != 0.0 || delta_y != 0.0) && translated == *self {
            return None;
        }
        Some(translated)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenAffine {
    pivot_x: f64,
    pivot_y: f64,
    m11: f64,
    m12: f64,
    m21: f64,
    m22: f64,
}

impl ScreenAffine {
    pub fn uniform_scale(pivot_x: f64, pivot_y: f64, scale: f64) -> Option<Self> {
        if !pivot_x.is_finite() || !pivot_y.is_finite() || !scale.is_finite() || scale <= 0.0 {
            return None;
        }
        Some(Self {
            pivot_x,
            pivot_y,
            m11: scale,
            m12: 0.0,
            m21: 0.0,
            m22: scale,
        })
    }

    pub fn transform_canvas_point(
        self,
        point: &CanvasPoint,
        camera: &CameraAddress,
        viewport_width: f64,
        viewport_height: f64,
    ) -> Option<CanvasPoint> {
        if !viewport_width.is_finite()
            || viewport_width <= 0.0
            || !viewport_height.is_finite()
            || viewport_height <= 0.0
        {
            return None;
        }
        let (screen_x, screen_y) =
            camera.canvas_to_screen(point, viewport_width, viewport_height)?;
        let relative_x = screen_x - self.pivot_x;
        let relative_y = screen_y - self.pivot_y;
        let transformed_x = self.pivot_x + relative_x * self.m11 + relative_y * self.m12;
        let transformed_y = self.pivot_y + relative_x * self.m21 + relative_y * self.m22;
        point.translated_by_screen_delta(
            camera.depth,
            camera.zoom,
            transformed_x - screen_x,
            transformed_y - screen_y,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraAddress {
    pub depth: i64,
    pub tile_x: BigInt,
    pub tile_y: BigInt,
    pub local_x: f64,
    pub local_y: f64,
    pub zoom: f64,
}

impl Default for CameraAddress {
    fn default() -> Self {
        Self {
            depth: 0,
            tile_x: BigInt::zero(),
            tile_y: BigInt::zero(),
            local_x: 0.5,
            local_y: 0.5,
            zoom: 1.0,
        }
    }
}

impl CameraAddress {
    pub fn center_point(&self) -> CanvasPoint {
        CanvasPoint::new(
            self.depth,
            self.tile_x.clone(),
            self.tile_y.clone(),
            self.local_x,
            self.local_y,
        )
    }

    pub fn screen_to_canvas(
        &self,
        screen_x: f64,
        screen_y: f64,
        viewport_width: f64,
        viewport_height: f64,
    ) -> CanvasPoint {
        let scale = TILE_PIXELS * self.zoom;
        CanvasPoint::new(
            self.depth,
            self.tile_x.clone(),
            self.tile_y.clone(),
            self.local_x + (screen_x - viewport_width * 0.5) / scale,
            self.local_y + (screen_y - viewport_height * 0.5) / scale,
        )
    }

    pub fn canvas_to_screen(
        &self,
        point: &CanvasPoint,
        viewport_width: f64,
        viewport_height: f64,
    ) -> Option<(f64, f64)> {
        let dx = relative_axis(
            point.depth,
            &point.tile_x,
            point.local_x,
            self.depth,
            &self.tile_x,
            self.local_x,
        )?;
        let dy = relative_axis(
            point.depth,
            &point.tile_y,
            point.local_y,
            self.depth,
            &self.tile_y,
            self.local_y,
        )?;
        let scale = TILE_PIXELS * self.zoom;
        Some((
            viewport_width * 0.5 + dx * scale,
            viewport_height * 0.5 + dy * scale,
        ))
    }

    pub fn pan_content_by(&mut self, delta_x: f64, delta_y: f64) {
        let scale = TILE_PIXELS * self.zoom;
        self.local_x -= delta_x / scale;
        self.local_y -= delta_y / scale;
        self.normalize_position();
    }

    pub fn zoom_at(
        &mut self,
        factor: f64,
        cursor_x: f64,
        cursor_y: f64,
        viewport_width: f64,
        viewport_height: f64,
    ) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let anchor = self.screen_to_canvas(cursor_x, cursor_y, viewport_width, viewport_height);
        self.zoom *= factor;
        self.normalize_zoom();
        if let Some((after_x, after_y)) =
            self.canvas_to_screen(&anchor, viewport_width, viewport_height)
        {
            self.pan_content_by(cursor_x - after_x, cursor_y - after_y);
        }
    }

    pub fn jump_to_depth(&mut self, target_depth: i64) {
        while self.depth < target_depth {
            self.depth += 1;
            promote_axis(&mut self.tile_x, &mut self.local_x);
            promote_axis(&mut self.tile_y, &mut self.local_y);
        }
        while self.depth > target_depth {
            self.depth -= 1;
            demote_axis(&mut self.tile_x, &mut self.local_x);
            demote_axis(&mut self.tile_y, &mut self.local_y);
        }
        self.normalize_position();
    }

    fn normalize_position(&mut self) {
        normalize_axis(&mut self.tile_x, &mut self.local_x);
        normalize_axis(&mut self.tile_y, &mut self.local_y);
    }

    fn normalize_zoom(&mut self) {
        while self.zoom >= MAX_ZOOM {
            self.zoom /= MAX_ZOOM;
            self.depth = self.depth.saturating_add(1);
            promote_axis(&mut self.tile_x, &mut self.local_x);
            promote_axis(&mut self.tile_y, &mut self.local_y);
        }
        while self.zoom < MIN_ZOOM {
            self.zoom *= MAX_ZOOM;
            self.depth = self.depth.saturating_sub(1);
            demote_axis(&mut self.tile_x, &mut self.local_x);
            demote_axis(&mut self.tile_y, &mut self.local_y);
        }
        self.normalize_position();
    }
}

fn normalize_axis(tile: &mut BigInt, local: &mut f64) {
    if !local.is_finite() {
        *local = 0.0;
        return;
    }
    let whole = local.floor();
    if whole != 0.0
        && let Some(whole_i64) = whole.to_i64()
    {
        *tile += whole_i64;
        *local -= whole;
    }
    if *local < 0.0 {
        *tile -= 1;
        *local += 1.0;
    } else if *local >= 1.0 {
        *tile += 1;
        *local -= 1.0;
    }
}

fn translated_axis(tile: &BigInt, local: f64, delta: f64) -> Option<(BigInt, f64)> {
    let translated = local + delta;
    if !translated.is_finite() {
        return None;
    }
    let whole = translated.floor();
    let whole = BigInt::from_f64(whole)?;
    Some((tile + whole, translated - translated.floor()))
}

fn promote_axis(tile: &mut BigInt, local: &mut f64) {
    *tile *= DEPTH_RATIO;
    *local *= DEPTH_RATIO as f64;
    normalize_axis(tile, local);
}

fn demote_axis(tile: &mut BigInt, local: &mut f64) {
    let ratio = BigInt::from(DEPTH_RATIO);
    let quotient = tile.div_euclid(&ratio);
    let remainder = tile.rem_euclid(&ratio);
    let remainder = remainder.to_f64().unwrap_or(0.0);
    *tile = quotient;
    *local = (remainder + *local) / DEPTH_RATIO as f64;
    normalize_axis(tile, local);
}

fn relative_axis(
    point_depth: i64,
    point_tile: &BigInt,
    point_local: f64,
    camera_depth: i64,
    camera_tile: &BigInt,
    camera_local: f64,
) -> Option<f64> {
    let delta = camera_depth.saturating_sub(point_depth);
    if delta >= 0 {
        let factor = checked_pow_bigint(delta as u64)?;
        let integer_delta = point_tile * &factor - camera_tile;
        let integer = integer_delta.to_f64()?;
        let scale = pow_ratio_f64(delta)?;
        Some(integer + point_local * scale - camera_local)
    } else {
        let levels = delta.unsigned_abs();
        let factor = checked_pow_bigint(levels)?;
        let integer_delta = point_tile - camera_tile * &factor;
        let integer = integer_delta.to_f64()?;
        let scale = pow_ratio_f64(levels as i64)?;
        Some((integer + point_local - camera_local * scale) / scale)
    }
}

fn checked_pow_bigint(exponent: u64) -> Option<BigInt> {
    if exponent > u32::MAX as u64 {
        return None;
    }
    Some(BigInt::from(DEPTH_RATIO).pow(exponent as u32))
}

fn pow_ratio_f64(exponent: i64) -> Option<f64> {
    if exponent > 340 {
        return None;
    }
    Some((DEPTH_RATIO as f64).powi(exponent as i32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn screen_round_trip_is_stable(
            depth in -1000i64..1000,
            tile_x in -1_000_000i64..1_000_000,
            tile_y in -1_000_000i64..1_000_000,
            local_x in 0.0f64..1.0,
            local_y in 0.0f64..1.0,
            screen_x in 0.0f64..1920.0,
            screen_y in 0.0f64..1080.0,
        ) {
            let camera = CameraAddress {
                depth,
                tile_x: tile_x.into(),
                tile_y: tile_y.into(),
                local_x,
                local_y,
                zoom: 2.5,
            };
            let point = camera.screen_to_canvas(screen_x, screen_y, 1920.0, 1080.0);
            let (result_x, result_y) = camera.canvas_to_screen(&point, 1920.0, 1080.0).unwrap();
            prop_assert!((result_x - screen_x).abs() < 0.001);
            prop_assert!((result_y - screen_y).abs() < 0.001);
        }
    }

    #[test]
    fn zoom_threshold_preserves_cursor_anchor() {
        let mut camera = CameraAddress::default();
        let anchor = camera.screen_to_canvas(123.0, 456.0, 1280.0, 720.0);
        for _ in 0..100 {
            camera.zoom_at(1.25, 123.0, 456.0, 1280.0, 720.0);
        }
        let (x, y) = camera
            .canvas_to_screen(&anchor, 1280.0, 720.0)
            .expect("anchor remains representable");
        assert!((x - 123.0).abs() < 0.01, "x={x}");
        assert!((y - 456.0).abs() < 0.01, "y={y}");
    }

    #[test]
    fn deep_negative_zoom_round_trip_stays_representable() {
        let mut camera = CameraAddress::default();
        let anchor = camera.screen_to_canvas(640.0, 360.0, 1280.0, 720.0);
        for _ in 0..120 {
            camera.zoom_at(0.75, 640.0, 360.0, 1280.0, 720.0);
        }
        assert!(camera.depth <= -15, "depth={}", camera.depth);
        for _ in 0..120 {
            camera.zoom_at(1.0 / 0.75, 640.0, 360.0, 1280.0, 720.0);
        }
        let (x, y) = camera
            .canvas_to_screen(&anchor, 1280.0, 720.0)
            .expect("anchor remains representable");
        assert!(x.is_finite() && y.is_finite(), "x={x} y={y}");
        assert!((-1..=1).contains(&camera.depth), "depth={}", camera.depth);
    }

    #[test]
    fn bigint_pan_has_no_distance_limit() {
        let huge = BigInt::from(10u8).pow(10_000);
        let camera = CameraAddress {
            tile_x: huge.clone(),
            tile_y: -huge,
            ..Default::default()
        };
        let center = camera.center_point();
        let (x, y) = camera
            .canvas_to_screen(&center, 1920.0, 1080.0)
            .expect("relative coordinates stay small");
        assert_eq!((x, y), (960.0, 540.0));
    }

    #[test]
    fn exact_hundred_level_zoom_keeps_center_and_screen_round_trips_stable() {
        for (target_depth, factor) in [(-100, 1.0 / DEPTH_RATIO as f64), (100, DEPTH_RATIO as f64)]
        {
            let mut camera = CameraAddress::default();
            let anchor = camera.center_point();
            for _ in 0..100 {
                camera.zoom_at(factor, 640.0, 360.0, 1280.0, 720.0);
            }

            assert_eq!(camera.depth, target_depth);
            assert_eq!(camera.zoom, 1.0);
            let (anchor_x, anchor_y) = camera
                .canvas_to_screen(&anchor, 1280.0, 720.0)
                .expect("center anchor remains representable");
            assert!((anchor_x - 640.0).abs() < 0.001, "x={anchor_x}");
            assert!((anchor_y - 360.0).abs() < 0.001, "y={anchor_y}");

            for (screen_x, screen_y) in [(0.0, 0.0), (321.5, 654.25), (1279.0, 719.0)] {
                let point = camera.screen_to_canvas(screen_x, screen_y, 1280.0, 720.0);
                let (result_x, result_y) = camera
                    .canvas_to_screen(&point, 1280.0, 720.0)
                    .expect("same-depth point remains representable");
                assert!((result_x - screen_x).abs() < 0.001, "x={result_x}");
                assert!((result_y - screen_y).abs() < 0.001, "y={result_y}");
            }
        }
    }

    #[test]
    fn direct_depth_jump_preserves_visible_center_at_each_target() {
        let original = CameraAddress {
            tile_x: BigInt::from(-123_456),
            tile_y: BigInt::from(987_654),
            local_x: 0.25,
            local_y: 0.75,
            zoom: 3.5,
            ..CameraAddress::default()
        };
        let center = original.center_point();

        for depth in [100, -100] {
            let mut camera = original.clone();
            camera.jump_to_depth(depth);
            assert_eq!(camera.depth, depth);
            assert_eq!(camera.zoom, original.zoom);
            let (x, y) = camera
                .canvas_to_screen(&center, 1280.0, 720.0)
                .expect("center remains representable");
            assert!((x - 640.0).abs() < 0.001, "depth={depth} x={x}");
            assert!((y - 360.0).abs() < 0.001, "depth={depth} y={y}");
        }
    }

    #[test]
    fn thousand_depth_same_level_round_trip_is_stable() {
        for depth in [-1000, 1000] {
            let camera = CameraAddress {
                depth,
                tile_x: BigInt::from(10u8).pow(500),
                tile_y: -BigInt::from(10u8).pow(500),
                local_x: 0.125,
                local_y: 0.875,
                zoom: 2.5,
            };
            for (screen_x, screen_y) in [(0.0, 0.0), (321.5, 654.25), (1279.0, 719.0)] {
                let point = camera.screen_to_canvas(screen_x, screen_y, 1280.0, 720.0);
                let (result_x, result_y) = camera
                    .canvas_to_screen(&point, 1280.0, 720.0)
                    .expect("same-depth point remains representable");
                assert!(
                    (result_x - screen_x).abs() < 0.001,
                    "depth={depth} x={result_x}"
                );
                assert!(
                    (result_y - screen_y).abs() < 0.001,
                    "depth={depth} y={result_y}"
                );
            }
        }
    }

    #[test]
    fn unrepresentable_cross_depth_projection_is_rejected() {
        let point = CanvasPoint::new(0, 0.into(), 0.into(), 0.5, 0.5);
        for depth in [-1000, 1000] {
            let camera = CameraAddress {
                depth,
                ..CameraAddress::default()
            };
            assert!(camera.canvas_to_screen(&point, 1280.0, 720.0).is_none());
        }
    }

    #[test]
    fn screen_delta_translation_preserves_extreme_bigint_tiles() {
        let huge = BigInt::from(10u8).pow(1000);
        let point = CanvasPoint::new(0, huge.clone(), -huge, 0.25, 0.75);

        let moved = point
            .translated_by_screen_delta(0, 2.0, 1024.0, -1024.0)
            .expect("same-depth movement is representable");

        assert_eq!(moved.tile_x, &point.tile_x + 1);
        assert_eq!(moved.tile_y, &point.tile_y - 1);
        assert!((moved.local_x - 0.25).abs() < f64::EPSILON);
        assert!((moved.local_y - 0.75).abs() < f64::EPSILON);
    }

    #[test]
    fn uniform_screen_affine_scales_around_pivot_at_extreme_coordinates() {
        let huge = BigInt::from(10u8).pow(1_000);
        let camera = CameraAddress {
            depth: 12,
            tile_x: huge.clone(),
            tile_y: -huge.clone(),
            local_x: 0.5,
            local_y: 0.5,
            zoom: 2.0,
        };
        let point = CanvasPoint::new(12, huge.clone(), -huge, 0.6, 0.7);
        let before = camera
            .canvas_to_screen(&point, 800.0, 600.0)
            .expect("project source");
        let transform = ScreenAffine::uniform_scale(400.0, 300.0, 2.0).expect("valid scale");

        let transformed = transform
            .transform_canvas_point(&point, &camera, 800.0, 600.0)
            .expect("transform point");
        let after = camera
            .canvas_to_screen(&transformed, 800.0, 600.0)
            .expect("project transformed");

        assert!((after.0 - (400.0 + (before.0 - 400.0) * 2.0)).abs() < 1.0e-8);
        assert!((after.1 - (300.0 + (before.1 - 300.0) * 2.0)).abs() < 1.0e-8);
    }

    #[test]
    fn uniform_screen_affine_rejects_invalid_scale() {
        assert!(ScreenAffine::uniform_scale(0.0, 0.0, 0.0).is_none());
        assert!(ScreenAffine::uniform_scale(0.0, 0.0, f64::NAN).is_none());
    }

    #[test]
    fn screen_delta_translation_scales_at_the_point_depth() {
        let camera = CameraAddress::default();
        let point = CanvasPoint::new(1, 4.into(), 4.into(), 0.0, 0.0);
        let before = camera
            .canvas_to_screen(&point, 1280.0, 720.0)
            .expect("point projects");

        let moved = point
            .translated_by_screen_delta(camera.depth, camera.zoom, 64.0, 0.0)
            .expect("adjacent-depth movement is representable");
        let after = camera
            .canvas_to_screen(&moved, 1280.0, 720.0)
            .expect("moved point projects");

        assert!((after.0 - before.0 - 64.0).abs() < 0.001);
        assert!((after.1 - before.1).abs() < 0.001);
    }

    #[test]
    fn screen_delta_translation_rejects_unrepresentable_depth_scale() {
        let point = CanvasPoint::new(1000, 0.into(), 0.into(), 0.5, 0.5);

        assert!(
            point
                .translated_by_screen_delta(0, 1.0, 10.0, 0.0)
                .is_none()
        );
    }
}
