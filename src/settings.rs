use crate::raster::RasterOptions;
use crate::tile_cache::{
    DEFAULT_TILE_RESOLUTION, PngCompression as CachePngCompression, TileCacheOptions,
    nearest_tile_resolution,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::time::Duration;

const DEFAULT_BRUSH_INPUT_SPACING_PX: f32 = 3.0;
const DEFAULT_FILL_INPUT_SPACING_PX: f32 = 2.0;
const DEFAULT_FILL_FALLBACK_MAX_POINTS: usize = 4096;
const DEFAULT_FILL_FALLBACK_MAX_DEPTH_DELTA: i64 = 6;
const DEFAULT_FAST_ZOOM_TILE_SETTLE_MS: u64 = 140;
const DEFAULT_TILE_WORKER_COUNT: usize = 1;
const DEFAULT_TILE_PREFETCH_RADIUS: u8 = 1;
const DEFAULT_CACHE_SIZE_MIB: u32 = 2048;
const DEFAULT_PREVIEW_FPS: u32 = 60;
const TILE_REBUILD_IDLE_MS: u64 = 180;
const CURRENT_SETTINGS_VERSION: u32 = 10;

pub const MIN_BRUSH_INPUT_SPACING_PX: f32 = 0.75;
pub const MAX_BRUSH_INPUT_SPACING_PX: f32 = 8.0;
pub const MIN_FILL_INPUT_SPACING_PX: f32 = 1.0;
pub const MAX_FILL_INPUT_SPACING_PX: f32 = 4.0;
const MIN_FILL_FALLBACK_MAX_POINTS: usize = 128;
const MAX_FILL_FALLBACK_MAX_POINTS: usize = 65_536;
const MIN_FILL_FALLBACK_MAX_DEPTH_DELTA: i64 = 0;
const MAX_FILL_FALLBACK_MAX_DEPTH_DELTA: i64 = 32;
const MIN_FAST_ZOOM_TILE_SETTLE_MS: u64 = 0;
const MAX_FAST_ZOOM_TILE_SETTLE_MS: u64 = 1_000;
const MIN_TILE_WORKER_COUNT: usize = 1;
const MAX_TILE_WORKER_COUNT: usize = 4;
pub const MAX_TILE_PREFETCH_RADIUS: u8 = 2;
pub const MIN_CACHE_SIZE_MIB: u32 = 128;
pub const MAX_CACHE_SIZE_MIB: u32 = 8192;
pub const MIN_PREVIEW_FPS: u32 = 15;
pub const MAX_PREVIEW_FPS: u32 = 120;

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PngCompression {
    #[default]
    Fast,
    Balanced,
    Small,
}

impl PngCompression {
    pub const ALL: [Self; 3] = [Self::Fast, Self::Balanced, Self::Small];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Fast => "Fast",
            Self::Balanced => "Balanced",
            Self::Small => "Small",
        }
    }

    const fn cache_value(self) -> CachePngCompression {
        match self {
            Self::Fast => CachePngCompression::Fast,
            Self::Balanced => CachePngCompression::Balanced,
            Self::Small => CachePngCompression::Small,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EdgeQuality {
    Performance,
    #[default]
    Balanced,
    Quality,
}

impl EdgeQuality {
    pub const ALL: [Self; 3] = [Self::Performance, Self::Balanced, Self::Quality];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Performance => "Performance",
            Self::Balanced => "Balanced",
            Self::Quality => "Quality",
        }
    }

    const fn raster_value(self) -> u8 {
        match self {
            Self::Performance => 0,
            Self::Balanced => 1,
            Self::Quality => 2,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SmoothingLevel {
    Off,
    Light,
    #[default]
    Balanced,
    Strong,
}

impl SmoothingLevel {
    pub const ALL: [Self; 4] = [Self::Off, Self::Light, Self::Balanced, Self::Strong];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Light => "Light",
            Self::Balanced => "Balanced",
            Self::Strong => "Strong",
        }
    }

    pub const fn passes(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Light => 1,
            Self::Balanced => 2,
            Self::Strong => 3,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TileRebuildPolicy {
    #[default]
    Immediate,
    AfterInteraction,
}

impl TileRebuildPolicy {
    pub const ALL: [Self; 2] = [Self::Immediate, Self::AfterInteraction];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Immediate => "Immediate",
            Self::AfterInteraction => "After interaction",
        }
    }

    pub fn idle_interval(self) -> Option<Duration> {
        match self {
            Self::Immediate => None,
            Self::AfterInteraction => Some(Duration::from_millis(TILE_REBUILD_IDLE_MS)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerformanceProfile {
    Performance,
    Balanced,
    Quality,
    Custom,
}

impl PerformanceProfile {
    pub const PRESETS: [Self; 3] = [Self::Performance, Self::Balanced, Self::Quality];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Performance => "Performance",
            Self::Balanced => "Balanced",
            Self::Quality => "Quality",
            Self::Custom => "Custom",
        }
    }

    const fn values(self) -> Option<(u32, usize, u64)> {
        match self {
            Self::Performance => Some((128, 1, 250)),
            Self::Balanced => Some((512, 1, 140)),
            Self::Quality => Some((1024, 2, 80)),
            Self::Custom => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AppSettings {
    pub settings_version: u32,
    #[serde(alias = "brush_point_spacing_px")]
    pub brush_input_spacing_px: f32,
    #[serde(alias = "fill_point_spacing_px")]
    pub fill_input_spacing_px: f32,
    pub fill_fallback_max_points: usize,
    pub fill_fallback_max_depth_delta: i64,
    pub fast_zoom_tile_settle_ms: u64,
    pub tile_worker_count: usize,
    pub tile_resolution_px: u32,
    pub pause_tile_generation: bool,
    pub pause_tile_generation_while_drawing: bool,
    pub deferred_drawing_preview: bool,
    pub tile_rebuild_policy: TileRebuildPolicy,
    pub tile_prefetch_radius: u8,
    pub edge_quality: EdgeQuality,
    pub smoothing: SmoothingLevel,
    pub cache_size_mib: u32,
    pub png_compression: PngCompression,
    pub preview_fps: u32,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            settings_version: CURRENT_SETTINGS_VERSION,
            brush_input_spacing_px: DEFAULT_BRUSH_INPUT_SPACING_PX,
            fill_input_spacing_px: DEFAULT_FILL_INPUT_SPACING_PX,
            fill_fallback_max_points: DEFAULT_FILL_FALLBACK_MAX_POINTS,
            fill_fallback_max_depth_delta: DEFAULT_FILL_FALLBACK_MAX_DEPTH_DELTA,
            fast_zoom_tile_settle_ms: DEFAULT_FAST_ZOOM_TILE_SETTLE_MS,
            tile_worker_count: DEFAULT_TILE_WORKER_COUNT,
            tile_resolution_px: DEFAULT_TILE_RESOLUTION,
            pause_tile_generation: false,
            pause_tile_generation_while_drawing: false,
            deferred_drawing_preview: false,
            tile_rebuild_policy: TileRebuildPolicy::Immediate,
            tile_prefetch_radius: DEFAULT_TILE_PREFETCH_RADIUS,
            edge_quality: EdgeQuality::Balanced,
            smoothing: SmoothingLevel::Balanced,
            cache_size_mib: DEFAULT_CACHE_SIZE_MIB,
            png_compression: PngCompression::Fast,
            preview_fps: DEFAULT_PREVIEW_FPS,
        }
    }
}

impl AppSettings {
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if path.exists() {
            return Self::load(path);
        }
        let settings = Self::default();
        settings.save(path)?;
        Ok(settings)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path)
            .with_context(|| format!("failed to read settings from {}", path.display()))?;
        let settings: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse settings from {}", path.display()))?;
        Ok(settings.normalized())
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create settings directory {}", parent.display())
            })?;
        }
        let json = serde_json::to_vec_pretty(&self.clone().normalized())?;
        fs::write(path, json)
            .with_context(|| format!("failed to write settings to {}", path.display()))?;
        Ok(())
    }

    pub fn normalize(&mut self) {
        *self = self.clone().normalized();
    }

    pub fn normalized(mut self) -> Self {
        if self.settings_version < 9 {
            self.brush_input_spacing_px = DEFAULT_BRUSH_INPUT_SPACING_PX;
            self.fill_input_spacing_px = DEFAULT_FILL_INPUT_SPACING_PX;
        }
        self.settings_version = CURRENT_SETTINGS_VERSION;
        self.brush_input_spacing_px = finite_clamp(
            self.brush_input_spacing_px,
            DEFAULT_BRUSH_INPUT_SPACING_PX,
            MIN_BRUSH_INPUT_SPACING_PX,
            MAX_BRUSH_INPUT_SPACING_PX,
        );
        self.fill_input_spacing_px = finite_clamp(
            self.fill_input_spacing_px,
            DEFAULT_FILL_INPUT_SPACING_PX,
            MIN_FILL_INPUT_SPACING_PX,
            MAX_FILL_INPUT_SPACING_PX,
        );
        self.fill_fallback_max_points = self
            .fill_fallback_max_points
            .clamp(MIN_FILL_FALLBACK_MAX_POINTS, MAX_FILL_FALLBACK_MAX_POINTS);
        self.fill_fallback_max_depth_delta = self.fill_fallback_max_depth_delta.clamp(
            MIN_FILL_FALLBACK_MAX_DEPTH_DELTA,
            MAX_FILL_FALLBACK_MAX_DEPTH_DELTA,
        );
        self.fast_zoom_tile_settle_ms = self
            .fast_zoom_tile_settle_ms
            .clamp(MIN_FAST_ZOOM_TILE_SETTLE_MS, MAX_FAST_ZOOM_TILE_SETTLE_MS);
        self.tile_worker_count = if self.tile_worker_count == 0 {
            DEFAULT_TILE_WORKER_COUNT
        } else {
            self.tile_worker_count
                .clamp(MIN_TILE_WORKER_COUNT, MAX_TILE_WORKER_COUNT)
        };
        self.tile_resolution_px = nearest_tile_resolution(self.tile_resolution_px);
        self.tile_prefetch_radius = self.tile_prefetch_radius.min(MAX_TILE_PREFETCH_RADIUS);
        self.cache_size_mib = self
            .cache_size_mib
            .clamp(MIN_CACHE_SIZE_MIB, MAX_CACHE_SIZE_MIB);
        self.preview_fps = self.preview_fps.clamp(MIN_PREVIEW_FPS, MAX_PREVIEW_FPS);
        self
    }

    pub fn fill_draft_max_points(&self) -> usize {
        self.fill_fallback_max_points.saturating_sub(1)
    }

    pub fn fast_zoom_tile_settle_interval(&self) -> Duration {
        Duration::from_millis(self.fast_zoom_tile_settle_ms)
    }

    pub fn raster_options(&self) -> RasterOptions {
        RasterOptions::new(self.edge_quality.raster_value(), self.smoothing.passes())
    }

    pub fn tile_cache_options(&self) -> TileCacheOptions {
        TileCacheOptions {
            render_options: self.raster_options(),
            max_cache_bytes: u64::from(self.cache_size_mib) * 1024 * 1024,
            png_compression: self.png_compression.cache_value(),
        }
    }

    pub fn preview_repaint_interval(&self) -> Duration {
        Duration::from_secs_f64(1.0 / f64::from(self.preview_fps))
    }

    pub fn performance_profile(&self) -> PerformanceProfile {
        PerformanceProfile::PRESETS
            .into_iter()
            .find(|profile| {
                profile
                    .values()
                    .is_some_and(|(resolution, workers, settle_ms)| {
                        self.tile_resolution_px == resolution
                            && self.tile_worker_count == workers
                            && self.fast_zoom_tile_settle_ms == settle_ms
                    })
            })
            .unwrap_or(PerformanceProfile::Custom)
    }

    pub fn apply_performance_profile(&mut self, profile: PerformanceProfile) {
        let Some((resolution, workers, settle_ms)) = profile.values() else {
            return;
        };
        self.tile_resolution_px = resolution;
        self.tile_worker_count = workers;
        self.fast_zoom_tile_settle_ms = settle_ms;
    }
}

