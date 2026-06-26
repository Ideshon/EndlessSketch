use num_bigint::BigInt;
use num_traits::{Euclid, ToPrimitive, Zero};
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
}
