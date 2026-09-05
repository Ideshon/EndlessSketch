use crate::raster::RasterOptions;
pub use crate::tile_cache::PngCompression;
use crate::tile_cache::{DEFAULT_TILE_RESOLUTION, TileCacheOptions, nearest_tile_resolution};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::time::Duration;

const DEFAULT_BRUSH_INPUT_SPACING_PX: f32 = 3.0;
const DEFAULT_FILL_INPUT_SPACING_PX: f32 = 2.0;
const DEFAULT_FILL_FALLBACK_MAX_POINTS: usize = 4096;
const DEFAULT_FILL_FALLBACK_MAX_DEPTH_DELTA: i64 = 6;
const DEFAULT_SAVED_FALLBACK_OPERATION_LIMIT: usize = 0;
const PERFORMANCE_SAVED_FALLBACK_OPERATION_LIMIT: usize = 1_500;
const DEFAULT_FAST_ZOOM_TILE_SETTLE_MS: u64 = 140;
const DEFAULT_TILE_WORKER_COUNT: usize = 1;
const DEFAULT_TILE_PREFETCH_RADIUS: u8 = 1;
const DEFAULT_CACHE_SIZE_MIB: u32 = 2048;
const DEFAULT_PREVIEW_FPS: u32 = 60;
const DEFAULT_DEPTH_CAPTURE_RADIUS: i64 = 2;
const DEFAULT_VECTOR_DEPTH_RADIUS: i64 = 2;
const TILE_REBUILD_IDLE_MS: u64 = 180;
const CURRENT_SETTINGS_VERSION: u32 = 19;

pub const MIN_BRUSH_INPUT_SPACING_PX: f32 = 0.75;
pub const MAX_BRUSH_INPUT_SPACING_PX: f32 = 8.0;
pub const MIN_FILL_INPUT_SPACING_PX: f32 = 1.0;
pub const MAX_FILL_INPUT_SPACING_PX: f32 = 4.0;
const MIN_FILL_FALLBACK_MAX_POINTS: usize = 128;
const MAX_FILL_FALLBACK_MAX_POINTS: usize = 65_536;
const MIN_FILL_FALLBACK_MAX_DEPTH_DELTA: i64 = 0;
const MAX_FILL_FALLBACK_MAX_DEPTH_DELTA: i64 = 32;
pub const MAX_SAVED_FALLBACK_OPERATION_LIMIT: usize = 100_000;
const MIN_FAST_ZOOM_TILE_SETTLE_MS: u64 = 0;
const MAX_FAST_ZOOM_TILE_SETTLE_MS: u64 = 1_000;
const MIN_TILE_WORKER_COUNT: usize = 1;
const MAX_TILE_WORKER_COUNT: usize = 4;
pub const MAX_TILE_PREFETCH_RADIUS: u8 = 2;
pub const MIN_CACHE_SIZE_MIB: u32 = 128;
pub const MAX_CACHE_SIZE_MIB: u32 = 8192;
pub const MIN_PREVIEW_FPS: u32 = 15;
pub const MAX_PREVIEW_FPS: u32 = 120;
pub const MAX_DEPTH_CAPTURE_RADIUS: i64 = 32;
pub const MAX_VECTOR_DEPTH_RADIUS: i64 = 32;

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
pub enum StrokeFallbackJoinMode {
    #[default]
    Auto,
    Quality,
    Performance,
}

impl StrokeFallbackJoinMode {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Quality, Self::Performance];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Quality => "Quality",
            Self::Performance => "Performance",
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

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObjectCompactionLimit {
    #[default]
    Unlimited,
    Keep5000,
    Keep1000,
    Keep100,
}

impl ObjectCompactionLimit {
    pub const ALL: [Self; 4] = [
        Self::Unlimited,
        Self::Keep5000,
        Self::Keep1000,
        Self::Keep100,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Unlimited => "Unlimited",
            Self::Keep5000 => "Keep 5000",
            Self::Keep1000 => "Keep 1000",
            Self::Keep100 => "Keep 100",
        }
    }

    pub const fn keep_latest(self) -> Option<usize> {
        match self {
            Self::Unlimited => None,
            Self::Keep5000 => Some(5_000),
            Self::Keep1000 => Some(1_000),
            Self::Keep100 => Some(100),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StorageCommitMode {
    #[default]
    Full,
    Fast,
}

impl StorageCommitMode {
    pub const ALL: [Self; 2] = [Self::Full, Self::Fast];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Full => "Full",
            Self::Fast => "Fast",
        }
    }

    pub const fn sqlite_synchronous(self) -> &'static str {
        match self {
            Self::Full => "FULL",
            Self::Fast => "NORMAL",
        }
    }
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionLoggingLevel {
    Off,
    #[default]
    Basic,
    Detailed,
}