fn finite_clamp(value: f32, default: f32, min: f32, max: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        default
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppSettings, EdgeQuality, MAX_CACHE_SIZE_MIB, MAX_PREVIEW_FPS, MAX_TILE_PREFETCH_RADIUS,
        PerformanceProfile, PngCompression, SmoothingLevel, TileRebuildPolicy,
    };
    use crate::tile_cache::PngCompression as CachePngCompression;
    use std::time::Duration;

    #[test]
    fn settings_are_normalized_to_safe_ranges() {
        let settings = AppSettings {
            settings_version: AppSettings::default().settings_version,
            brush_input_spacing_px: f32::NAN,
            fill_input_spacing_px: 10_000.0,
            fill_fallback_max_points: 1,
            fill_fallback_max_depth_delta: 100,
            fast_zoom_tile_settle_ms: 10_000,
            tile_worker_count: 100,
            tile_resolution_px: 300,
            pause_tile_generation: true,
            pause_tile_generation_while_drawing: true,
            deferred_drawing_preview: true,
            tile_rebuild_policy: TileRebuildPolicy::AfterInteraction,
            tile_prefetch_radius: u8::MAX,
            edge_quality: EdgeQuality::Quality,
            smoothing: SmoothingLevel::Strong,
            cache_size_mib: u32::MAX,
            png_compression: PngCompression::Small,
            preview_fps: u32::MAX,
        }
        .normalized();

        assert_eq!(
            settings.settings_version,
            AppSettings::default().settings_version
        );
        assert_eq!(
            settings.brush_input_spacing_px,
            AppSettings::default().brush_input_spacing_px
        );
        assert_eq!(settings.fill_input_spacing_px, 4.0);
        assert_eq!(settings.fill_fallback_max_points, 128);
        assert_eq!(settings.fill_fallback_max_depth_delta, 32);
        assert_eq!(settings.fast_zoom_tile_settle_ms, 1_000);
        assert_eq!(settings.tile_worker_count, 4);
        assert_eq!(settings.tile_resolution_px, 256);
        assert!(settings.pause_tile_generation);
        assert!(settings.pause_tile_generation_while_drawing);
        assert!(settings.deferred_drawing_preview);
        assert_eq!(
            settings.tile_rebuild_policy,
            TileRebuildPolicy::AfterInteraction
        );
        assert_eq!(settings.tile_prefetch_radius, MAX_TILE_PREFETCH_RADIUS);
        assert_eq!(settings.edge_quality, EdgeQuality::Quality);
        assert_eq!(settings.smoothing, SmoothingLevel::Strong);
        assert_eq!(settings.cache_size_mib, MAX_CACHE_SIZE_MIB);
        assert_eq!(settings.png_compression, PngCompression::Small);
        assert_eq!(settings.preview_fps, MAX_PREVIEW_FPS);
    }

    #[test]
    fn legacy_spacing_semantics_reset_to_safe_input_defaults() {
        let settings = AppSettings {
            settings_version: 8,
            brush_input_spacing_px: 1024.0,
            fill_input_spacing_px: 120.0,
            tile_worker_count: 0,
            ..AppSettings::default()
        }
        .normalized();

        assert_eq!(
            settings.brush_input_spacing_px,
            AppSettings::default().brush_input_spacing_px
        );
        assert_eq!(
            settings.fill_input_spacing_px,
            AppSettings::default().fill_input_spacing_px
        );
        assert_eq!(
            settings.tile_worker_count,
            AppSettings::default().tile_worker_count
        );
    }

    #[test]
    fn settings_load_or_create_round_trips_json() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let path = temp_dir.path().join("settings.json");
        let settings = AppSettings::load_or_create(&path).expect("create settings");

        assert_eq!(settings, AppSettings::default());
        assert!(path.exists());

        let mut changed = settings;
        changed.brush_input_spacing_px = 1.5;
        changed.fill_input_spacing_px = 1.25;
        changed.fast_zoom_tile_settle_ms = 50;
        changed.tile_resolution_px = 64;
        changed.pause_tile_generation = true;
        changed.pause_tile_generation_while_drawing = true;
        changed.deferred_drawing_preview = true;
        changed.tile_rebuild_policy = TileRebuildPolicy::AfterInteraction;
        changed.tile_prefetch_radius = 2;
        changed.edge_quality = EdgeQuality::Performance;
        changed.smoothing = SmoothingLevel::Off;
        changed.cache_size_mib = 512;
        changed.png_compression = PngCompression::Balanced;
        changed.preview_fps = 30;
        changed.save(&path).expect("save settings");

        assert_eq!(
            AppSettings::load_or_create(&path).expect("reload settings"),
            changed
        );
    }

    #[test]
    fn version_two_settings_without_resolution_migrate_to_512_px() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let path = temp_dir.path().join("settings.json");
        std::fs::write(
            &path,
            br#"{
                "settings_version": 2,
                "brush_point_spacing_px": 8.0,
                "fill_point_spacing_px": 12.0,
                "fill_fallback_max_points": 4096,
                "fill_fallback_max_depth_delta": 6,
                "fast_zoom_tile_settle_ms": 140,
                "tile_worker_count": 1
            }"#,
        )
        .expect("write legacy settings");

        let settings = AppSettings::load(&path).expect("load legacy settings");

        assert_eq!(settings.settings_version, 10);
        assert_eq!(settings.tile_resolution_px, 512);
        assert!(!settings.pause_tile_generation);
        assert!(!settings.pause_tile_generation_while_drawing);
        assert!(!settings.deferred_drawing_preview);
        assert_eq!(settings.tile_rebuild_policy, TileRebuildPolicy::Immediate);
        assert_eq!(settings.tile_prefetch_radius, 1);
        assert_eq!(settings.edge_quality, EdgeQuality::Balanced);
        assert_eq!(settings.smoothing, SmoothingLevel::Balanced);
        assert_eq!(settings.brush_input_spacing_px, 3.0);
        assert_eq!(settings.fill_input_spacing_px, 2.0);
        assert_eq!(settings.cache_size_mib, 2048);
        assert_eq!(settings.png_compression, PngCompression::Fast);
        assert_eq!(settings.preview_fps, 60);
    }

    #[test]
    fn performance_profiles_apply_exact_values_and_detect_custom_changes() {
        let mut settings = AppSettings::default();

        assert_eq!(settings.performance_profile(), PerformanceProfile::Balanced);
        settings.apply_performance_profile(PerformanceProfile::Performance);
        assert_eq!(settings.tile_resolution_px, 128);
        assert_eq!(settings.tile_worker_count, 1);
        assert_eq!(settings.fast_zoom_tile_settle_ms, 250);
        assert_eq!(
            settings.performance_profile(),
            PerformanceProfile::Performance
        );

        settings.apply_performance_profile(PerformanceProfile::Quality);
        assert_eq!(settings.tile_resolution_px, 1024);
        assert_eq!(settings.tile_worker_count, 2);
        assert_eq!(settings.fast_zoom_tile_settle_ms, 80);
        assert_eq!(settings.performance_profile(), PerformanceProfile::Quality);

        settings.fast_zoom_tile_settle_ms = 81;
        assert_eq!(settings.performance_profile(), PerformanceProfile::Custom);
        settings.apply_performance_profile(PerformanceProfile::Custom);
        assert_eq!(settings.fast_zoom_tile_settle_ms, 81);
    }

    #[test]
    fn performance_profiles_leave_unmanaged_settings_unchanged() {
        let mut settings = AppSettings {
            brush_input_spacing_px: 7.0,
            fill_input_spacing_px: 3.5,
            fill_fallback_max_points: 777,
            fill_fallback_max_depth_delta: 11,
            pause_tile_generation: true,
            pause_tile_generation_while_drawing: true,
            deferred_drawing_preview: true,
            tile_rebuild_policy: TileRebuildPolicy::AfterInteraction,
            tile_prefetch_radius: 2,
            edge_quality: EdgeQuality::Performance,
            smoothing: SmoothingLevel::Strong,
            cache_size_mib: 512,
            png_compression: PngCompression::Small,
            preview_fps: 30,
            ..AppSettings::default()
        };

        settings.apply_performance_profile(PerformanceProfile::Quality);

        assert_eq!(settings.brush_input_spacing_px, 7.0);
        assert_eq!(settings.fill_input_spacing_px, 3.5);
        assert_eq!(settings.fill_fallback_max_points, 777);
        assert_eq!(settings.fill_fallback_max_depth_delta, 11);
        assert!(settings.pause_tile_generation);
        assert!(settings.pause_tile_generation_while_drawing);
        assert!(settings.deferred_drawing_preview);
        assert_eq!(
            settings.tile_rebuild_policy,
            TileRebuildPolicy::AfterInteraction
        );
        assert_eq!(settings.tile_prefetch_radius, 2);
        assert_eq!(settings.edge_quality, EdgeQuality::Performance);
        assert_eq!(settings.smoothing, SmoothingLevel::Strong);
        assert_eq!(settings.cache_size_mib, 512);
        assert_eq!(settings.png_compression, PngCompression::Small);
        assert_eq!(settings.preview_fps, 30);
    }

    #[test]
    fn performance_profile_is_derived_after_settings_reload() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let path = temp_dir.path().join("settings.json");
        let mut settings = AppSettings::default();
        settings.apply_performance_profile(PerformanceProfile::Performance);
        settings.save(&path).expect("save profile");

        let reloaded = AppSettings::load(&path).expect("reload profile");

        assert_eq!(
            reloaded.performance_profile(),
            PerformanceProfile::Performance
        );
    }

    #[test]
    fn edge_quality_and_smoothing_map_to_independent_raster_options() {
        let mut settings = AppSettings::default();
        assert_eq!(settings.raster_options().cache_tag(), "e1_s2");

        settings.edge_quality = EdgeQuality::Quality;
        settings.smoothing = SmoothingLevel::Off;
        assert_eq!(settings.raster_options().cache_tag(), "e2_s0");

        settings.edge_quality = EdgeQuality::Performance;
        settings.smoothing = SmoothingLevel::Strong;
        assert_eq!(settings.raster_options().cache_tag(), "e0_s3");
    }

    #[test]
    fn cache_and_preview_settings_map_to_runtime_options() {
        let settings = AppSettings {
            cache_size_mib: 512,
            png_compression: PngCompression::Small,
            preview_fps: 30,
            ..AppSettings::default()
        };
        let cache = settings.tile_cache_options();

        assert_eq!(cache.max_cache_bytes, 512 * 1024 * 1024);
        assert_eq!(cache.png_compression, CachePngCompression::Small);
        assert_eq!(
            settings.preview_repaint_interval(),
            Duration::from_secs_f64(1.0 / 30.0)
        );
    }
}