impl SessionLoggingLevel {
    pub const ALL: [Self; 3] = [Self::Off, Self::Basic, Self::Detailed];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Basic => "Basic",
            Self::Detailed => "Detailed",
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

#[derive(Debug, Clone, Copy, PartialEq)]
struct PerformanceValues {
    brush_input_spacing_px: f32,
    fill_input_spacing_px: f32,
    fill_fallback_max_points: usize,
    fill_fallback_max_depth_delta: i64,
    saved_fallback_operation_limit: usize,
    fast_zoom_tile_settle_ms: u64,
    tile_worker_count: usize,
    tile_resolution_px: u32,
    pause_tile_generation_while_drawing: bool,
    deferred_drawing_preview: bool,
    tile_rebuild_policy: TileRebuildPolicy,
    tile_prefetch_radius: u8,
    edge_quality: EdgeQuality,
    smoothing: SmoothingLevel,
    stroke_fallback_joins: StrokeFallbackJoinMode,
    storage_commit_mode: StorageCommitMode,
    png_compression: PngCompression,
    preview_fps: u32,
    vector_depth_radius: i64,
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

    const fn values(self) -> Option<PerformanceValues> {
        match self {
            Self::Performance => Some(PerformanceValues {
                brush_input_spacing_px: 6.0,
                fill_input_spacing_px: 4.0,
                fill_fallback_max_points: 1024,
                fill_fallback_max_depth_delta: 3,
                saved_fallback_operation_limit: PERFORMANCE_SAVED_FALLBACK_OPERATION_LIMIT,
                fast_zoom_tile_settle_ms: 250,
                tile_worker_count: 1,
                tile_resolution_px: 128,
                pause_tile_generation_while_drawing: true,
                deferred_drawing_preview: true,
                tile_rebuild_policy: TileRebuildPolicy::AfterInteraction,
                tile_prefetch_radius: 0,
                edge_quality: EdgeQuality::Performance,
                smoothing: SmoothingLevel::Light,
                stroke_fallback_joins: StrokeFallbackJoinMode::Performance,
                storage_commit_mode: StorageCommitMode::Fast,
                png_compression: PngCompression::Fast,
                preview_fps: 30,
                vector_depth_radius: 1,
            }),
            Self::Balanced => Some(PerformanceValues {
                brush_input_spacing_px: DEFAULT_BRUSH_INPUT_SPACING_PX,
                fill_input_spacing_px: DEFAULT_FILL_INPUT_SPACING_PX,
                fill_fallback_max_points: DEFAULT_FILL_FALLBACK_MAX_POINTS,
                fill_fallback_max_depth_delta: DEFAULT_FILL_FALLBACK_MAX_DEPTH_DELTA,
                saved_fallback_operation_limit: DEFAULT_SAVED_FALLBACK_OPERATION_LIMIT,
                fast_zoom_tile_settle_ms: DEFAULT_FAST_ZOOM_TILE_SETTLE_MS,
                tile_worker_count: DEFAULT_TILE_WORKER_COUNT,
                tile_resolution_px: DEFAULT_TILE_RESOLUTION,
                pause_tile_generation_while_drawing: false,
                deferred_drawing_preview: false,
                tile_rebuild_policy: TileRebuildPolicy::Immediate,
                tile_prefetch_radius: DEFAULT_TILE_PREFETCH_RADIUS,
                edge_quality: EdgeQuality::Balanced,
                smoothing: SmoothingLevel::Balanced,
                stroke_fallback_joins: StrokeFallbackJoinMode::Auto,
                storage_commit_mode: StorageCommitMode::Full,
                png_compression: PngCompression::Fast,
                preview_fps: DEFAULT_PREVIEW_FPS,
                vector_depth_radius: DEFAULT_VECTOR_DEPTH_RADIUS,
            }),
            Self::Quality => Some(PerformanceValues {
                brush_input_spacing_px: 1.5,
                fill_input_spacing_px: 1.0,
                fill_fallback_max_points: 32_768,
                fill_fallback_max_depth_delta: 12,
                saved_fallback_operation_limit: DEFAULT_SAVED_FALLBACK_OPERATION_LIMIT,
                fast_zoom_tile_settle_ms: 80,
                tile_worker_count: 2,
                tile_resolution_px: 1024,
                pause_tile_generation_while_drawing: false,
                deferred_drawing_preview: false,
                tile_rebuild_policy: TileRebuildPolicy::Immediate,
                tile_prefetch_radius: MAX_TILE_PREFETCH_RADIUS,
                edge_quality: EdgeQuality::Quality,
                smoothing: SmoothingLevel::Strong,
                stroke_fallback_joins: StrokeFallbackJoinMode::Quality,
                storage_commit_mode: StorageCommitMode::Full,
                png_compression: PngCompression::Small,
                preview_fps: MAX_PREVIEW_FPS,
                vector_depth_radius: 4,
            }),
            Self::Custom => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayProfile {
    Minimal,
    Standard,
    Diagnostics,
    Custom,
}

impl OverlayProfile {
    pub const PRESETS: [Self; 3] = [Self::Minimal, Self::Standard, Self::Diagnostics];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Minimal => "Minimal",
            Self::Standard => "Standard",
            Self::Diagnostics => "Diagnostics",
            Self::Custom => "Custom",
        }
    }

    const fn values(self) -> Option<OverlayValues> {
        match self {
            Self::Minimal => Some(OverlayValues {
                enabled: true,
                depth: false,
                zoom: true,
                tile_coordinates: false,
                local_coordinates: false,
                operation_count: false,
                performance: false,
                status: true,
                tile_state: false,
            }),
            Self::Standard => Some(OverlayValues {
                enabled: true,
                depth: false,
                zoom: true,
                tile_coordinates: false,
                local_coordinates: false,
                operation_count: true,
                performance: true,
                status: true,
                tile_state: true,
            }),
            Self::Diagnostics => Some(OverlayValues {
                enabled: true,
                depth: true,
                zoom: true,
                tile_coordinates: true,
                local_coordinates: true,
                operation_count: true,
                performance: true,
                status: true,
                tile_state: true,
            }),
            Self::Custom => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OverlayValues {
    enabled: bool,
    depth: bool,
    zoom: bool,
    tile_coordinates: bool,
    local_coordinates: bool,
    operation_count: bool,
    performance: bool,
    status: bool,
    tile_state: bool,
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
    pub saved_fallback_operation_limit: usize,
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
    pub stroke_fallback_joins: StrokeFallbackJoinMode,
    pub storage_commit_mode: StorageCommitMode,
    pub cache_size_mib: u32,
    pub png_compression: PngCompression,
    pub preview_fps: u32,
    pub object_compaction_limit: ObjectCompactionLimit,
    pub distant_tiles_enabled: bool,
    pub vector_depth_radius: i64,
    pub depth_capture_auto: bool,
    pub depth_capture_radius: i64,
    pub overlay_enabled: bool,
    pub overlay_show_depth: bool,
    pub overlay_show_zoom: bool,
    pub overlay_show_tile_coordinates: bool,
    pub overlay_show_local_coordinates: bool,
    pub overlay_show_operation_count: bool,
    pub overlay_show_performance: bool,
    pub overlay_show_status: bool,
    pub overlay_show_tile_state: bool,
    pub session_logging: SessionLoggingLevel,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            settings_version: CURRENT_SETTINGS_VERSION,
            brush_input_spacing_px: DEFAULT_BRUSH_INPUT_SPACING_PX,
            fill_input_spacing_px: DEFAULT_FILL_INPUT_SPACING_PX,
            fill_fallback_max_points: DEFAULT_FILL_FALLBACK_MAX_POINTS,
            fill_fallback_max_depth_delta: DEFAULT_FILL_FALLBACK_MAX_DEPTH_DELTA,
            saved_fallback_operation_limit: DEFAULT_SAVED_FALLBACK_OPERATION_LIMIT,
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
            stroke_fallback_joins: StrokeFallbackJoinMode::Auto,
            storage_commit_mode: StorageCommitMode::Full,
            cache_size_mib: DEFAULT_CACHE_SIZE_MIB,
            png_compression: PngCompression::Fast,
            preview_fps: DEFAULT_PREVIEW_FPS,
            object_compaction_limit: ObjectCompactionLimit::Unlimited,
            distant_tiles_enabled: true,
            vector_depth_radius: DEFAULT_VECTOR_DEPTH_RADIUS,
            depth_capture_auto: true,
            depth_capture_radius: DEFAULT_DEPTH_CAPTURE_RADIUS,
            overlay_enabled: true,
            overlay_show_depth: false,
            overlay_show_zoom: true,
            overlay_show_tile_coordinates: false,
            overlay_show_local_coordinates: false,
            overlay_show_operation_count: true,
            overlay_show_performance: true,
            overlay_show_status: true,
            overlay_show_tile_state: true,
            session_logging: SessionLoggingLevel::Basic,
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
        self.saved_fallback_operation_limit = self
            .saved_fallback_operation_limit
            .min(MAX_SAVED_FALLBACK_OPERATION_LIMIT);
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
        self.vector_depth_radius = self.vector_depth_radius.clamp(0, MAX_VECTOR_DEPTH_RADIUS);
        self.depth_capture_radius = self.depth_capture_radius.clamp(0, MAX_DEPTH_CAPTURE_RADIUS);
        self.sync_depth_capture_radius();
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
            png_compression: self.png_compression,
        }
    }

    pub fn preview_repaint_interval(&self) -> Duration {
        Duration::from_secs_f64(1.0 / f64::from(self.preview_fps))
    }

    pub fn performance_profile(&self) -> PerformanceProfile {
        let values = self.performance_values();
        PerformanceProfile::PRESETS
            .into_iter()
            .find(|profile| profile.values() == Some(values))
            .unwrap_or(PerformanceProfile::Custom)
    }

    pub fn apply_performance_profile(&mut self, profile: PerformanceProfile) {
        let Some(values) = profile.values() else {
            return;
        };
        self.brush_input_spacing_px = values.brush_input_spacing_px;
        self.fill_input_spacing_px = values.fill_input_spacing_px;
        self.fill_fallback_max_points = values.fill_fallback_max_points;
        self.fill_fallback_max_depth_delta = values.fill_fallback_max_depth_delta;
        self.saved_fallback_operation_limit = values.saved_fallback_operation_limit;
        self.fast_zoom_tile_settle_ms = values.fast_zoom_tile_settle_ms;
        self.tile_worker_count = values.tile_worker_count;
        self.tile_resolution_px = values.tile_resolution_px;
        self.pause_tile_generation_while_drawing = values.pause_tile_generation_while_drawing;
        self.deferred_drawing_preview = values.deferred_drawing_preview;
        self.tile_rebuild_policy = values.tile_rebuild_policy;
        self.tile_prefetch_radius = values.tile_prefetch_radius;
        self.edge_quality = values.edge_quality;
        self.smoothing = values.smoothing;
        self.stroke_fallback_joins = values.stroke_fallback_joins;
        self.storage_commit_mode = values.storage_commit_mode;
        self.png_compression = values.png_compression;
        self.preview_fps = values.preview_fps;
        self.vector_depth_radius = values.vector_depth_radius;
        self.sync_depth_capture_radius();
    }

    fn sync_depth_capture_radius(&mut self) {
        self.depth_capture_radius = if self.depth_capture_auto {
            self.vector_depth_radius
        } else {
            self.depth_capture_radius.min(self.vector_depth_radius)
        };
    }

    pub fn overlay_profile(&self) -> OverlayProfile {
        let values = self.overlay_values();
        OverlayProfile::PRESETS
            .into_iter()
            .find(|profile| profile.values() == Some(values))
            .unwrap_or(OverlayProfile::Custom)
    }

    pub fn apply_overlay_profile(&mut self, profile: OverlayProfile) {
        let Some(values) = profile.values() else {
            return;
        };
        self.overlay_enabled = values.enabled;
        self.overlay_show_depth = values.depth;
        self.overlay_show_zoom = values.zoom;
        self.overlay_show_tile_coordinates = values.tile_coordinates;
        self.overlay_show_local_coordinates = values.local_coordinates;
        self.overlay_show_operation_count = values.operation_count;
        self.overlay_show_performance = values.performance;
        self.overlay_show_status = values.status;
        self.overlay_show_tile_state = values.tile_state;
    }

    fn overlay_values(&self) -> OverlayValues {
        OverlayValues {
            enabled: self.overlay_enabled,
            depth: self.overlay_show_depth,
            zoom: self.overlay_show_zoom,
            tile_coordinates: self.overlay_show_tile_coordinates,
            local_coordinates: self.overlay_show_local_coordinates,
            operation_count: self.overlay_show_operation_count,
            performance: self.overlay_show_performance,
            status: self.overlay_show_status,
            tile_state: self.overlay_show_tile_state,
        }
    }

    fn performance_values(&self) -> PerformanceValues {
        PerformanceValues {
            brush_input_spacing_px: self.brush_input_spacing_px,
            fill_input_spacing_px: self.fill_input_spacing_px,
            fill_fallback_max_points: self.fill_fallback_max_points,
            fill_fallback_max_depth_delta: self.fill_fallback_max_depth_delta,
            saved_fallback_operation_limit: self.saved_fallback_operation_limit,
            fast_zoom_tile_settle_ms: self.fast_zoom_tile_settle_ms,
            tile_worker_count: self.tile_worker_count,
            tile_resolution_px: self.tile_resolution_px,
            pause_tile_generation_while_drawing: self.pause_tile_generation_while_drawing,
            deferred_drawing_preview: self.deferred_drawing_preview,
            tile_rebuild_policy: self.tile_rebuild_policy,
            tile_prefetch_radius: self.tile_prefetch_radius,
            edge_quality: self.edge_quality,
            smoothing: self.smoothing,
            stroke_fallback_joins: self.stroke_fallback_joins,
            storage_commit_mode: self.storage_commit_mode,
            png_compression: self.png_compression,
            preview_fps: self.preview_fps,
            vector_depth_radius: self.vector_depth_radius,
        }
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
        AppSettings, EdgeQuality, MAX_CACHE_SIZE_MIB, MAX_DEPTH_CAPTURE_RADIUS, MAX_PREVIEW_FPS,
        MAX_SAVED_FALLBACK_OPERATION_LIMIT, MAX_TILE_PREFETCH_RADIUS, MAX_VECTOR_DEPTH_RADIUS,
        ObjectCompactionLimit, OverlayProfile, PerformanceProfile, PngCompression,
        SessionLoggingLevel, SmoothingLevel, StorageCommitMode, StrokeFallbackJoinMode,
        TileRebuildPolicy,
    };
    use std::time::Duration;

    #[test]
    fn settings_are_normalized_to_safe_ranges() {
        let settings = AppSettings {
            settings_version: AppSettings::default().settings_version,
            brush_input_spacing_px: f32::NAN,
            fill_input_spacing_px: 10_000.0,
            fill_fallback_max_points: 1,
            fill_fallback_max_depth_delta: 100,
            saved_fallback_operation_limit: usize::MAX,
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
            stroke_fallback_joins: StrokeFallbackJoinMode::Performance,
            storage_commit_mode: StorageCommitMode::Fast,
            cache_size_mib: u32::MAX,
            png_compression: PngCompression::Small,
            preview_fps: u32::MAX,
            object_compaction_limit: ObjectCompactionLimit::Keep1000,
            vector_depth_radius: i64::MAX,
            depth_capture_radius: i64::MAX,
            session_logging: SessionLoggingLevel::Detailed,
            ..AppSettings::default()
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
        assert_eq!(
            settings.saved_fallback_operation_limit,
            MAX_SAVED_FALLBACK_OPERATION_LIMIT
        );
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
        assert_eq!(
            settings.stroke_fallback_joins,
            StrokeFallbackJoinMode::Performance
        );
        assert_eq!(settings.storage_commit_mode, StorageCommitMode::Fast);
        assert_eq!(settings.cache_size_mib, MAX_CACHE_SIZE_MIB);
        assert_eq!(settings.png_compression, PngCompression::Small);
        assert_eq!(settings.preview_fps, MAX_PREVIEW_FPS);
        assert_eq!(
            settings.object_compaction_limit,
            ObjectCompactionLimit::Keep1000
        );
        assert_eq!(settings.vector_depth_radius, MAX_VECTOR_DEPTH_RADIUS);
        assert_eq!(settings.depth_capture_radius, MAX_DEPTH_CAPTURE_RADIUS);
        assert_eq!(settings.session_logging, SessionLoggingLevel::Detailed);
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
    fn depth_capture_follows_vector_radius_or_is_capped_by_it() {
        let automatic = AppSettings {
            vector_depth_radius: 7,
            depth_capture_auto: true,
            depth_capture_radius: 1,
            ..AppSettings::default()
        }
        .normalized();
        assert_eq!(automatic.depth_capture_radius, 7);

        let manual = AppSettings {
            vector_depth_radius: 3,
            depth_capture_auto: false,
            depth_capture_radius: 7,
            ..AppSettings::default()
        }
        .normalized();
        assert_eq!(manual.depth_capture_radius, 3);

        let manual_inside_scope = AppSettings {
            vector_depth_radius: 7,
            depth_capture_auto: false,
            depth_capture_radius: 3,
            ..AppSettings::default()
        }
        .normalized();
        assert_eq!(manual_inside_scope.depth_capture_radius, 3);
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
        changed.saved_fallback_operation_limit = 1234;
        changed.fast_zoom_tile_settle_ms = 50;
        changed.tile_resolution_px = 64;
        changed.pause_tile_generation = true;
        changed.pause_tile_generation_while_drawing = true;
        changed.deferred_drawing_preview = true;
        changed.tile_rebuild_policy = TileRebuildPolicy::AfterInteraction;
        changed.tile_prefetch_radius = 2;
        changed.edge_quality = EdgeQuality::Performance;
        changed.smoothing = SmoothingLevel::Off;
        changed.storage_commit_mode = StorageCommitMode::Fast;
        changed.cache_size_mib = 512;
        changed.png_compression = PngCompression::Balanced;
        changed.preview_fps = 30;
        changed.distant_tiles_enabled = false;
        changed.vector_depth_radius = 7;
        changed.depth_capture_auto = false;
        changed.depth_capture_radius = 5;
        changed.overlay_show_depth = true;
        changed.overlay_show_tile_coordinates = true;
        changed.overlay_show_performance = false;
        changed.session_logging = SessionLoggingLevel::Off;
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

        assert_eq!(settings.settings_version, 19);
        assert_eq!(settings.tile_resolution_px, 512);
        assert!(!settings.pause_tile_generation);
        assert!(!settings.pause_tile_generation_while_drawing);
        assert!(!settings.deferred_drawing_preview);
        assert_eq!(settings.tile_rebuild_policy, TileRebuildPolicy::Immediate);
        assert_eq!(settings.tile_prefetch_radius, 1);
        assert_eq!(settings.edge_quality, EdgeQuality::Balanced);
        assert_eq!(settings.smoothing, SmoothingLevel::Balanced);
        assert_eq!(settings.stroke_fallback_joins, StrokeFallbackJoinMode::Auto);
        assert_eq!(settings.storage_commit_mode, StorageCommitMode::Full);
        assert_eq!(settings.brush_input_spacing_px, 3.0);
        assert_eq!(settings.fill_input_spacing_px, 2.0);
        assert_eq!(settings.saved_fallback_operation_limit, 0);
        assert_eq!(settings.cache_size_mib, 2048);
        assert_eq!(settings.png_compression, PngCompression::Fast);
        assert_eq!(settings.preview_fps, 60);
        assert!(settings.distant_tiles_enabled);
        assert_eq!(settings.vector_depth_radius, 2);
        assert!(settings.depth_capture_auto);
        assert_eq!(settings.depth_capture_radius, 2);
        assert_eq!(settings.overlay_profile(), OverlayProfile::Standard);
        assert_eq!(
            settings.object_compaction_limit,
            ObjectCompactionLimit::Unlimited
        );
        assert_eq!(settings.session_logging, SessionLoggingLevel::Basic);
    }

    #[test]
    fn performance_profiles_apply_exact_values_and_detect_custom_changes() {
        let mut settings = AppSettings::default();

        assert_eq!(settings.performance_profile(), PerformanceProfile::Balanced);
        settings.apply_performance_profile(PerformanceProfile::Performance);
        assert_eq!(settings.brush_input_spacing_px, 6.0);
        assert_eq!(settings.fill_input_spacing_px, 4.0);
        assert_eq!(settings.fill_fallback_max_points, 1024);
        assert_eq!(settings.fill_fallback_max_depth_delta, 3);
        assert_eq!(settings.saved_fallback_operation_limit, 1_500);
        assert_eq!(settings.tile_resolution_px, 128);
        assert_eq!(settings.tile_worker_count, 1);
        assert_eq!(settings.fast_zoom_tile_settle_ms, 250);
        assert!(settings.pause_tile_generation_while_drawing);
        assert!(settings.deferred_drawing_preview);
        assert_eq!(
            settings.tile_rebuild_policy,
            TileRebuildPolicy::AfterInteraction
        );
        assert_eq!(settings.tile_prefetch_radius, 0);
        assert_eq!(settings.edge_quality, EdgeQuality::Performance);
        assert_eq!(settings.smoothing, SmoothingLevel::Light);
        assert_eq!(
            settings.stroke_fallback_joins,
            StrokeFallbackJoinMode::Performance
        );
        assert_eq!(settings.storage_commit_mode, StorageCommitMode::Fast);
        assert_eq!(settings.png_compression, PngCompression::Fast);
        assert_eq!(settings.preview_fps, 30);
        assert_eq!(settings.vector_depth_radius, 1);
        assert_eq!(settings.depth_capture_radius, 1);
        assert_eq!(
            settings.performance_profile(),
            PerformanceProfile::Performance
        );

        settings.apply_performance_profile(PerformanceProfile::Quality);
        assert_eq!(settings.brush_input_spacing_px, 1.5);
        assert_eq!(settings.fill_input_spacing_px, 1.0);
        assert_eq!(settings.fill_fallback_max_points, 32_768);
        assert_eq!(settings.fill_fallback_max_depth_delta, 12);
        assert_eq!(settings.saved_fallback_operation_limit, 0);
        assert_eq!(settings.tile_resolution_px, 1024);
        assert_eq!(settings.tile_worker_count, 2);
        assert_eq!(settings.fast_zoom_tile_settle_ms, 80);
        assert!(!settings.pause_tile_generation_while_drawing);
        assert!(!settings.deferred_drawing_preview);
        assert_eq!(settings.tile_rebuild_policy, TileRebuildPolicy::Immediate);
        assert_eq!(settings.tile_prefetch_radius, MAX_TILE_PREFETCH_RADIUS);
        assert_eq!(settings.edge_quality, EdgeQuality::Quality);
        assert_eq!(settings.smoothing, SmoothingLevel::Strong);
        assert_eq!(
            settings.stroke_fallback_joins,
            StrokeFallbackJoinMode::Quality
        );
        assert_eq!(settings.storage_commit_mode, StorageCommitMode::Full);
        assert_eq!(settings.png_compression, PngCompression::Small);
        assert_eq!(settings.preview_fps, MAX_PREVIEW_FPS);
        assert_eq!(settings.vector_depth_radius, 4);
        assert_eq!(settings.depth_capture_radius, 4);
        assert_eq!(settings.performance_profile(), PerformanceProfile::Quality);

        settings.fast_zoom_tile_settle_ms = 81;
        assert_eq!(settings.performance_profile(), PerformanceProfile::Custom);
        settings.apply_performance_profile(PerformanceProfile::Custom);
        assert_eq!(settings.fast_zoom_tile_settle_ms, 81);
    }

    #[test]
    fn performance_profiles_leave_unmanaged_settings_unchanged() {
        let mut settings = AppSettings {
            pause_tile_generation: true,
            cache_size_mib: 512,
            object_compaction_limit: ObjectCompactionLimit::Keep100,
            distant_tiles_enabled: false,
            depth_capture_auto: false,
            depth_capture_radius: 3,
            overlay_enabled: false,
            overlay_show_depth: true,
            session_logging: SessionLoggingLevel::Detailed,
            ..AppSettings::default()
        };

        settings.apply_performance_profile(PerformanceProfile::Quality);

        assert!(settings.pause_tile_generation);
        assert_eq!(settings.cache_size_mib, 512);
        assert_eq!(
            settings.object_compaction_limit,
            ObjectCompactionLimit::Keep100
        );
        assert!(!settings.depth_capture_auto);
        assert_eq!(settings.depth_capture_radius, 3);
        assert!(!settings.distant_tiles_enabled);
        assert!(!settings.overlay_enabled);
        assert!(settings.overlay_show_depth);
        assert_eq!(settings.session_logging, SessionLoggingLevel::Detailed);
        assert_eq!(settings.performance_profile(), PerformanceProfile::Quality);
    }

    #[test]
    fn overlay_profiles_apply_exact_values_and_detect_custom_changes() {
        let mut settings = AppSettings::default();

        assert_eq!(settings.overlay_profile(), OverlayProfile::Standard);
        settings.apply_overlay_profile(OverlayProfile::Minimal);
        assert!(settings.overlay_enabled);
        assert!(settings.overlay_show_zoom);
        assert!(settings.overlay_show_status);
        assert!(!settings.overlay_show_depth);
        assert!(!settings.overlay_show_operation_count);
        assert!(!settings.overlay_show_performance);
        assert_eq!(settings.overlay_profile(), OverlayProfile::Minimal);

        settings.apply_overlay_profile(OverlayProfile::Diagnostics);
        assert!(settings.overlay_show_depth);
        assert!(settings.overlay_show_tile_coordinates);
        assert!(settings.overlay_show_local_coordinates);
        assert!(settings.overlay_show_operation_count);
        assert!(settings.overlay_show_performance);
        assert!(settings.overlay_show_tile_state);
        assert_eq!(settings.overlay_profile(), OverlayProfile::Diagnostics);

        settings.overlay_show_local_coordinates = false;
        assert_eq!(settings.overlay_profile(), OverlayProfile::Custom);
        settings.apply_overlay_profile(OverlayProfile::Custom);
        assert!(!settings.overlay_show_local_coordinates);
    }

    #[test]
    fn version_ten_settings_migrate_to_standard_overlay() {
        let settings: AppSettings = serde_json::from_str(r#"{"settings_version": 10}"#)
            .expect("deserialize version ten settings");
        let settings = settings.normalized();

        assert_eq!(settings.settings_version, 19);
        assert_eq!(settings.overlay_profile(), OverlayProfile::Standard);
        assert_eq!(
            settings.object_compaction_limit,
            ObjectCompactionLimit::Unlimited
        );
    }

    #[test]
    fn overlay_profile_changes_only_display_fields() {
        let mut settings = AppSettings {
            tile_resolution_px: 1024,
            tile_worker_count: 3,
            fast_zoom_tile_settle_ms: 77,
            brush_input_spacing_px: 7.0,
            ..AppSettings::default()
        };

        settings.apply_overlay_profile(OverlayProfile::Diagnostics);

        assert_eq!(settings.tile_resolution_px, 1024);
        assert_eq!(settings.tile_worker_count, 3);
        assert_eq!(settings.fast_zoom_tile_settle_ms, 77);
        assert_eq!(settings.brush_input_spacing_px, 7.0);
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
    fn stroke_fallback_join_mode_round_trips_json() {
        let settings = AppSettings {
            stroke_fallback_joins: StrokeFallbackJoinMode::Quality,
            ..AppSettings::default()
        };
        let json = serde_json::to_string(&settings).expect("serialize settings");

        assert!(json.contains(r#""stroke_fallback_joins":"quality""#));

        let reloaded: AppSettings = serde_json::from_str(&json).expect("deserialize settings");
        assert_eq!(
            reloaded.stroke_fallback_joins,
            StrokeFallbackJoinMode::Quality
        );
    }

    #[test]
    fn storage_commit_mode_round_trips_json() {
        let settings = AppSettings {
            storage_commit_mode: StorageCommitMode::Fast,
            ..AppSettings::default()
        };
        let json = serde_json::to_string(&settings).expect("serialize settings");

        assert!(json.contains(r#""storage_commit_mode":"fast""#));

        let reloaded: AppSettings = serde_json::from_str(&json).expect("deserialize settings");
        assert_eq!(reloaded.storage_commit_mode, StorageCommitMode::Fast);
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
        assert_eq!(cache.png_compression, PngCompression::Small);
        assert_eq!(
            settings.preview_repaint_interval(),
            Duration::from_secs_f64(1.0 / 30.0)
        );
    }
}
