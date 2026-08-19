use crate::coords::{CameraAddress, CanvasPoint, ScreenAffine};
use crate::document::{CanvasDocument, VisibleDepthMode, recompute_compact_block_bounds};
use crate::help::HelpLibrary;
use crate::local_paths::{default_canvas_path, session_logs_path, settings_path};
use crate::model::{Bookmark, Color, DEFAULT_LAYER_ID, EditKind, EditOperation, ToolKind};
use crate::mouse_history::{MouseHistory, NativeWindow, native_window};
use crate::projection_cache::{ProjectedGeometryCache, VisibleOperationCache};
use crate::raster::operation_width;
use crate::selection_geometry::{Point2 as SelectionPoint, Rect2 as SelectionRect, SelectionShape};
use crate::session_log::{LOW_FPS_EMA, SLOW_FRAME_MS, SessionLogger};
use crate::settings::{
    AppSettings, EdgeQuality, MAX_BRUSH_INPUT_SPACING_PX, MAX_CACHE_SIZE_MIB,
    MAX_FILL_INPUT_SPACING_PX, MAX_PREVIEW_FPS, MAX_SAVED_FALLBACK_OPERATION_LIMIT,
    MAX_TILE_PREFETCH_RADIUS, MAX_VECTOR_DEPTH_RADIUS, MIN_BRUSH_INPUT_SPACING_PX,
    MIN_CACHE_SIZE_MIB, MIN_FILL_INPUT_SPACING_PX, MIN_PREVIEW_FPS, ObjectCompactionLimit,
    OverlayProfile, PerformanceProfile, PngCompression, SessionLoggingLevel, SmoothingLevel,
    StorageCommitMode, StrokeFallbackJoinMode, TileRebuildPolicy,
};
use crate::smoothing::{
    GeometryClipRect, clip_polygon_to_rect, clip_polyline_to_rect,
    simplify_render_points_with_tolerance, smooth_closed_points_stable, smooth_stroke_points,
    smooth_stroke_points_stable,
};
use crate::spatial::{OperationBounds, operation_bounds};
use crate::tile_cache::{
    TILE_BLEED, TILE_RESOLUTIONS, TILE_SIZE, TileCache, TileKey, tile_lod_for_resolution,
    tile_resolution,
};
use crate::tile_scheduler::{IncrementalTileUpdate, TileJob, TileScheduler};
use anyhow::{Context, Result, bail};
use eframe::egui::{self, Color32, Painter, PointerButton, Pos2, Rect, Sense, Stroke};
use num_bigint::BigInt;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet, hash_map::Entry};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

const BACKGROUND: Color = Color::WHITE;
const DRAFT_SAVE_INTERVAL: Duration = Duration::from_millis(500);
const TILE_POLL_REPAINT_INTERVAL: Duration = Duration::from_millis(50);
const MAX_BRUSH_INTERPOLATION_GAP_PX: f32 = 8.0;
const MAX_FILL_INTERPOLATION_GAP_PX: f32 = 4.0;
const ZOOM_DRAG_SENSITIVITY: f64 = 0.01;
const MIN_ZOOM_DRAG_FACTOR: f64 = 0.5;
const MAX_ZOOM_DRAG_FACTOR: f64 = 2.0;
const MIN_BRUSH_SIZE: f32 = 1.0;
const MAX_BRUSH_SIZE: f32 = 100.0;
const BRUSH_SIZE_DRAG_SCALE: f32 = 0.25;
const MIN_PALETTE_WIDTH: f32 = 220.0;
const DEFAULT_PALETTE_WIDTH: f32 = 300.0;
const MAX_PALETTE_WIDTH: f32 = 640.0;
const FRAME_TIME_EMA_ALPHA: f32 = 0.15;
const MAX_MEASURED_FRAME_TIME: f32 = 0.25;
const MAX_FALLBACK_SMOOTHING_INPUT_POINTS: usize = 4096;
const MAX_FALLBACK_SMOOTHING_OUTPUT_POINTS: usize = 8192;
const AUTO_SEGMENTED_FALLBACK_POINTS: usize = 128;
const QUALITY_SEGMENTED_FALLBACK_POINTS: usize = 512;
const MAX_SEGMENTED_FALLBACK_SHAPES_PER_FRAME: usize = 200_000;
const MAX_TILE_JOBS_QUEUED_PER_FRAME: usize = 4;
const WHEEL_LINE_SCROLL_POINTS: f32 = 40.0;
const WHEEL_ZOOM_SCALE: f32 = 0.002;
const WHEEL_ZOOM_DIRECTION_LATCH_TIMEOUT: Duration = Duration::from_millis(250);
const MIN_QUICK_DEPTH: i64 = -10_000;
const MAX_QUICK_DEPTH: i64 = 10_000;
const MAX_LATERAL_COORDINATE_DIGITS: usize = 10_000;
const MAX_EXACT_OVERLAY_COORDINATE_DIGITS: usize = 18;
const OVERLAY_COORDINATE_EDGE_DIGITS: usize = 8;
const MIN_LASSO_PREVIEW_SPACING_PX: f32 = 2.0;
const MAX_LASSO_PREVIEW_GAP_PX: f32 = 4.0;
const MAX_LASSO_PREVIEW_INTERPOLATION_STEPS: usize = 128;
const MAX_FILL_DIFFERENCE_FRAGMENTS: usize = 4096;
const MIN_FILL_GEOMETRY_AREA_TWICE: f32 = 0.0001;
const MIN_FILL_FRAGMENT_AREA_TWICE: f32 = 1.0;
const CLICK_SELECTION_DRAG_THRESHOLD_PX: f32 = 4.0;
const ALT_SELECTION_DRAG_STEP_PX: f32 = 28.0;
const CLICK_SELECTION_TOLERANCE_PX: f64 = 6.0;
const SELECTION_CYCLE_POSITION_TOLERANCE_PX: f32 = 6.0;
const SELECTION_HANDLE_SIZE_PX: f32 = 10.0;
const SELECTION_HANDLE_HIT_RADIUS_PX: f32 = 9.0;
const MIN_SELECTION_SCALE: f32 = 0.05;
const MAX_SELECTION_SCALE: f32 = 64.0;
const SELECTION_ROTATE_HANDLE_OFFSET_PX: f32 = 30.0;
const SELECTION_ROTATE_HANDLE_RADIUS_PX: f32 = 7.0;
const SELECTION_ROTATE_SNAP_RADIANS: f32 = std::f32::consts::PI / 12.0;
const LARGE_SELECTION_FAST_OVERLAY_THRESHOLD: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PointAppendDecision {
    Append,
    Skip,
    Interpolate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TileFallbackMode {
    Current,
    OverlayAfter(i64),
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DepthRenderMode {
    Empty,
    VectorsNear,
    DistantTile,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct FallbackFrameStats {
    stroke_shape_count: usize,
    fill_shape_count: usize,
    segmented_strokes: usize,
    segmented_budget_fallbacks: usize,
    fast_strokes: usize,
    skipped_operations: usize,
    projected_operations: usize,
    painted_operations: usize,
    fallback_cache_hits: usize,
    fallback_cache_misses: usize,
    fallback_bounds_cache_hits: usize,
    fallback_bounds_cache_misses: usize,
    fallback_visible_cache_hits: usize,
    fallback_visible_cache_misses: usize,
    project_ms: f64,
    visible_operations_ms: f64,
    projection_loop_ms: f64,
    derive_ms: f64,
    clip_ms: f64,
    paint_ms: f64,
}

impl FallbackFrameStats {
    fn add(&mut self, other: Self) {
        self.stroke_shape_count = self
            .stroke_shape_count
            .saturating_add(other.stroke_shape_count);
        self.fill_shape_count = self.fill_shape_count.saturating_add(other.fill_shape_count);
        self.segmented_strokes = self
            .segmented_strokes
            .saturating_add(other.segmented_strokes);
        self.segmented_budget_fallbacks = self
            .segmented_budget_fallbacks
            .saturating_add(other.segmented_budget_fallbacks);
        self.fast_strokes = self.fast_strokes.saturating_add(other.fast_strokes);
        self.skipped_operations = self
            .skipped_operations
            .saturating_add(other.skipped_operations);
        self.projected_operations = self
            .projected_operations
            .saturating_add(other.projected_operations);
        self.painted_operations = self
            .painted_operations
            .saturating_add(other.painted_operations);
        self.fallback_cache_hits = self
            .fallback_cache_hits
            .saturating_add(other.fallback_cache_hits);
        self.fallback_cache_misses = self
            .fallback_cache_misses
            .saturating_add(other.fallback_cache_misses);
        self.fallback_bounds_cache_hits = self
            .fallback_bounds_cache_hits
            .saturating_add(other.fallback_bounds_cache_hits);
        self.fallback_bounds_cache_misses = self
            .fallback_bounds_cache_misses
            .saturating_add(other.fallback_bounds_cache_misses);
        self.fallback_visible_cache_hits = self
            .fallback_visible_cache_hits
            .saturating_add(other.fallback_visible_cache_hits);
        self.fallback_visible_cache_misses = self
            .fallback_visible_cache_misses
            .saturating_add(other.fallback_visible_cache_misses);
        self.project_ms += other.project_ms;
        self.visible_operations_ms += other.visible_operations_ms;
        self.projection_loop_ms += other.projection_loop_ms;
        self.derive_ms += other.derive_ms;
        self.clip_ms += other.clip_ms;
        self.paint_ms += other.paint_ms;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OperationBoundsLookup {
    cache_hit: bool,
    may_intersect: bool,
}

#[derive(Default)]
struct OperationBoundsCache {
    entries: HashMap<Uuid, Option<OperationBounds>>,
}

impl OperationBoundsCache {
    fn operation_may_intersect_screen_rect(
        &mut self,
        operation: &EditOperation,
        camera: &CameraAddress,
        rect: Rect,
        width: f32,
    ) -> OperationBoundsLookup {
        let (bounds, cache_hit) = match self.entries.entry(operation.id) {
            Entry::Occupied(entry) => (entry.into_mut().as_ref(), true),
            Entry::Vacant(entry) => (entry.insert(operation_bounds(operation)).as_ref(), false),
        };
        OperationBoundsLookup {
            cache_hit,
            may_intersect: bounds.is_some_and(|bounds| {
                operation_bounds_may_intersect_screen_rect(bounds, camera, rect, width)
            }),
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Debug, Clone, PartialEq)]
struct SavedFallbackRenderFrameKey {
    camera: CameraAddress,
    rect_min_x: u32,
    rect_min_y: u32,
    rect_max_x: u32,
    rect_max_y: u32,
    smoothing: SmoothingLevel,
    stroke_fallback_joins: StrokeFallbackJoinMode,
    fill_fallback_max_points: usize,
}

impl SavedFallbackRenderFrameKey {
    fn new(
        _revision: u64,
        camera: &CameraAddress,
        rect: Rect,
        settings: &AppSettings,
        _auto_fast_stroke_fallback: bool,
    ) -> Self {
        Self {
            camera: camera.clone(),
            rect_min_x: rect.min.x.to_bits(),
            rect_min_y: rect.min.y.to_bits(),
            rect_max_x: rect.max.x.to_bits(),
            rect_max_y: rect.max.y.to_bits(),
            smoothing: settings.smoothing,
            stroke_fallback_joins: settings.stroke_fallback_joins,
            fill_fallback_max_points: settings.fill_fallback_max_points,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SavedFallbackRenderOperationKey {
    scope_id: Uuid,
    operation_id: Uuid,
    sequence: i64,
    render_variant: u8,
}

fn saved_fallback_render_variant(
    operation: &EditOperation,
    fallback_join_mode: StrokeFallbackJoinMode,
    auto_fast_stroke_fallback: bool,
    smoothing: SmoothingLevel,
) -> u8 {
    if operation.kind.is_area() {
        return if operation.smooth_area {
            1u8.saturating_add(smoothing.passes())
        } else {
            0
        };
    }
    let passes = stroke_fallback_smoothing_passes(
        fallback_join_mode,
        auto_fast_stroke_fallback,
        smoothing.passes(),
    );
    1u8.saturating_add(passes.min(usize::from(u8::MAX - 1)) as u8)
}

fn saved_fallback_operation_key(
    scope_id: Uuid,
    operation: &EditOperation,
    fallback_join_mode: StrokeFallbackJoinMode,
    auto_fast_stroke_fallback: bool,
    smoothing: SmoothingLevel,
) -> SavedFallbackRenderOperationKey {
    SavedFallbackRenderOperationKey {
        scope_id,
        operation_id: operation.id,
        sequence: operation.sequence,
        render_variant: saved_fallback_render_variant(
            operation,
            fallback_join_mode,
            auto_fast_stroke_fallback,
            smoothing,
        ),
    }
}

#[derive(Default)]
struct SavedFallbackRenderCache {
    frame_key: Option<SavedFallbackRenderFrameKey>,
    points: HashMap<SavedFallbackRenderOperationKey, Arc<[Pos2]>>,
    clipped_stroke_runs: HashMap<SavedFallbackRenderOperationKey, Arc<[Arc<[Pos2]>]>>,
    clipped_area_polygons: HashMap<SavedFallbackRenderOperationKey, Arc<[Pos2]>>,
}

impl SavedFallbackRenderCache {
    fn begin_frame(&mut self, frame_key: SavedFallbackRenderFrameKey) {
        if self.frame_key.as_ref() != Some(&frame_key) {
            self.frame_key = Some(frame_key);
            self.points.clear();
            self.clipped_stroke_runs.clear();
            self.clipped_area_polygons.clear();
        }
    }

    fn get(&self, operation_key: &SavedFallbackRenderOperationKey) -> Option<Arc<[Pos2]>> {
        self.points.get(operation_key).cloned()
    }

    fn insert(
        &mut self,
        operation_key: SavedFallbackRenderOperationKey,
        points: Vec<Pos2>,
    ) -> Arc<[Pos2]> {
        let points = Arc::<[Pos2]>::from(points);
        self.points.insert(operation_key, Arc::clone(&points));
        points
    }

    fn insert_transformed_from(
        &mut self,
        source_key: &SavedFallbackRenderOperationKey,
        target_key: SavedFallbackRenderOperationKey,
        transform: impl Fn(Pos2) -> Pos2,
    ) -> bool {
        let Some(points) = self.points.get(source_key) else {
            return false;
        };
        let transformed = points.iter().copied().map(transform).collect::<Vec<_>>();
        self.insert(target_key, transformed);
        true
    }

    fn get_clipped_stroke_runs(
        &self,
        operation_key: &SavedFallbackRenderOperationKey,
    ) -> Option<Arc<[Arc<[Pos2]>]>> {
        self.clipped_stroke_runs.get(operation_key).cloned()
    }

    fn insert_clipped_stroke_runs(
        &mut self,
        operation_key: SavedFallbackRenderOperationKey,
        runs: Vec<Vec<Pos2>>,
    ) -> Arc<[Arc<[Pos2]>]> {
        let runs = runs
            .into_iter()
            .map(Arc::<[Pos2]>::from)
            .collect::<Vec<_>>();
        let runs = Arc::<[Arc<[Pos2]>]>::from(runs);
        self.clipped_stroke_runs
            .insert(operation_key, Arc::clone(&runs));
        runs
    }

    fn get_clipped_area_polygon(
        &self,
        operation_key: &SavedFallbackRenderOperationKey,
    ) -> Option<Arc<[Pos2]>> {
        self.clipped_area_polygons.get(operation_key).cloned()
    }

    fn insert_clipped_area_polygon(
        &mut self,
        operation_key: SavedFallbackRenderOperationKey,
        polygon: Vec<Pos2>,
    ) -> Arc<[Pos2]> {
        let polygon = Arc::<[Pos2]>::from(polygon);
        self.clipped_area_polygons
            .insert(operation_key, Arc::clone(&polygon));
        polygon
    }

    fn clear(&mut self) {
        self.frame_key = None;
        self.points.clear();
        self.clipped_stroke_runs.clear();
        self.clipped_area_polygons.clear();
    }

    #[cfg(test)]
    fn cached_operation_count(&self) -> usize {
        self.points.len()
    }
}

struct FallbackRenderer {
    segmented_shape_budget_remaining: usize,
    projected_geometry: ProjectedGeometryCache,
    visible_operations: VisibleOperationCache,
    saved_render_cache: SavedFallbackRenderCache,
    operation_bounds_cache: OperationBoundsCache,
}

impl Default for FallbackRenderer {
    fn default() -> Self {
        Self {
            segmented_shape_budget_remaining: MAX_SEGMENTED_FALLBACK_SHAPES_PER_FRAME,
            projected_geometry: ProjectedGeometryCache::default(),
            visible_operations: VisibleOperationCache::default(),
            saved_render_cache: SavedFallbackRenderCache::default(),
            operation_bounds_cache: OperationBoundsCache::default(),
        }
    }
}

impl FallbackRenderer {
    fn begin_frame(&mut self, frame_key: SavedFallbackRenderFrameKey) {
        self.saved_render_cache.begin_frame(frame_key);
        self.segmented_shape_budget_remaining = MAX_SEGMENTED_FALLBACK_SHAPES_PER_FRAME;
    }

    fn visible_operations(
        &mut self,
        revision: u64,
        visible_tiles: &[TileKey],
        query: impl FnOnce() -> Vec<usize>,
    ) -> (Arc<[usize]>, bool) {
        let mut cache_miss = false;
        let operations = self
            .visible_operations
            .get_or_update(revision, visible_tiles, || {
                cache_miss = true;
                query()
            });
        (operations, !cache_miss)
    }

    #[allow(clippy::too_many_arguments)]
    fn project_operations(
        &mut self,
        operations: &[Arc<EditOperation>],
        camera: &CameraAddress,
        rect: Rect,
        after_sequence: Option<i64>,
        operation_limit: Option<usize>,
        fallback_join_mode: StrokeFallbackJoinMode,
        auto_fast_stroke_fallback: bool,
        smoothing: SmoothingLevel,
        project_start: Instant,
    ) -> (Vec<FallbackPaintOperation>, usize, FallbackFrameStats) {
        let projection_loop_start = Instant::now();
        let eligible_operation_indices = operations
            .iter()
            .enumerate()
            .filter(|(_, operation)| {
                after_sequence.is_none_or(|sequence| operation.sequence > sequence)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let eligible_operation_count = eligible_operation_indices.len();
        let mut skipped_operations =
            fallback_operation_skip_count(eligible_operation_count, operation_limit);
        while skipped_operations > 0 && skipped_operations < eligible_operation_count {
            let current = &operations[eligible_operation_indices[skipped_operations]];
            let previous = &operations[eligible_operation_indices[skipped_operations - 1]];
            if !current.shares_fill_group_with(previous) {
                break;
            }
            skipped_operations -= 1;
        }
        let Some(frame) = self.projected_geometry.begin_frame(camera, rect) else {
            return (
                Vec::new(),
                skipped_operations,
                FallbackFrameStats {
                    project_ms: project_start.elapsed().as_secs_f64() * 1_000.0,
                    projection_loop_ms: projection_loop_start.elapsed().as_secs_f64() * 1_000.0,
                    ..FallbackFrameStats::default()
                },
            );
        };
        let mut projected =
            Vec::with_capacity(eligible_operation_count.saturating_sub(skipped_operations));
        let mut projected_operation_count = 0usize;
        let mut screen_culled_operations = 0usize;
        let mut fallback_cache_hits = 0usize;
        let mut fallback_bounds_cache_hits = 0usize;
        let mut fallback_bounds_cache_misses = 0usize;
        for operation_index in eligible_operation_indices
            .into_iter()
            .skip(skipped_operations)
        {
            let operation = &operations[operation_index];
            if operation.is_compact_block() {
                projected.push(FallbackPaintOperation::Projected {
                    operation_index,
                    points: Vec::new(),
                });
                continue;
            }
            let operation_key = saved_fallback_operation_key(
                operation.id,
                operation,
                fallback_join_mode,
                auto_fast_stroke_fallback,
                smoothing,
            );
            if let Some(points) = self.saved_render_cache.get(&operation_key) {
                fallback_cache_hits = fallback_cache_hits.saturating_add(1);
                projected.push(FallbackPaintOperation::CachedDerived {
                    operation_index,
                    points,
                });
                continue;
            }
            let bounds_lookup = self
                .operation_bounds_cache
                .operation_may_intersect_screen_rect(
                    operation,
                    camera,
                    rect,
                    operation_width(operation, camera.depth, camera.zoom),
                );
            if bounds_lookup.cache_hit {
                fallback_bounds_cache_hits = fallback_bounds_cache_hits.saturating_add(1);
            } else {
                fallback_bounds_cache_misses = fallback_bounds_cache_misses.saturating_add(1);
            }
            if !bounds_lookup.may_intersect {
                screen_culled_operations = screen_culled_operations.saturating_add(1);
                continue;
            }
            let points = self.projected_geometry.project_operation(operation, frame);
            projected_operation_count = projected_operation_count.saturating_add(1);
            projected.push(FallbackPaintOperation::Projected {
                operation_index,
                points,
            });
        }
        (
            projected,
            skipped_operations,
            FallbackFrameStats {
                skipped_operations: screen_culled_operations,
                projected_operations: projected_operation_count,
                fallback_cache_hits,
                fallback_bounds_cache_hits,
                fallback_bounds_cache_misses,
                project_ms: project_start.elapsed().as_secs_f64() * 1_000.0,
                projection_loop_ms: projection_loop_start.elapsed().as_secs_f64() * 1_000.0,
                ..FallbackFrameStats::default()
            },
        )
    }

    fn clear(&mut self) {
        self.projected_geometry.clear();
        self.visible_operations.clear();
        self.saved_render_cache.clear();
        self.operation_bounds_cache.clear();
        self.segmented_shape_budget_remaining = MAX_SEGMENTED_FALLBACK_SHAPES_PER_FRAME;
    }
}

#[derive(Debug, Clone, Copy)]
struct OperationPaintContext {
    is_draft: bool,
    auto_fast_stroke_fallback: bool,
    cache_scope_id: Uuid,
}

enum FallbackPaintOperation {
    Projected {
        operation_index: usize,
        points: Vec<Pos2>,
    },
    CachedDerived {
        operation_index: usize,
        points: Arc<[Pos2]>,
    },
}

impl FallbackPaintOperation {
    fn operation_index(&self) -> usize {
        match self {
            Self::Projected {
                operation_index, ..
            }
            | Self::CachedDerived {
                operation_index, ..
            } => *operation_index,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct TileFrameStats {
    collect_upload_ms: f64,
    request_queue_ms: f64,
    draw_ms: f64,
    uploaded_textures: usize,
    queued_jobs: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct FramePhaseStats {
    frame_ms: Option<f32>,
    fps_ema: Option<f32>,
    input_ms: f64,
    tile_total_ms: f64,
    tile_collect_upload_ms: f64,
    tile_request_queue_ms: f64,
    tile_draw_ms: f64,
    fallback_total_ms: f64,
    fallback_project_ms: f64,
    fallback_derive_ms: f64,
    fallback_clip_ms: f64,
    fallback_shape_paint_ms: f64,
    overlay_ms: f64,
    total_frame_ms: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RectangleSelectionMode {
    Inside,
    Crossing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionTargetMode {
    Object,
    Area,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AreaSelectionShape {
    Rectangle,
    Lasso,
}

#[derive(Debug, Clone)]
struct AreaSelection {
    shape: AreaSelectionShape,
    points: Vec<CanvasPoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileWindowTab {
    File,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionScaleHandle {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl SelectionScaleHandle {
    const ALL: [Self; 4] = [
        Self::TopLeft,
        Self::TopRight,
        Self::BottomLeft,
        Self::BottomRight,
    ];

    fn position(self, bounds: Rect) -> Pos2 {
        match self {
            Self::TopLeft => bounds.left_top(),
            Self::TopRight => bounds.right_top(),
            Self::BottomLeft => bounds.left_bottom(),
            Self::BottomRight => bounds.right_bottom(),
        }
    }

    fn pivot(self, bounds: Rect) -> Pos2 {
        match self {
            Self::TopLeft => bounds.right_bottom(),
            Self::TopRight => bounds.left_bottom(),
            Self::BottomLeft => bounds.right_top(),
            Self::BottomRight => bounds.left_top(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SelectionScaleGesture {
    handle: SelectionScaleHandle,
    bounds: Rect,
    pointer_start: Pos2,
    current: Pos2,
}

impl SelectionScaleGesture {
    fn scale(self) -> f32 {
        let handle_position = self.handle.position(self.bounds);
        let dragged_handle = handle_position + (self.current - self.pointer_start);
        selection_scale_factor(self.bounds, self.handle, dragged_handle)
    }

    fn transform_position(self, position: Pos2) -> Pos2 {
        let pivot = self.handle.pivot(self.bounds);
        pivot + (position - pivot) * self.scale()
    }

    fn transformed_bounds(self) -> Rect {
        Rect::from_two_pos(
            self.handle.pivot(self.bounds),
            self.transform_position(self.handle.position(self.bounds)),
        )
    }
}

#[derive(Debug, Clone, Copy)]
struct SelectionRotateGesture {
    bounds: Rect,
    pointer_start: Pos2,
    current: Pos2,
    snap: bool,
}

impl SelectionRotateGesture {
    fn angle(self) -> f32 {
        let center = self.bounds.center();
        let start = self.pointer_start - center;
        let current = self.current - center;
        let denominator = start.length() * current.length();
        if !denominator.is_finite() || denominator <= f32::EPSILON {
            return 0.0;
        }
        let mut angle = current.y.atan2(current.x) - start.y.atan2(start.x);
        if self.snap {
            angle = (angle / SELECTION_ROTATE_SNAP_RADIANS).round() * SELECTION_ROTATE_SNAP_RADIANS;
        }
        angle
    }

    fn transform_position(self, position: Pos2) -> Pos2 {
        rotate_position_around(position, self.bounds.center(), self.angle())
    }

    fn transformed_bounds(self) -> Rect {
        transformed_rect_bounds(self.bounds, |position| self.transform_position(position))
    }
}

#[derive(Debug, Clone, Copy)]
struct AltPointerGesture {
    start: Pos2,
    current: Pos2,
    applied_cycle_steps: isize,
    cycle_selection: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionFlipAxis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy)]
struct SelectionModifiers {
    shift: bool,
    ctrl: bool,
    alt: bool,
}

#[derive(Debug, Clone)]
struct SelectionCandidate {
    id: Uuid,
    effective_distance: f64,
    bounds_area: f64,
    layer_order: i64,
    paint_order: i64,
    is_eraser: bool,
}

#[derive(Default)]
struct SelectionClipboard {
    operations: Vec<EditOperation>,
    paste_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipboardCommand {
    Copy,
    Cut,
    Paste,
    PasteInPlace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionMenuAction {
    TargetMode(SelectionTargetMode),
    AreaShape(AreaSelectionShape),
    Clipboard(ClipboardCommand),
    Deselect,
    Delete,
    Flip(SelectionFlipAxis),
    Recolor,
    FreezeCompact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CanvasContextMenuKind {
    Brush,
    Fill,
    Selection,
}

#[derive(Default)]
struct ClipboardShortcutKeys {
    copy_down: bool,
    cut_down: bool,
    paste_down: bool,
}

impl ClipboardShortcutKeys {
    fn poll(&mut self) -> Option<ClipboardCommand> {
        let ctrl = native_key_down(0x11);
        let shift = native_key_down(0x10);
        self.update(
            ctrl,
            shift,
            native_key_down(0x43),
            native_key_down(0x58),
            native_key_down(0x56),
        )
    }

    fn update(
        &mut self,
        ctrl: bool,
        shift: bool,
        copy_down: bool,
        cut_down: bool,
        paste_down: bool,
    ) -> Option<ClipboardCommand> {
        let command = if ctrl && paste_down && !self.paste_down {
            Some(if shift {
                ClipboardCommand::PasteInPlace
            } else {
                ClipboardCommand::Paste
            })
        } else if ctrl && cut_down && !self.cut_down {
            Some(ClipboardCommand::Cut)
        } else if ctrl && copy_down && !self.copy_down {
            Some(ClipboardCommand::Copy)
        } else {
            None
        };
        self.copy_down = copy_down;
        self.cut_down = cut_down;
        self.paste_down = paste_down;
        command
    }
}

struct LoadedTileTexture {
    revision: u64,
    snapshot_sequence: i64,
    texture: egui::TextureHandle,
}

#[derive(Default)]
struct FrameRateTracker {
    average_frame_time: Option<f32>,
    interaction_active_last_frame: bool,
}

impl FrameRateTracker {
    fn update(&mut self, frame_time: f32, interaction_active: bool) -> Option<f32> {
        if !interaction_active {
            self.interaction_active_last_frame = false;
            return None;
        }
        if !frame_time.is_finite()
            || frame_time <= f32::EPSILON
            || frame_time > MAX_MEASURED_FRAME_TIME
        {
            return None;
        }

        if !self.interaction_active_last_frame || self.average_frame_time.is_none() {
            self.average_frame_time = Some(frame_time);
        } else if let Some(average) = self.average_frame_time.as_mut() {
            *average += (frame_time - *average) * FRAME_TIME_EMA_ALPHA;
        }
        self.interaction_active_last_frame = true;
        Some(frame_time * 1_000.0)
    }

    fn metrics(&self) -> Option<(f32, f32)> {
        self.average_frame_time
            .map(|frame_time| (1.0 / frame_time, frame_time * 1_000.0))
    }
}

#[derive(Default)]
struct CoordinateOverlayCache {
    tile_x: Option<BigInt>,
    tile_y: Option<BigInt>,
    formatted_x: String,
    formatted_y: String,
    exact_x: String,
    exact_y: String,
}

impl CoordinateOverlayCache {
    fn update(&mut self, camera: &CameraAddress) {
        if self.tile_x.as_ref() != Some(&camera.tile_x) {
            self.formatted_x = format_overlay_coordinate(&camera.tile_x);
            self.exact_x = camera.tile_x.to_string();
            self.tile_x = Some(camera.tile_x.clone());
        }
        if self.tile_y.as_ref() != Some(&camera.tile_y) {
            self.formatted_y = format_overlay_coordinate(&camera.tile_y);
            self.exact_y = camera.tile_y.to_string();
            self.tile_y = Some(camera.tile_y.clone());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SelectionBoundsSignature {
    len: usize,
    xor: u128,
    sum: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectionOverlayBoundsKey {
    document_revision: u64,
    signature: SelectionBoundsSignature,
}

#[derive(Debug, Clone)]
struct SelectionOverlayWidthHint {
    native_depth: i64,
    native_zoom: f64,
    width_px: f32,
}

#[derive(Debug, Clone)]
struct CanvasAxisBounds {
    min_tile: BigInt,
    min_local: f64,
    max_tile: BigInt,
    max_local: f64,
}

impl CanvasAxisBounds {
    fn new(tile: &BigInt, local: f64) -> Self {
        Self {
            min_tile: tile.clone(),
            min_local: local,
            max_tile: tile.clone(),
            max_local: local,
        }
    }

    fn include(&mut self, tile: &BigInt, local: f64) {
        if canvas_axis_less(tile, local, &self.min_tile, self.min_local) {
            self.min_tile = tile.clone();
            self.min_local = local;
        }
        if canvas_axis_less(&self.max_tile, self.max_local, tile, local) {
            self.max_tile = tile.clone();
            self.max_local = local;
        }
    }
}

#[derive(Debug, Clone)]
struct SelectionDepthCanvasBounds {
    depth: i64,
    x: CanvasAxisBounds,
    y: CanvasAxisBounds,
}

impl SelectionDepthCanvasBounds {
    fn new(point: &CanvasPoint) -> Self {
        Self {
            depth: point.depth,
            x: CanvasAxisBounds::new(&point.tile_x, point.local_x),
            y: CanvasAxisBounds::new(&point.tile_y, point.local_y),
        }
    }

    fn include(&mut self, point: &CanvasPoint) {
        self.x.include(&point.tile_x, point.local_x);
        self.y.include(&point.tile_y, point.local_y);
    }

    fn project(&self, camera: &CameraAddress, rect: Rect) -> Option<Rect> {
        let corners = [
            CanvasPoint::new(
                self.depth,
                self.x.min_tile.clone(),
                self.y.min_tile.clone(),
                self.x.min_local,
                self.y.min_local,
            ),
            CanvasPoint::new(
                self.depth,
                self.x.max_tile.clone(),
                self.y.min_tile.clone(),
                self.x.max_local,
                self.y.min_local,
            ),
            CanvasPoint::new(
                self.depth,
                self.x.max_tile.clone(),
                self.y.max_tile.clone(),
                self.x.max_local,
                self.y.max_local,
            ),
            CanvasPoint::new(
                self.depth,
                self.x.min_tile.clone(),
                self.y.max_tile.clone(),
                self.x.min_local,
                self.y.max_local,
            ),
        ];
        let mut projected = None::<Rect>;
        for corner in corners {
            let (x, y) =
                camera.canvas_to_screen(&corner, rect.width() as f64, rect.height() as f64)?;
            if !x.is_finite() || !y.is_finite() {
                return None;
            }
            let position = Pos2::new(rect.left() + x as f32, rect.top() + y as f32);
            let point_rect = Rect::from_min_size(position, egui::Vec2::ZERO);
            projected = Some(projected.map_or(point_rect, |bounds| bounds.union(point_rect)));
        }
        projected
    }
}

#[derive(Default)]
struct SelectionOverlayBoundsCache {
    key: Option<SelectionOverlayBoundsKey>,
    depth_bounds: Vec<SelectionDepthCanvasBounds>,
    width_hints: Vec<SelectionOverlayWidthHint>,
}

impl SelectionOverlayBoundsCache {
    fn bounds_for(
        &mut self,
        document_revision: u64,
        operations: &[EditOperation],
        selected: &HashSet<Uuid>,
    ) -> (&[SelectionDepthCanvasBounds], &[SelectionOverlayWidthHint]) {
        let key = SelectionOverlayBoundsKey {
            document_revision,
            signature: selection_bounds_signature(selected),
        };
        if self.key.as_ref() != Some(&key) {
            self.rebuild(key, operations, selected);
        }
        (&self.depth_bounds, &self.width_hints)
    }

    fn rebuild(
        &mut self,
        key: SelectionOverlayBoundsKey,
        operations: &[EditOperation],
        selected: &HashSet<Uuid>,
    ) {
        self.key = Some(key);
        self.depth_bounds.clear();
        self.width_hints.clear();
        for operation in operations
            .iter()
            .filter(|operation| selected.contains(&operation.id))
        {
            if !operation.kind.is_area() {
                self.width_hints.push(SelectionOverlayWidthHint {
                    native_depth: operation.native_depth,
                    native_zoom: operation.native_zoom,
                    width_px: operation.width_px,
                });
            }
            for point in &operation.points {
                if let Some(bounds) = self
                    .depth_bounds
                    .iter_mut()
                    .find(|bounds| bounds.depth == point.depth)
                {
                    bounds.include(point);
                } else {
                    self.depth_bounds
                        .push(SelectionDepthCanvasBounds::new(point));
                }
            }
        }
    }
}

#[derive(Default)]
struct WheelZoomDirectionLatch {
    direction: Option<f32>,
    last_input_at: Option<Instant>,
}

struct WheelZoomLatchResult {
    applied_scrolls: Vec<f32>,
    suppressed_scroll_count: usize,
    direction_before: Option<f32>,
    direction_used: Option<f32>,
    direction_after: Option<f32>,
}

impl WheelZoomDirectionLatch {
    fn apply(
        &mut self,
        scrolls: &[f32],
        gesture_ended: bool,
        now: Instant,
    ) -> WheelZoomLatchResult {
        if self.last_input_at.is_some_and(|last_input_at| {
            now.saturating_duration_since(last_input_at) >= WHEEL_ZOOM_DIRECTION_LATCH_TIMEOUT
        }) {
            self.reset();
        }
        let direction_before = self.direction;
        if self.direction.is_none() {
            let batch_total = scrolls.iter().copied().sum::<f32>();
            if batch_total.abs() > f32::EPSILON {
                self.direction = Some(batch_total.signum());
            }
        }
        let direction_used = self.direction;
        let mut suppressed_scroll_count = 0usize;
        let applied_scrolls = scrolls
            .iter()
            .copied()
            .filter(|scroll| {
                let matches_direction =
                    direction_used.is_some_and(|direction| scroll.signum() == direction.signum());
                if !matches_direction {
                    suppressed_scroll_count = suppressed_scroll_count.saturating_add(1);
                }
                matches_direction
            })
            .collect();
        if !scrolls.is_empty() {
            self.last_input_at = Some(now);
        }
        if gesture_ended {
            self.reset();
        }
        WheelZoomLatchResult {
            applied_scrolls,
            suppressed_scroll_count,
            direction_before,
            direction_used,
            direction_after: self.direction,
        }
    }

    fn reset(&mut self) {
        self.direction = None;
        self.last_input_at = None;
    }
}

pub struct EndlessSketchApp {
    document: CanvasDocument,
    camera: CameraAddress,
    tool: ToolKind,
    color: Color,
    brush_size: f32,
    draft: Option<EditOperation>,
    eraser_target_ids: HashSet<Uuid>,
    last_draft_save: Instant,
    last_pointer_position: Option<Pos2>,
    status_message: String,
    tile_scheduler: TileScheduler,
    tile_textures: HashMap<TileKey, LoadedTileTexture>,
    pending_tiles: HashSet<(TileKey, u64)>,
    visible_tile_generation: u64,
    visible_tile_set: HashSet<TileKey>,
    last_zoom_change: Option<Instant>,
    wheel_zoom_point_remainder: f32,
    wheel_zoom_direction_latch: WheelZoomDirectionLatch,
    last_tile_rebuild_interaction: Option<Instant>,
    settings: AppSettings,
    settings_path: PathBuf,
    session_logger: SessionLogger,
    bookmarks: Vec<Bookmark>,
    bookmark_edits: HashMap<Uuid, String>,
    new_bookmark_name: String,
    show_file: bool,
    file_window_tab: FileWindowTab,
    help_library: HelpLibrary,
    help_language_id: String,
    show_navigation: bool,
    show_bookmarks: bool,
    show_settings: bool,
    show_layers: bool,
    show_palette: bool,
    pending_layer_delete: Option<Uuid>,
    brush_sizing_drag_active: bool,
    frame_rate: FrameRateTracker,
    last_visible_render_operation_count: Option<usize>,
    last_depth_render_mode: Option<DepthRenderMode>,
    last_fallback_mode: Option<TileFallbackMode>,
    last_fallback_frame_stats: FallbackFrameStats,
    last_tile_frame_stats: TileFrameStats,
    last_frame_phase_stats: FramePhaseStats,
    last_auto_fast_stroke_fallback: bool,
    last_large_selection_fast_overlay: bool,
    fallback_renderer: FallbackRenderer,
    automatic_tile_generation_pause: bool,
    mouse_history: MouseHistory,
    depth_jump_target: i64,
    lateral_jump_x: String,
    lateral_jump_y: String,
    coordinate_overlay: CoordinateOverlayCache,
    selection_overlay_bounds_cache: SelectionOverlayBoundsCache,
    active_layer_id: Uuid,
    layer_name_edit: String,
    selection_clipboard: SelectionClipboard,
    clipboard_shortcut_keys: ClipboardShortcutKeys,
    selected_operation_ids: HashSet<Uuid>,
    selection_drag_start: Option<Pos2>,
    selection_drag_current: Option<Pos2>,
    selection_drag_active: bool,
    selection_move_start: Option<Pos2>,
    selection_move_current: Option<Pos2>,
    selection_move_active: bool,
    selection_scale_gesture: Option<SelectionScaleGesture>,
    selection_rotate_gesture: Option<SelectionRotateGesture>,
    selection_target_mode: SelectionTargetMode,
    rectangle_selection_mode: RectangleSelectionMode,
    area_selection_shape: AreaSelectionShape,
    area_selection: Option<AreaSelection>,
    area_selection_drag_points: Vec<Pos2>,
    area_selection_drag_active: bool,
    hovered_operation_id: Option<Uuid>,
    selection_hover_position: Option<Pos2>,
    selection_cycle_position: Option<Pos2>,
    selection_cycle_candidates: Vec<Uuid>,
    selection_cycle_index: usize,
    selection_capture_camera_depth: i64,
    alt_pointer_gesture: Option<AltPointerGesture>,
    eraser_lasso_points: Vec<Pos2>,
    eraser_lasso_active: bool,
    last_canvas_rect: Option<Rect>,
}

impl EndlessSketchApp {
    pub fn new(_context: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Result<Self> {
        let path = path.unwrap_or_else(default_canvas_path);
        let settings_path = settings_path();
        let mut status_message = "Ready".to_owned();
        let settings = match AppSettings::load_or_create(&settings_path) {
            Ok(settings) => settings,
            Err(error) => {
                status_message = format!("Settings failed: {error:#}");
                AppSettings::default()
            }
        };
        let session_logger = SessionLogger::new(session_logs_path(), settings.session_logging);
        let document = CanvasDocument::open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        if let Err(error) = document.set_storage_commit_mode(settings.storage_commit_mode) {
            status_message = format!("Storage commit setting failed: {error:#}");
        }
        let tile_scheduler = TileScheduler::new(
            TileCache::with_options(document.root(), settings.tile_cache_options())?,
            settings.tile_worker_count,
        );
        let bookmarks = document.bookmarks()?;
        let bookmark_edits = bookmark_edit_names(&bookmarks);
        let new_bookmark_name = format!("Bookmark {}", bookmarks.len() + 1);
        let active_layer = document
            .layers()
            .last()
            .cloned()
            .unwrap_or_else(crate::model::Layer::default_layer);
        let active_layer_id = active_layer.id;
        let layer_name_edit = active_layer.name;
        let help_library = HelpLibrary::load();
        for warning in help_library.warnings() {
            log::warn!("Help translation ignored: {warning}");
        }
        let help_language_id = help_library.preferred_language_id().to_owned();
        let mut app = Self {
            document,
            camera: CameraAddress::default(),
            tool: ToolKind::Brush,
            color: Color::BLACK,
            brush_size: 5.0,
            draft: None,
            eraser_target_ids: HashSet::new(),
            last_draft_save: Instant::now(),
            last_pointer_position: None,
            status_message,
            tile_scheduler,
            tile_textures: HashMap::new(),
            pending_tiles: HashSet::new(),
            visible_tile_generation: 0,
            visible_tile_set: HashSet::new(),
            last_zoom_change: None,
            wheel_zoom_point_remainder: 0.0,
            wheel_zoom_direction_latch: WheelZoomDirectionLatch::default(),
            last_tile_rebuild_interaction: None,
            settings,
            settings_path,
            session_logger,
            bookmarks,
            bookmark_edits,
            new_bookmark_name,
            show_file: false,
            file_window_tab: FileWindowTab::File,
            help_library,
            help_language_id,
            show_navigation: false,
            show_bookmarks: false,
            show_settings: false,
            show_layers: false,
            show_palette: false,
            pending_layer_delete: None,
            brush_sizing_drag_active: false,
            frame_rate: FrameRateTracker::default(),
            last_visible_render_operation_count: None,
            last_depth_render_mode: None,
            last_fallback_mode: None,
            last_fallback_frame_stats: FallbackFrameStats::default(),
            last_tile_frame_stats: TileFrameStats::default(),
            last_frame_phase_stats: FramePhaseStats::default(),
            last_auto_fast_stroke_fallback: false,
            last_large_selection_fast_overlay: false,
            fallback_renderer: FallbackRenderer::default(),
            automatic_tile_generation_pause: false,
            mouse_history: MouseHistory::default(),
            depth_jump_target: 0,
            lateral_jump_x: "0".to_owned(),
            lateral_jump_y: "0".to_owned(),
            coordinate_overlay: CoordinateOverlayCache::default(),
            selection_overlay_bounds_cache: SelectionOverlayBoundsCache::default(),
            active_layer_id,
            layer_name_edit,
            selection_clipboard: SelectionClipboard::default(),
            clipboard_shortcut_keys: ClipboardShortcutKeys::default(),
            selected_operation_ids: HashSet::new(),
            selection_drag_start: None,
            selection_drag_current: None,
            selection_drag_active: false,
            selection_move_start: None,
            selection_move_current: None,
            selection_move_active: false,
            selection_scale_gesture: None,
            selection_rotate_gesture: None,
            selection_target_mode: SelectionTargetMode::Object,
            rectangle_selection_mode: RectangleSelectionMode::Inside,
            area_selection_shape: AreaSelectionShape::Rectangle,
            area_selection: None,
            area_selection_drag_points: Vec::new(),
            area_selection_drag_active: false,
            hovered_operation_id: None,
            selection_hover_position: None,
            selection_cycle_position: None,
            selection_cycle_candidates: Vec::new(),
            selection_cycle_index: 0,
            selection_capture_camera_depth: 0,
            alt_pointer_gesture: None,
            eraser_lasso_points: Vec::new(),
            eraser_lasso_active: false,
            last_canvas_rect: None,
        };
        app.log_session_event("launch");
        Ok(app)
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let mut selection_action = None;
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.tool, ToolKind::Brush, "Brush (B)");
            ui.selectable_value(&mut self.tool, ToolKind::Eraser, "Eraser (E)");
            ui.selectable_value(&mut self.tool, ToolKind::LassoFill, "Area Fill (L/X)");
            ui.selectable_value(&mut self.tool, ToolKind::Eyedropper, "Picker (I)");
            ui.selectable_value(&mut self.tool, ToolKind::Selection, "Select (S)");
            ui.selectable_value(&mut self.tool, ToolKind::EraserLasso, "Area Erase (X)");
            if self.tool == ToolKind::Selection {
                ui.separator();
                let mode = match self.selection_target_mode {
                    SelectionTargetMode::Object => match self.rectangle_selection_mode {
                        RectangleSelectionMode::Inside => "Object: Inside",
                        RectangleSelectionMode::Crossing => "Object: Crossing",
                    },
                    SelectionTargetMode::Area => match self.area_selection_shape {
                        AreaSelectionShape::Rectangle => "Area: Rectangle",
                        AreaSelectionShape::Lasso => "Area: Lasso",
                    },
                };
                ui.menu_button(format!("Selection: {mode}"), |ui| {
                    selection_action = self.selection_menu_content(ui);
                });
            }
            ui.separator();
            ui.add(
                egui::Slider::new(&mut self.brush_size, MIN_BRUSH_SIZE..=MAX_BRUSH_SIZE)
                    .text("Size"),
            );
            let swatch = egui::RichText::new("Color")
                .background_color(Color32::from_rgb(self.color.r, self.color.g, self.color.b))
                .color(contrasting_text_color(self.color));
            if ui.button(swatch).clicked() {
                self.show_palette = true;
            }
            ui.separator();
            if ui.button("Undo").clicked() {
                self.run_undo();
            }
            if ui.button("Redo").clicked() {
                self.run_redo();
            }
            if ui.button("File").clicked() {
                self.file_window_tab = FileWindowTab::File;
                self.show_file = true;
            }
            if ui.button("Navigation").clicked() {
                self.show_navigation = true;
            }
            if ui.button("Bookmarks").clicked() {
                self.show_bookmarks = true;
            }
            if ui.button("Settings").clicked() {
                self.show_settings = true;
            }
            if ui.button("Layers").clicked() {
                self.show_layers = true;
            }
            if ui.button("Palette").clicked() {
                self.show_palette = true;
            }
        });
        if let Some(action) = selection_action {
            self.run_selection_menu_action(action);
        }
    }

    fn selection_menu_content(&mut self, ui: &mut egui::Ui) -> Option<SelectionMenuAction> {
        let has_selection = !self.selected_operation_ids.is_empty();
        let can_paste = !self.selection_clipboard.operations.is_empty()
            && self.document.layer_is_editable(self.active_layer_id);
        let mut action = None;
        ui.horizontal(|ui| {
            if ui
                .selectable_label(
                    self.selection_target_mode == SelectionTargetMode::Object,
                    "Object",
                )
                .clicked()
            {
                action = Some(SelectionMenuAction::TargetMode(SelectionTargetMode::Object));
            }
            if ui
                .selectable_label(
                    self.selection_target_mode == SelectionTargetMode::Area,
                    "Area",
                )
                .clicked()
            {
                action = Some(SelectionMenuAction::TargetMode(SelectionTargetMode::Area));
            }
        });
        match self.selection_target_mode {
            SelectionTargetMode::Object => {
                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut self.rectangle_selection_mode,
                        RectangleSelectionMode::Inside,
                        "Inside",
                    );
                    ui.selectable_value(
                        &mut self.rectangle_selection_mode,
                        RectangleSelectionMode::Crossing,
                        "Crossing",
                    );
                });
            }
            SelectionTargetMode::Area => {
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(
                            self.area_selection_shape == AreaSelectionShape::Rectangle,
                            "Rectangle",
                        )
                        .clicked()
                    {
                        action = Some(SelectionMenuAction::AreaShape(
                            AreaSelectionShape::Rectangle,
                        ));
                    }
                    if ui
                        .selectable_label(
                            self.area_selection_shape == AreaSelectionShape::Lasso,
                            "Lasso",
                        )
                        .clicked()
                    {
                        action = Some(SelectionMenuAction::AreaShape(AreaSelectionShape::Lasso));
                    }
                });
            }
        }
        ui.separator();
        if ui
            .add_enabled(has_selection, egui::Button::new("Cut"))
            .clicked()
        {
            action = Some(SelectionMenuAction::Clipboard(ClipboardCommand::Cut));
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("Copy"))
            .clicked()
        {
            action = Some(SelectionMenuAction::Clipboard(ClipboardCommand::Copy));
            ui.close();
        }
        if ui
            .add_enabled(can_paste, egui::Button::new("Paste"))
            .clicked()
        {
            action = Some(SelectionMenuAction::Clipboard(ClipboardCommand::Paste));
            ui.close();
        }
        if ui
            .add_enabled(can_paste, egui::Button::new("Paste in place"))
            .clicked()
        {
            action = Some(SelectionMenuAction::Clipboard(
                ClipboardCommand::PasteInPlace,
            ));
            ui.close();
        }
        ui.separator();
        if ui
            .add_enabled(has_selection, egui::Button::new("Flip Horizontal"))
            .clicked()
        {
            action = Some(SelectionMenuAction::Flip(SelectionFlipAxis::Horizontal));
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("Flip Vertical"))
            .clicked()
        {
            action = Some(SelectionMenuAction::Flip(SelectionFlipAxis::Vertical));
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("Recolor selected"))
            .clicked()
        {
            action = Some(SelectionMenuAction::Recolor);
            ui.close();
        }
        if ui
            .add_enabled(
                has_selection && self.selection_target_mode == SelectionTargetMode::Object,
                egui::Button::new("Freeze/Compact selected"),
            )
            .clicked()
        {
            action = Some(SelectionMenuAction::FreezeCompact);
            ui.close();
        }
        ui.separator();
        if ui
            .add_enabled(
                has_selection || self.area_selection.is_some(),
                egui::Button::new("Deselect"),
            )
            .clicked()
        {
            action = Some(SelectionMenuAction::Deselect);
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("Delete"))
            .clicked()
        {
            action = Some(SelectionMenuAction::Delete);
            ui.close();
        }
        action
    }

    fn run_selection_menu_action(&mut self, action: SelectionMenuAction) {
        match action {
            SelectionMenuAction::TargetMode(mode) => self.set_selection_target_mode(mode),
            SelectionMenuAction::AreaShape(shape) => self.set_area_selection_shape(shape),
            SelectionMenuAction::Clipboard(command) => self.run_clipboard_command(command),
            SelectionMenuAction::Deselect => {
                self.clear_transient_tools();
                self.status_message = "Selection cleared".to_owned();
            }
            SelectionMenuAction::Delete => self.delete_selected(),
            SelectionMenuAction::Flip(axis) => self.flip_selected(axis),
            SelectionMenuAction::Recolor => self.recolor_selected(),
            SelectionMenuAction::FreezeCompact => self.freeze_compact_selected(),
        }
    }

    fn drawing_tool_menu_content(&mut self, ui: &mut egui::Ui) {
        ui.label("Color");
        ui.set_min_width(280.0);
        self.color_controls(ui, 280.0);
        ui.add(
            egui::Slider::new(&mut self.brush_size, MIN_BRUSH_SIZE..=MAX_BRUSH_SIZE).text("Size"),
        );
    }

    fn color_controls(&mut self, ui: &mut egui::Ui, width: f32) {
        let previous_slider_width = ui.spacing().slider_width;
        ui.spacing_mut().slider_width = width.clamp(MIN_PALETTE_WIDTH, MAX_PALETTE_WIDTH);
        let mut color = Color32::from_rgb(self.color.r, self.color.g, self.color.b);
        if egui::color_picker::color_picker_color32(
            ui,
            &mut color,
            egui::color_picker::Alpha::Opaque,
        ) {
            self.color = color_from_srgb([color.r(), color.g(), color.b()]);
        }
        ui.spacing_mut().slider_width = previous_slider_width;
    }

    fn palette_window(&mut self, context: &egui::Context) {
        if !self.show_palette {
            return;
        }

        let mut open = self.show_palette;
        egui::Window::new("Palette")
            .open(&mut open)
            .default_width(DEFAULT_PALETTE_WIDTH)
            .default_height(360.0)
            .min_width(MIN_PALETTE_WIDTH + 24.0)
            .resizable(true)
            .show(context, |ui| {
                let picker_width = ui
                    .available_width()
                    .clamp(MIN_PALETTE_WIDTH, MAX_PALETTE_WIDTH);
                self.color_controls(ui, picker_width);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("RGB");
                    ui.monospace(format!(
                        "#{:02X}{:02X}{:02X}",
                        self.color.r, self.color.g, self.color.b
                    ));
                });
            });
        self.show_palette = open;
    }

    fn canvas_context_menu(&mut self, response: &egui::Response) {
        let Some(kind) = canvas_context_menu_kind(self.tool) else {
            return;
        };
        let mut action = None;
        response.context_menu(|ui| match kind {
            CanvasContextMenuKind::Brush => {
                ui.strong("Brush");
                ui.separator();
                self.drawing_tool_menu_content(ui);
            }
            CanvasContextMenuKind::Fill => {
                ui.strong("Fill");
                ui.separator();
                self.drawing_tool_menu_content(ui);
            }
            CanvasContextMenuKind::Selection => {
                ui.strong("Selection");
                ui.separator();
                action = self.selection_menu_content(ui);
            }
        });
        if let Some(action) = action {
            self.run_selection_menu_action(action);
        }
    }

    fn file_window(&mut self, context: &egui::Context) {
        if !self.show_file {
            return;
        }

        let mut open = self.show_file;
        let mut new_requested = false;
        let mut open_requested = false;
        egui::Window::new("File")
            .open(&mut open)
            .default_width(680.0)
            .resizable(true)
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.file_window_tab, FileWindowTab::File, "File");
                    ui.selectable_value(&mut self.file_window_tab, FileWindowTab::Help, "Help");
                });
                ui.separator();
                match self.file_window_tab {
                    FileWindowTab::File => {
                        ui.horizontal(|ui| {
                            new_requested = ui.button("New").clicked();
                            open_requested = ui.button("Open").clicked();
                        });
                        ui.separator();
                        ui.add(egui::Label::new(self.document.root().display().to_string()).wrap());
                    }
                    FileWindowTab::Help => {
                        help_content(ui, &self.help_library, &mut self.help_language_id);
                    }
                }
            });
        self.show_file = open;
        if new_requested {
            self.show_file = false;
            self.new_dialog();
        } else if open_requested {
            self.show_file = false;
            self.open_dialog();
        }
    }

    fn navigation_window(&mut self, context: &egui::Context) {
        if !self.show_navigation {
            return;
        }

        self.coordinate_overlay.update(&self.camera);
        let mut open = self.show_navigation;
        let mut depth_jump_requested = false;
        let mut lateral_jump_requested = false;
        let mut origin_requested = false;
        let mut bookmarks_requested = false;
        let local_x = format!("{:.17}", self.camera.local_x);
        let local_y = format!("{:.17}", self.camera.local_y);

        egui::Window::new("Navigation")
            .open(&mut open)
            .default_width(520.0)
            .resizable(true)
            .show(context, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Depth");
                    ui.monospace(self.camera.depth.to_string());
                    ui.separator();
                    ui.label("Zoom");
                    ui.monospace(format!("{:.9}x", self.camera.zoom));
                });
                ui.separator();
                navigation_coordinate_row(
                    ui,
                    "Tile X",
                    &self.coordinate_overlay.formatted_x,
                    &self.coordinate_overlay.exact_x,
                );
                navigation_coordinate_row(
                    ui,
                    "Tile Y",
                    &self.coordinate_overlay.formatted_y,
                    &self.coordinate_overlay.exact_y,
                );
                navigation_coordinate_row(ui, "Local X", &local_x, &local_x);
                navigation_coordinate_row(ui, "Local Y", &local_y, &local_y);
                ui.separator();
                ui.label("Depth jump");
                ui.horizontal(|ui| {
                    ui.label("Target");
                    let response = ui.add(
                        egui::DragValue::new(&mut self.depth_jump_target)
                            .range(MIN_QUICK_DEPTH..=MAX_QUICK_DEPTH)
                            .speed(1),
                    );
                    let submit_by_enter = response.lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    depth_jump_requested = ui.button("Go").clicked() || submit_by_enter;
                });
                ui.separator();
                ui.label("Tile X/Y jump");
                ui.horizontal(|ui| {
                    ui.label("X");
                    let width = ui.available_width();
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut self.lateral_jump_x)
                            .desired_width(width)
                            .char_limit(MAX_LATERAL_COORDINATE_DIGITS + 8),
                    );
                    if response.lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter))
                    {
                        lateral_jump_requested = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Y");
                    let width = ui.available_width();
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut self.lateral_jump_y)
                            .desired_width(width)
                            .char_limit(MAX_LATERAL_COORDINATE_DIGITS + 8),
                    );
                    if response.lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter))
                    {
                        lateral_jump_requested = true;
                    }
                });
                ui.horizontal(|ui| {
                    lateral_jump_requested |= ui.button("Go XY").clicked();
                    origin_requested = ui.button("Origin").clicked();
                    bookmarks_requested = ui.button("Bookmarks").clicked();
                });
            });

        self.show_navigation = open;
        if depth_jump_requested {
            self.jump_to_target_depth();
        }
        if lateral_jump_requested {
            self.jump_to_lateral_target();
        } else if origin_requested {
            self.jump_to_lateral_origin();
        }
        if bookmarks_requested {
            self.show_bookmarks = true;
        }
    }

    fn settings_window(&mut self, context: &egui::Context) {
        if !self.show_settings {
            return;
        }

        let mut open = self.show_settings;
        let mut changed = false;
        let mut reset_requested = false;
        let previous_tile_worker_count = self.settings.tile_worker_count;
        let previous_tile_resolution = self.settings.tile_resolution_px;
        let previous_pause_tile_generation = self.settings.pause_tile_generation;
        let previous_pause_while_drawing = self.settings.pause_tile_generation_while_drawing;
        let previous_rebuild_policy = self.settings.tile_rebuild_policy;
        let previous_prefetch_radius = self.settings.tile_prefetch_radius;
        let previous_distant_tiles_enabled = self.settings.distant_tiles_enabled;
        let previous_vector_depth_radius = self.settings.vector_depth_radius;
        let previous_depth_capture_radius = self.settings.depth_capture_radius;
        let previous_edge_quality = self.settings.edge_quality;
        let previous_smoothing = self.settings.smoothing;
        let previous_cache_size_mib = self.settings.cache_size_mib;
        let previous_png_compression = self.settings.png_compression;
        let previous_storage_commit_mode = self.settings.storage_commit_mode;
        let previous_session_logging = self.settings.session_logging;

        egui::Window::new("Settings")
            .open(&mut open)
            .default_width(360.0)
            .resizable(false)
            .collapsible(false)
            .show(context, |ui| {
                let mut active_profile = self.settings.performance_profile();
                ui.horizontal(|ui| {
                    ui.label("Profile");
                    for profile in PerformanceProfile::PRESETS {
                        if ui
                            .selectable_label(active_profile == profile, profile.label())
                            .clicked()
                        {
                            self.settings.apply_performance_profile(profile);
                            active_profile = profile;
                            changed = true;
                        }
                    }
                    let _ = ui.selectable_label(
                        active_profile == PerformanceProfile::Custom,
                        PerformanceProfile::Custom.label(),
                    );
                });
                ui.separator();
                changed |= ui
                    .add(
                        egui::Slider::new(
                            &mut self.settings.brush_input_spacing_px,
                            MIN_BRUSH_INPUT_SPACING_PX..=MAX_BRUSH_INPUT_SPACING_PX,
                        )
                        .text("Brush input px"),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::Slider::new(
                            &mut self.settings.fill_input_spacing_px,
                            MIN_FILL_INPUT_SPACING_PX..=MAX_FILL_INPUT_SPACING_PX,
                        )
                        .text("Fill input px"),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::Slider::new(
                            &mut self.settings.fill_fallback_max_points,
                            128..=65_536,
                        )
                        .text("Fill fallback points"),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::Slider::new(&mut self.settings.fill_fallback_max_depth_delta, 0..=32)
                            .text("Fill fallback depth"),
                    )
                    .changed();
                ui.horizontal(|ui| {
                    ui.label("Stroke fallback joins");
                    egui::ComboBox::from_id_salt("stroke_fallback_joins")
                        .selected_text(self.settings.stroke_fallback_joins.label())
                        .show_ui(ui, |ui| {
                            for mode in StrokeFallbackJoinMode::ALL {
                                changed |= ui
                                    .selectable_value(
                                        &mut self.settings.stroke_fallback_joins,
                                        mode,
                                        mode.label(),
                                    )
                                    .changed();
                            }
                        });
                });
                changed |= ui
                    .add(
                        egui::Slider::new(
                            &mut self.settings.saved_fallback_operation_limit,
                            0..=MAX_SAVED_FALLBACK_OPERATION_LIMIT,
                        )
                        .text("Saved fallback ops"),
                    )
                    .changed();
                if self.settings.saved_fallback_operation_limit == 0 {
                    ui.small("Saved fallback ops: Unlimited");
                }
                changed |= ui
                    .add(
                        egui::Slider::new(&mut self.settings.fast_zoom_tile_settle_ms, 0..=1_000)
                            .text("Zoom settle ms"),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::Slider::new(&mut self.settings.tile_worker_count, 1..=4)
                            .text("Tile workers"),
                    )
                    .changed();
                changed |= ui
                    .checkbox(&mut self.settings.distant_tiles_enabled, "Distant tiles")
                    .changed();
                ui.add_enabled_ui(self.settings.distant_tiles_enabled, |ui| {
                    changed |= ui
                        .add(
                            egui::Slider::new(
                                &mut self.settings.vector_depth_radius,
                                0..=MAX_VECTOR_DEPTH_RADIUS,
                            )
                            .text("Vector depth radius"),
                        )
                        .changed();
                    ui.small(format!(
                        "±{} ({} levels)",
                        self.settings.vector_depth_radius,
                        self.settings
                            .vector_depth_radius
                            .saturating_mul(2)
                            .saturating_add(1)
                    ));
                });
                ui.horizontal(|ui| {
                    ui.label("Tile resolution");
                    egui::ComboBox::from_id_salt("tile_resolution")
                        .selected_text(format!("{} px", self.settings.tile_resolution_px))
                        .show_ui(ui, |ui| {
                            for resolution in TILE_RESOLUTIONS {
                                changed |= ui
                                    .selectable_value(
                                        &mut self.settings.tile_resolution_px,
                                        resolution,
                                        format!("{resolution} px"),
                                    )
                                    .changed();
                            }
                        });
                });
                changed |= ui
                    .checkbox(
                        &mut self.settings.pause_tile_generation,
                        "Pause tile generation",
                    )
                    .changed();
                changed |= ui
                    .checkbox(
                        &mut self.settings.pause_tile_generation_while_drawing,
                        "Pause tile generation while drawing",
                    )
                    .changed();
                changed |= ui
                    .checkbox(
                        &mut self.settings.deferred_drawing_preview,
                        "Deferred drawing preview",
                    )
                    .changed();
                ui.horizontal(|ui| {
                    ui.label("Rebuild policy");
                    egui::ComboBox::from_id_salt("tile_rebuild_policy")
                        .selected_text(self.settings.tile_rebuild_policy.label())
                        .show_ui(ui, |ui| {
                            for policy in TileRebuildPolicy::ALL {
                                changed |= ui
                                    .selectable_value(
                                        &mut self.settings.tile_rebuild_policy,
                                        policy,
                                        policy.label(),
                                    )
                                    .changed();
                            }
                        });
                });
                changed |= ui
                    .add(
                        egui::Slider::new(
                            &mut self.settings.tile_prefetch_radius,
                            0..=MAX_TILE_PREFETCH_RADIUS,
                        )
                        .text("Prefetch tiles"),
                    )
                    .changed();
                ui.horizontal(|ui| {
                    ui.label("Edge quality");
                    egui::ComboBox::from_id_salt("edge_quality")
                        .selected_text(self.settings.edge_quality.label())
                        .show_ui(ui, |ui| {
                            for quality in EdgeQuality::ALL {
                                changed |= ui
                                    .selectable_value(
                                        &mut self.settings.edge_quality,
                                        quality,
                                        quality.label(),
                                    )
                                    .changed();
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.label("Smoothing");
                    egui::ComboBox::from_id_salt("smoothing")
                        .selected_text(self.settings.smoothing.label())
                        .show_ui(ui, |ui| {
                            for smoothing in SmoothingLevel::ALL {
                                changed |= ui
                                    .selectable_value(
                                        &mut self.settings.smoothing,
                                        smoothing,
                                        smoothing.label(),
                                    )
                                    .changed();
                            }
                        });
                });
                changed |= ui
                    .add(
                        egui::Slider::new(
                            &mut self.settings.cache_size_mib,
                            MIN_CACHE_SIZE_MIB..=MAX_CACHE_SIZE_MIB,
                        )
                        .logarithmic(true)
                        .text("Cache size MiB"),
                    )
                    .changed();
                ui.horizontal(|ui| {
                    ui.label("PNG compression");
                    egui::ComboBox::from_id_salt("png_compression")
                        .selected_text(self.settings.png_compression.label())
                        .show_ui(ui, |ui| {
                            for compression in PngCompression::ALL {
                                changed |= ui
                                    .selectable_value(
                                        &mut self.settings.png_compression,
                                        compression,
                                        compression.label(),
                                    )
                                    .changed();
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.label("Storage commit");
                    egui::ComboBox::from_id_salt("storage_commit_mode")
                        .selected_text(self.settings.storage_commit_mode.label())
                        .show_ui(ui, |ui| {
                            for mode in StorageCommitMode::ALL {
                                changed |= ui
                                    .selectable_value(
                                        &mut self.settings.storage_commit_mode,
                                        mode,
                                        mode.label(),
                                    )
                                    .changed();
                            }
                        });
                });
                if self.settings.storage_commit_mode == StorageCommitMode::Fast {
                    ui.small("Fast uses SQLite synchronous=NORMAL: lower commit latency, less protection from OS or power loss.");
                }
                changed |= ui
                    .add(
                        egui::Slider::new(
                            &mut self.settings.preview_fps,
                            MIN_PREVIEW_FPS..=MAX_PREVIEW_FPS,
                        )
                        .text("Preview FPS"),
                    )
                    .changed();
                ui.horizontal(|ui| {
                    ui.label("Keep latest objects");
                    egui::ComboBox::from_id_salt("object_compaction_limit")
                        .selected_text(self.settings.object_compaction_limit.label())
                        .show_ui(ui, |ui| {
                            for limit in ObjectCompactionLimit::ALL {
                                changed |= ui
                                    .selectable_value(
                                        &mut self.settings.object_compaction_limit,
                                        limit,
                                        limit.label(),
                                    )
                                    .changed();
                            }
                        });
                });
                let compact_enabled = self
                    .settings
                    .object_compaction_limit
                    .keep_latest()
                    .is_some();
                if ui
                    .add_enabled(compact_enabled, egui::Button::new("Compact older now"))
                    .clicked()
                {
                    self.compact_older_objects_now();
                }
                ui.separator();
                egui::CollapsingHeader::new("Display")
                    .default_open(false)
                    .show(ui, |ui| {
                        let mut active_profile = self.settings.overlay_profile();
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Overlay");
                            for profile in OverlayProfile::PRESETS {
                                if ui
                                    .selectable_label(active_profile == profile, profile.label())
                                    .clicked()
                                {
                                    self.settings.apply_overlay_profile(profile);
                                    active_profile = profile;
                                    changed = true;
                                }
                            }
                            let _ = ui.selectable_label(
                                active_profile == OverlayProfile::Custom,
                                OverlayProfile::Custom.label(),
                            );
                        });
                        changed |= ui
                            .checkbox(&mut self.settings.overlay_enabled, "Show canvas overlay")
                            .changed();
                        ui.add_enabled_ui(self.settings.overlay_enabled, |ui| {
                            egui::Grid::new("canvas_overlay_fields")
                                .num_columns(2)
                                .show(ui, |ui| {
                                    changed |= ui
                                        .checkbox(&mut self.settings.overlay_show_depth, "Depth")
                                        .changed();
                                    changed |= ui
                                        .checkbox(&mut self.settings.overlay_show_zoom, "Zoom")
                                        .changed();
                                    ui.end_row();
                                    changed |= ui
                                        .checkbox(
                                            &mut self.settings.overlay_show_tile_coordinates,
                                            "Tile X/Y",
                                        )
                                        .changed();
                                    changed |= ui
                                        .checkbox(
                                            &mut self.settings.overlay_show_local_coordinates,
                                            "Local X/Y",
                                        )
                                        .changed();
                                    ui.end_row();
                                    changed |= ui
                                        .checkbox(
                                            &mut self.settings.overlay_show_operation_count,
                                            "Operation count",
                                        )
                                        .changed();
                                    changed |= ui
                                        .checkbox(
                                            &mut self.settings.overlay_show_performance,
                                            "FPS / frame time",
                                        )
                                        .changed();
                                    ui.end_row();
                                    changed |= ui
                                        .checkbox(&mut self.settings.overlay_show_status, "Status")
                                        .changed();
                                    changed |= ui
                                        .checkbox(
                                            &mut self.settings.overlay_show_tile_state,
                                            "Tile / rebuild state",
                                        )
                                        .changed();
                                    ui.end_row();
                                });
                        });
                    });
                egui::CollapsingHeader::new("Diagnostics")
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Session logging");
                            egui::ComboBox::from_id_salt("session_logging")
                                .selected_text(self.settings.session_logging.label())
                                .show_ui(ui, |ui| {
                                    for level in SessionLoggingLevel::ALL {
                                        changed |= ui
                                            .selectable_value(
                                                &mut self.settings.session_logging,
                                                level,
                                                level.label(),
                                            )
                                            .changed();
                                    }
                                });
                        });
                        if ui
                            .add_enabled(
                                self.settings.session_logging != SessionLoggingLevel::Off,
                                egui::Button::new("Mark log"),
                            )
                            .clicked()
                        {
                            self.mark_session_log();
                        }
                        if let Some(path) = self.session_logger.path() {
                            ui.small(path.display().to_string());
                        } else {
                            ui.small("local/logs/session-YYYYMMDD-HHMMSS.jsonl");
                        }
                    });
                ui.separator();
                if ui.button("Reset").clicked() {
                    reset_requested = true;
                }
            });

        self.show_settings = open;
        if reset_requested {
            self.settings = AppSettings::default();
            changed = true;
        }
        if changed {
            self.settings.normalize();
            if self.settings.depth_capture_radius != previous_depth_capture_radius {
                self.clear_object_selection_state();
            }
            let tile_worker_count_changed =
                self.settings.tile_worker_count != previous_tile_worker_count;
            let tile_resolution_changed =
                self.settings.tile_resolution_px != previous_tile_resolution;
            let pause_tile_generation_changed =
                self.settings.pause_tile_generation != previous_pause_tile_generation;
            let pause_while_drawing_changed =
                self.settings.pause_tile_generation_while_drawing != previous_pause_while_drawing;
            let rebuild_policy_changed =
                self.settings.tile_rebuild_policy != previous_rebuild_policy;
            let prefetch_radius_changed =
                self.settings.tile_prefetch_radius != previous_prefetch_radius;
            let distant_tile_gate_changed = self.settings.distant_tiles_enabled
                != previous_distant_tiles_enabled
                || self.settings.vector_depth_radius != previous_vector_depth_radius;
            let render_quality_changed = self.settings.edge_quality != previous_edge_quality
                || self.settings.smoothing != previous_smoothing;
            let cache_settings_changed = self.settings.cache_size_mib != previous_cache_size_mib
                || self.settings.png_compression != previous_png_compression;
            let storage_commit_mode_changed =
                self.settings.storage_commit_mode != previous_storage_commit_mode;
            let session_logging_changed = self.settings.session_logging != previous_session_logging;
            if tile_worker_count_changed || render_quality_changed || cache_settings_changed {
                self.recreate_tile_scheduler();
            } else if tile_resolution_changed {
                self.invalidate_tile_rendering();
            }
            if pause_tile_generation_changed {
                self.apply_tile_generation_pause();
            }
            if pause_while_drawing_changed {
                self.apply_pause_while_drawing_setting();
            }
            if rebuild_policy_changed || prefetch_radius_changed || distant_tile_gate_changed {
                self.apply_tile_request_settings();
            }
            if storage_commit_mode_changed {
                self.apply_storage_commit_mode();
            }
            if session_logging_changed {
                match self.session_logger.set_level(self.settings.session_logging) {
                    Ok(()) => self.log_session_event("settings_logging_changed"),
                    Err(error) => {
                        self.status_message = format!("Session logging failed: {error:#}");
                        log::error!("{}", self.status_message);
                    }
                }
            }
            self.save_settings();
        }
    }

    fn layers_window(&mut self, context: &egui::Context) {
        if !self.show_layers {
            return;
        }

        let layers = self.document.layers().to_vec();
        let operation_counts = layer_operation_counts(self.document.operations());
        let active_index = layers
            .iter()
            .position(|layer| layer.id == self.active_layer_id);
        let can_move_up = active_index.is_some_and(|index| index + 1 < layers.len());
        let can_move_down = active_index.is_some_and(|index| index > 0);
        let can_merge_down = active_index
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| layers.get(index))
            .is_some_and(|destination| {
                self.document.layer_is_editable(self.active_layer_id)
                    && destination.visible
                    && !destination.locked
            });
        let mut open = self.show_layers;
        let mut create_requested = false;
        let mut duplicate_requested = false;
        let mut delete_requested = false;
        let mut rename_requested = false;
        let mut merge_down_requested = false;
        let mut move_direction = 0;
        let mut move_selection_target = None;
        let mut visibility_request = None;
        let mut lock_request = None;
        let mut depth_capture_changed = false;
        egui::Window::new("Layers")
            .open(&mut open)
            .default_width(280.0)
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    create_requested = ui.button("+").on_hover_text("New layer").clicked();
                    duplicate_requested = ui
                        .add_enabled(active_index.is_some(), egui::Button::new("Duplicate"))
                        .clicked();
                    delete_requested = ui
                        .add_enabled(layers.len() > 1, egui::Button::new("Delete"))
                        .clicked();
                    if ui
                        .add_enabled(can_move_up, egui::Button::new("Up"))
                        .clicked()
                    {
                        move_direction = 1;
                    }
                    if ui
                        .add_enabled(can_move_down, egui::Button::new("Down"))
                        .clicked()
                    {
                        move_direction = -1;
                    }
                });
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.layer_name_edit);
                    rename_requested = ui.button("Rename").clicked();
                });
                let can_move_selection = !self.selected_operation_ids.is_empty()
                    && self.document.layer_is_editable(self.active_layer_id)
                    && layers.iter().any(|layer| {
                        layer.id != self.active_layer_id && layer.visible && !layer.locked
                    });
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(can_move_selection, |ui| {
                        ui.menu_button("Move selection to", |ui| {
                            for layer in layers.iter().rev() {
                                if layer.id == self.active_layer_id {
                                    continue;
                                }
                                if ui
                                    .add_enabled(
                                        layer.visible && !layer.locked,
                                        egui::Button::new(&layer.name),
                                    )
                                    .clicked()
                                {
                                    move_selection_target = Some(layer.id);
                                    ui.close();
                                }
                            }
                        });
                    });
                    merge_down_requested = ui
                        .add_enabled(can_merge_down, egui::Button::new("Merge Down"))
                        .clicked();
                });
                ui.horizontal(|ui| {
                    depth_capture_changed |= ui
                        .checkbox(&mut self.settings.depth_capture_auto, "Auto")
                        .on_hover_text("Follow Vector depth radius")
                        .changed();
                    ui.add_enabled_ui(!self.settings.depth_capture_auto, |ui| {
                        depth_capture_changed |= ui
                            .add(
                                egui::Slider::new(
                                    &mut self.settings.depth_capture_radius,
                                    0..=self.settings.vector_depth_radius,
                                )
                                .text("Depth capture ±"),
                            )
                            .on_hover_text(
                                "Object Selection can target complete vector objects in this \
                                 camera-relative native-depth radius",
                            )
                            .changed();
                    });
                    let levels = self
                        .settings
                        .depth_capture_radius
                        .saturating_mul(2)
                        .saturating_add(1);
                    ui.small(format!("{levels} levels"));
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(420.0)
                    .show(ui, |ui| {
                        for layer in layers.iter().rev() {
                            ui.horizontal(|ui| {
                                let count = operation_counts.get(&layer.id).copied().unwrap_or(0);
                                let mut visible = layer.visible;
                                if ui
                                    .checkbox(&mut visible, "")
                                    .on_hover_text("Layer visibility")
                                    .changed()
                                {
                                    visibility_request = Some((layer.id, visible));
                                }
                                let mut locked = layer.locked;
                                if ui
                                    .checkbox(&mut locked, "")
                                    .on_hover_text("Lock layer editing")
                                    .changed()
                                {
                                    lock_request = Some((layer.id, locked));
                                }
                                if ui
                                    .selectable_label(self.active_layer_id == layer.id, &layer.name)
                                    .clicked()
                                {
                                    self.activate_layer(layer.id, layer.name.clone());
                                }
                                ui.label(format!("{count} ops"));
                            });
                        }
                    });
            });
        self.show_layers = open;
        if depth_capture_changed {
            self.settings.normalize();
            self.clear_object_selection_state();
            self.save_settings();
        }
        if create_requested {
            self.create_layer();
        } else if duplicate_requested {
            self.duplicate_active_layer();
        } else if delete_requested {
            self.request_delete_active_layer();
        } else if rename_requested {
            self.rename_active_layer();
        } else if merge_down_requested {
            self.merge_active_layer_down();
        } else if move_direction != 0 {
            self.move_active_layer(move_direction);
        } else if let Some(layer_id) = move_selection_target {
            self.move_selection_to_layer(layer_id);
        } else if let Some((layer_id, visible)) = visibility_request {
            self.set_layer_visibility(layer_id, visible);
        } else if let Some((layer_id, locked)) = lock_request {
            self.set_layer_locked(layer_id, locked);
        }
    }

    fn create_layer(&mut self) {
        let name = format!("Layer {}", self.document.layers().len() + 1);
        match self.document.create_layer(&name) {
            Ok(layer) => {
                self.activate_layer(layer.id, layer.name);
                self.status_message = "Layer created".to_owned();
            }
            Err(error) => self.status_message = format!("Layer creation failed: {error:#}"),
        }
    }

    fn rename_active_layer(&mut self) {
        match self
            .document
            .rename_layer(self.active_layer_id, &self.layer_name_edit)
        {
            Ok(true) => {
                self.layer_name_edit = self.layer_name_edit.trim().to_owned();
                self.status_message = "Layer renamed".to_owned();
            }
            Ok(false) => self.status_message = "Active layer not found".to_owned(),
            Err(error) => self.status_message = format!("Layer rename failed: {error:#}"),
        }
    }

    fn duplicate_active_layer(&mut self) {
        match self.document.duplicate_layer(self.active_layer_id) {
            Ok(Some((layer, operation_count))) => {
                self.activate_layer(layer.id, layer.name);
                self.invalidate_tile_rendering();
                self.status_message = format!("Layer duplicated ({operation_count} objects)");
            }
            Ok(None) => self.status_message = "Active layer not found".to_owned(),
            Err(error) => self.status_message = format!("Layer duplication failed: {error:#}"),
        }
    }

    fn request_delete_active_layer(&mut self) {
        if self.document.layers().len() <= 1 {
            self.status_message = "The last layer cannot be deleted".to_owned();
            return;
        }
        let operation_count = self
            .document
            .operations()
            .iter()
            .filter(|operation| operation.layer_id == self.active_layer_id)
            .count();
        if operation_count == 0 {
            self.delete_layer(self.active_layer_id);
        } else {
            self.pending_layer_delete = Some(self.active_layer_id);
        }
    }

    fn layer_delete_confirmation(&mut self, context: &egui::Context) {
        let Some(layer_id) = self.pending_layer_delete else {
            return;
        };
        let Some(layer) = self
            .document
            .layers()
            .iter()
            .find(|layer| layer.id == layer_id)
        else {
            self.pending_layer_delete = None;
            return;
        };
        let layer_name = layer.name.clone();
        let operation_count = self
            .document
            .operations()
            .iter()
            .filter(|operation| operation.layer_id == layer_id)
            .count();
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new("Delete layer?")
            .collapsible(false)
            .resizable(false)
            .show(context, |ui| {
                ui.label(format!(
                    "\"{layer_name}\" contains {operation_count} objects."
                ));
                ui.label("The layer and its contents can be restored with Undo.");
                ui.horizontal(|ui| {
                    confirm = ui.button("Delete").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if confirm {
            self.pending_layer_delete = None;
            self.delete_layer(layer_id);
        } else if cancel {
            self.pending_layer_delete = None;
        }
    }

    fn delete_layer(&mut self, layer_id: Uuid) {
        let layers = self.document.layers().to_vec();
        let Some(index) = layers.iter().position(|layer| layer.id == layer_id) else {
            self.status_message = "Layer not found".to_owned();
            return;
        };
        let next_active = layers
            .get(index + 1)
            .or_else(|| index.checked_sub(1).and_then(|index| layers.get(index)))
            .map(|layer| (layer.id, layer.name.clone()));
        match self.document.delete_layer(layer_id) {
            Ok(true) => {
                if let Some((id, name)) = next_active {
                    self.activate_layer(id, name);
                }
                self.clear_transient_tools();
                self.invalidate_tile_rendering();
                self.status_message = "Layer deleted".to_owned();
            }
            Ok(false) => self.status_message = "Layer not found".to_owned(),
            Err(error) => self.status_message = format!("Layer deletion failed: {error:#}"),
        }
    }

    fn move_active_layer(&mut self, direction: i32) {
        match self.document.move_layer(self.active_layer_id, direction) {
            Ok(true) => {
                self.invalidate_tile_rendering();
                self.status_message = if direction > 0 {
                    "Layer moved up".to_owned()
                } else {
                    "Layer moved down".to_owned()
                };
            }
            Ok(false) => self.status_message = "Layer is already at the edge".to_owned(),
            Err(error) => self.status_message = format!("Layer reorder failed: {error:#}"),
        }
    }

    fn merge_active_layer_down(&mut self) {
        match self.document.merge_layer_down(self.active_layer_id) {
            Ok(Some((destination, merged_count))) => {
                self.activate_layer(destination.id, destination.name);
                self.invalidate_tile_rendering();
                self.status_message = format!("Merged {merged_count} objects down");
            }
            Ok(None) => self.status_message = "No lower layer to merge into".to_owned(),
            Err(error) => self.status_message = format!("Merge Down failed: {error:#}"),
        }
    }

    fn move_selection_to_layer(&mut self, target_layer_id: Uuid) {
        let Some(target_name) = self
            .document
            .layers()
            .iter()
            .find(|layer| layer.id == target_layer_id)
            .map(|layer| layer.name.clone())
        else {
            self.status_message = "Target layer not found".to_owned();
            return;
        };
        let selected = self.selected_operation_ids.clone();
        match self.document.move_operations_to_layer(
            &selected,
            self.active_layer_id,
            target_layer_id,
        ) {
            Ok(ids) if !ids.is_empty() => {
                self.activate_layer(target_layer_id, target_name.clone());
                self.selected_operation_ids = ids.into_iter().collect();
                self.invalidate_tile_rendering();
                self.status_message = format!(
                    "Moved {} objects to {target_name}",
                    self.selected_object_count()
                );
            }
            Ok(_) => self.status_message = "Nothing moved".to_owned(),
            Err(error) => self.status_message = format!("Move to layer failed: {error:#}"),
        }
    }

    fn set_layer_visibility(&mut self, layer_id: Uuid, visible: bool) {
        match self.document.set_layer_visibility(layer_id, visible) {
            Ok(true) => {
                if layer_id == self.active_layer_id && !visible {
                    self.clear_transient_tools();
                }
                self.invalidate_tile_rendering();
                self.status_message = if visible {
                    "Layer shown".to_owned()
                } else {
                    "Layer hidden".to_owned()
                };
            }
            Ok(false) => self.status_message = "Layer visibility is unchanged".to_owned(),
            Err(error) => self.status_message = format!("Layer visibility failed: {error:#}"),
        }
    }

    fn set_layer_locked(&mut self, layer_id: Uuid, locked: bool) {
        match self.document.set_layer_locked(layer_id, locked) {
            Ok(true) => {
                if layer_id == self.active_layer_id && locked {
                    self.clear_transient_tools();
                }
                self.status_message = if locked {
                    "Layer locked".to_owned()
                } else {
                    "Layer unlocked".to_owned()
                };
            }
            Ok(false) => self.status_message = "Layer lock is unchanged".to_owned(),
            Err(error) => self.status_message = format!("Layer lock failed: {error:#}"),
        }
    }

    fn activate_layer(&mut self, layer_id: Uuid, name: String) {
        let changed = self.active_layer_id != layer_id;
        self.active_layer_id = layer_id;
        self.layer_name_edit = name;
        if changed {
            self.clear_transient_tools();
            self.status_message = "Active layer changed".to_owned();
        }
    }

    fn bookmark_window(&mut self, context: &egui::Context) {
        if !self.show_bookmarks {
            return;
        }

        let mut open = self.show_bookmarks;
        let mut close_requested = false;
        let mut selected_camera = None;
        let mut add_requested = false;
        let mut rename_request = None;
        let mut delete_request = None;
        let bookmarks = self.bookmarks.clone();

        egui::Window::new("Bookmarks")
            .open(&mut open)
            .default_width(520.0)
            .resizable(true)
            .collapsible(false)
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut self.new_bookmark_name)
                            .desired_width(300.0)
                            .char_limit(128),
                    );
                    let add_by_enter = response.lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    add_requested = ui.button("Add").clicked() || add_by_enter;
                });
                ui.separator();
                if bookmarks.is_empty() {
                    ui.label("No bookmarks");
                } else {
                    for bookmark in bookmarks {
                        let edit_name = self
                            .bookmark_edits
                            .entry(bookmark.id)
                            .or_insert_with(|| bookmark.name.clone());
                        ui.horizontal(|ui| {
                            if ui.button("Open").clicked() {
                                selected_camera = Some(bookmark.camera.clone());
                            }
                            let response = ui.add(
                                egui::TextEdit::singleline(edit_name)
                                    .desired_width(260.0)
                                    .clip_text(false),
                            );
                            let rename_by_enter = response.lost_focus()
                                && ui.input(|input| input.key_pressed(egui::Key::Enter));
                            if ui.button("Rename").clicked() || rename_by_enter {
                                rename_request = Some((bookmark.id, edit_name.trim().to_owned()));
                            }
                            if ui.button("Delete").clicked() {
                                delete_request = Some(bookmark.id);
                            }
                        });
                    }
                }

                ui.separator();
                if ui.button("Close").clicked() {
                    close_requested = true;
                }
            });

        if add_requested {
            self.add_current_bookmark();
        }
        if let Some(camera) = selected_camera {
            self.camera = camera;
            self.depth_jump_target = self.camera.depth;
            self.mark_zoom_changed();
            self.status_message = "Bookmark opened".to_owned();
        }
        if let Some((id, name)) = rename_request {
            self.rename_bookmark(id, name);
        }
        if let Some(id) = delete_request {
            self.delete_bookmark(id);
        }
        if close_requested {
            open = false;
        }
        self.show_bookmarks = open;
    }

    fn add_current_bookmark(&mut self) {
        let name = if self.new_bookmark_name.trim().is_empty() {
            format!("Bookmark {}", self.bookmarks.len() + 1)
        } else {
            self.new_bookmark_name.trim().to_owned()
        };
        match self.document.add_bookmark(&name, &self.camera) {
            Ok(bookmark) => {
                self.bookmark_edits
                    .insert(bookmark.id, bookmark.name.clone());
                self.bookmarks.push(bookmark);
                self.new_bookmark_name = format!("Bookmark {}", self.bookmarks.len() + 1);
                self.status_message = "Bookmark saved".to_owned();
            }
            Err(error) => self.status_message = format!("Bookmark failed: {error:#}"),
        }
    }

    fn session_log_fields(&self, fallback_mode: Option<TileFallbackMode>) -> Value {
        let fallback_mode = fallback_mode
            .or(self.last_fallback_mode)
            .map(tile_fallback_mode_label)
            .unwrap_or("unknown");
        let depth_render_mode = self
            .last_depth_render_mode
            .map(depth_render_mode_label)
            .unwrap_or("unknown");
        let (fps_ema, frame_ms_ema) = self
            .frame_rate
            .metrics()
            .map_or((None, None), |(fps, frame_ms)| (Some(fps), Some(frame_ms)));
        let canvas_rect = self.last_canvas_rect.map(|rect| {
            json!({
                "min_x": rect.min.x,
                "min_y": rect.min.y,
                "max_x": rect.max.x,
                "max_y": rect.max.y,
                "min_x_bits": rect.min.x.to_bits(),
                "min_y_bits": rect.min.y.to_bits(),
                "max_x_bits": rect.max.x.to_bits(),
                "max_y_bits": rect.max.y.to_bits(),
            })
        });
        let pointer_screen = self
            .last_pointer_position
            .map(|position| json!({ "x": position.x, "y": position.y }));
        let (pointer_history_mode, pointer_history_samples) = self.mouse_history.diagnostics();
        json!({
            "camera": {
                "depth": self.camera.depth,
                "zoom": self.camera.zoom,
                "tile_x": self.camera.tile_x.to_string(),
                "tile_y": self.camera.tile_y.to_string(),
                "local_x": self.camera.local_x,
                "local_y": self.camera.local_y,
            },
            "document": {
                "revision": self.document.revision(),
                "operation_count": self.document.operations().len(),
                "visible_render_operation_count": self.last_visible_render_operation_count,
                "max_sequence": self.document.max_sequence(),
            },
            "tiles": {
                "paused": tile_generation_is_paused(
                    self.settings.pause_tile_generation,
                    self.automatic_tile_generation_pause,
                ),
                "manual": self.settings.pause_tile_generation,
                "drawing": self.automatic_tile_generation_pause,
                "fallback_mode": fallback_mode,
                "depth_render_mode": depth_render_mode,
                "distant_tiles_enabled": self.settings.distant_tiles_enabled,
                "vector_depth_radius": self.settings.vector_depth_radius,
                "pending_count": self.pending_tiles.len(),
                "visible_tile_count": self.visible_tile_set.len(),
                "uploaded_textures": self.last_tile_frame_stats.uploaded_textures,
                "queued_jobs": self.last_tile_frame_stats.queued_jobs,
                "generation": self.visible_tile_generation,
            },
            "perf": {
                "frame_ms": self.last_frame_phase_stats.frame_ms,
                "fps_ema": fps_ema,
                "frame_ms_ema": frame_ms_ema,
                "last_fps_ema": self.last_frame_phase_stats.fps_ema,
                "input_ms": self.last_frame_phase_stats.input_ms,
                "tile_total_ms": self.last_frame_phase_stats.tile_total_ms,
                "tile_collect_upload_ms": self.last_frame_phase_stats.tile_collect_upload_ms,
                "tile_request_queue_ms": self.last_frame_phase_stats.tile_request_queue_ms,
                "tile_draw_ms": self.last_frame_phase_stats.tile_draw_ms,
                "fallback_total_ms": self.last_frame_phase_stats.fallback_total_ms,
                "fallback_stroke_shape_count": self.last_fallback_frame_stats.stroke_shape_count,
                "fallback_fill_shape_count": self.last_fallback_frame_stats.fill_shape_count,
                "fallback_segmented_strokes": self.last_fallback_frame_stats.segmented_strokes,
                "fallback_segmented_budget_fallbacks": self.last_fallback_frame_stats.segmented_budget_fallbacks,
                "fallback_fast_strokes": self.last_fallback_frame_stats.fast_strokes,
                "fallback_skipped_operations": self.last_fallback_frame_stats.skipped_operations,
                "fallback_projected_operations": self.last_fallback_frame_stats.projected_operations,
                "fallback_painted_operations": self.last_fallback_frame_stats.painted_operations,
                "fallback_cache_hits": self.last_fallback_frame_stats.fallback_cache_hits,
                "fallback_cache_misses": self.last_fallback_frame_stats.fallback_cache_misses,
                "fallback_bounds_cache_hits": self.last_fallback_frame_stats.fallback_bounds_cache_hits,
                "fallback_bounds_cache_misses": self.last_fallback_frame_stats.fallback_bounds_cache_misses,
                "fallback_visible_cache_hits": self.last_fallback_frame_stats.fallback_visible_cache_hits,
                "fallback_visible_cache_misses": self.last_fallback_frame_stats.fallback_visible_cache_misses,
                "fallback_project_ms": self.last_frame_phase_stats.fallback_project_ms,
                "fallback_visible_operations_ms": self.last_fallback_frame_stats.visible_operations_ms,
                "fallback_projection_loop_ms": self.last_fallback_frame_stats.projection_loop_ms,
                "fallback_derive_ms": self.last_frame_phase_stats.fallback_derive_ms,
                "fallback_clip_ms": self.last_frame_phase_stats.fallback_clip_ms,
                "fallback_shape_paint_ms": self.last_frame_phase_stats.fallback_shape_paint_ms,
                "overlay_ms": self.last_frame_phase_stats.overlay_ms,
                "total_frame_ms": self.last_frame_phase_stats.total_frame_ms,
            },
            "context": {
                "tool": format!("{:?}", self.tool),
                "selection_mode": format!("{:?}", self.selection_target_mode),
                "smoothing": self.settings.smoothing.label(),
                "stroke_fallback_joins": self.settings.stroke_fallback_joins.label(),
                "storage_commit_mode": self.settings.storage_commit_mode.label(),
                "auto_fast_stroke_fallback": self.last_auto_fast_stroke_fallback,
                "large_selection_fast_overlay": self.last_large_selection_fast_overlay,
                "saved_fallback_operation_limit": self.settings.saved_fallback_operation_limit,
                "tile_resolution": self.settings.tile_resolution_px,
                "canvas_rect": canvas_rect,
                "compact_limit": self.settings.object_compaction_limit.label(),
                "depth_capture_auto": self.settings.depth_capture_auto,
                "depth_capture_radius": self.settings.depth_capture_radius,
                "pointer_history_mode": pointer_history_mode,
                "pointer_history_samples": pointer_history_samples,
                "pointer_screen": pointer_screen,
                "selected_count": self.selected_operation_ids.len(),
                "status_message": self.status_message,
            },
        })
    }

    fn log_session_event(&mut self, event: &str) {
        let fields = self.session_log_fields(None);
        self.session_logger.log_event(event, fields);
    }

    fn log_session_event_with(&mut self, event: &str, extra: Value) {
        let fields = merge_json_objects(self.session_log_fields(None), extra);
        self.session_logger.log_event(event, fields);
    }

    fn log_navigation_event(&mut self, label: &str) {
        let fields = self.session_log_fields(None);
        self.session_logger.log_navigation(label, fields);
    }

    fn mark_session_log(&mut self) {
        let marker_id = self
            .session_logger
            .log_marker(self.session_log_fields(None));
        self.status_message = format!("Log marker #{marker_id}");
    }

    fn canvas(&mut self, ui: &mut egui::Ui, context: &egui::Context, window: Option<NativeWindow>) {
        let total_phase_start = Instant::now();
        let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        self.last_canvas_rect = Some(response.rect);
        painter.rect_filled(response.rect, 0.0, to_color32(BACKGROUND));
        let frame_time = ui.input(|input| input.unstable_dt);
        let pointer_pressed = context.input(|input| input.pointer.any_pressed());
        surrender_canvas_keyboard_focus(context, response.hovered(), pointer_pressed);

        let input_phase_start = Instant::now();
        self.handle_shortcuts(context);
        self.handle_navigation(ui, &response);
        let revision_before_input = self.document.revision();
        let mut depth_render_mode = self.depth_render_mode(response.rect);
        self.apply_depth_render_mode(depth_render_mode);
        self.handle_drawing(ui, &response, window);
        if self.document.revision() != revision_before_input {
            depth_render_mode = self.depth_render_mode(response.rect);
            self.apply_depth_render_mode(depth_render_mode);
        }
        let input_phase_ms = input_phase_start.elapsed().as_secs_f64() * 1_000.0;
        let pointer_down = ui.input(|input| input.pointer.any_down());
        let transient_preview_active = self.selection_drag_active
            || self.selection_move_active
            || self.area_selection_drag_active
            || self.eraser_lasso_active;
        let input_active = self.draft.is_some() || (pointer_down && !transient_preview_active);
        self.update_tile_rebuild_interaction(input_active);
        let interaction_active =
            input_active || self.zoom_tile_requests_deferred() || self.tile_rebuild_is_deferred();
        let overlay_interaction_active = interaction_active || transient_preview_active;
        let frame_ms = self.frame_rate.update(frame_time, interaction_active);
        let tile_phase_start = Instant::now();
        let (fallback_mode, tile_frame_stats) = if depth_render_mode == DepthRenderMode::DistantTile
        {
            self.paint_cached_tiles(context, &painter, response.rect)
        } else {
            let stats = TileFrameStats {
                uploaded_textures: self.collect_tile_results(context),
                ..TileFrameStats::default()
            };
            (TileFallbackMode::Full, stats)
        };
        self.last_tile_frame_stats = tile_frame_stats;
        let tile_phase_ms = tile_phase_start.elapsed().as_secs_f64() * 1_000.0;
        if self.last_fallback_mode != Some(fallback_mode) {
            self.last_fallback_mode = Some(fallback_mode);
            self.log_session_event_with(
                "fallback_mode",
                json!({ "tiles": { "fallback_mode": tile_fallback_mode_label(fallback_mode) } }),
            );
        }

        let fallback_phase_start = Instant::now();
        let tile_generation_paused = tile_generation_is_paused(
            self.settings.pause_tile_generation,
            self.automatic_tile_generation_pause,
        );
        let auto_fast_stroke_fallback = auto_fast_stroke_fallback_active(
            interaction_active,
            !self.pending_tiles.is_empty(),
            tile_generation_paused,
        );
        self.last_auto_fast_stroke_fallback = auto_fast_stroke_fallback;
        self.fallback_renderer
            .begin_frame(SavedFallbackRenderFrameKey::new(
                self.document.revision(),
                &self.camera,
                response.rect,
                &self.settings,
                auto_fast_stroke_fallback,
            ));
        let fallback_operation_limit = fallback_operation_limit(&self.settings, fallback_mode);
        self.last_fallback_frame_stats = match fallback_mode {
            TileFallbackMode::Current => FallbackFrameStats::default(),
            TileFallbackMode::OverlayAfter(sequence) => self.paint_operations_after(
                &painter,
                response.rect,
                sequence,
                auto_fast_stroke_fallback,
                fallback_operation_limit,
            ),
            TileFallbackMode::Full => self.paint_operations(
                &painter,
                response.rect,
                auto_fast_stroke_fallback,
                fallback_operation_limit,
            ),
        };
        let fallback_phase_ms = fallback_phase_start.elapsed().as_secs_f64() * 1_000.0;
        let overlay_phase_start = Instant::now();
        self.paint_draft(&painter, response.rect);
        let selection_transform_active = self.selection_move_active
            || self.selection_scale_gesture.is_some()
            || self.selection_rotate_gesture.is_some();
        self.last_large_selection_fast_overlay = large_selection_fast_overlay_active(
            self.selected_operation_ids.len(),
            overlay_interaction_active,
            selection_transform_active,
        );
        self.paint_transient_overlays(
            &painter,
            response.rect,
            self.last_large_selection_fast_overlay,
        );
        self.paint_overlay(&painter, response.rect);
        let overlay_phase_ms = overlay_phase_start.elapsed().as_secs_f64() * 1_000.0;
        self.canvas_context_menu(&response);
        let total_phase_ms = total_phase_start.elapsed().as_secs_f64() * 1_000.0;

        let mut phases = BTreeMap::new();
        phases.insert("input".to_owned(), input_phase_ms);
        phases.insert("tile_total".to_owned(), tile_phase_ms);
        phases.insert(
            "tile_collect_upload".to_owned(),
            tile_frame_stats.collect_upload_ms,
        );
        phases.insert(
            "tile_request_queue".to_owned(),
            tile_frame_stats.request_queue_ms,
        );
        phases.insert("tile_draw".to_owned(), tile_frame_stats.draw_ms);
        phases.insert("fallback_paint".to_owned(), fallback_phase_ms);
        phases.insert(
            "fallback_project".to_owned(),
            self.last_fallback_frame_stats.project_ms,
        );
        phases.insert(
            "fallback_derive".to_owned(),
            self.last_fallback_frame_stats.derive_ms,
        );
        phases.insert(
            "fallback_clip".to_owned(),
            self.last_fallback_frame_stats.clip_ms,
        );
        phases.insert(
            "fallback_shape_paint".to_owned(),
            self.last_fallback_frame_stats.paint_ms,
        );
        phases.insert("overlay".to_owned(), overlay_phase_ms);
        phases.insert("total_frame".to_owned(), total_phase_ms);
        let fps_ema = self.frame_rate.metrics().map(|(fps, _)| fps);
        self.last_frame_phase_stats = FramePhaseStats {
            frame_ms,
            fps_ema,
            input_ms: input_phase_ms,
            tile_total_ms: tile_phase_ms,
            tile_collect_upload_ms: tile_frame_stats.collect_upload_ms,
            tile_request_queue_ms: tile_frame_stats.request_queue_ms,
            tile_draw_ms: tile_frame_stats.draw_ms,
            fallback_total_ms: fallback_phase_ms,
            fallback_project_ms: self.last_fallback_frame_stats.project_ms,
            fallback_derive_ms: self.last_fallback_frame_stats.derive_ms,
            fallback_clip_ms: self.last_fallback_frame_stats.clip_ms,
            fallback_shape_paint_ms: self.last_fallback_frame_stats.paint_ms,
            overlay_ms: overlay_phase_ms,
            total_frame_ms: total_phase_ms,
        };
        let fields = self.session_log_fields(Some(fallback_mode));
        self.session_logger
            .record_frame_phases(interaction_active, &phases, &fields);
        if let Some(frame_ms) = frame_ms
            && (frame_ms >= SLOW_FRAME_MS as f32 || fps_ema.is_some_and(|fps| fps < LOW_FPS_EMA))
        {
            self.session_logger.log_fps_drop(frame_ms, fps_ema, fields);
        }
        self.session_logger.flush_due_navigation(Instant::now());
    }

    fn handle_shortcuts(&mut self, context: &egui::Context) {
        if !context.text_edit_focused() && context.input(|input| input.key_pressed(egui::Key::F1)) {
            self.file_window_tab = FileWindowTab::Help;
            self.show_file = true;
        }
        if !context.text_edit_focused() && context.input(|input| input.key_pressed(egui::Key::F12))
        {
            if self.settings.session_logging == SessionLoggingLevel::Off {
                self.status_message = "Session logging off".to_owned();
            } else {
                self.mark_session_log();
            }
        }
        let native_clipboard_command = self.clipboard_shortcut_keys.poll();
        if !context.text_edit_focused()
            && let Some(command) = context
                .input(|input| clipboard_command_from_events(&input.events, input.modifiers))
                .or(native_clipboard_command)
        {
            self.run_clipboard_command(command);
        }
        let keyboard_focus_blocks_tool_shortcuts = context.text_edit_focused();
        context.input(|input| {
            let tool_shortcut_allowed =
                !keyboard_focus_blocks_tool_shortcuts && tool_shortcut_allowed(input.modifiers);
            if tool_shortcut_allowed && input.key_pressed(egui::Key::B) {
                self.tool = ToolKind::Brush;
            } else if tool_shortcut_allowed && input.key_pressed(egui::Key::E) {
                self.tool = ToolKind::Eraser;
            } else if tool_shortcut_allowed && input.key_pressed(egui::Key::L) {
                self.tool = ToolKind::LassoFill;
            } else if tool_shortcut_allowed && input.key_pressed(egui::Key::I) {
                self.tool = ToolKind::Eyedropper;
            } else if tool_shortcut_allowed && input.key_pressed(egui::Key::Q) {
                if self.tool == ToolKind::Selection {
                    self.set_selection_target_mode(selection_target_cycle(
                        self.selection_target_mode,
                    ));
                } else {
                    self.tool = ToolKind::Selection;
                }
            } else if tool_shortcut_allowed && input.key_pressed(egui::Key::S) {
                if self.tool == ToolKind::Selection
                    && self.selection_target_mode == SelectionTargetMode::Object
                {
                    self.rectangle_selection_mode =
                        rectangle_selection_mode_cycle(self.rectangle_selection_mode);
                } else {
                    self.tool = ToolKind::Selection;
                }
            } else if tool_shortcut_allowed && input.key_pressed(egui::Key::X) {
                self.tool = area_cycle_tool(self.tool);
            }
        });
        if context.input(|input| input.key_pressed(egui::Key::Escape)) {
            if self.selection_scale_gesture.take().is_some()
                || self.selection_rotate_gesture.take().is_some()
            {
                self.status_message = "Transform cancelled".to_owned();
            } else {
                self.clear_transient_tools();
                self.status_message = "Selection cleared".to_owned();
            }
        }
        if context.input(|input| input.key_pressed(egui::Key::Delete))
            && !self.selected_operation_ids.is_empty()
        {
            self.delete_selected();
        }
        let redo = context.input_mut(|input| {
            redo_shortcuts()
                .iter()
                .any(|shortcut| input.consume_shortcut(shortcut))
        });
        let undo = context.input_mut(|input| {
            input.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::CTRL,
                egui::Key::Z,
            ))
        });
        if undo {
            self.run_undo();
        }
        if redo {
            self.run_redo();
        }
    }

    fn run_clipboard_command(&mut self, command: ClipboardCommand) {
        match command {
            ClipboardCommand::Copy => self.copy_selected(),
            ClipboardCommand::Cut => self.cut_selected(),
            ClipboardCommand::Paste => self.paste_selection(false),
            ClipboardCommand::PasteInPlace => self.paste_selection(true),
        }
    }

    fn set_selection_target_mode(&mut self, mode: SelectionTargetMode) {
        if self.selection_target_mode == mode {
            return;
        }
        self.selection_target_mode = mode;
        match mode {
            SelectionTargetMode::Object => self.clear_area_selection_state(),
            SelectionTargetMode::Area => self.clear_object_selection_state(),
        }
        self.status_message = match mode {
            SelectionTargetMode::Object => "Object selection".to_owned(),
            SelectionTargetMode::Area => "Area selection".to_owned(),
        };
    }

    fn set_area_selection_shape(&mut self, shape: AreaSelectionShape) {
        if self.area_selection_shape == shape {
            return;
        }
        self.area_selection_shape = shape;
        self.clear_area_selection_state();
        self.status_message = match shape {
            AreaSelectionShape::Rectangle => "Area rectangle".to_owned(),
            AreaSelectionShape::Lasso => "Area lasso".to_owned(),
        };
    }

    fn handle_navigation(&mut self, ui: &egui::Ui, response: &egui::Response) {
        if response.hovered() {
            if self.tool == ToolKind::Selection
                && self.selection_target_mode == SelectionTargetMode::Object
                && let Some(position) = response.hover_pos()
            {
                let reorder_steps =
                    ui.input(|input| paint_order_wheel_steps(input.events.as_slice()));
                if !reorder_steps.is_empty() {
                    for step in reorder_steps {
                        self.reorder_selection(step);
                    }
                    return;
                }
                let wheel_steps = ui.input(|input| selection_wheel_steps(input.events.as_slice()));
                if !wheel_steps.is_empty() {
                    for step in wheel_steps {
                        self.cycle_selection_with_wheel(position, response.rect, step);
                    }
                    return;
                }
            }

            if let Some(position) = response.hover_pos() {
                if self.tool == ToolKind::Selection
                    && self.selection_target_mode == SelectionTargetMode::Object
                    && ui.input(|input| input.modifiers.alt)
                {
                    return;
                }
                let (
                    raw_wheel_events,
                    wheel_scrolls,
                    wheel_gesture_ended,
                    remainder_before,
                    remainder_after,
                ) = ui.input(|input| {
                    let diagnostics = wheel_zoom_input_diagnostics(input.events.as_slice());
                    let remainder_before = self.wheel_zoom_point_remainder;
                    let scrolls = wheel_zoom_scrolls(
                        input.events.as_slice(),
                        &mut self.wheel_zoom_point_remainder,
                        response.rect.height(),
                    );
                    (
                        diagnostics,
                        scrolls,
                        wheel_zoom_gesture_ended(input.events.as_slice()),
                        remainder_before,
                        self.wheel_zoom_point_remainder,
                    )
                });
                let latch_result = self.wheel_zoom_direction_latch.apply(
                    &wheel_scrolls,
                    wheel_gesture_ended,
                    Instant::now(),
                );
                let camera_before = self.camera.clone();
                if !latch_result.applied_scrolls.is_empty() {
                    for scroll in &latch_result.applied_scrolls {
                        let factor = (f64::from(*scroll) * f64::from(WHEEL_ZOOM_SCALE)).exp();
                        self.camera.zoom_at(
                            factor,
                            (position.x - response.rect.left()) as f64,
                            (position.y - response.rect.top()) as f64,
                            response.rect.width() as f64,
                            response.rect.height() as f64,
                        );
                    }
                }
                if !raw_wheel_events.is_empty() {
                    self.log_session_event_with(
                        "wheel_zoom_input",
                        json!({
                            "wheel_zoom": {
                                "raw_events": raw_wheel_events,
                                "produced_scrolls": wheel_scrolls,
                                "applied_scrolls": latch_result.applied_scrolls,
                                "suppressed_scroll_count": latch_result.suppressed_scroll_count,
                                "direction_latch_before": latch_result.direction_before,
                                "direction_latch_used": latch_result.direction_used,
                                "direction_latch_after": latch_result.direction_after,
                                "gesture_ended": wheel_gesture_ended,
                                "point_remainder_before": remainder_before,
                                "point_remainder_after": remainder_after,
                                "pointer_screen": {
                                    "x": position.x,
                                    "y": position.y,
                                },
                                "pointer_canvas": {
                                    "x": position.x - response.rect.left(),
                                    "y": position.y - response.rect.top(),
                                },
                                "camera_before": wheel_zoom_camera_diagnostics(&camera_before),
                                "camera_after": wheel_zoom_camera_diagnostics(&self.camera),
                            }
                        }),
                    );
                }
                if !latch_result.applied_scrolls.is_empty() {
                    self.mark_zoom_changed();
                    self.log_navigation_event("wheel_zoom");
                }
            }
        }

        if is_space_pan_active(ui) {
            let delta = ui.input(|input| input.pointer.delta());
            if delta.x.abs() > f32::EPSILON || delta.y.abs() > f32::EPSILON {
                self.camera.pan_content_by(delta.x as f64, delta.y as f64);
                self.hovered_operation_id = None;
                self.selection_hover_position = None;
                self.log_navigation_event("space_pan");
            }
            return;
        }

        if is_z_zoom_active(ui) {
            let delta = ui.input(|input| input.pointer.delta());
            if delta.y.abs() > f32::EPSILON
                && let Some(position) = response.hover_pos()
            {
                let factor = z_drag_zoom_factor(delta.y);
                self.camera.zoom_at(
                    factor,
                    (position.x - response.rect.left()) as f64,
                    (position.y - response.rect.top()) as f64,
                    response.rect.width() as f64,
                    response.rect.height() as f64,
                );
                self.mark_zoom_changed();
                self.log_navigation_event("z_drag_zoom");
            }
            return;
        }

        if ui.input(|input| input.pointer.button_down(PointerButton::Middle)) {
            let delta = ui.input(|input| input.pointer.delta());
            if delta.x.abs() > f32::EPSILON || delta.y.abs() > f32::EPSILON {
                self.camera.pan_content_by(delta.x as f64, delta.y as f64);
                self.hovered_operation_id = None;
                self.selection_hover_position = None;
                self.log_navigation_event("middle_pan");
            }
        }
    }

    fn handle_drawing(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        window: Option<NativeWindow>,
    ) {
        if is_space_pan_active(ui) || is_z_zoom_active(ui) {
            self.mouse_history.reset();
            self.alt_pointer_gesture = None;
            return;
        }

        let pointer = ui.input(|input| input.pointer.clone());
        let pixels_per_point = ui.ctx().pixels_per_point();
        let position = pointer.interact_pos();
        let primary_pressed = pointer.button_pressed(PointerButton::Primary);
        let primary_down = pointer.button_down(PointerButton::Primary);
        let primary_released = pointer.button_released(PointerButton::Primary);
        let events = ui.input(|input| input.events.clone());
        let (event_press_position, mut drag_positions, touch_event_history) =
            primary_pointer_positions(&events, primary_down);
        let modifiers = ui.input(|input| SelectionModifiers {
            shift: input.modifiers.shift,
            ctrl: input.modifiers.ctrl,
            alt: input.modifiers.alt,
        });
        let shift_down = modifiers.shift;

        if self.handle_alt_pointer_gesture(
            response,
            event_press_position,
            position,
            primary_pressed,
            primary_down,
            primary_released,
            modifiers,
        ) {
            self.mouse_history.reset();
            self.last_pointer_position = position;
            return;
        }

        if primary_pressed
            && modifiers.ctrl
            && response.hovered()
            && matches!(
                self.tool,
                ToolKind::Brush | ToolKind::Eraser | ToolKind::LassoFill
            )
        {
            self.brush_sizing_drag_active = true;
        }
        if self.brush_sizing_drag_active {
            self.mouse_history.reset();
            if primary_down {
                let delta = pointer.delta();
                if delta.y.abs() > f32::EPSILON {
                    self.adjust_brush_size(-delta.y * BRUSH_SIZE_DRAG_SCALE);
                }
                self.last_pointer_position = position;
                return;
            }
            if primary_released || !primary_down {
                self.brush_sizing_drag_active = false;
                self.last_pointer_position = position;
                return;
            }
        }

        if self.tool != ToolKind::Selection {
            self.hovered_operation_id = None;
            self.selection_hover_position = None;
        }
        if self.tool == ToolKind::Selection {
            self.mouse_history.reset();
            if !self.existing_object_tools_are_editable() {
                self.hovered_operation_id = None;
                self.selection_hover_position = None;
                if primary_pressed && response.hovered() {
                    self.status_message =
                        "Distant tile is read-only; move closer to edit objects".to_owned();
                }
                self.last_pointer_position = position;
                return;
            }
            self.handle_transient_tool(
                response,
                event_press_position,
                drag_positions,
                position,
                primary_pressed,
                primary_down,
                primary_released,
                modifiers,
            );
            self.last_pointer_position = position;
            return;
        }
        if self.tool == ToolKind::EraserLasso {
            if !self.existing_object_tools_are_editable() {
                self.mouse_history.reset();
                if primary_pressed && response.hovered() {
                    self.status_message =
                        "Distant tile is read-only; move closer to erase".to_owned();
                }
                self.last_pointer_position = position;
                return;
            }
            if !touch_event_history
                && primary_pressed
                && response.hovered()
                && let Some(position) = event_press_position.or(position)
            {
                self.mouse_history.begin(window, position, pixels_per_point);
            }
            if !touch_event_history
                && (primary_down || primary_released)
                && let Some(position) = position
                && let Some(history_positions) =
                    self.mouse_history
                        .positions_since(window, position, pixels_per_point)
                && history_positions.len() > drag_positions.len()
            {
                drag_positions = history_positions;
            }
            if touch_event_history && (primary_pressed || primary_down || primary_released) {
                self.mouse_history.record_touch_events(drag_positions.len());
            }
            self.handle_transient_tool(
                response,
                event_press_position,
                drag_positions,
                position,
                primary_pressed,
                primary_down,
                primary_released,
                modifiers,
            );
            if primary_released || !primary_down {
                self.mouse_history.reset();
            }
            self.last_pointer_position = position;
            return;
        }

        if primary_pressed && response.hovered() {
            self.eraser_target_ids.clear();
            let press_position = event_press_position.or(position);
            if self.tool == ToolKind::Eyedropper {
                self.pick_color_at(press_position, response.rect);
                return;
            }
            if self.tool == ToolKind::Eraser && !self.existing_object_tools_are_editable() {
                self.status_message = "Distant tile is read-only; move closer to erase".to_owned();
                return;
            }
            if !self.document.layer_is_editable(self.active_layer_id) {
                self.status_message = "Active layer is hidden or locked".to_owned();
                return;
            }
            if let Some(position) = press_position {
                let point = self.position_to_canvas(position, response.rect);
                let kind = match self.tool {
                    ToolKind::Brush => EditKind::Paint,
                    ToolKind::Eraser => EditKind::Erase,
                    ToolKind::LassoFill => EditKind::Fill,
                    ToolKind::Eyedropper | ToolKind::Selection | ToolKind::EraserLasso => return,
                };
                let color = if kind == EditKind::Erase {
                    BACKGROUND
                } else {
                    self.color
                };
                if kind == EditKind::Erase {
                    self.eraser_target_ids = self.visible_eraser_target_ids(response.rect);
                }
                let mut draft = EditOperation::draft(
                    kind,
                    self.camera.depth,
                    self.camera.zoom,
                    vec![point],
                    color,
                    self.brush_size,
                );
                draft.smooth_area = kind == EditKind::Fill;
                draft.layer_id = self.active_layer_id;
                self.draft = Some(draft);
                if !touch_event_history {
                    self.mouse_history.begin(window, position, pixels_per_point);
                }
                self.last_draft_save = Instant::now();
                self.begin_drawing_tile_pause();
            }
        }

        if primary_down
            && let Some(position) = position
            && drag_positions.last().is_none_or(|last| *last != position)
        {
            drag_positions.push(position);
        }
        if self.draft.is_some()
            && !touch_event_history
            && (primary_down || primary_released)
            && let Some(position) = position
            && let Some(history_positions) =
                self.mouse_history
                    .positions_since(window, position, pixels_per_point)
            && history_positions.len() > drag_positions.len()
        {
            drag_positions = history_positions;
        }
        if touch_event_history
            && (self.draft.is_some() || primary_pressed || primary_down || primary_released)
        {
            self.mouse_history.record_touch_events(drag_positions.len());
        }
        for drag_position in drag_positions {
            if response.rect.contains(drag_position) && self.draft.is_some() {
                self.extend_draft_to(drag_position, response.rect, shift_down);
            }
        }

        if self.draft.is_some() && self.last_draft_save.elapsed() >= DRAFT_SAVE_INTERVAL {
            if let Some(draft) = &self.draft
                && let Err(error) = self.document.save_draft(draft)
            {
                self.status_message = format!("Autosave failed: {error:#}");
            }
            self.last_draft_save = Instant::now();
        }

        if primary_released {
            if let Some(position) = position
                && response.rect.contains(position)
            {
                self.preserve_draft_endpoint(position, response.rect, shift_down);
            }
            self.finish_draft(response.rect);
        }
        self.last_pointer_position = position;
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_alt_pointer_gesture(
        &mut self,
        response: &egui::Response,
        event_press_position: Option<Pos2>,
        position: Option<Pos2>,
        primary_pressed: bool,
        primary_down: bool,
        primary_released: bool,
        modifiers: SelectionModifiers,
    ) -> bool {
        if primary_pressed
            && modifiers.alt
            && response.hovered()
            && let Some(start) = event_press_position.or(position)
        {
            self.alt_pointer_gesture = Some(AltPointerGesture {
                start,
                current: start,
                applied_cycle_steps: 0,
                cycle_selection: self.tool == ToolKind::Selection
                    && self.selection_target_mode == SelectionTargetMode::Object,
            });
            self.hovered_operation_id = None;
            self.selection_hover_position = None;
            return true;
        }

        let Some(mut gesture) = self.alt_pointer_gesture else {
            return false;
        };

        if let Some(position) = position {
            gesture.current = position;
        }

        if gesture.cycle_selection {
            let target_steps = alt_vertical_drag_cycle_steps(gesture.start, gesture.current);
            let pending_steps = target_steps - gesture.applied_cycle_steps;
            if pending_steps != 0 {
                let direction = pending_steps.signum();
                for _ in 0..pending_steps.unsigned_abs().min(64) {
                    self.cycle_selection_with_wheel(gesture.start, response.rect, direction);
                    gesture.applied_cycle_steps += direction;
                }
            }
        }

        if primary_released || !primary_down {
            if gesture.applied_cycle_steps == 0
                && gesture.start.distance(gesture.current) <= CLICK_SELECTION_DRAG_THRESHOLD_PX
            {
                self.pick_color_at(Some(gesture.start), response.rect);
            }
            self.alt_pointer_gesture = None;
        } else {
            self.alt_pointer_gesture = Some(gesture);
        }

        true
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_transient_tool(
        &mut self,
        response: &egui::Response,
        event_press_position: Option<Pos2>,
        mut drag_positions: Vec<Pos2>,
        position: Option<Pos2>,
        primary_pressed: bool,
        primary_down: bool,
        primary_released: bool,
        modifiers: SelectionModifiers,
    ) {
        if primary_down
            && let Some(position) = position
            && drag_positions.last().is_none_or(|last| *last != position)
        {
            drag_positions.push(position);
        }

        match self.tool {
            ToolKind::Selection => {
                if self.selection_target_mode == SelectionTargetMode::Area {
                    self.handle_area_selection_tool(
                        response,
                        event_press_position,
                        drag_positions,
                        position,
                        primary_pressed,
                        primary_released,
                    );
                    return;
                }
                if !primary_down
                    && !self.selection_drag_active
                    && !self.selection_move_active
                    && self.selection_scale_gesture.is_none()
                    && self.selection_rotate_gesture.is_none()
                    && !response.ctx.input(|input| {
                        input.pointer.any_down() || raw_wheel_input_present(input.events.as_slice())
                    })
                    && !self.zoom_tile_requests_deferred()
                    && position != self.selection_hover_position
                {
                    self.selection_hover_position = position;
                    self.hovered_operation_id = position
                        .filter(|position| response.rect.contains(*position))
                        .and_then(|position| {
                            self.selection_candidates_at(position, response.rect)
                                .first()
                                .map(|candidate| candidate.id)
                        });
                }
                if primary_pressed
                    && response.hovered()
                    && let Some(position) = event_press_position.or(position)
                {
                    self.hovered_operation_id = None;
                    self.selection_hover_position = None;
                    let transform_bounds = (!modifiers.ctrl
                        && !modifiers.alt
                        && !self.selected_operation_ids.is_empty())
                    .then(|| self.selection_screen_bounds(response.rect))
                    .flatten();
                    let rotate_bounds = transform_bounds
                        .filter(|bounds| selection_rotate_handle_at(*bounds, position));
                    let scale_handle = (!modifiers.shift)
                        .then_some(transform_bounds)
                        .flatten()
                        .and_then(|bounds| {
                            selection_scale_handle_at(bounds, position)
                                .map(|handle| (handle, bounds))
                        });
                    if let Some(bounds) = rotate_bounds {
                        self.selection_rotate_gesture = Some(SelectionRotateGesture {
                            bounds,
                            pointer_start: position,
                            current: position,
                            snap: modifiers.shift,
                        });
                        self.selection_move_active = false;
                        self.selection_drag_active = false;
                    } else if let Some((handle, bounds)) = scale_handle {
                        self.selection_scale_gesture = Some(SelectionScaleGesture {
                            handle,
                            bounds,
                            pointer_start: position,
                            current: position,
                        });
                        self.selection_move_active = false;
                        self.selection_drag_active = false;
                    } else if !modifiers.shift
                        && !modifiers.ctrl
                        && !modifiers.alt
                        && self.position_hits_selected(position, response.rect)
                    {
                        self.selection_move_start = Some(position);
                        self.selection_move_current = Some(position);
                        self.selection_move_active = true;
                        self.selection_drag_active = false;
                    } else {
                        self.selection_drag_start = Some(position);
                        self.selection_drag_current = Some(position);
                        self.selection_drag_active = true;
                        self.selection_move_active = false;
                    }
                }
                if self.selection_rotate_gesture.is_some() {
                    for position in drag_positions {
                        if let Some(gesture) = self.selection_rotate_gesture.as_mut() {
                            gesture.current = position;
                            gesture.snap = modifiers.shift;
                        }
                    }
                    if primary_released {
                        if let (Some(position), Some(gesture)) =
                            (position, self.selection_rotate_gesture.as_mut())
                        {
                            gesture.current = position;
                            gesture.snap = modifiers.shift;
                        }
                        self.finish_selection_rotate(response.rect);
                    }
                    return;
                }
                if self.selection_scale_gesture.is_some() {
                    for position in drag_positions {
                        if let Some(gesture) = self.selection_scale_gesture.as_mut() {
                            gesture.current = position;
                        }
                    }
                    if primary_released {
                        if let (Some(position), Some(gesture)) =
                            (position, self.selection_scale_gesture.as_mut())
                        {
                            gesture.current = position;
                        }
                        self.finish_selection_scale(response.rect);
                    }
                    return;
                }
                if self.selection_move_active {
                    for position in drag_positions {
                        self.selection_move_current = Some(position);
                    }
                    if primary_released {
                        if let Some(position) = position {
                            self.selection_move_current = Some(position);
                        }
                        self.finish_selection_move();
                    }
                    return;
                }
                if self.selection_drag_active {
                    for position in drag_positions {
                        if response.rect.contains(position) {
                            self.selection_drag_current = Some(position);
                        }
                    }
                }
                if primary_released && self.selection_drag_active {
                    if let Some(position) = position
                        && response.rect.contains(position)
                    {
                        self.selection_drag_current = Some(position);
                    }
                    self.selection_drag_active = false;
                    let drag_distance =
                        match (self.selection_drag_start, self.selection_drag_current) {
                            (Some(start), Some(end)) => start.distance(end),
                            _ => 0.0,
                        };
                    if drag_distance <= CLICK_SELECTION_DRAG_THRESHOLD_PX {
                        if let Some(position) = self.selection_drag_current {
                            self.complete_click_selection(position, response.rect, modifiers);
                        }
                        self.selection_drag_start = None;
                        self.selection_drag_current = None;
                    } else {
                        self.complete_rectangle_selection(response.rect, modifiers);
                    }
                }
            }
            ToolKind::EraserLasso => {
                self.hovered_operation_id = None;
                if primary_pressed
                    && response.hovered()
                    && let Some(position) = event_press_position.or(position)
                {
                    if !self.document.layer_is_editable(self.active_layer_id) {
                        self.eraser_target_ids.clear();
                        self.status_message = "Active layer is hidden or locked".to_owned();
                        return;
                    }
                    self.eraser_target_ids = self.visible_eraser_target_ids(response.rect);
                    self.eraser_lasso_points.clear();
                    self.eraser_lasso_points.push(position);
                    self.eraser_lasso_active = true;
                }
                if self.eraser_lasso_active {
                    for position in drag_positions {
                        if response.rect.contains(position) {
                            append_preview_point(
                                &mut self.eraser_lasso_points,
                                position,
                                MIN_LASSO_PREVIEW_SPACING_PX,
                            );
                        }
                    }
                }
                if primary_released && self.eraser_lasso_active {
                    if let Some(position) = position
                        && response.rect.contains(position)
                    {
                        append_preview_point(
                            &mut self.eraser_lasso_points,
                            position,
                            MIN_LASSO_PREVIEW_SPACING_PX,
                        );
                    }
                    self.eraser_lasso_active = false;
                    self.commit_eraser_lasso(response.rect);
                    self.eraser_lasso_points.clear();
                }
            }
            ToolKind::Brush | ToolKind::Eraser | ToolKind::LassoFill | ToolKind::Eyedropper => {}
        }
    }

    fn handle_area_selection_tool(
        &mut self,
        response: &egui::Response,
        event_press_position: Option<Pos2>,
        drag_positions: Vec<Pos2>,
        position: Option<Pos2>,
        primary_pressed: bool,
        primary_released: bool,
    ) {
        self.hovered_operation_id = None;
        self.selection_hover_position = None;
        if primary_pressed
            && response.hovered()
            && let Some(position) = event_press_position.or(position)
        {
            self.clear_object_selection_state();
            self.area_selection_drag_points.clear();
            self.area_selection_drag_points.push(position);
            self.area_selection_drag_active = true;
        }
        if self.area_selection_drag_active {
            for position in drag_positions {
                if response.rect.contains(position) {
                    match self.area_selection_shape {
                        AreaSelectionShape::Rectangle => {
                            if self.area_selection_drag_points.len() == 1 {
                                self.area_selection_drag_points.push(position);
                            } else if let Some(current) = self.area_selection_drag_points.get_mut(1)
                            {
                                *current = position;
                            }
                        }
                        AreaSelectionShape::Lasso => append_preview_point(
                            &mut self.area_selection_drag_points,
                            position,
                            MIN_LASSO_PREVIEW_SPACING_PX,
                        ),
                    }
                }
            }
        }
        if primary_released && self.area_selection_drag_active {
            if let Some(position) = position
                && response.rect.contains(position)
            {
                match self.area_selection_shape {
                    AreaSelectionShape::Rectangle => {
                        if self.area_selection_drag_points.len() == 1 {
                            self.area_selection_drag_points.push(position);
                        } else if let Some(current) = self.area_selection_drag_points.get_mut(1) {
                            *current = position;
                        }
                    }
                    AreaSelectionShape::Lasso => append_preview_point(
                        &mut self.area_selection_drag_points,
                        position,
                        MIN_LASSO_PREVIEW_SPACING_PX,
                    ),
                }
            }
            self.area_selection_drag_active = false;
            self.complete_area_selection(response.rect);
            self.area_selection_drag_points.clear();
        }
    }

    fn complete_area_selection(&mut self, canvas_rect: Rect) {
        let points = match self.area_selection_shape {
            AreaSelectionShape::Rectangle => {
                let [start, end] = match self.area_selection_drag_points.as_slice() {
                    [start, end, ..] => [*start, *end],
                    _ => {
                        self.area_selection = None;
                        self.status_message = "Area selection cleared".to_owned();
                        return;
                    }
                };
                if start.distance(end) <= CLICK_SELECTION_DRAG_THRESHOLD_PX {
                    self.area_selection = None;
                    self.status_message = "Area selection cleared".to_owned();
                    return;
                }
                area_rectangle_points(start, end, &self.camera, canvas_rect)
            }
            AreaSelectionShape::Lasso => {
                let Some(points) =
                    area_lasso_points(&self.area_selection_drag_points, &self.camera, canvas_rect)
                else {
                    self.area_selection = None;
                    self.status_message = "Area selection cleared".to_owned();
                    return;
                };
                points
            }
        };
        self.area_selection = Some(AreaSelection {
            shape: self.area_selection_shape,
            points,
        });
        self.status_message = "Area selected".to_owned();
    }

    fn complete_rectangle_selection(&mut self, canvas_rect: Rect, modifiers: SelectionModifiers) {
        let (Some(start), Some(end)) = (self.selection_drag_start, self.selection_drag_current)
        else {
            return;
        };
        let selection_rect =
            SelectionRect::from_points(selection_point(start), selection_point(end));
        let selection = SelectionShape::Rectangle(selection_rect);
        let candidates = self.visible_editable_operation_ids(canvas_rect);
        let matches: HashSet<_> = self
            .document
            .operations()
            .iter()
            .filter(|operation| candidates.contains(&operation.id))
            .filter_map(|operation| {
                let points: Vec<_> = operation
                    .points
                    .iter()
                    .filter_map(|point| self.point_to_position(point, canvas_rect))
                    .map(selection_point)
                    .collect();
                let width = operation_width(operation, self.camera.depth, self.camera.zoom) as f64;
                let selected = match self.rectangle_selection_mode {
                    RectangleSelectionMode::Inside => operation_contained_by_rectangle(
                        operation.kind,
                        &points,
                        width,
                        selection_rect,
                    ),
                    RectangleSelectionMode::Crossing => {
                        operation_intersects_selection(operation.kind, &points, width, &selection)
                    }
                };
                selected.then_some(operation.id)
            })
            .collect();
        let matches = if self.rectangle_selection_mode == RectangleSelectionMode::Inside {
            fully_contained_object_operation_ids(
                self.document.operations(),
                &matches,
                self.active_layer_id,
            )
        } else {
            matches
        };
        self.apply_selection_ids(matches, modifiers);
        self.reset_selection_cycle();
        self.status_message = format!("Selected {} objects", self.selected_object_count());
        self.selection_drag_start = None;
        self.selection_drag_current = None;
    }

    fn complete_click_selection(
        &mut self,
        position: Pos2,
        canvas_rect: Rect,
        modifiers: SelectionModifiers,
    ) {
        let candidates = self.selection_candidates_at(position, canvas_rect);
        let candidate_ids: Vec<_> = candidates.iter().map(|candidate| candidate.id).collect();
        let selected = if modifiers.alt && !candidate_ids.is_empty() {
            self.cycle_selection_candidate(position, candidate_ids, 1)
        } else {
            self.reset_selection_cycle();
            candidates.first().map(|candidate| candidate.id)
        };

        if let Some(selected) = selected {
            self.apply_selection_ids(HashSet::from([selected]), modifiers);
        } else if !modifiers.shift && !modifiers.ctrl {
            self.selected_operation_ids.clear();
        }
        self.status_message = format!("Selected {} objects", self.selected_object_count());
    }

    fn cycle_selection_with_wheel(&mut self, position: Pos2, canvas_rect: Rect, step: isize) {
        let candidate_ids = self
            .selection_candidates_at(position, canvas_rect)
            .into_iter()
            .map(|candidate| candidate.id)
            .collect();
        let Some(selected) = self.cycle_selection_candidate(position, candidate_ids, step) else {
            self.status_message = "No object under cursor".to_owned();
            return;
        };
        self.selected_operation_ids = self
            .document
            .expanded_object_group_ids(&HashSet::from([selected]), self.active_layer_id);
        self.hovered_operation_id = Some(selected);
        self.selection_hover_position = Some(position);
        self.selection_drag_start = None;
        self.selection_drag_current = None;
        self.status_message = "Cycled selection".to_owned();
    }

    fn reorder_selection(&mut self, direction: isize) {
        if self.selected_operation_ids.is_empty() {
            self.status_message = "Nothing selected".to_owned();
            return;
        }
        match self.document.reorder_operations(
            &self.selected_operation_ids,
            self.active_layer_id,
            direction,
        ) {
            Ok(true) => {
                self.begin_new_document_revision_for_layer(self.active_layer_id);
                self.status_message = if direction > 0 {
                    "Selection moved forward".to_owned()
                } else {
                    "Selection moved backward".to_owned()
                };
                self.log_session_event_with(
                    "selection_transform",
                    json!({ "context": { "action": "reorder" } }),
                );
            }
            Ok(false) => self.status_message = "Selection is already at the edge".to_owned(),
            Err(error) => self.status_message = format!("Reorder failed: {error:#}"),
        }
    }

    fn cycle_selection_candidate(
        &mut self,
        position: Pos2,
        candidate_ids: Vec<Uuid>,
        step: isize,
    ) -> Option<Uuid> {
        if candidate_ids.is_empty() {
            self.reset_selection_cycle();
            return None;
        }
        let continues_cycle = self.selection_cycle_position.is_some_and(|previous| {
            previous.distance(position) <= SELECTION_CYCLE_POSITION_TOLERANCE_PX
        }) && self.selection_cycle_candidates == candidate_ids;
        self.selection_cycle_index = if continues_cycle {
            wrapped_cycle_index(self.selection_cycle_index, candidate_ids.len(), step)
        } else {
            let current = candidate_ids
                .iter()
                .position(|id| self.selected_operation_ids.contains(id));
            current.map_or_else(
                || {
                    if step < 0 { candidate_ids.len() - 1 } else { 0 }
                },
                |current| wrapped_cycle_index(current, candidate_ids.len(), step),
            )
        };
        self.selection_cycle_position = Some(position);
        self.selection_cycle_candidates = candidate_ids;
        self.selection_cycle_candidates
            .get(self.selection_cycle_index)
            .copied()
    }

    fn visible_editable_operation_ids(&self, canvas_rect: Rect) -> HashSet<Uuid> {
        if !self.existing_object_tools_are_editable() {
            return HashSet::new();
        }
        let lod = tile_lod_for_resolution(self.settings.tile_resolution_px);
        let visible_tiles = self.visible_tiles(canvas_rect, lod);
        self.document
            .editable_operation_ids_for_tiles(
                &visible_tiles,
                self.active_layer_id,
                self.camera.depth,
                self.settings.depth_capture_radius,
            )
            .into_iter()
            .collect()
    }

    fn visible_eraser_target_ids(&self, canvas_rect: Rect) -> HashSet<Uuid> {
        let editable_ids = self.visible_editable_operation_ids(canvas_rect);
        let target_ids = self
            .document
            .operations()
            .iter()
            .filter(|operation| {
                matches!(
                    operation.kind,
                    EditKind::Paint | EditKind::Fill | EditKind::CompactBlock
                ) && editable_ids.contains(&operation.id)
            })
            .map(|operation| operation.id)
            .collect();
        self.document
            .expanded_object_group_ids(&target_ids, self.active_layer_id)
    }

    fn existing_object_tools_are_editable(&self) -> bool {
        self.last_depth_render_mode != Some(DepthRenderMode::DistantTile)
    }

    fn selection_candidates_at(
        &self,
        position: Pos2,
        canvas_rect: Rect,
    ) -> Vec<SelectionCandidate> {
        let candidate_ids = self.visible_editable_operation_ids(canvas_rect);
        let click = selection_point(position);
        let mut seen_object_ids = HashSet::new();
        let mut candidates: Vec<_> = self
            .document
            .operations()
            .iter()
            .filter(|operation| candidate_ids.contains(&operation.id))
            .filter_map(|operation| {
                let points: Vec<_> = operation
                    .points
                    .iter()
                    .filter_map(|point| self.point_to_position(point, canvas_rect))
                    .map(selection_point)
                    .collect();
                let width = operation_width(operation, self.camera.depth, self.camera.zoom) as f64;
                let (effective_distance, bounds_area) =
                    operation_click_score(operation.kind, &points, width, click)?;
                if !seen_object_ids.insert(operation.object_group_id()) {
                    return None;
                }
                Some(SelectionCandidate {
                    id: operation.id,
                    effective_distance,
                    bounds_area,
                    layer_order: self.document.layer_sort_order(operation.layer_id),
                    paint_order: operation.effective_paint_order(),
                    is_eraser: operation.kind.is_erase(),
                })
            })
            .collect();
        candidates.sort_by(|left, right| {
            left.effective_distance
                .total_cmp(&right.effective_distance)
                .then_with(|| left.bounds_area.total_cmp(&right.bounds_area))
                .then_with(|| left.is_eraser.cmp(&right.is_eraser))
                .then_with(|| right.layer_order.cmp(&left.layer_order))
                .then_with(|| right.paint_order.cmp(&left.paint_order))
        });
        candidates
    }

    fn apply_selection_ids(&mut self, matches: HashSet<Uuid>, modifiers: SelectionModifiers) {
        let matches = self
            .document
            .expanded_object_group_ids(&matches, self.active_layer_id);
        if modifiers.ctrl {
            let remove = matches
                .iter()
                .all(|id| self.selected_operation_ids.contains(id));
            if remove {
                self.selected_operation_ids
                    .retain(|id| !matches.contains(id));
            } else {
                self.selected_operation_ids.extend(matches);
            }
        } else if modifiers.shift {
            self.selected_operation_ids.extend(matches);
        } else {
            self.selected_operation_ids = matches;
        }
    }

    fn selected_object_count(&self) -> usize {
        self.document
            .operations()
            .iter()
            .filter(|operation| self.selected_operation_ids.contains(&operation.id))
            .map(EditOperation::object_group_id)
            .collect::<HashSet<_>>()
            .len()
    }

    fn reset_selection_cycle(&mut self) {
        self.selection_cycle_position = None;
        self.selection_cycle_candidates.clear();
        self.selection_cycle_index = 0;
    }

    fn position_hits_selected(&self, position: Pos2, canvas_rect: Rect) -> bool {
        if self.selected_operation_ids.is_empty() {
            return false;
        }
        let selection = SelectionShape::Rectangle(SelectionRect::from_points(
            SelectionPoint::new((position.x - 3.0) as f64, (position.y - 3.0) as f64),
            SelectionPoint::new((position.x + 3.0) as f64, (position.y + 3.0) as f64),
        ));
        self.document
            .operations()
            .iter()
            .filter(|operation| self.selected_operation_ids.contains(&operation.id))
            .any(|operation| {
                let points: Vec<_> = operation
                    .points
                    .iter()
                    .filter_map(|point| self.point_to_position(point, canvas_rect))
                    .map(selection_point)
                    .collect();
                let width = operation_width(operation, self.camera.depth, self.camera.zoom) as f64;
                operation_intersects_selection(operation.kind, &points, width, &selection)
            })
    }

    fn selection_screen_bounds(&self, canvas_rect: Rect) -> Option<Rect> {
        let mut min = Pos2::new(f32::INFINITY, f32::INFINITY);
        let mut max = Pos2::new(f32::NEG_INFINITY, f32::NEG_INFINITY);
        let mut found = false;
        for operation in self
            .document
            .operations()
            .iter()
            .filter(|operation| self.selected_operation_ids.contains(&operation.id))
        {
            let radius = if operation.kind.is_area() {
                0.0
            } else {
                operation_width(operation, self.camera.depth, self.camera.zoom) * 0.5
            };
            let projected_points = operation
                .points
                .iter()
                .filter_map(|point| self.point_to_position(point, canvas_rect))
                .collect::<Vec<_>>();
            for position in selection_highlight_render_points(
                operation,
                projected_points,
                usize::from(self.settings.smoothing.passes()),
                self.settings.fill_fallback_max_points,
            ) {
                min.x = min.x.min(position.x - radius);
                min.y = min.y.min(position.y - radius);
                max.x = max.x.max(position.x + radius);
                max.y = max.y.max(position.y + radius);
                found = true;
            }
        }
        found.then(|| Rect::from_min_max(min, max))
    }

    fn fast_selection_overlay_bounds(
        &mut self,
        canvas_rect: Rect,
        move_delta: egui::Vec2,
    ) -> Option<Rect> {
        let (depth_bounds, width_hints) = self.selection_overlay_bounds_cache.bounds_for(
            self.document.revision(),
            self.document.operations(),
            &self.selected_operation_ids,
        );
        let mut projected = None::<Rect>;
        for bounds in depth_bounds {
            let bounds = bounds.project(&self.camera, canvas_rect)?;
            projected = Some(projected.map_or(bounds, |current| current.union(bounds)));
        }
        let expand =
            selection_overlay_width_expansion(width_hints, self.camera.depth, self.camera.zoom);
        projected.map(|bounds| {
            Rect::from_min_max(bounds.min + move_delta, bounds.max + move_delta).expand(expand)
        })
    }

    fn finish_selection_scale(&mut self, canvas_rect: Rect) {
        let Some(gesture) = self.selection_scale_gesture.take() else {
            return;
        };
        let scale = gesture.scale();
        if (scale - 1.0).abs() < 0.001 {
            self.status_message = "Scale unchanged".to_owned();
            return;
        }
        if !self.document.layer_is_editable(self.active_layer_id) {
            self.status_message = "Active layer is hidden or locked".to_owned();
            return;
        }
        let pivot = gesture.handle.pivot(gesture.bounds) - canvas_rect.min;
        let Some(transform) =
            ScreenAffine::uniform_scale(pivot.x as f64, pivot.y as f64, scale as f64)
        else {
            self.status_message = "Scale is not representable".to_owned();
            return;
        };
        self.commit_selection_transform(
            canvas_rect,
            transform,
            scale,
            "Nothing scaled",
            "Scale",
            |count| format!("Scaled {count} objects to {:.1}%", scale * 100.0),
        );
    }

    fn finish_selection_rotate(&mut self, canvas_rect: Rect) {
        let Some(gesture) = self.selection_rotate_gesture.take() else {
            return;
        };
        let angle = gesture.angle();
        if angle.abs() < 0.001 {
            self.status_message = "Rotation unchanged".to_owned();
            return;
        }
        if !self.document.layer_is_editable(self.active_layer_id) {
            self.status_message = "Active layer is hidden or locked".to_owned();
            return;
        }
        let pivot = gesture.bounds.center() - canvas_rect.min;
        let Some(transform) = ScreenAffine::rotation(pivot.x as f64, pivot.y as f64, angle as f64)
        else {
            self.status_message = "Rotation is not representable".to_owned();
            return;
        };
        let degrees = angle.to_degrees();
        self.commit_selection_transform(
            canvas_rect,
            transform,
            1.0,
            "Nothing rotated",
            "Rotate",
            |count| format!("Rotated {count} objects by {degrees:.1} deg"),
        );
    }

    fn flip_selected(&mut self, axis: SelectionFlipAxis) {
        let Some(canvas_rect) = self.last_canvas_rect else {
            self.status_message = "Canvas is not ready".to_owned();
            return;
        };
        if !self.document.layer_is_editable(self.active_layer_id) {
            self.status_message = "Active layer is hidden or locked".to_owned();
            return;
        }
        let Some(bounds) = self.selection_screen_bounds(canvas_rect) else {
            self.status_message = "Nothing flipped".to_owned();
            return;
        };
        let pivot = bounds.center() - canvas_rect.min;
        let transform = match axis {
            SelectionFlipAxis::Horizontal => {
                ScreenAffine::flip_horizontal(pivot.x as f64, pivot.y as f64)
            }
            SelectionFlipAxis::Vertical => {
                ScreenAffine::flip_vertical(pivot.x as f64, pivot.y as f64)
            }
        };
        let Some(transform) = transform else {
            self.status_message = "Flip is not representable".to_owned();
            return;
        };
        let label = match axis {
            SelectionFlipAxis::Horizontal => "horizontally",
            SelectionFlipAxis::Vertical => "vertically",
        };
        self.commit_selection_transform(
            canvas_rect,
            transform,
            1.0,
            "Nothing flipped",
            "Flip",
            |count| format!("Flipped {count} objects {label}"),
        );
    }

    fn selected_operations_in_document_order(
        &self,
        selected: &HashSet<Uuid>,
    ) -> Vec<EditOperation> {
        let selected = self
            .document
            .expanded_object_group_ids(selected, self.active_layer_id);
        self.document
            .operations()
            .iter()
            .filter(|operation| selected.contains(&operation.id))
            .cloned()
            .collect()
    }

    fn selected_operations_in_paint_order(
        &self,
        selected: &HashSet<Uuid>,
        layer_id: Uuid,
    ) -> Vec<EditOperation> {
        let selected = self.document.expanded_object_group_ids(selected, layer_id);
        let mut operations = self
            .document
            .operations()
            .iter()
            .filter(|operation| selected.contains(&operation.id) && operation.layer_id == layer_id)
            .cloned()
            .collect::<Vec<_>>();
        operations.sort_by_key(|operation| operation.effective_paint_order());
        operations
    }

    fn warm_replacement_fallback_cache(
        &mut self,
        source_operations: &[EditOperation],
        replacement_ids: &[Uuid],
        transform: impl Fn(Pos2) -> Pos2 + Copy,
    ) -> usize {
        if source_operations.is_empty() || source_operations.len() != replacement_ids.len() {
            return 0;
        }
        let replacement_id_set = replacement_ids.iter().copied().collect::<HashSet<_>>();
        let replacement_operations = self
            .document
            .operations()
            .iter()
            .filter(|operation| replacement_id_set.contains(&operation.id))
            .map(|operation| (operation.id, operation.clone()))
            .collect::<HashMap<_, _>>();
        let mut warmed = 0usize;
        for (source, replacement_id) in source_operations.iter().zip(replacement_ids) {
            let Some(replacement) = replacement_operations.get(replacement_id) else {
                continue;
            };
            let source_key = saved_fallback_operation_key(
                source.id,
                source,
                self.settings.stroke_fallback_joins,
                self.last_auto_fast_stroke_fallback,
                self.settings.smoothing,
            );
            let replacement_key = saved_fallback_operation_key(
                replacement.id,
                replacement,
                self.settings.stroke_fallback_joins,
                self.last_auto_fast_stroke_fallback,
                self.settings.smoothing,
            );
            if self
                .fallback_renderer
                .saved_render_cache
                .insert_transformed_from(&source_key, replacement_key, transform)
            {
                warmed = warmed.saturating_add(1);
            }
        }
        warmed
    }

    fn recolor_selected(&mut self) {
        let selected = self.selected_operation_ids.clone();
        let mut source_operations =
            self.selected_operations_in_paint_order(&selected, self.active_layer_id);
        source_operations
            .retain(|operation| matches!(operation.kind, EditKind::Paint | EditKind::Fill));
        match self
            .document
            .recolor_operations(&selected, self.active_layer_id, self.color)
        {
            Ok(replacements) if !replacements.is_empty() => {
                let replacement_ids = replacements
                    .iter()
                    .map(|(_, replacement_id)| *replacement_id)
                    .collect::<Vec<_>>();
                self.warm_replacement_fallback_cache(
                    &source_operations,
                    &replacement_ids,
                    |point| point,
                );
                let replacement_map = replacements.into_iter().collect::<HashMap<_, _>>();
                self.selected_operation_ids = selected
                    .into_iter()
                    .map(|id| replacement_map.get(&id).copied().unwrap_or(id))
                    .collect();
                self.begin_new_document_revision_for_layer(self.active_layer_id);
                self.status_message = format!("Recolored {} objects", self.selected_object_count());
                self.log_session_event_with(
                    "selection_transform",
                    json!({ "context": { "action": "recolor", "count": replacement_map.len() } }),
                );
            }
            Ok(_) => self.status_message = "Nothing recolored".to_owned(),
            Err(error) => self.status_message = format!("Recolor failed: {error:#}"),
        }
    }

    fn freeze_compact_selected(&mut self) {
        let selected = self.selected_operation_ids.clone();
        match self
            .document
            .compact_operations(&selected, self.active_layer_id)
        {
            Ok(Some(block_id)) => {
                self.selected_operation_ids.clear();
                self.selected_operation_ids.insert(block_id);
                self.invalidate_tile_rendering();
                self.status_message = format!("Compacted {} objects", selected.len());
                self.log_session_event_with(
                    "compact",
                    json!({ "document": { "selected_count": selected.len() } }),
                );
            }
            Ok(None) => self.status_message = "Nothing compacted".to_owned(),
            Err(error) => self.status_message = format!("Compact failed: {error:#}"),
        }
    }

    fn commit_selection_transform<F>(
        &mut self,
        canvas_rect: Rect,
        transform: ScreenAffine,
        width_scale: f32,
        empty_message: &str,
        error_prefix: &str,
        success_message: F,
    ) where
        F: FnOnce(usize) -> String,
    {
        let selected = self.selected_operation_ids.clone();
        let source_operations =
            self.selected_operations_in_paint_order(&selected, self.active_layer_id);
        match self.document.transform_operations(
            &selected,
            &self.camera,
            canvas_rect.width() as f64,
            canvas_rect.height() as f64,
            transform,
            width_scale,
            self.active_layer_id,
        ) {
            Ok(replacement_ids) if !replacement_ids.is_empty() => {
                let commit_stats = self.document.last_operation_commit_stats();
                let warm_cache_start = Instant::now();
                let warmed_fallback_entries = self.warm_replacement_fallback_cache(
                    &source_operations,
                    &replacement_ids,
                    |point| {
                        let local_x = f64::from(point.x - canvas_rect.left());
                        let local_y = f64::from(point.y - canvas_rect.top());
                        transform
                            .transform_screen_point(local_x, local_y)
                            .map(|(x, y)| {
                                Pos2::new(
                                    canvas_rect.left() + x as f32,
                                    canvas_rect.top() + y as f32,
                                )
                            })
                            .unwrap_or(point)
                    },
                );
                let warm_fallback_cache_ms = warm_cache_start.elapsed().as_secs_f64() * 1_000.0;
                self.selected_operation_ids = replacement_ids.into_iter().collect();
                let count = self.selected_object_count();
                let revision_start = Instant::now();
                self.begin_new_document_revision_for_layer(self.active_layer_id);
                let revision_invalidation_ms = revision_start.elapsed().as_secs_f64() * 1_000.0;
                self.status_message = success_message(count);
                self.log_session_event_with(
                    "selection_transform",
                    json!({
                        "context": { "action": error_prefix, "count": count },
                        "selection_commit": {
                            "document": commit_stats,
                            "warm_fallback_cache_ms": warm_fallback_cache_ms,
                            "warmed_fallback_entries": warmed_fallback_entries,
                            "revision_invalidation_ms": revision_invalidation_ms,
                        },
                    }),
                );
            }
            Ok(_) => self.status_message = empty_message.to_owned(),
            Err(error) => self.status_message = format!("{error_prefix} failed: {error:#}"),
        }
    }

    fn finish_selection_move(&mut self) {
        let delta = self.selection_move_delta();
        self.selection_move_active = false;
        self.selection_move_start = None;
        self.selection_move_current = None;
        if delta.length() < 0.5 {
            return;
        }

        let selected = self.selected_operation_ids.clone();
        let source_operations = self.selected_operations_in_document_order(&selected);
        match self.document.move_operations(
            &selected,
            self.camera.depth,
            self.camera.zoom,
            delta.x as f64,
            delta.y as f64,
            self.active_layer_id,
        ) {
            Ok(replacement_ids) if !replacement_ids.is_empty() => {
                let commit_stats = self.document.last_operation_commit_stats();
                let warm_cache_start = Instant::now();
                let warmed_fallback_entries = self.warm_replacement_fallback_cache(
                    &source_operations,
                    &replacement_ids,
                    |point| point + delta,
                );
                let warm_fallback_cache_ms = warm_cache_start.elapsed().as_secs_f64() * 1_000.0;
                self.selected_operation_ids = replacement_ids.into_iter().collect();
                if let Some(start) = self.selection_drag_start.as_mut() {
                    *start += delta;
                }
                if let Some(current) = self.selection_drag_current.as_mut() {
                    *current += delta;
                }
                let revision_start = Instant::now();
                self.begin_new_document_revision_for_layer(self.active_layer_id);
                let revision_invalidation_ms = revision_start.elapsed().as_secs_f64() * 1_000.0;
                self.status_message = format!("Moved {} objects", self.selected_object_count());
                self.log_session_event_with(
                    "selection_transform",
                    json!({
                        "context": { "action": "move", "count": self.selected_object_count() },
                        "selection_commit": {
                            "document": commit_stats,
                            "warm_fallback_cache_ms": warm_fallback_cache_ms,
                            "warmed_fallback_entries": warmed_fallback_entries,
                            "revision_invalidation_ms": revision_invalidation_ms,
                        },
                    }),
                );
            }
            Ok(_) => self.status_message = "Nothing moved".to_owned(),
            Err(error) => self.status_message = format!("Move failed: {error:#}"),
        }
    }

    fn commit_eraser_lasso(&mut self, canvas_rect: Rect) {
        if !self.document.layer_is_editable(self.active_layer_id) {
            self.eraser_target_ids.clear();
            self.status_message = "Active layer is hidden or locked".to_owned();
            return;
        }
        let Some(operation) = eraser_lasso_operation(
            &self.eraser_lasso_points,
            &self.camera,
            canvas_rect,
            self.active_layer_id,
        ) else {
            self.eraser_target_ids.clear();
            self.status_message = "Eraser Lasso needs a non-degenerate area".to_owned();
            return;
        };
        let layer_id = operation.layer_id;
        let masks = self.clip_draft_to_area_selection(operation, canvas_rect);
        if masks.is_empty() {
            self.eraser_target_ids.clear();
            self.status_message = "Clipped outside area".to_owned();
            return;
        }
        match self.commit_vector_eraser_lasso(masks, canvas_rect) {
            Ok(true) => {
                self.clear_object_selection_state();
                self.begin_new_document_revision_for_layer(layer_id);
                self.status_message = "Area erased".to_owned();
                self.log_session_event("save");
            }
            Ok(false) => self.status_message = "Nothing erased".to_owned(),
            Err(error) => self.status_message = format!("Eraser Lasso failed: {error:#}"),
        }
    }

    fn commit_vector_eraser_lasso(
        &mut self,
        masks: Vec<EditOperation>,
        rect: Rect,
    ) -> Result<bool> {
        let target_ids = std::mem::take(&mut self.eraser_target_ids);
        if target_ids.is_empty() {
            return Ok(false);
        }
        let polygons = masks
            .iter()
            .map(|mask| {
                let points = mask
                    .points
                    .iter()
                    .map(|point| canvas_point_to_position(&self.camera, point, rect))
                    .collect::<Option<Vec<_>>>()
                    .context("eraser lasso is not representable at the current camera")?;
                let polygon = normalized_pos_polygon(&points);
                (polygon.len() >= 3).then_some(polygon).context(
                    "eraser lasso produced a degenerate polygon after Area selection clipping",
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let fill_masks = masks
            .iter()
            .map(|mask| mask.points.clone())
            .collect::<Vec<_>>();
        let sources = self.snapshotted_eraser_sources(&target_ids)?;
        let changes = subtract_eraser_sources(&sources, &fill_masks, &self.camera, |source| {
            let mut runs = vec![source.points.clone()];
            for polygon in &polygons {
                let mut next = Vec::new();
                for points in runs {
                    let mut fragment = source.clone();
                    fragment.points = points;
                    next.extend(
                        subtract_paint_stroke_by_polygon(&fragment, &self.camera, rect, polygon)
                            .context(
                                "paint subtraction is not representable at the current camera",
                            )?,
                    );
                }
                runs = next;
            }
            Ok(runs)
        })?;
        Ok(self
            .document
            .commit_eraser_subtraction(changes, self.active_layer_id)?
            .is_some())
    }

    fn selection_move_delta(&self) -> egui::Vec2 {
        match (self.selection_move_start, self.selection_move_current) {
            (Some(start), Some(current)) => current - start,
            _ => egui::Vec2::ZERO,
        }
    }

    fn extend_draft_to(&mut self, position: Pos2, rect: Rect, shift_down: bool) {
        let Some(draft) = self.draft.as_ref() else {
            return;
        };
        let point = screen_to_canvas_for_camera(&self.camera, position, rect);
        let append_decision = draft
            .points
            .last()
            .map_or(PointAppendDecision::Append, |last| {
                self.camera
                    .canvas_to_screen(last, rect.width() as f64, rect.height() as f64)
                    .map_or(PointAppendDecision::Append, |(x, y)| {
                        let previous = Pos2::new(rect.left() + x as f32, rect.top() + y as f32);
                        point_append_decision(
                            draft.kind,
                            previous.distance(position),
                            &self.settings,
                        )
                    })
            });
        if straight_line_requested(draft.kind, shift_down) {
            if should_update_straight_endpoint(draft, &self.camera, position, rect, &self.settings)
                && let Some(draft) = self.draft.as_mut()
            {
                set_straight_draft_endpoint(draft, point);
            }
            return;
        }

        match append_decision {
            PointAppendDecision::Append => {
                if let Some(draft) = self.draft.as_mut() {
                    append_draft_point(draft, point, &self.settings);
                }
            }
            PointAppendDecision::Skip => {}
            PointAppendDecision::Interpolate => {
                if let Some(draft) = self.draft.as_mut() {
                    append_interpolated_points(draft, &self.camera, position, rect, &self.settings);
                }
            }
        }
    }

    fn preserve_draft_endpoint(&mut self, position: Pos2, rect: Rect, shift_down: bool) {
        let Some(draft) = self.draft.as_ref() else {
            return;
        };
        let endpoint = screen_to_canvas_for_camera(&self.camera, position, rect);
        if straight_line_requested(draft.kind, shift_down) {
            if let Some(draft) = self.draft.as_mut() {
                set_straight_draft_endpoint(draft, endpoint);
            }
            return;
        }
        let endpoint_is_new = draft
            .points
            .last()
            .and_then(|last| {
                self.camera
                    .canvas_to_screen(last, rect.width() as f64, rect.height() as f64)
            })
            .is_none_or(|(x, y)| {
                let last = Pos2::new(rect.left() + x as f32, rect.top() + y as f32);
                last.distance(position) > f32::EPSILON
            });
        if endpoint_is_new && let Some(draft) = self.draft.as_mut() {
            append_interpolated_points(draft, &self.camera, position, rect, &self.settings);
        }
    }

    fn finish_draft(&mut self, rect: Rect) {
        self.mouse_history.reset();
        let Some(mut draft) = self.draft.take() else {
            self.eraser_target_ids.clear();
            self.end_drawing_tile_pause();
            return;
        };
        prepare_draft_for_commit(&mut draft);
        if !draft_is_committable(&draft) {
            self.eraser_target_ids.clear();
            let _ = self.document.discard_draft(&draft);
            self.end_drawing_tile_pause();
            return;
        }
        if draft.kind == EditKind::Erase {
            let layer_id = draft.layer_id;
            let result = self.commit_vector_eraser(draft, rect);
            match result {
                Ok(true) => {
                    self.clear_object_selection_state();
                    self.begin_new_document_revision_for_layer(layer_id);
                    self.status_message = "Erased".to_owned();
                    self.log_session_event("save");
                }
                Ok(false) => self.status_message = "Nothing erased".to_owned(),
                Err(error) => self.status_message = format!("Erase failed: {error:#}"),
            }
            self.end_drawing_tile_pause();
            return;
        }
        self.eraser_target_ids.clear();
        let layer_id = draft.layer_id;
        let draft_for_discard = draft.clone();
        let operations = self.clip_draft_to_area_selection(draft, rect);
        if operations.is_empty() {
            let _ = self.document.discard_draft(&draft_for_discard);
            self.end_drawing_tile_pause();
            self.status_message = "Clipped outside area".to_owned();
            return;
        }
        let replaces_draft_id = operations.len() != 1 || operations[0].id != draft_for_discard.id;
        let commit_result = if operations.len() == 1 {
            self.document
                .commit(operations.into_iter().next().expect("one operation"))
        } else {
            self.document.commit_group(operations)
        };
        match commit_result {
            Ok(()) => {
                if replaces_draft_id {
                    let _ = self.document.discard_draft(&draft_for_discard);
                }
                self.begin_new_document_revision_for_layer(layer_id);
                self.status_message = "Saved".to_owned();
                self.log_session_event("save");
            }
            Err(error) => self.status_message = format!("Save failed: {error:#}"),
        }
        self.end_drawing_tile_pause();
    }

    fn commit_vector_eraser(&mut self, draft: EditOperation, rect: Rect) -> Result<bool> {
        let target_ids = std::mem::take(&mut self.eraser_target_ids);
        if target_ids.is_empty() {
            return Ok(false);
        }
        let layer_id = draft.layer_id;
        let masks = self.clip_draft_to_area_selection(draft, rect);
        if masks.is_empty() {
            return Ok(false);
        }
        let masks = masks
            .iter()
            .map(|mask| {
                let points = mask
                    .points
                    .iter()
                    .map(|point| canvas_point_to_position(&self.camera, point, rect))
                    .collect::<Option<Vec<_>>>()
                    .context("eraser mask is not representable at the current camera")?;
                let width = operation_width(mask, self.camera.depth, self.camera.zoom);
                Ok((points, width))
            })
            .collect::<Result<Vec<_>>>()?;
        let fill_masks = eraser_path_fill_masks(&masks, &self.camera, rect)?;
        let sources = self.snapshotted_eraser_sources(&target_ids)?;
        let changes = subtract_eraser_sources(&sources, &fill_masks, &self.camera, |source| {
            let mut runs = vec![source.points.clone()];
            for (points, width) in &masks {
                let mut next = Vec::new();
                for points_to_subtract in runs {
                    let mut fragment = source.clone();
                    fragment.points = points_to_subtract;
                    next.extend(
                        subtract_paint_stroke_by_path(
                            &fragment,
                            &self.camera,
                            rect,
                            points,
                            *width,
                        )
                        .context("paint subtraction is not representable at the current camera")?,
                    );
                }
                runs = next;
            }
            Ok(runs)
        })?;
        Ok(self
            .document
            .commit_eraser_subtraction(changes, layer_id)?
            .is_some())
    }

    fn snapshotted_eraser_sources(&self, target_ids: &HashSet<Uuid>) -> Result<Vec<EditOperation>> {
        let sources = self
            .document
            .operations()
            .iter()
            .filter(|operation| target_ids.contains(&operation.id))
            .cloned()
            .collect::<Vec<_>>();
        if sources.len() != target_ids.len() {
            anyhow::bail!("eraser target snapshot is stale");
        }
        Ok(sources)
    }

    fn clip_draft_to_area_selection(&self, draft: EditOperation, rect: Rect) -> Vec<EditOperation> {
        if !matches!(
            draft.kind,
            EditKind::Paint | EditKind::Erase | EditKind::Fill | EditKind::EraseArea
        ) {
            return vec![draft];
        }
        let Some(area_selection) = &self.area_selection else {
            return vec![draft];
        };
        let area_points: Vec<_> = area_selection
            .points
            .iter()
            .filter_map(|point| self.point_to_position(point, rect))
            .collect();
        if area_points.len() != area_selection.points.len() {
            return vec![draft];
        }
        let area_polygon = normalized_pos_polygon(&area_points);
        if area_polygon.len() < 3 {
            return vec![draft];
        }
        let draft_points: Vec<_> = draft
            .points
            .iter()
            .filter_map(|point| self.point_to_position(point, rect))
            .collect();
        if draft_points.len() != draft.points.len() {
            return vec![draft];
        }

        let clipped_screen_points = match draft.kind {
            EditKind::Paint | EditKind::Erase => clip_pos_polyline_to_polygon(
                &draft_points,
                &area_polygon,
                draft.width_px.max(0.0) * 0.5,
            ),
            EditKind::Fill | EditKind::EraseArea => {
                clip_pos_area_operation_to_polygon(&draft_points, &area_polygon)
            }
            EditKind::CompactBlock => return vec![draft],
        };
        if clipped_screen_points.is_empty() {
            return Vec::new();
        }
        if clipped_screen_points.len() == 1
            && same_pos_points(&clipped_screen_points[0], &draft_points)
        {
            return vec![draft];
        }

        let transaction_id = Uuid::new_v4();
        clipped_screen_points
            .into_iter()
            .filter_map(|points| {
                let mut operation = draft.clone();
                operation.id = Uuid::new_v4();
                operation.sequence = 0;
                operation.paint_order = None;
                operation.transaction_id = transaction_id;
                operation.native_depth = self.camera.depth;
                operation.native_zoom = self.camera.zoom;
                operation.affects_before_sequence = None;
                operation.points = points
                    .into_iter()
                    .map(|point| screen_to_canvas_for_camera(&self.camera, point, rect))
                    .collect();
                if matches!(operation.kind, EditKind::Fill | EditKind::EraseArea) {
                    operation.smooth_area = false;
                    prepare_draft_for_commit(&mut operation);
                }
                draft_is_committable(&operation).then_some(operation)
            })
            .collect()
    }

    fn paint_operations(
        &mut self,
        painter: &Painter,
        rect: Rect,
        auto_fast_stroke_fallback: bool,
        operation_limit: Option<usize>,
    ) -> FallbackFrameStats {
        let (operations, projected, skipped_operations, mut stats) =
            self.projected_operations(rect, None, operation_limit, auto_fast_stroke_fallback);
        stats.add(FallbackFrameStats {
            skipped_operations,
            ..FallbackFrameStats::default()
        });
        stats.add(self.paint_fallback_operation_items(
            painter,
            rect,
            operations.as_ref(),
            projected,
            auto_fast_stroke_fallback,
        ));
        stats
    }

    fn paint_operations_after(
        &mut self,
        painter: &Painter,
        rect: Rect,
        sequence: i64,
        auto_fast_stroke_fallback: bool,
        operation_limit: Option<usize>,
    ) -> FallbackFrameStats {
        let (operations, projected, skipped_operations, mut stats) = self.projected_operations(
            rect,
            Some(sequence),
            operation_limit,
            auto_fast_stroke_fallback,
        );
        stats.add(FallbackFrameStats {
            skipped_operations,
            ..FallbackFrameStats::default()
        });
        stats.add(self.paint_fallback_operation_items(
            painter,
            rect,
            operations.as_ref(),
            projected,
            auto_fast_stroke_fallback,
        ));
        stats
    }

    fn paint_fallback_operation_items(
        &mut self,
        painter: &Painter,
        rect: Rect,
        operations: &[Arc<EditOperation>],
        projected: Vec<FallbackPaintOperation>,
        auto_fast_stroke_fallback: bool,
    ) -> FallbackFrameStats {
        let mut stats = FallbackFrameStats::default();
        let mut items = projected.into_iter().peekable();
        while let Some(item) = items.next() {
            let Some(operation) = operations.get(item.operation_index()) else {
                continue;
            };
            let mut group = vec![item];
            while operation.fill_group_id.is_some()
                && items.peek().is_some_and(|candidate| {
                    operations
                        .get(candidate.operation_index())
                        .is_some_and(|candidate| operation.shares_fill_group_with(candidate))
                })
            {
                group.push(items.next().expect("peeked fallback operation"));
            }
            if group.len() > 1 {
                stats.add(self.paint_fallback_fill_group_items(
                    painter,
                    rect,
                    operations,
                    group,
                    auto_fast_stroke_fallback,
                ));
            } else {
                stats.add(self.paint_fallback_operation_item(
                    painter,
                    rect,
                    operations,
                    group.pop().expect("one fallback operation"),
                    auto_fast_stroke_fallback,
                ));
            }
        }
        stats
    }

    fn paint_fallback_fill_group_items(
        &mut self,
        painter: &Painter,
        rect: Rect,
        operations: &[Arc<EditOperation>],
        items: Vec<FallbackPaintOperation>,
        auto_fast_stroke_fallback: bool,
    ) -> FallbackFrameStats {
        let Some(first) = items
            .first()
            .and_then(|item| operations.get(item.operation_index()))
        else {
            return FallbackFrameStats::default();
        };
        let color = to_color32(first.opaque_visible_color(BACKGROUND));
        let operation_count = items.len();
        let mut stats = FallbackFrameStats::default();
        let mut polygons = Vec::with_capacity(operation_count);
        for item in items {
            let Some(operation) = operations.get(item.operation_index()) else {
                continue;
            };
            let operation_key = saved_fallback_operation_key(
                operation.id,
                operation,
                self.settings.stroke_fallback_joins,
                auto_fast_stroke_fallback,
                self.settings.smoothing,
            );
            let points = match item {
                FallbackPaintOperation::Projected { points, .. } => {
                    let derive_start = Instant::now();
                    let points = area_saved_fallback_render_points(
                        &points,
                        self.settings.fill_fallback_max_points,
                        if operation.smooth_area {
                            usize::from(self.settings.smoothing.passes())
                        } else {
                            0
                        },
                    );
                    stats.derive_ms += derive_start.elapsed().as_secs_f64() * 1_000.0;
                    stats.fallback_cache_misses = stats.fallback_cache_misses.saturating_add(1);
                    self.fallback_renderer
                        .saved_render_cache
                        .insert(operation_key, points)
                }
                FallbackPaintOperation::CachedDerived { points, .. } => points,
            };
            if !should_paint_fill_fallback(
                operation,
                self.camera.depth,
                points.len(),
                &self.settings,
            ) {
                continue;
            }
            let clipped = if let Some(clipped) = self
                .fallback_renderer
                .saved_render_cache
                .get_clipped_area_polygon(&operation_key)
            {
                clipped
            } else {
                let clip_start = Instant::now();
                let clipped = clip_pos2_polygon(points.as_ref(), rect);
                stats.clip_ms += clip_start.elapsed().as_secs_f64() * 1_000.0;
                self.fallback_renderer
                    .saved_render_cache
                    .insert_clipped_area_polygon(operation_key, clipped)
            };
            polygons.push(clipped);
        }
        let polygon_slices = polygons
            .iter()
            .map(|polygon| polygon.as_ref())
            .collect::<Vec<_>>();
        let paint_start = Instant::now();
        stats.fill_shape_count = paint_fill_group_scanlines(painter, rect, &polygon_slices, color);
        stats.paint_ms += paint_start.elapsed().as_secs_f64() * 1_000.0;
        stats.painted_operations = operation_count;
        stats
    }

    fn paint_fallback_operation_item(
        &mut self,
        painter: &Painter,
        rect: Rect,
        operations: &[Arc<EditOperation>],
        item: FallbackPaintOperation,
        auto_fast_stroke_fallback: bool,
    ) -> FallbackFrameStats {
        match item {
            FallbackPaintOperation::Projected {
                operation_index,
                points,
            } => {
                let Some(operation) = operations.get(operation_index) else {
                    return FallbackFrameStats::default();
                };
                self.paint_operation_points(
                    painter,
                    rect,
                    operation,
                    points,
                    OperationPaintContext {
                        is_draft: false,
                        auto_fast_stroke_fallback,
                        cache_scope_id: operation.id,
                    },
                )
            }
            FallbackPaintOperation::CachedDerived {
                operation_index,
                points,
            } => {
                let Some(operation) = operations.get(operation_index) else {
                    return FallbackFrameStats::default();
                };
                self.paint_saved_operation_derived_points(
                    painter,
                    rect,
                    operation,
                    saved_fallback_operation_key(
                        operation.id,
                        operation,
                        self.settings.stroke_fallback_joins,
                        auto_fast_stroke_fallback,
                        self.settings.smoothing,
                    ),
                    points.as_ref(),
                    auto_fast_stroke_fallback,
                )
            }
        }
    }

    fn projected_operations(
        &mut self,
        rect: Rect,
        after_sequence: Option<i64>,
        operation_limit: Option<usize>,
        auto_fast_stroke_fallback: bool,
    ) -> (
        Vec<Arc<EditOperation>>,
        Vec<FallbackPaintOperation>,
        usize,
        FallbackFrameStats,
    ) {
        let project_start = Instant::now();
        let visible_operations_start = Instant::now();
        let (operations, visible_cache_hit) = self.visible_render_operations(rect);
        let visible_operations_ms = visible_operations_start.elapsed().as_secs_f64() * 1_000.0;
        let (projected, skipped_operations, mut stats) = self.fallback_renderer.project_operations(
            operations.as_ref(),
            &self.camera,
            rect,
            after_sequence,
            operation_limit,
            self.settings.stroke_fallback_joins,
            auto_fast_stroke_fallback,
            self.settings.smoothing,
            project_start,
        );
        stats.visible_operations_ms = visible_operations_ms;
        if visible_cache_hit {
            stats.fallback_visible_cache_hits = 1;
        } else {
            stats.fallback_visible_cache_misses = 1;
        }
        (operations, projected, skipped_operations, stats)
    }

    fn visible_render_operations(&mut self, rect: Rect) -> (Vec<Arc<EditOperation>>, bool) {
        let lod = tile_lod_for_resolution(self.settings.tile_resolution_px);
        let visible_tiles = self.visible_tiles(rect, lod);
        let revision = self.document.revision();
        let document = &self.document;
        let (indices, cache_hit) =
            self.fallback_renderer
                .visible_operations(revision, &visible_tiles, || {
                    document.render_operation_indices_for_tiles(&visible_tiles)
                });
        let operations = self.document.render_operations_by_indices(&indices);
        self.last_visible_render_operation_count = Some(operations.len());
        (operations, cache_hit)
    }

    fn paint_draft(&mut self, painter: &Painter, rect: Rect) {
        if !should_paint_live_draft(self.settings.deferred_drawing_preview) {
            return;
        }
        if let Some(operation) = self.draft.clone() {
            self.paint_operation(painter, rect, &operation, true);
        }
    }

    fn paint_operation(
        &mut self,
        painter: &Painter,
        rect: Rect,
        operation: &EditOperation,
        is_draft: bool,
    ) {
        let points: Vec<Pos2> = operation
            .points
            .iter()
            .filter_map(|point| self.point_to_position(point, rect))
            .collect();
        let _ = self.paint_operation_points(
            painter,
            rect,
            operation,
            points,
            OperationPaintContext {
                is_draft,
                auto_fast_stroke_fallback: false,
                cache_scope_id: operation.id,
            },
        );
    }

    fn paint_operation_points(
        &mut self,
        painter: &Painter,
        rect: Rect,
        operation: &EditOperation,
        points: Vec<Pos2>,
        context: OperationPaintContext,
    ) -> FallbackFrameStats {
        if operation.is_compact_block() {
            let mut stats = FallbackFrameStats::default();
            for source in &operation.compact_sources {
                let operation_key = saved_fallback_operation_key(
                    context.cache_scope_id,
                    source,
                    self.settings.stroke_fallback_joins,
                    context.auto_fast_stroke_fallback,
                    self.settings.smoothing,
                );
                if !context.is_draft
                    && let Some(points) = self
                        .fallback_renderer
                        .saved_render_cache
                        .get(&operation_key)
                {
                    stats.fallback_cache_hits = stats.fallback_cache_hits.saturating_add(1);
                    stats.add(self.paint_saved_operation_derived_points(
                        painter,
                        rect,
                        source,
                        operation_key,
                        points.as_ref(),
                        context.auto_fast_stroke_fallback,
                    ));
                    continue;
                }
                let bounds_lookup = self
                    .fallback_renderer
                    .operation_bounds_cache
                    .operation_may_intersect_screen_rect(
                        source,
                        &self.camera,
                        rect,
                        operation_width(source, self.camera.depth, self.camera.zoom),
                    );
                if bounds_lookup.cache_hit {
                    stats.fallback_bounds_cache_hits =
                        stats.fallback_bounds_cache_hits.saturating_add(1);
                } else {
                    stats.fallback_bounds_cache_misses =
                        stats.fallback_bounds_cache_misses.saturating_add(1);
                }
                if !bounds_lookup.may_intersect {
                    stats.skipped_operations = stats.skipped_operations.saturating_add(1);
                    continue;
                }
                let project_start = Instant::now();
                let points: Vec<Pos2> = source
                    .points
                    .iter()
                    .filter_map(|point| self.point_to_position(point, rect))
                    .collect();
                stats.projected_operations = stats.projected_operations.saturating_add(1);
                stats.project_ms += project_start.elapsed().as_secs_f64() * 1_000.0;
                stats.add(self.paint_operation_points(painter, rect, source, points, context));
            }
            return stats;
        }
        if points.len() < 2 {
            return FallbackFrameStats::default();
        }

        let width = operation_width(operation, self.camera.depth, self.camera.zoom);
        let color = to_color32(operation.opaque_visible_color(BACKGROUND));
        if operation.kind.is_area() {
            if context.is_draft {
                let points =
                    smooth_pos2_draft(&points, usize::from(self.settings.smoothing.passes()));
                painter.add(egui::Shape::line(points, Stroke::new(1.5, color)));
            } else {
                let mut stats = FallbackFrameStats::default();
                let operation_key = saved_fallback_operation_key(
                    context.cache_scope_id,
                    operation,
                    self.settings.stroke_fallback_joins,
                    context.auto_fast_stroke_fallback,
                    self.settings.smoothing,
                );
                let points = if let Some(cached) = self
                    .fallback_renderer
                    .saved_render_cache
                    .get(&operation_key)
                {
                    stats.fallback_cache_hits = stats.fallback_cache_hits.saturating_add(1);
                    cached
                } else {
                    let derive_start = Instant::now();
                    let points = area_saved_fallback_render_points(
                        &points,
                        self.settings.fill_fallback_max_points,
                        if operation.smooth_area {
                            usize::from(self.settings.smoothing.passes())
                        } else {
                            0
                        },
                    );
                    stats.derive_ms += derive_start.elapsed().as_secs_f64() * 1_000.0;
                    let points = self
                        .fallback_renderer
                        .saved_render_cache
                        .insert(operation_key, points);
                    stats.fallback_cache_misses = stats.fallback_cache_misses.saturating_add(1);
                    points
                };
                if should_paint_fill_fallback(
                    operation,
                    self.camera.depth,
                    points.len(),
                    &self.settings,
                ) {
                    stats.add(self.paint_saved_operation_derived_points(
                        painter,
                        rect,
                        operation,
                        operation_key,
                        points.as_ref(),
                        context.auto_fast_stroke_fallback,
                    ));
                }
                return stats;
            }
        } else if context.is_draft {
            let points = smooth_pos2_draft(&points, usize::from(self.settings.smoothing.passes()));
            paint_stroke_fallback(painter, &points, width, color, None, true);
        } else {
            let mut stats = FallbackFrameStats::default();
            let fallback_join_mode = self.settings.stroke_fallback_joins;
            let smoothing_passes = stroke_fallback_smoothing_passes(
                fallback_join_mode,
                context.auto_fast_stroke_fallback,
                self.settings.smoothing.passes(),
            );
            let operation_key = saved_fallback_operation_key(
                context.cache_scope_id,
                operation,
                fallback_join_mode,
                context.auto_fast_stroke_fallback,
                self.settings.smoothing,
            );
            let points = if let Some(cached) = self
                .fallback_renderer
                .saved_render_cache
                .get(&operation_key)
            {
                stats.fallback_cache_hits = stats.fallback_cache_hits.saturating_add(1);
                cached
            } else {
                let derive_start = Instant::now();
                let points = smooth_pos2_saved_fallback(&points, smoothing_passes, false);
                stats.derive_ms += derive_start.elapsed().as_secs_f64() * 1_000.0;
                let points = self
                    .fallback_renderer
                    .saved_render_cache
                    .insert(operation_key, points);
                stats.fallback_cache_misses = stats.fallback_cache_misses.saturating_add(1);
                points
            };
            stats.add(self.paint_saved_operation_derived_points(
                painter,
                rect,
                operation,
                operation_key,
                points.as_ref(),
                context.auto_fast_stroke_fallback,
            ));
            return stats;
        }
        FallbackFrameStats::default()
    }

    fn paint_saved_operation_derived_points(
        &mut self,
        painter: &Painter,
        rect: Rect,
        operation: &EditOperation,
        operation_key: SavedFallbackRenderOperationKey,
        points: &[Pos2],
        auto_fast_stroke_fallback: bool,
    ) -> FallbackFrameStats {
        if points.len() < 2 {
            return FallbackFrameStats::default();
        }

        let width = operation_width(operation, self.camera.depth, self.camera.zoom);
        let color = to_color32(operation.opaque_visible_color(BACKGROUND));
        if operation.kind.is_area() {
            let mut stats = FallbackFrameStats::default();
            if should_paint_fill_fallback(
                operation,
                self.camera.depth,
                points.len(),
                &self.settings,
            ) {
                let clipped = if let Some(clipped) = self
                    .fallback_renderer
                    .saved_render_cache
                    .get_clipped_area_polygon(&operation_key)
                {
                    clipped
                } else {
                    let clip_start = Instant::now();
                    let clipped = clip_pos2_polygon(points, rect);
                    stats.clip_ms += clip_start.elapsed().as_secs_f64() * 1_000.0;
                    self.fallback_renderer
                        .saved_render_cache
                        .insert_clipped_area_polygon(operation_key, clipped)
                };
                let paint_start = Instant::now();
                let fill_shape_count = paint_fill_scanlines(painter, rect, clipped.as_ref(), color);
                stats.fill_shape_count = stats.fill_shape_count.saturating_add(fill_shape_count);
                stats.paint_ms += paint_start.elapsed().as_secs_f64() * 1_000.0;
                stats.painted_operations = stats.painted_operations.saturating_add(1);
            }
            return stats;
        }

        let fallback_join_mode = self.settings.stroke_fallback_joins;
        let segmented_point_limit =
            stroke_fallback_segmented_point_limit(fallback_join_mode, auto_fast_stroke_fallback);
        let fast_endpoint_caps =
            stroke_fallback_fast_endpoint_caps(fallback_join_mode, auto_fast_stroke_fallback);
        let clip_rect = rect.expand(width * 0.5 + 1.0);
        let mut stats = FallbackFrameStats::default();
        let runs = if let Some(runs) = self
            .fallback_renderer
            .saved_render_cache
            .get_clipped_stroke_runs(&operation_key)
        {
            runs
        } else {
            let clip_start = Instant::now();
            let runs = clip_pos2_polyline(points, clip_rect);
            stats.clip_ms += clip_start.elapsed().as_secs_f64() * 1_000.0;
            self.fallback_renderer
                .saved_render_cache
                .insert_clipped_stroke_runs(operation_key, runs)
        };
        for run in runs.iter() {
            let paint_start = Instant::now();
            let (run_segmented_point_limit, budget_fallback) = consume_segmented_fallback_budget(
                run.len(),
                segmented_point_limit,
                &mut self.fallback_renderer.segmented_shape_budget_remaining,
            );
            if budget_fallback {
                stats.segmented_budget_fallbacks =
                    stats.segmented_budget_fallbacks.saturating_add(1);
            }
            stats.add(paint_stroke_fallback(
                painter,
                run.as_ref(),
                width,
                color,
                run_segmented_point_limit,
                fast_endpoint_caps,
            ));
            stats.paint_ms += paint_start.elapsed().as_secs_f64() * 1_000.0;
        }
        if stats.fast_strokes > 0 || stats.segmented_strokes > 0 {
            stats.painted_operations = stats.painted_operations.saturating_add(1);
        }
        stats
    }

    fn paint_transient_overlays(
        &mut self,
        painter: &Painter,
        rect: Rect,
        large_selection_fast_overlay: bool,
    ) {
        let selection_color = Color32::from_rgba_unmultiplied(20, 120, 220, 180);
        let selection_fill = Color32::from_rgba_unmultiplied(20, 120, 220, 28);
        let rectangle_color = match self.rectangle_selection_mode {
            RectangleSelectionMode::Inside => selection_color,
            RectangleSelectionMode::Crossing => Color32::from_rgba_unmultiplied(220, 130, 20, 190),
        };
        let move_delta = self.selection_move_delta();
        let scale_gesture = self.selection_scale_gesture;
        let rotate_gesture = self.selection_rotate_gesture;

        if self.area_selection_drag_points.len() >= 2 {
            let area_color = Color32::from_rgba_unmultiplied(120, 80, 220, 210);
            let area_fill = Color32::from_rgba_unmultiplied(120, 80, 220, 24);
            match self.area_selection_shape {
                AreaSelectionShape::Rectangle => {
                    let preview_rect = Rect::from_two_pos(
                        self.area_selection_drag_points[0],
                        self.area_selection_drag_points[1],
                    )
                    .intersect(rect);
                    painter.rect_filled(preview_rect, 0.0, area_fill);
                    paint_dashed_closed_outline(
                        painter,
                        &[
                            preview_rect.left_top(),
                            preview_rect.right_top(),
                            preview_rect.right_bottom(),
                            preview_rect.left_bottom(),
                        ],
                        Stroke::new(1.5, area_color),
                    );
                }
                AreaSelectionShape::Lasso => {
                    painter.add(egui::Shape::line(
                        self.area_selection_drag_points.clone(),
                        Stroke::new(1.5, area_color),
                    ));
                }
            }
        }

        if let Some(area_selection) = &self.area_selection {
            let points: Vec<_> = area_selection
                .points
                .iter()
                .filter_map(|point| self.point_to_position(point, rect))
                .collect();
            if points.len() >= 2 {
                let area_color = match area_selection.shape {
                    AreaSelectionShape::Rectangle => {
                        Color32::from_rgba_unmultiplied(120, 80, 220, 230)
                    }
                    AreaSelectionShape::Lasso => Color32::from_rgba_unmultiplied(80, 95, 220, 230),
                };
                let area_bounds_color = Color32::from_rgba_unmultiplied(120, 80, 220, 130);
                paint_dashed_closed_outline(painter, &points, Stroke::new(1.5, area_color));
                if let Some(bounds) = pos2_bounds(&points) {
                    paint_rect_outline(painter, bounds, Stroke::new(1.0, area_bounds_color));
                }
            }
        }

        if let (Some(start), Some(end)) = (self.selection_drag_start, self.selection_drag_current) {
            let selection_rect =
                Rect::from_two_pos(start + move_delta, end + move_delta).intersect(rect);
            painter.rect_filled(selection_rect, 0.0, selection_fill);
            paint_rect_outline(painter, selection_rect, Stroke::new(1.5, rectangle_color));
        }

        let transformed_selection_bounds = rotate_gesture
            .map(SelectionRotateGesture::transformed_bounds)
            .or_else(|| scale_gesture.map(SelectionScaleGesture::transformed_bounds));
        let selected_highlight_bounds = if large_selection_fast_overlay {
            transformed_selection_bounds
                .or_else(|| self.fast_selection_overlay_bounds(rect, move_delta))
                .or_else(|| {
                    self.selection_screen_bounds(rect).map(|bounds| {
                        Rect::from_min_max(bounds.min + move_delta, bounds.max + move_delta)
                    })
                })
        } else {
            let mut selected_highlight_min = Pos2::new(f32::INFINITY, f32::INFINITY);
            let mut selected_highlight_max = Pos2::new(f32::NEG_INFINITY, f32::NEG_INFINITY);
            let mut selected_highlight_found = false;
            for operation in self
                .document
                .operations()
                .iter()
                .filter(|operation| self.selected_operation_ids.contains(&operation.id))
            {
                let projected_points: Vec<_> = operation
                    .points
                    .iter()
                    .filter_map(|point| self.point_to_position(point, rect))
                    .collect();
                let points: Vec<_> = selection_highlight_render_points(
                    operation,
                    projected_points,
                    usize::from(self.settings.smoothing.passes()),
                    self.settings.fill_fallback_max_points,
                )
                .into_iter()
                .map(|point| {
                    if let Some(gesture) = rotate_gesture {
                        gesture.transform_position(point)
                    } else if let Some(gesture) = scale_gesture {
                        gesture.transform_position(point)
                    } else {
                        point + move_delta
                    }
                })
                .collect();
                let width = operation_width(operation, self.camera.depth, self.camera.zoom)
                    * scale_gesture.map_or(1.0, SelectionScaleGesture::scale);
                let bounds_radius = if operation.kind.is_area() {
                    0.0
                } else {
                    width * 0.5
                };
                for point in &points {
                    selected_highlight_min.x =
                        selected_highlight_min.x.min(point.x - bounds_radius);
                    selected_highlight_min.y =
                        selected_highlight_min.y.min(point.y - bounds_radius);
                    selected_highlight_max.x =
                        selected_highlight_max.x.max(point.x + bounds_radius);
                    selected_highlight_max.y =
                        selected_highlight_max.y.max(point.y + bounds_radius);
                    selected_highlight_found = true;
                }
                paint_operation_highlight(painter, operation.kind, &points, width, selection_color);
            }
            selected_highlight_found
                .then(|| Rect::from_min_max(selected_highlight_min, selected_highlight_max))
        };

        let selection_bounds = (self.tool == ToolKind::Selection)
            .then(|| transformed_selection_bounds.or(selected_highlight_bounds))
            .flatten();
        if let Some(bounds) = selection_bounds {
            paint_rect_outline(painter, bounds, Stroke::new(1.5, selection_color));
            let rotate_handle = selection_rotate_handle_position(bounds);
            painter.line_segment(
                [bounds.center_top(), rotate_handle],
                Stroke::new(1.2, selection_color),
            );
            painter.circle_filled(
                rotate_handle,
                SELECTION_ROTATE_HANDLE_RADIUS_PX,
                Color32::WHITE,
            );
            painter.circle_stroke(
                rotate_handle,
                SELECTION_ROTATE_HANDLE_RADIUS_PX,
                Stroke::new(1.5, selection_color),
            );
            for handle in SelectionScaleHandle::ALL {
                let handle_rect = Rect::from_center_size(
                    handle.position(bounds),
                    egui::vec2(SELECTION_HANDLE_SIZE_PX, SELECTION_HANDLE_SIZE_PX),
                );
                painter.rect_filled(handle_rect, 0.0, Color32::WHITE);
                paint_rect_outline(painter, handle_rect, Stroke::new(1.5, selection_color));
            }
        }

        if let Some(hovered) = self.hovered_operation_id.and_then(|id| {
            self.document
                .operations()
                .iter()
                .find(|operation| operation.id == id)
        }) && !self.selected_operation_ids.contains(&hovered.id)
        {
            let object_id = hovered.object_group_id();
            for operation in self.document.operations().iter().filter(|operation| {
                operation.layer_id == hovered.layer_id && operation.object_group_id() == object_id
            }) {
                let projected_points: Vec<_> = operation
                    .points
                    .iter()
                    .filter_map(|point| self.point_to_position(point, rect))
                    .collect();
                let points = selection_highlight_render_points(
                    operation,
                    projected_points,
                    usize::from(self.settings.smoothing.passes()),
                    self.settings.fill_fallback_max_points,
                );
                let width = operation_width(operation, self.camera.depth, self.camera.zoom);
                paint_operation_highlight(
                    painter,
                    operation.kind,
                    &points,
                    width,
                    Color32::from_rgba_unmultiplied(40, 180, 80, 190),
                );
            }
        }

        if self.eraser_lasso_points.len() >= 2 {
            let stroke = Stroke::new(2.0, Color32::from_rgba_unmultiplied(220, 45, 45, 210));
            if self.eraser_lasso_active {
                painter.add(egui::Shape::line(self.eraser_lasso_points.clone(), stroke));
            } else {
                paint_closed_outline(painter, &self.eraser_lasso_points, stroke);
            }
        }
    }

    fn clear_transient_tools(&mut self) {
        self.clear_object_selection_state();
        self.clear_area_selection_state();
        self.eraser_target_ids.clear();
        self.eraser_lasso_points.clear();
        self.eraser_lasso_active = false;
    }

    fn clear_object_selection_state(&mut self) {
        self.selected_operation_ids.clear();
        self.selection_drag_start = None;
        self.selection_drag_current = None;
        self.selection_drag_active = false;
        self.selection_move_start = None;
        self.selection_move_current = None;
        self.selection_move_active = false;
        self.selection_scale_gesture = None;
        self.selection_rotate_gesture = None;
        self.hovered_operation_id = None;
        self.selection_hover_position = None;
        self.reset_selection_cycle();
    }

    fn clear_area_selection_state(&mut self) {
        self.area_selection = None;
        self.area_selection_drag_points.clear();
        self.area_selection_drag_active = false;
    }

    fn paint_overlay(&mut self, painter: &Painter, rect: Rect) {
        if !self.settings.overlay_enabled {
            return;
        }
        if self.settings.overlay_show_tile_coordinates
            || self.settings.overlay_show_local_coordinates
        {
            self.coordinate_overlay.update(&self.camera);
        }
        let mut tile_state = self
            .last_depth_render_mode
            .map(depth_render_mode_label)
            .unwrap_or("unknown")
            .to_owned();
        let tile_activity = if self.settings.pause_tile_generation {
            "tiles paused"
        } else if self.automatic_tile_generation_pause {
            "tiles paused: drawing"
        } else if self.tile_rebuild_is_deferred() {
            "rebuild waiting"
        } else {
            ""
        };
        if !tile_activity.is_empty() {
            tile_state.push_str(" · ");
            tile_state.push_str(tile_activity);
        }
        let performance = self.frame_rate.metrics().map_or_else(
            || "fps --".to_owned(),
            |(fps, frame_time)| format!("fps {fps:.1} ({frame_time:.1} ms)"),
        );
        let Some(text) = canvas_overlay_text(
            &self.settings,
            &self.camera,
            self.document.operations().len(),
            &performance,
            &self.status_message,
            &tile_state,
            &self.coordinate_overlay.formatted_x,
            &self.coordinate_overlay.formatted_y,
        ) else {
            return;
        };
        painter.text(
            rect.left_top() + egui::vec2(12.0, 12.0),
            egui::Align2::LEFT_TOP,
            text,
            egui::FontId::monospace(13.0),
            Color32::from_black_alpha(180),
        );
    }

    fn paint_cached_tiles(
        &mut self,
        context: &egui::Context,
        painter: &Painter,
        rect: Rect,
    ) -> (TileFallbackMode, TileFrameStats) {
        let mut stats = TileFrameStats::default();
        let collect_start = Instant::now();
        stats.uploaded_textures = self.collect_tile_results(context);
        stats.collect_upload_ms = collect_start.elapsed().as_secs_f64() * 1_000.0;

        let revision = self.document.revision();
        self.pending_tiles
            .retain(|(_, cached_revision)| *cached_revision == revision);

        if self.zoom_tile_requests_deferred() {
            return (TileFallbackMode::Full, stats);
        }

        let request_start = Instant::now();
        let lod = tile_lod_for_resolution(self.settings.tile_resolution_px);
        let visible = self.visible_tiles(rect, lod);
        let requested =
            tile_keys_for_view(&self.camera, rect, lod, self.settings.tile_prefetch_radius);
        let visible_set: HashSet<_> = requested.iter().cloned().collect();
        self.tile_textures
            .retain(|key, _| visible_set.contains(key));
        if visible_set != self.visible_tile_set {
            self.visible_tile_generation = self.visible_tile_generation.saturating_add(1);
            self.visible_tile_set = visible_set;
            self.tile_scheduler
                .set_generation(self.visible_tile_generation);
            self.pending_tiles.retain(|(key, cached_revision)| {
                *cached_revision == revision && self.visible_tile_set.contains(key)
            });
            self.log_session_event_with(
                "tile_generation",
                json!({ "tiles": { "reason": "visible_set_changed" } }),
            );
        }

        if !tile_generation_is_paused(
            self.settings.pause_tile_generation,
            self.automatic_tile_generation_pause,
        ) && !self.tile_rebuild_is_deferred()
        {
            let missing = tile_request_batch(
                requested
                    .iter()
                    .filter(|key| {
                        self.tile_textures
                            .get(key)
                            .is_none_or(|loaded| loaded.revision != revision)
                            && !self.pending_tiles.contains(&((*key).clone(), revision))
                    })
                    .cloned(),
            );
            let queued_count = missing.len();
            let snapshot_sequence = self.document.max_sequence();
            for key in missing {
                let operations = self.document.operations_for_tile(&key);
                let incremental_update = self.tile_textures.get(&key).and_then(|loaded| {
                    (loaded.revision < revision)
                        .then(|| {
                            incremental_paint_operations(loaded.snapshot_sequence, &operations)
                        })
                        .flatten()
                        .map(|incremental_operations| IncrementalTileUpdate {
                            base_revision: loaded.revision,
                            operations: Arc::new(incremental_operations),
                        })
                });
                let operations = Arc::new(operations);
                if self
                    .tile_scheduler
                    .request(TileJob {
                        key: key.clone(),
                        revision,
                        snapshot_sequence,
                        generation: self.visible_tile_generation,
                        operations: Arc::clone(&operations),
                        incremental_update,
                        background: BACKGROUND,
                    })
                    .is_ok()
                {
                    self.pending_tiles.insert((key, revision));
                    stats.queued_jobs = stats.queued_jobs.saturating_add(1);
                }
            }
            if queued_count > 0 {
                self.log_session_event_with(
                    "tile_queue",
                    json!({ "tiles": { "queued": queued_count } }),
                );
            }
        }
        stats.request_queue_ms = request_start.elapsed().as_secs_f64() * 1_000.0;

        let draw_start = Instant::now();
        let mut snapshots = Vec::with_capacity(visible.len());
        for key in visible {
            let Some(loaded) = self.tile_textures.get(&key) else {
                snapshots.push(None);
                continue;
            };
            snapshots.push(Some((loaded.revision, loaded.snapshot_sequence)));
            let top_left = CanvasPoint::new(key.depth, key.x, key.y, 0.0, 0.0);
            let Some(position) = self.point_to_position(&top_left, rect) else {
                continue;
            };
            let (tile_rect, source_rect) =
                tile_display_rects(position, self.camera.zoom as f32, key.lod);
            painter.image(loaded.texture.id(), tile_rect, source_rect, Color32::WHITE);
        }
        stats.draw_ms = draw_start.elapsed().as_secs_f64() * 1_000.0;
        (tile_fallback_mode(revision, snapshots), stats)
    }

    fn texture_name(&self, key: &TileKey, revision: u64) -> String {
        format!(
            "tile-d{}-p{}-x{}-y{}-r{}",
            key.depth,
            tile_resolution(key.lod),
            key.x,
            key.y,
            revision
        )
    }

    fn collect_tile_results(&mut self, context: &egui::Context) -> usize {
        let mut uploaded_textures = 0usize;
        while let Some(result) = self.tile_scheduler.try_result() {
            if result.generation != self.visible_tile_generation {
                continue;
            }
            self.pending_tiles
                .remove(&(result.key.clone(), result.revision));
            match result.result {
                Ok(image) => {
                    let size = [image.width() as usize, image.height() as usize];
                    let color_image =
                        egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
                    let texture = context.load_texture(
                        self.texture_name(&result.key, result.revision),
                        color_image,
                        egui::TextureOptions::LINEAR,
                    );
                    let should_replace = self
                        .tile_textures
                        .get(&result.key)
                        .is_none_or(|loaded| loaded.revision <= result.revision);
                    if should_replace {
                        uploaded_textures = uploaded_textures.saturating_add(1);
                        self.tile_textures.insert(
                            result.key,
                            LoadedTileTexture {
                                revision: result.revision,
                                snapshot_sequence: result.snapshot_sequence,
                                texture,
                            },
                        );
                    }
                }
                Err(error) => self.status_message = format!("Tile rebuild failed: {error}"),
            }
        }
        uploaded_textures
    }

    fn visible_tiles(&self, rect: Rect, lod: u8) -> Vec<TileKey> {
        tile_keys_for_view(&self.camera, rect, lod, 0)
    }

    fn depth_render_mode(&self, rect: Rect) -> DepthRenderMode {
        let lod = tile_lod_for_resolution(self.settings.tile_resolution_px);
        let visible_tiles = self.visible_tiles(rect, lod);
        let visible_mode = self.document.visible_depth_mode_for_tiles(
            &visible_tiles,
            self.active_layer_id,
            self.camera.depth,
            self.settings.vector_depth_radius,
        );
        depth_render_mode(visible_mode, self.settings.distant_tiles_enabled)
    }

    fn apply_depth_render_mode(&mut self, mode: DepthRenderMode) {
        if self.last_depth_render_mode == Some(mode) {
            return;
        }
        self.reset_tile_requests();
        if mode == DepthRenderMode::DistantTile {
            self.clear_object_selection_state();
            self.eraser_lasso_points.clear();
            self.eraser_lasso_active = false;
        }
        self.last_depth_render_mode = Some(mode);
        self.log_session_event_with(
            "depth_render_mode",
            json!({ "tiles": { "depth_render_mode": depth_render_mode_label(mode) } }),
        );
    }

    fn update_tile_rebuild_interaction(&mut self, interaction_active: bool) {
        let Some(idle_interval) = self.settings.tile_rebuild_policy.idle_interval() else {
            self.last_tile_rebuild_interaction = None;
            return;
        };
        let now = Instant::now();
        let was_deferred =
            tile_rebuild_deferred_since(self.last_tile_rebuild_interaction, now, idle_interval);
        if interaction_active {
            if !was_deferred {
                self.pending_tiles.clear();
                self.visible_tile_generation = self.visible_tile_generation.saturating_add(1);
                self.tile_scheduler
                    .set_generation(self.visible_tile_generation);
            }
            self.last_tile_rebuild_interaction = Some(now);
        } else if !was_deferred {
            self.last_tile_rebuild_interaction = None;
        }
    }

    fn tile_rebuild_is_deferred(&self) -> bool {
        self.settings
            .tile_rebuild_policy
            .idle_interval()
            .is_some_and(|idle_interval| {
                tile_rebuild_deferred_since(
                    self.last_tile_rebuild_interaction,
                    Instant::now(),
                    idle_interval,
                )
            })
    }

    fn apply_tile_request_settings(&mut self) {
        self.last_tile_rebuild_interaction = None;
        self.reset_tile_requests();
        self.log_session_event_with(
            "tile_generation",
            json!({ "tiles": { "reason": "request_settings" } }),
        );
    }

    fn reset_tile_requests(&mut self) {
        self.pending_tiles.clear();
        self.visible_tile_generation = self.visible_tile_generation.saturating_add(1);
        self.visible_tile_set.clear();
        self.tile_scheduler
            .set_generation(self.visible_tile_generation);
    }

    fn position_to_canvas(&self, position: Pos2, rect: Rect) -> CanvasPoint {
        screen_to_canvas_for_camera(&self.camera, position, rect)
    }

    fn point_to_position(&self, point: &CanvasPoint, rect: Rect) -> Option<Pos2> {
        canvas_point_to_position(&self.camera, point, rect)
    }

    fn pick_color_at(&mut self, position: Option<Pos2>, rect: Rect) {
        let Some(position) = position else {
            return;
        };
        if let Some(color) = self.sample_color_at(position, rect) {
            self.color = color;
            self.status_message = if color == BACKGROUND {
                "Background picked".to_owned()
            } else {
                "Color picked".to_owned()
            };
        }
    }

    fn sample_color_at(&mut self, position: Pos2, rect: Rect) -> Option<Color> {
        let (operations, _) = self.visible_render_operations(rect);
        for operation in operations.iter().rev() {
            let points = operation
                .points
                .iter()
                .filter_map(|point| self.point_to_position(point, rect))
                .collect::<Vec<_>>();
            let width = operation_width(operation, self.camera.depth, self.camera.zoom);
            if operation_pick_hit(operation.kind, &points, width, position) {
                let color = operation.opaque_visible_color(BACKGROUND);
                return Some(color);
            }
        }
        Some(BACKGROUND)
    }

    fn run_undo(&mut self) {
        self.clear_transient_tools();
        match self.document.undo() {
            Ok(true) => {
                self.sync_active_layer_after_history();
                self.invalidate_tile_rendering();
                self.status_message = "Undone".to_owned();
                self.log_session_event("undo");
            }
            Ok(false) => self.status_message = "Nothing to undo".to_owned(),
            Err(error) => self.status_message = format!("Undo failed: {error:#}"),
        }
    }

    fn run_redo(&mut self) {
        self.clear_transient_tools();
        match self.document.redo() {
            Ok(true) => {
                self.sync_active_layer_after_history();
                self.invalidate_tile_rendering();
                self.status_message = "Redone".to_owned();
                self.log_session_event("redo");
            }
            Ok(false) => self.status_message = "Nothing to redo".to_owned(),
            Err(error) => self.status_message = format!("Redo failed: {error:#}"),
        }
    }

    fn sync_active_layer_after_history(&mut self) {
        if let Some(layer) = self
            .document
            .layers()
            .iter()
            .find(|layer| layer.id == self.active_layer_id)
        {
            self.layer_name_edit.clone_from(&layer.name);
            return;
        }
        if let Some(layer) = self.document.layers().last() {
            self.active_layer_id = layer.id;
            self.layer_name_edit.clone_from(&layer.name);
        }
    }

    fn selected_operation_snapshots(&self) -> Vec<EditOperation> {
        let selected = self
            .document
            .expanded_object_group_ids(&self.selected_operation_ids, self.active_layer_id);
        let mut operations = self
            .document
            .operations()
            .iter()
            .filter(|operation| selected.contains(&operation.id))
            .cloned()
            .collect::<Vec<_>>();
        operations.sort_by_key(|operation| {
            (
                operation.effective_paint_order(),
                operation.sequence,
                operation.id,
            )
        });
        operations
    }

    fn copy_selected(&mut self) {
        let operations = self.selected_operation_snapshots();
        if operations.is_empty() {
            self.status_message = "Nothing selected".to_owned();
            return;
        }
        let count = operations
            .iter()
            .map(EditOperation::object_group_id)
            .collect::<HashSet<_>>()
            .len();
        self.selection_clipboard = SelectionClipboard {
            operations,
            paste_count: 0,
        };
        self.status_message = format!("Copied {count} objects");
    }

    fn cut_selected(&mut self) {
        if !self.document.layer_is_editable(self.active_layer_id) {
            self.status_message = "Active layer is hidden or locked".to_owned();
            return;
        }
        let operations = self.selected_operation_snapshots();
        if operations.is_empty() {
            self.status_message = "Nothing selected".to_owned();
            return;
        }
        let count = operations
            .iter()
            .map(EditOperation::object_group_id)
            .collect::<HashSet<_>>()
            .len();
        self.selection_clipboard = SelectionClipboard {
            operations,
            paste_count: 0,
        };
        let selected = self.selected_operation_ids.clone();
        match self
            .document
            .delete_operations(&selected, self.active_layer_id)
        {
            Ok(deleted) => {
                self.clear_transient_tools();
                self.invalidate_tile_rendering();
                self.status_message = format!("Cut {deleted} objects");
                self.log_session_event_with("cut", json!({ "document": { "deleted": deleted } }));
            }
            Err(error) => {
                self.status_message = format!("Cut failed after copying {count} objects: {error:#}")
            }
        }
    }

    fn paste_selection(&mut self, in_place: bool) {
        if self.selection_clipboard.operations.is_empty() {
            self.status_message = "Clipboard is empty".to_owned();
            return;
        }
        if !self.document.layer_is_editable(self.active_layer_id) {
            self.status_message = "Active layer is hidden or locked".to_owned();
            return;
        }
        let operations = self.selection_clipboard.operations.clone();
        let offset = if in_place {
            0.0
        } else {
            f64::from(
                self.selection_clipboard
                    .paste_count
                    .saturating_add(1)
                    .min(1024),
            ) * 16.0
        };
        match self.document.paste_operations(
            &operations,
            self.active_layer_id,
            self.camera.depth,
            self.camera.zoom,
            offset,
            offset,
        ) {
            Ok(ids) if !ids.is_empty() => {
                if !in_place {
                    self.selection_clipboard.paste_count =
                        self.selection_clipboard.paste_count.saturating_add(1);
                }
                self.clear_transient_tools();
                self.selected_operation_ids = ids.into_iter().collect();
                self.invalidate_tile_rendering();
                self.status_message = format!(
                    "Pasted {} objects{}",
                    self.selected_object_count(),
                    if in_place { " in place" } else { "" }
                );
                self.log_session_event_with(
                    "paste",
                    json!({ "document": { "pasted": self.selected_object_count() } }),
                );
            }
            Ok(_) => self.status_message = "Clipboard is empty".to_owned(),
            Err(error) => self.status_message = format!("Paste failed: {error:#}"),
        }
    }

    fn delete_selected(&mut self) {
        let selected = self.selected_operation_ids.clone();
        match self
            .document
            .delete_operations(&selected, self.active_layer_id)
        {
            Ok(0) => {
                self.clear_transient_tools();
                self.status_message = "Nothing selected".to_owned();
            }
            Ok(deleted) => {
                self.clear_transient_tools();
                self.invalidate_tile_rendering();
                self.status_message = format!("Deleted {deleted} objects");
                self.log_session_event_with(
                    "delete",
                    json!({ "document": { "deleted": deleted } }),
                );
            }
            Err(error) => self.status_message = format!("Delete failed: {error:#}"),
        }
    }

    fn rename_bookmark(&mut self, id: Uuid, name: String) {
        if name.is_empty() {
            self.status_message = "Bookmark name is empty".to_owned();
            return;
        }
        match self.document.rename_bookmark(id, &name) {
            Ok(true) => {
                if let Some(bookmark) = self.bookmarks.iter_mut().find(|bookmark| bookmark.id == id)
                {
                    bookmark.name = name.clone();
                }
                self.bookmark_edits.insert(id, name);
                self.status_message = "Bookmark renamed".to_owned();
            }
            Ok(false) => self.status_message = "Bookmark not found".to_owned(),
            Err(error) => self.status_message = format!("Rename failed: {error:#}"),
        }
    }

    fn delete_bookmark(&mut self, id: Uuid) {
        match self.document.delete_bookmark(id) {
            Ok(true) => {
                self.bookmarks.retain(|bookmark| bookmark.id != id);
                self.bookmark_edits.remove(&id);
                self.status_message = "Bookmark deleted".to_owned();
            }
            Ok(false) => self.status_message = "Bookmark not found".to_owned(),
            Err(error) => self.status_message = format!("Delete failed: {error:#}"),
        }
    }

    fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            self.replace_document(path);
        }
    }

    fn new_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_file_name("untitled.esketch")
            .save_file()
        {
            self.replace_document(path);
        }
    }

    fn replace_document(&mut self, path: PathBuf) {
        match CanvasDocument::open(path) {
            Ok(document) => {
                self.document = document;
                self.pending_layer_delete = None;
                self.selection_clipboard = SelectionClipboard::default();
                self.active_layer_id = self
                    .document
                    .layers()
                    .last()
                    .map_or(DEFAULT_LAYER_ID, |layer| layer.id);
                self.layer_name_edit = self
                    .document
                    .layers()
                    .last()
                    .map_or_else(|| "Layer 1".to_owned(), |layer| layer.name.clone());
                self.clear_transient_tools();
                match TileCache::with_options(
                    self.document.root(),
                    self.settings.tile_cache_options(),
                ) {
                    Ok(cache) => {
                        self.tile_scheduler =
                            TileScheduler::new(cache, self.settings.tile_worker_count)
                    }
                    Err(error) => {
                        self.status_message = format!("Tile cache failed: {error:#}");
                        return;
                    }
                }
                self.tile_textures.clear();
                self.pending_tiles.clear();
                self.invalidate_projection_caches();
                self.visible_tile_generation = self.visible_tile_generation.saturating_add(1);
                self.visible_tile_set.clear();
                self.last_zoom_change = None;
                self.last_tile_rebuild_interaction = None;
                self.tile_scheduler
                    .set_generation(self.visible_tile_generation);
                self.bookmarks = self.document.bookmarks().unwrap_or_default();
                self.bookmark_edits = bookmark_edit_names(&self.bookmarks);
                self.new_bookmark_name = format!("Bookmark {}", self.bookmarks.len() + 1);
                self.camera = CameraAddress::default();
                self.selection_capture_camera_depth = self.camera.depth;
                self.depth_jump_target = self.camera.depth;
                self.status_message = "Canvas opened".to_owned();
                self.log_session_event("document_opened");
            }
            Err(error) => self.status_message = format!("Open failed: {error:#}"),
        }
    }

    fn adjust_brush_size(&mut self, delta: f32) {
        self.brush_size = adjusted_brush_size(self.brush_size, delta);
    }

    fn jump_to_target_depth(&mut self) {
        let target = clamp_quick_depth(self.depth_jump_target);
        self.depth_jump_target = target;
        if target == self.camera.depth {
            self.status_message = format!("Already at depth {target}");
            return;
        }
        self.camera.jump_to_depth(target);
        self.mark_zoom_changed();
        self.status_message = format!("Depth {target}");
        self.log_session_event_with(
            "navigation",
            json!({ "navigation": { "kind": "depth_jump" } }),
        );
    }

    fn jump_to_lateral_target(&mut self) {
        let Some(tile_x) = parse_lateral_coordinate(&self.lateral_jump_x) else {
            self.status_message = "Invalid Tile X".to_owned();
            return;
        };
        let Some(tile_y) = parse_lateral_coordinate(&self.lateral_jump_y) else {
            self.status_message = "Invalid Tile Y".to_owned();
            return;
        };
        self.camera.tile_x = tile_x;
        self.camera.tile_y = tile_y;
        self.mark_zoom_changed();
        self.status_message = "Tile position updated".to_owned();
        self.log_session_event_with(
            "navigation",
            json!({ "navigation": { "kind": "tile_jump" } }),
        );
    }

    fn jump_to_lateral_origin(&mut self) {
        self.camera.tile_x = 0.into();
        self.camera.tile_y = 0.into();
        self.camera.local_x = 0.5;
        self.camera.local_y = 0.5;
        self.lateral_jump_x = "0".to_owned();
        self.lateral_jump_y = "0".to_owned();
        self.mark_zoom_changed();
        self.status_message = "Tile origin".to_owned();
        self.log_session_event_with("navigation", json!({ "navigation": { "kind": "origin" } }));
    }

    fn mark_zoom_changed(&mut self) {
        if self.selection_capture_camera_depth != self.camera.depth {
            self.selection_capture_camera_depth = self.camera.depth;
            self.clear_object_selection_state();
        }
        self.depth_jump_target = self.camera.depth;
        self.hovered_operation_id = None;
        self.selection_hover_position = None;
        self.last_zoom_change = Some(Instant::now());
        self.visible_tile_generation = self.visible_tile_generation.saturating_add(1);
        self.tile_scheduler
            .set_generation(self.visible_tile_generation);
        self.pending_tiles.clear();
        if self.session_logger.level() == SessionLoggingLevel::Detailed {
            self.log_session_event_with(
                "tile_generation",
                json!({ "tiles": { "reason": "navigation" } }),
            );
        }
    }

    fn zoom_tile_requests_deferred(&self) -> bool {
        zoom_tile_requests_deferred_since(
            self.last_zoom_change,
            Instant::now(),
            self.settings.fast_zoom_tile_settle_interval(),
        )
    }

    fn save_settings(&mut self) {
        match self.settings.save(&self.settings_path) {
            Ok(()) => {
                self.status_message = "Settings saved".to_owned();
                self.log_session_event("settings_saved");
            }
            Err(error) => self.status_message = format!("Settings failed: {error:#}"),
        }
    }

    fn apply_storage_commit_mode(&mut self) {
        match self
            .document
            .set_storage_commit_mode(self.settings.storage_commit_mode)
        {
            Ok(()) => {
                self.log_session_event_with(
                    "settings_storage_commit_mode_changed",
                    json!({ "context": {
                        "storage_commit_mode": self.settings.storage_commit_mode.label(),
                        "sqlite_synchronous": self.settings.storage_commit_mode.sqlite_synchronous(),
                    } }),
                );
            }
            Err(error) => {
                self.status_message = format!("Storage commit setting failed: {error:#}");
            }
        }
    }

    fn compact_older_objects_now(&mut self) {
        let Some(keep_latest) = self.settings.object_compaction_limit.keep_latest() else {
            self.status_message = "Object compaction is Unlimited".to_owned();
            return;
        };
        match self.document.compact_older_operations(keep_latest) {
            Ok(0) => {
                self.status_message =
                    format!("No old object groups above Keep latest {keep_latest}");
            }
            Ok(count) => {
                self.clear_object_selection_state();
                self.clear_area_selection_state();
                self.selection_clipboard.paste_count = 0;
                self.invalidate_tile_rendering();
                self.status_message = format!("Compacted {count} old object groups");
                self.log_session_event_with(
                    "compact_older",
                    json!({ "document": { "compacted_groups": count } }),
                );
            }
            Err(error) => {
                self.status_message = format!("Compact older failed: {error:#}");
            }
        }
    }

    fn recreate_tile_scheduler(&mut self) {
        match TileCache::with_options(self.document.root(), self.settings.tile_cache_options()) {
            Ok(cache) => {
                self.tile_scheduler = TileScheduler::new(cache, self.settings.tile_worker_count);
                self.invalidate_tile_rendering();
                self.log_session_event_with(
                    "tile_generation",
                    json!({ "tiles": { "reason": "scheduler_recreated" } }),
                );
            }
            Err(error) => self.status_message = format!("Tile cache failed: {error:#}"),
        }
    }

    fn invalidate_tile_rendering(&mut self) {
        self.tile_textures.clear();
        self.pending_tiles.clear();
        self.invalidate_projection_caches();
        self.visible_tile_generation = self.visible_tile_generation.saturating_add(1);
        self.visible_tile_set.clear();
        self.tile_scheduler
            .set_generation(self.visible_tile_generation);
        self.log_session_event_with(
            "tile_generation",
            json!({ "tiles": { "reason": "invalidate_rendering" } }),
        );
    }

    fn invalidate_projection_caches(&mut self) {
        self.fallback_renderer.clear();
    }

    fn begin_new_document_revision(&mut self) {
        self.pending_tiles.clear();
        self.visible_tile_generation = self.visible_tile_generation.saturating_add(1);
        self.tile_scheduler
            .set_generation(self.visible_tile_generation);
        self.log_session_event_with(
            "tile_generation",
            json!({ "tiles": { "reason": "document_revision" } }),
        );
    }

    fn begin_new_document_revision_for_layer(&mut self, layer_id: Uuid) {
        if self.document.is_top_layer(layer_id) {
            self.begin_new_document_revision();
        } else {
            self.invalidate_tile_rendering();
        }
    }

    fn apply_tile_generation_pause(&mut self) {
        self.pending_tiles.clear();
        self.visible_tile_generation = self.visible_tile_generation.saturating_add(1);
        self.tile_scheduler
            .set_generation(self.visible_tile_generation);
        if self.settings.pause_tile_generation {
            self.status_message = "Tile generation paused".to_owned();
            self.log_session_event("tile_pause");
        } else if self.automatic_tile_generation_pause {
            self.status_message = "Tile generation paused while drawing".to_owned();
            self.log_session_event("tile_pause");
        } else {
            self.visible_tile_set.clear();
            self.status_message = "Tile generation resumed".to_owned();
            self.log_session_event("tile_resume");
        }
    }

    fn apply_pause_while_drawing_setting(&mut self) {
        if self.settings.pause_tile_generation_while_drawing && self.draft.is_some() {
            self.begin_drawing_tile_pause();
        } else {
            self.end_drawing_tile_pause();
        }
    }

    fn begin_drawing_tile_pause(&mut self) {
        if !self.settings.pause_tile_generation_while_drawing
            || self.automatic_tile_generation_pause
        {
            return;
        }
        self.automatic_tile_generation_pause = true;
        self.pending_tiles.clear();
        self.visible_tile_generation = self.visible_tile_generation.saturating_add(1);
        self.tile_scheduler
            .set_generation(self.visible_tile_generation);
        self.log_session_event("tile_pause");
    }

    fn end_drawing_tile_pause(&mut self) {
        if !self.automatic_tile_generation_pause {
            return;
        }
        self.automatic_tile_generation_pause = false;
        self.visible_tile_set.clear();
        self.log_session_event("tile_resume");
    }
}

fn tile_generation_is_paused(manual_pause: bool, drawing_pause: bool) -> bool {
    manual_pause || drawing_pause
}

fn tile_fallback_mode_label(mode: TileFallbackMode) -> &'static str {
    match mode {
        TileFallbackMode::Current => "current",
        TileFallbackMode::OverlayAfter(_) => "overlay_after",
        TileFallbackMode::Full => "full",
    }
}

fn depth_render_mode(
    visible_mode: VisibleDepthMode,
    distant_tiles_enabled: bool,
) -> DepthRenderMode {
    match visible_mode {
        VisibleDepthMode::Empty => DepthRenderMode::Empty,
        VisibleDepthMode::Distant if distant_tiles_enabled => DepthRenderMode::DistantTile,
        VisibleDepthMode::Near | VisibleDepthMode::Distant => DepthRenderMode::VectorsNear,
    }
}

fn depth_render_mode_label(mode: DepthRenderMode) -> &'static str {
    match mode {
        DepthRenderMode::Empty => "empty",
        DepthRenderMode::VectorsNear => "vectors_near",
        DepthRenderMode::DistantTile => "distant_tile",
    }
}

fn merge_json_objects(base: Value, extra: Value) -> Value {
    match (base, extra) {
        (Value::Object(mut base), Value::Object(extra)) => {
            for (key, value) in extra {
                let merged = if let Some(base) = base.remove(&key) {
                    merge_json_objects(base, value)
                } else {
                    value
                };
                base.insert(key, merged);
            }
            Value::Object(base)
        }
        (_, extra) => extra,
    }
}

fn fill_fallback_render_points(points: &[Pos2], max_points: usize) -> Vec<Pos2> {
    if points.len() <= max_points || max_points < 3 {
        return points.to_vec();
    }
    let target_len = max_points.max(3);
    let mut sampled = Vec::with_capacity(target_len);
    for output_index in 0..target_len {
        let source_index = output_index * points.len() / target_len;
        let point = points[source_index];
        if sampled.last() != Some(&point) {
            sampled.push(point);
        }
    }
    sampled
}

fn area_saved_fallback_render_points(
    points: &[Pos2],
    max_points: usize,
    smoothing_passes: usize,
) -> Vec<Pos2> {
    let points = fill_fallback_render_points(points, max_points);
    smooth_pos2_saved_fallback(&points, smoothing_passes, true)
}

fn smooth_pos2_draft(points: &[Pos2], level: usize) -> Vec<Pos2> {
    let tuples: Vec<_> = points.iter().map(|point| (point.x, point.y)).collect();
    smooth_stroke_points(&tuples, level)
        .into_iter()
        .map(|(x, y)| Pos2::new(x, y))
        .collect()
}

fn smooth_pos2_saved_fallback(points: &[Pos2], level: usize, closed: bool) -> Vec<Pos2> {
    if level == 0 || points.len() > MAX_FALLBACK_SMOOTHING_INPUT_POINTS {
        return points.to_vec();
    }
    let tuples: Vec<_> = points.iter().map(|point| (point.x, point.y)).collect();
    let smoothed = if closed {
        smooth_closed_points_stable(&tuples, level)
    } else {
        smooth_stroke_points_stable(&tuples, level)
    };
    if smoothed.len() > MAX_FALLBACK_SMOOTHING_OUTPUT_POINTS {
        return points.to_vec();
    }
    smoothed.into_iter().map(|(x, y)| Pos2::new(x, y)).collect()
}

fn clip_pos2_polyline(points: &[Pos2], rect: Rect) -> Vec<Vec<Pos2>> {
    let tuples: Vec<_> = points.iter().map(|point| (point.x, point.y)).collect();
    clip_polyline_to_rect(
        &tuples,
        GeometryClipRect::new(rect.min.x, rect.min.y, rect.max.x, rect.max.y),
    )
    .into_iter()
    .map(|run| run.into_iter().map(|(x, y)| Pos2::new(x, y)).collect())
    .collect()
}

fn clip_pos2_polygon(points: &[Pos2], rect: Rect) -> Vec<Pos2> {
    let tuples: Vec<_> = points.iter().map(|point| (point.x, point.y)).collect();
    clip_polygon_to_rect(
        &tuples,
        GeometryClipRect::new(rect.min.x, rect.min.y, rect.max.x, rect.max.y),
    )
    .into_iter()
    .map(|(x, y)| Pos2::new(x, y))
    .collect()
}

fn should_paint_fill_fallback(
    operation: &EditOperation,
    camera_depth: i64,
    screen_point_count: usize,
    settings: &AppSettings,
) -> bool {
    screen_point_count <= settings.fill_fallback_max_points
        && camera_depth.saturating_sub(operation.native_depth).abs()
            <= settings.fill_fallback_max_depth_delta
}

fn paint_fill_scanlines(
    painter: &Painter,
    clip_rect: Rect,
    points: &[Pos2],
    color: Color32,
) -> usize {
    paint_fill_group_scanlines(painter, clip_rect, &[points], color)
}

fn paint_fill_group_scanlines(
    painter: &Painter,
    clip_rect: Rect,
    polygons: &[&[Pos2]],
    color: Color32,
) -> usize {
    if polygons.iter().all(|points| points.len() < 3) {
        return 0;
    }
    let min_y = polygons
        .iter()
        .flat_map(|points| points.iter().map(|point| point.y))
        .fold(f32::INFINITY, f32::min)
        .floor()
        .max(clip_rect.top())
        .max(i32::MIN as f32) as i32;
    let max_y = polygons
        .iter()
        .flat_map(|points| points.iter().map(|point| point.y))
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil()
        .min(clip_rect.bottom())
        .min(i32::MAX as f32) as i32;
    if min_y > max_y {
        return 0;
    }

    let mut intersections = Vec::new();
    let mut spans = Vec::new();
    let mut shape_count = 0usize;
    for y in min_y..=max_y {
        let scan_y = y as f32 + 0.5;
        spans.clear();
        for points in polygons.iter().copied().filter(|points| points.len() >= 3) {
            intersections.clear();
            let mut previous = points.len() - 1;
            for current in 0..points.len() {
                let a = points[current];
                let b = points[previous];
                if ((a.y > scan_y) != (b.y > scan_y)) && (b.y - a.y).abs() > f32::EPSILON {
                    let t = (scan_y - a.y) / (b.y - a.y);
                    let x = a.x + t * (b.x - a.x);
                    if x.is_finite() {
                        intersections.push(x);
                    }
                }
                previous = current;
            }
            intersections.sort_by(|a, b| a.total_cmp(b));
            spans.extend(intersections.chunks_exact(2).map(|span| (span[0], span[1])));
        }
        spans.sort_by(|left, right| left.0.total_cmp(&right.0));

        let mut merged = Vec::<(f32, f32)>::new();
        for (left, right) in spans.iter().copied() {
            if let Some((_, merged_right)) = merged.last_mut()
                && left <= *merged_right + 1.0e-3
            {
                *merged_right = merged_right.max(right);
            } else {
                merged.push((left, right));
            }
        }

        for (left, right) in merged {
            let left = left.max(clip_rect.left());
            let right = right.min(clip_rect.right());
            if right > left {
                painter.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(left, y as f32 - 0.5),
                        Pos2::new(right, y as f32 + 1.5),
                    ),
                    0.0,
                    color,
                );
                shape_count = shape_count.saturating_add(1);
            }
        }
    }
    shape_count
}

fn help_content(ui: &mut egui::Ui, library: &HelpLibrary, selected_language_id: &mut String) {
    if library.catalog(selected_language_id).is_none() {
        *selected_language_id = library.preferred_language_id().to_owned();
    }

    ui.horizontal_wrapped(|ui| {
        for catalog in library.catalogs() {
            if ui
                .selectable_label(selected_language_id == &catalog.id, &catalog.tab_label)
                .clicked()
            {
                selected_language_id.clone_from(&catalog.id);
            }
        }
    });
    ui.separator();

    let Some(catalog) = library.catalog(selected_language_id) else {
        ui.label("Help is unavailable.");
        return;
    };
    ui.heading(&catalog.title);
    ui.small(format!("EndlessSketch {}", env!("CARGO_PKG_VERSION")));
    ui.add(egui::Label::new(&catalog.intro).wrap());
    if !library.warnings().is_empty() {
        let warning_title = if selected_language_id == "ru" {
            "Ошибки файлов перевода"
        } else {
            "Translation file warnings"
        };
        egui::CollapsingHeader::new(warning_title).show(ui, |ui| {
            for warning in library.warnings() {
                ui.colored_label(Color32::YELLOW, warning);
            }
        });
    }
    ui.add_space(4.0);

    egui::ScrollArea::vertical()
        .id_salt("file_help_scroll")
        .max_height(520.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (section_index, section) in catalog.sections.iter().enumerate() {
                egui::CollapsingHeader::new(&section.title)
                    .default_open(section_index == 0)
                    .show(ui, |ui| {
                        for item in &section.items {
                            ui.label(egui::RichText::new(&item.name).strong());
                            ui.add(egui::Label::new(&item.description).wrap());
                            ui.add_space(6.0);
                        }
                    });
            }
        });
}

fn navigation_coordinate_row(ui: &mut egui::Ui, label: &str, compact: &str, exact: &str) {
    ui.horizontal(|ui| {
        ui.label(label);
        if compact != exact {
            ui.small(format!("({compact})"));
        }
    });
    egui::ScrollArea::horizontal()
        .id_salt(("navigation_coordinate", label))
        .max_height(24.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(egui::RichText::new(exact).monospace())
                    .selectable(true)
                    .sense(Sense::click()),
            );
        });
}

#[allow(clippy::too_many_arguments)]
fn canvas_overlay_text(
    settings: &AppSettings,
    camera: &CameraAddress,
    operation_count: usize,
    performance: &str,
    status: &str,
    tile_state: &str,
    tile_x: &str,
    tile_y: &str,
) -> Option<String> {
    if !settings.overlay_enabled {
        return None;
    }

    let mut summary = Vec::new();
    if settings.overlay_show_depth {
        summary.push(format!("depth {}", camera.depth));
    }
    if settings.overlay_show_zoom {
        summary.push(format!("zoom {:.3}x", camera.zoom));
    }
    if settings.overlay_show_operation_count {
        summary.push(format!("{operation_count} ops"));
    }
    if settings.overlay_show_performance {
        summary.push(performance.to_owned());
    }
    if settings.overlay_show_status && !status.is_empty() {
        summary.push(status.to_owned());
    }
    if settings.overlay_show_tile_state && !tile_state.is_empty() {
        summary.push(tile_state.to_owned());
    }

    let mut lines = Vec::new();
    if !summary.is_empty() {
        lines.push(summary.join("   "));
    }
    if settings.overlay_show_tile_coordinates {
        lines.push(format!("tile X {tile_x}"));
        lines.push(format!("tile Y {tile_y}"));
    }
    if settings.overlay_show_local_coordinates {
        lines.push(format!(
            "local X {:.6}   Y {:.6}",
            camera.local_x, camera.local_y
        ));
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn stroke_fallback_segmented_point_limit(
    mode: StrokeFallbackJoinMode,
    auto_fast_path: bool,
) -> Option<usize> {
    match mode {
        StrokeFallbackJoinMode::Auto if auto_fast_path => None,
        StrokeFallbackJoinMode::Auto => Some(AUTO_SEGMENTED_FALLBACK_POINTS),
        StrokeFallbackJoinMode::Quality => Some(QUALITY_SEGMENTED_FALLBACK_POINTS),
        StrokeFallbackJoinMode::Performance => None,
    }
}

fn stroke_fallback_fast_endpoint_caps(mode: StrokeFallbackJoinMode, auto_fast_path: bool) -> bool {
    !matches!(
        (mode, auto_fast_path),
        (StrokeFallbackJoinMode::Performance, _) | (StrokeFallbackJoinMode::Auto, true)
    )
}

fn stroke_fallback_smoothing_passes(
    mode: StrokeFallbackJoinMode,
    auto_fast_path: bool,
    configured_passes: u8,
) -> usize {
    match (mode, auto_fast_path) {
        (StrokeFallbackJoinMode::Performance, _) | (StrokeFallbackJoinMode::Auto, true) => 0,
        _ => usize::from(configured_passes),
    }
}

fn auto_fast_stroke_fallback_active(
    interaction_active: bool,
    tile_jobs_pending: bool,
    _tile_generation_paused: bool,
) -> bool {
    interaction_active || tile_jobs_pending
}

fn large_selection_fast_overlay_active(
    selected_count: usize,
    interaction_active: bool,
    selection_transform_active: bool,
) -> bool {
    interaction_active
        && !selection_transform_active
        && selected_count >= LARGE_SELECTION_FAST_OVERLAY_THRESHOLD
}

fn selection_bounds_signature(selected: &HashSet<Uuid>) -> SelectionBoundsSignature {
    let mut xor = 0u128;
    let mut sum = 0u128;
    for id in selected {
        let value = u128::from_be_bytes(*id.as_bytes());
        xor ^= value;
        sum = sum.wrapping_add(value);
    }
    SelectionBoundsSignature {
        len: selected.len(),
        xor,
        sum,
    }
}

fn canvas_axis_less(
    left_tile: &BigInt,
    left_local: f64,
    right_tile: &BigInt,
    right_local: f64,
) -> bool {
    left_tile < right_tile || (left_tile == right_tile && left_local < right_local)
}

fn selection_overlay_width_expansion(
    width_hints: &[SelectionOverlayWidthHint],
    target_depth: i64,
    target_zoom: f64,
) -> f32 {
    width_hints
        .iter()
        .map(|hint| {
            let delta = target_depth
                .saturating_sub(hint.native_depth)
                .clamp(-40, 40) as i32;
            let depth_scale = (crate::coords::DEPTH_RATIO as f64).powi(delta);
            (hint.width_px as f64 * target_zoom * depth_scale / hint.native_zoom.max(1e-12))
                .clamp(0.35, 100_000.0) as f32
        })
        .fold(2.0f32, f32::max)
        * 0.5
        + 2.0
}

fn fallback_operation_limit(
    settings: &AppSettings,
    fallback_mode: TileFallbackMode,
) -> Option<usize> {
    if fallback_mode == TileFallbackMode::Current || settings.saved_fallback_operation_limit == 0 {
        None
    } else {
        Some(settings.saved_fallback_operation_limit)
    }
}

fn fallback_operation_skip_count(operation_count: usize, operation_limit: Option<usize>) -> usize {
    operation_limit
        .map(|limit| operation_count.saturating_sub(limit))
        .unwrap_or(0)
}

fn paint_stroke_fallback(
    painter: &Painter,
    points: &[Pos2],
    width: f32,
    color: Color32,
    segmented_point_limit: Option<usize>,
    fast_endpoint_caps: bool,
) -> FallbackFrameStats {
    let (shapes, stats) = stroke_fallback_shapes(
        points,
        width,
        color,
        segmented_point_limit,
        fast_endpoint_caps,
    );
    painter.extend(shapes);
    stats
}

fn consume_segmented_fallback_budget(
    point_count: usize,
    segmented_point_limit: Option<usize>,
    remaining_shape_budget: &mut usize,
) -> (Option<usize>, bool) {
    if segmented_point_limit.is_none_or(|limit| point_count > limit) {
        return (segmented_point_limit, false);
    }
    let required_shapes = segmented_fallback_shape_count(point_count);
    if required_shapes <= *remaining_shape_budget {
        *remaining_shape_budget -= required_shapes;
        (segmented_point_limit, false)
    } else {
        (None, true)
    }
}

fn segmented_fallback_shape_count(point_count: usize) -> usize {
    point_count.saturating_mul(2).saturating_sub(1)
}

fn stroke_fallback_shapes(
    points: &[Pos2],
    width: f32,
    color: Color32,
    segmented_point_limit: Option<usize>,
    fast_endpoint_caps: bool,
) -> (Vec<egui::Shape>, FallbackFrameStats) {
    if points.len() < 2 {
        return (Vec::new(), FallbackFrameStats::default());
    }
    debug_assert_eq!(color.a(), u8::MAX);
    let radius = width * 0.5;
    if points.len() == 2 && points[0].distance(points[1]) <= f32::EPSILON {
        return (
            vec![egui::Shape::circle_filled(points[0], radius, color)],
            FallbackFrameStats {
                stroke_shape_count: 1,
                fast_strokes: 1,
                ..FallbackFrameStats::default()
            },
        );
    }
    if segmented_point_limit.is_some_and(|limit| points.len() <= limit) {
        let mut shapes = Vec::with_capacity(segmented_fallback_shape_count(points.len()));
        for segment in points.windows(2) {
            if segment[0].distance(segment[1]) > f32::EPSILON {
                shapes.push(egui::Shape::line_segment(
                    [segment[0], segment[1]],
                    Stroke::new(width, color),
                ));
            }
        }
        for &point in points {
            shapes.push(egui::Shape::circle_filled(point, radius, color));
        }
        let stroke_shape_count = shapes.len();
        return (
            shapes,
            FallbackFrameStats {
                stroke_shape_count,
                segmented_strokes: 1,
                ..FallbackFrameStats::default()
            },
        );
    }
    let start = points[0];
    let end = *points.last().expect("stroke has at least two points");
    let mut shapes = vec![egui::Shape::line(
        points.to_vec(),
        Stroke::new(width, color),
    )];
    if fast_endpoint_caps {
        shapes.push(egui::Shape::circle_filled(start, radius, color));
        shapes.push(egui::Shape::circle_filled(end, radius, color));
    }
    let stroke_shape_count = shapes.len();
    (
        shapes,
        FallbackFrameStats {
            stroke_shape_count,
            fast_strokes: 1,
            ..FallbackFrameStats::default()
        },
    )
}

impl eframe::App for EndlessSketchApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let context = ui.ctx().clone();
        let window = native_window(frame);
        egui::Panel::top("toolbar").show_inside(ui, |ui| self.toolbar(ui));
        self.file_window(&context);
        self.navigation_window(&context);
        self.bookmark_window(&context);
        self.settings_window(&context);
        self.layers_window(&context);
        self.palette_window(&context);
        self.layer_delete_confirmation(&context);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| self.canvas(ui, &context, window));
        if let Some(interval) = self.repaint_interval() {
            context.request_repaint_after(interval);
        }
    }

    fn on_exit(&mut self) {
        self.log_session_event("shutdown");
        if let Some(draft) = &self.draft {
            let _ = self.document.save_draft(draft);
        }
        if let Err(error) = self.document.checkpoint() {
            let message = format!("checkpoint failed during shutdown: {error:#}");
            log::error!("{message}");
            self.session_logger
                .log_error_event(&message, self.session_log_fields(None));
        }
        self.session_logger.flush_navigation();
        let _ = self.session_logger.flush();
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        if let Err(error) = self.document.checkpoint() {
            let message = format!("periodic checkpoint failed: {error:#}");
            log::error!("{message}");
            self.session_logger
                .log_error_event(&message, self.session_log_fields(None));
        }
        self.session_logger.flush_navigation();
        let _ = self.session_logger.flush();
    }
}

impl EndlessSketchApp {
    fn repaint_interval(&self) -> Option<Duration> {
        repaint_interval_for_work(
            self.draft.is_some(),
            !self.pending_tiles.is_empty(),
            self.zoom_tile_requests_deferred(),
            self.tile_rebuild_is_deferred(),
            self.settings.preview_repaint_interval(),
        )
    }
}

fn repaint_interval_for_work(
    has_draft: bool,
    has_pending_tiles: bool,
    zoom_requests_deferred: bool,
    rebuild_requests_deferred: bool,
    preview_interval: Duration,
) -> Option<Duration> {
    if has_draft || zoom_requests_deferred || rebuild_requests_deferred {
        Some(preview_interval)
    } else if has_pending_tiles {
        Some(preview_interval.max(TILE_POLL_REPAINT_INTERVAL))
    } else {
        None
    }
}

fn should_paint_live_draft(deferred_drawing_preview: bool) -> bool {
    !deferred_drawing_preview
}

fn screen_to_canvas_for_camera(camera: &CameraAddress, position: Pos2, rect: Rect) -> CanvasPoint {
    camera.screen_to_canvas(
        (position.x - rect.left()) as f64,
        (position.y - rect.top()) as f64,
        rect.width() as f64,
        rect.height() as f64,
    )
}

fn canvas_point_to_position(
    camera: &CameraAddress,
    point: &CanvasPoint,
    rect: Rect,
) -> Option<Pos2> {
    let (x, y) = camera.canvas_to_screen(point, rect.width() as f64, rect.height() as f64)?;
    let position = Pos2::new(rect.left() + x as f32, rect.top() + y as f32);
    (position.x.is_finite() && position.y.is_finite()).then_some(position)
}

fn operation_bounds_may_intersect_screen_rect(
    bounds: &OperationBounds,
    camera: &CameraAddress,
    rect: Rect,
    width: f32,
) -> bool {
    let corners = [
        CanvasPoint::new(
            bounds.depth,
            bounds.min_x.clone(),
            bounds.min_y.clone(),
            0.0,
            0.0,
        ),
        CanvasPoint::new(
            bounds.depth,
            bounds.max_x.clone(),
            bounds.min_y.clone(),
            1.0,
            0.0,
        ),
        CanvasPoint::new(
            bounds.depth,
            bounds.max_x.clone(),
            bounds.max_y.clone(),
            1.0,
            1.0,
        ),
        CanvasPoint::new(
            bounds.depth,
            bounds.min_x.clone(),
            bounds.max_y.clone(),
            0.0,
            1.0,
        ),
    ];
    let mut projected = None::<Rect>;
    for corner in corners {
        let Some((x, y)) =
            camera.canvas_to_screen(&corner, rect.width() as f64, rect.height() as f64)
        else {
            return true;
        };
        if !x.is_finite() || !y.is_finite() {
            return true;
        }
        let position = Pos2::new(rect.left() + x as f32, rect.top() + y as f32);
        let point_rect = Rect::from_min_size(position, egui::Vec2::ZERO);
        projected = Some(projected.map_or(point_rect, |bounds| bounds.union(point_rect)));
    }
    projected.is_some_and(|bounds| bounds.expand(width.max(1.0) * 0.5 + 2.0).intersects(rect))
}

fn is_space_pan_active(ui: &egui::Ui) -> bool {
    ui.input(|input| {
        space_pan_requested(
            input.key_down(egui::Key::Space),
            input.pointer.primary_down(),
        )
    })
}

fn space_pan_requested(space_down: bool, primary_down: bool) -> bool {
    space_down && primary_down
}

fn is_z_zoom_active(ui: &egui::Ui) -> bool {
    ui.input(|input| z_zoom_requested(input.key_down(egui::Key::Z), input.pointer.primary_down()))
}

fn z_zoom_requested(z_down: bool, primary_down: bool) -> bool {
    z_down && primary_down
}

fn z_drag_zoom_factor(delta_y: f32) -> f64 {
    (-(delta_y as f64) * ZOOM_DRAG_SENSITIVITY)
        .exp()
        .clamp(MIN_ZOOM_DRAG_FACTOR, MAX_ZOOM_DRAG_FACTOR)
}

fn zoom_tile_requests_deferred_since(
    last_zoom_change: Option<Instant>,
    now: Instant,
    settle_interval: Duration,
) -> bool {
    last_zoom_change
        .is_some_and(|last_change| now.saturating_duration_since(last_change) < settle_interval)
}

fn tile_rebuild_deferred_since(
    last_interaction: Option<Instant>,
    now: Instant,
    idle_interval: Duration,
) -> bool {
    last_interaction.is_some_and(|last| now.saturating_duration_since(last) < idle_interval)
}

fn tile_keys_for_view(
    camera: &CameraAddress,
    rect: Rect,
    lod: u8,
    prefetch_radius: u8,
) -> Vec<TileKey> {
    let half_width = rect.width() as f64 / (2.0 * crate::coords::TILE_PIXELS * camera.zoom);
    let half_height = rect.height() as f64 / (2.0 * crate::coords::TILE_PIXELS * camera.zoom);
    let min_x = (camera.local_x - half_width).floor() as i64;
    let max_x = (camera.local_x + half_width).ceil() as i64;
    let min_y = (camera.local_y - half_height).floor() as i64;
    let max_y = (camera.local_y + half_height).ceil() as i64;
    let radius = prefetch_radius.min(MAX_TILE_PREFETCH_RADIUS) as i64;
    let width = max_x.saturating_sub(min_x).saturating_add(1);
    let height = max_y.saturating_sub(min_y).saturating_add(1);
    let expanded_width = width.saturating_add(radius.saturating_mul(2));
    let expanded_height = height.saturating_add(radius.saturating_mul(2));
    let capacity = expanded_width.saturating_mul(expanded_height).max(0) as usize;
    let mut keys = Vec::with_capacity(capacity);

    for ring in 0..=radius {
        let ring_min_x = min_x.saturating_sub(ring);
        let ring_max_x = max_x.saturating_add(ring);
        let ring_min_y = min_y.saturating_sub(ring);
        let ring_max_y = max_y.saturating_add(ring);
        for y in ring_min_y..=ring_max_y {
            for x in ring_min_x..=ring_max_x {
                if ring > 0
                    && x != ring_min_x
                    && x != ring_max_x
                    && y != ring_min_y
                    && y != ring_max_y
                {
                    continue;
                }
                keys.push(TileKey {
                    depth: camera.depth,
                    x: &camera.tile_x + x,
                    y: &camera.tile_y + y,
                    lod,
                });
            }
        }
    }
    keys
}

fn tile_request_batch(missing: impl IntoIterator<Item = TileKey>) -> Vec<TileKey> {
    missing
        .into_iter()
        .take(MAX_TILE_JOBS_QUEUED_PER_FRAME)
        .collect()
}

fn tile_display_rects(position: Pos2, zoom: f32, lod: u8) -> (Rect, Rect) {
    let destination_size = TILE_SIZE as f32 * zoom;
    let destination = Rect::from_min_size(position, egui::vec2(destination_size, destination_size));

    let content_pixels = tile_resolution(lod) as f32;
    let texture_pixels = content_pixels + TILE_BLEED as f32 * 2.0;
    let bleed_uv = TILE_BLEED as f32 / texture_pixels;
    let source = Rect::from_min_max(
        Pos2::new(bleed_uv, bleed_uv),
        Pos2::new(1.0 - bleed_uv, 1.0 - bleed_uv),
    );

    (destination, source)
}

fn tile_fallback_mode(
    current_revision: u64,
    snapshots: impl IntoIterator<Item = Option<(u64, i64)>>,
) -> TileFallbackMode {
    let mut oldest_retained_sequence = None;
    for snapshot in snapshots {
        let Some((revision, sequence)) = snapshot else {
            return TileFallbackMode::Full;
        };
        if revision > current_revision {
            return TileFallbackMode::Full;
        }
        if revision < current_revision {
            oldest_retained_sequence =
                Some(oldest_retained_sequence.map_or(sequence, |oldest: i64| oldest.min(sequence)));
        }
    }
    oldest_retained_sequence.map_or(TileFallbackMode::Current, TileFallbackMode::OverlayAfter)
}

fn incremental_paint_operations(
    snapshot_sequence: i64,
    operations: &[EditOperation],
) -> Option<Vec<EditOperation>> {
    let Some(first_newer) = operations
        .iter()
        .position(|operation| operation.sequence > snapshot_sequence)
    else {
        return Some(Vec::new());
    };
    let newer = &operations[first_newer..];
    if newer
        .iter()
        .any(|operation| operation.sequence <= snapshot_sequence)
    {
        return None;
    }
    if newer
        .iter()
        .all(|operation| operation.kind == EditKind::Paint && operation.color.is_opaque())
    {
        Some(newer.to_vec())
    } else {
        None
    }
}

fn primary_pointer_positions(
    events: &[egui::Event],
    primary_down_after_events: bool,
) -> (Option<Pos2>, Vec<Pos2>, bool) {
    if let Some((press_position, drag_positions)) =
        primary_touch_positions(events, primary_down_after_events)
    {
        return (press_position, drag_positions, true);
    }
    let mut primary_down = primary_down_after_events;
    for event in events.iter().rev() {
        if let egui::Event::PointerButton {
            button: PointerButton::Primary,
            pressed,
            ..
        } = event
        {
            primary_down = !pressed;
        }
    }

    let mut press_position = None;
    let mut drag_positions = Vec::new();
    for event in events {
        match event {
            egui::Event::PointerMoved(position) if primary_down => {
                drag_positions.push(*position);
            }
            egui::Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                ..
            } => {
                if *pressed {
                    press_position = Some(*pos);
                } else if primary_down {
                    drag_positions.push(*pos);
                }
                primary_down = *pressed;
            }
            _ => {}
        }
    }

    (press_position, drag_positions, false)
}

fn primary_touch_positions(
    events: &[egui::Event],
    primary_down_after_events: bool,
) -> Option<(Option<Pos2>, Vec<Pos2>)> {
    let touch_id = events.iter().find_map(|event| match event {
        egui::Event::Touch { id, .. } => Some(*id),
        _ => None,
    })?;
    let mut primary_down = primary_down_after_events;
    for event in events.iter().rev() {
        if let egui::Event::Touch { id, phase, .. } = event
            && *id == touch_id
        {
            match phase {
                egui::TouchPhase::Start => primary_down = false,
                egui::TouchPhase::End | egui::TouchPhase::Cancel => primary_down = true,
                egui::TouchPhase::Move => {}
            }
        }
    }

    let mut press_position = None;
    let mut drag_positions = Vec::new();
    for event in events {
        let egui::Event::Touch { id, phase, pos, .. } = event else {
            continue;
        };
        if *id != touch_id {
            continue;
        }
        match phase {
            egui::TouchPhase::Start => {
                press_position = Some(*pos);
                primary_down = true;
            }
            egui::TouchPhase::Move if primary_down => drag_positions.push(*pos),
            egui::TouchPhase::End if primary_down => {
                drag_positions.push(*pos);
                primary_down = false;
            }
            egui::TouchPhase::End | egui::TouchPhase::Cancel => primary_down = false,
            egui::TouchPhase::Move => {}
        }
    }
    Some((press_position, drag_positions))
}

fn append_interpolated_points(
    draft: &mut EditOperation,
    camera: &CameraAddress,
    position: Pos2,
    rect: Rect,
    settings: &AppSettings,
) {
    let Some(last) = draft.points.last() else {
        append_draft_point(
            draft,
            screen_to_canvas_for_camera(camera, position, rect),
            settings,
        );
        return;
    };
    let Some((last_x, last_y)) =
        camera.canvas_to_screen(last, rect.width() as f64, rect.height() as f64)
    else {
        append_draft_point(
            draft,
            screen_to_canvas_for_camera(camera, position, rect),
            settings,
        );
        return;
    };
    let previous = Pos2::new(rect.left() + last_x as f32, rect.top() + last_y as f32);
    let distance = previous.distance(position);
    if !distance.is_finite() || distance <= max_interpolation_gap_px(draft.kind) {
        append_draft_point(
            draft,
            screen_to_canvas_for_camera(camera, position, rect),
            settings,
        );
        return;
    }

    let step_count = interpolation_step_count(draft.kind, draft.points.len(), distance, settings);
    if step_count == 0 {
        return;
    }
    for step in 1..=step_count {
        let t = step as f32 / step_count as f32;
        let interpolated = previous.lerp(position, t);
        append_draft_point(
            draft,
            screen_to_canvas_for_camera(camera, interpolated, rect),
            settings,
        );
    }
}

fn prepare_draft_for_commit(draft: &mut EditOperation) {
    if draft.kind == EditKind::Paint && draft.points.len() == 1 {
        draft.points.push(draft.points[0].clone());
    }
    if matches!(draft.kind, EditKind::Fill | EditKind::EraseArea) && draft.points.len() >= 3 {
        let needs_close = draft.points.first() != draft.points.last();
        if needs_close {
            draft.points.push(draft.points[0].clone());
        }
    }
}

fn draft_is_committable(draft: &EditOperation) -> bool {
    if matches!(draft.kind, EditKind::Fill | EditKind::EraseArea) {
        draft.points.len() >= 4
    } else {
        draft.points.len() >= 2
    }
}

fn straight_line_requested(kind: EditKind, shift_down: bool) -> bool {
    shift_down && matches!(kind, EditKind::Paint | EditKind::Erase)
}

fn should_update_straight_endpoint(
    draft: &EditOperation,
    camera: &CameraAddress,
    position: Pos2,
    rect: Rect,
    settings: &AppSettings,
) -> bool {
    let Some(start) = draft.points.first() else {
        return true;
    };
    let Some((start_x, start_y)) =
        camera.canvas_to_screen(start, rect.width() as f64, rect.height() as f64)
    else {
        return true;
    };
    let start = Pos2::new(rect.left() + start_x as f32, rect.top() + start_y as f32);
    start.distance(position) >= input_spacing_px(draft.kind, settings)
}

fn set_straight_draft_endpoint(draft: &mut EditOperation, endpoint: CanvasPoint) {
    if draft.points.is_empty() {
        draft.points.push(endpoint);
        return;
    }
    if draft.points.len() == 1 {
        draft.points.push(endpoint);
    } else {
        draft.points.truncate(2);
        draft.points[1] = endpoint;
    }
}

fn adjusted_brush_size(current: f32, delta: f32) -> f32 {
    (current + delta).clamp(MIN_BRUSH_SIZE, MAX_BRUSH_SIZE)
}

fn redo_shortcuts() -> [egui::KeyboardShortcut; 2] {
    [
        egui::KeyboardShortcut::new(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::Z),
        egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::Y),
    ]
}

fn tool_shortcut_allowed(modifiers: egui::Modifiers) -> bool {
    !modifiers.ctrl && !modifiers.command && !modifiers.alt && !modifiers.mac_cmd
}

fn area_cycle_tool(current: ToolKind) -> ToolKind {
    match current {
        ToolKind::LassoFill => ToolKind::EraserLasso,
        ToolKind::EraserLasso => ToolKind::LassoFill,
        ToolKind::Brush | ToolKind::Eraser | ToolKind::Eyedropper | ToolKind::Selection => {
            ToolKind::LassoFill
        }
    }
}

fn rectangle_selection_mode_cycle(current: RectangleSelectionMode) -> RectangleSelectionMode {
    match current {
        RectangleSelectionMode::Inside => RectangleSelectionMode::Crossing,
        RectangleSelectionMode::Crossing => RectangleSelectionMode::Inside,
    }
}

fn selection_target_cycle(current: SelectionTargetMode) -> SelectionTargetMode {
    match current {
        SelectionTargetMode::Object => SelectionTargetMode::Area,
        SelectionTargetMode::Area => SelectionTargetMode::Object,
    }
}

fn clipboard_command_from_events(
    events: &[egui::Event],
    modifiers: egui::Modifiers,
) -> Option<ClipboardCommand> {
    events.iter().find_map(|event| match event {
        egui::Event::Copy => Some(ClipboardCommand::Copy),
        egui::Event::Cut => Some(ClipboardCommand::Cut),
        egui::Event::Paste(_) => Some(if modifiers.shift {
            ClipboardCommand::PasteInPlace
        } else {
            ClipboardCommand::Paste
        }),
        _ => None,
    })
}

#[cfg(target_os = "windows")]
fn native_key_down(virtual_key: i32) -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

    // SAFETY: GetAsyncKeyState reads process-independent keyboard state and has no pointer inputs.
    unsafe { GetAsyncKeyState(virtual_key) as u16 & 0x8000 != 0 }
}

#[cfg(not(target_os = "windows"))]
fn native_key_down(_virtual_key: i32) -> bool {
    false
}

fn surrender_canvas_keyboard_focus(
    context: &egui::Context,
    canvas_hovered: bool,
    pointer_pressed: bool,
) {
    if !canvas_hovered || !pointer_pressed {
        return;
    }
    context.memory_mut(|memory| {
        if let Some(focused) = memory.focused() {
            memory.surrender_focus(focused);
        }
    });
}

fn clamp_quick_depth(depth: i64) -> i64 {
    depth.clamp(MIN_QUICK_DEPTH, MAX_QUICK_DEPTH)
}

fn parse_lateral_coordinate(input: &str) -> Option<BigInt> {
    let input = input.trim();
    if input.is_empty() || input.len() > MAX_LATERAL_COORDINATE_DIGITS + 8 {
        return None;
    }

    let exponent_marker = input.find(['e', 'E']);
    let Some(marker) = exponent_marker else {
        let digits = input.trim_start_matches(['+', '-']);
        if digits.is_empty()
            || digits.len() > MAX_LATERAL_COORDINATE_DIGITS
            || !digits.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        return input.parse().ok();
    };
    if input[marker + 1..].contains(['e', 'E']) {
        return None;
    }

    let mantissa_text = &input[..marker];
    let exponent_text = input[marker + 1..]
        .strip_prefix('+')
        .unwrap_or(&input[marker + 1..]);
    let mantissa_digits = mantissa_text.trim_start_matches(['+', '-']);
    if mantissa_digits.is_empty()
        || !mantissa_digits.bytes().all(|byte| byte.is_ascii_digit())
        || exponent_text.is_empty()
        || !exponent_text.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let exponent: u32 = exponent_text.parse().ok()?;
    let expanded_digits = mantissa_digits.len().checked_add(exponent as usize)?;
    if expanded_digits > MAX_LATERAL_COORDINATE_DIGITS {
        return None;
    }
    let mantissa: BigInt = mantissa_text.parse().ok()?;
    if mantissa == BigInt::from(0u8) {
        return Some(mantissa);
    }
    Some(mantissa * BigInt::from(10u8).pow(exponent))
}

fn format_overlay_coordinate(value: &BigInt) -> String {
    let text = value.to_string();
    let (sign, digits) = text
        .strip_prefix('-')
        .map_or(("", text.as_str()), |digits| ("-", digits));
    if digits.len() <= MAX_EXACT_OVERLAY_COORDINATE_DIGITS {
        return text;
    }
    if digits.starts_with('1') && digits[1..].bytes().all(|byte| byte == b'0') {
        return format!("{sign}1e{}", digits.len() - 1);
    }

    let leading = &digits[..OVERLAY_COORDINATE_EDGE_DIGITS];
    let trailing = &digits[digits.len() - OVERLAY_COORDINATE_EDGE_DIGITS..];
    format!("{sign}{leading}...{trailing} ({}d)", digits.len())
}

fn point_append_decision(
    kind: EditKind,
    distance: f32,
    settings: &AppSettings,
) -> PointAppendDecision {
    if distance < input_spacing_px(kind, settings) {
        return PointAppendDecision::Skip;
    }
    if distance > max_interpolation_gap_px(kind) {
        if matches!(kind, EditKind::Paint | EditKind::Erase | EditKind::Fill) {
            PointAppendDecision::Interpolate
        } else {
            PointAppendDecision::Skip
        }
    } else {
        PointAppendDecision::Append
    }
}

fn append_draft_point(draft: &mut EditOperation, point: CanvasPoint, _settings: &AppSettings) {
    draft.points.push(point);
}

fn interpolation_step_count(
    kind: EditKind,
    _existing_points: usize,
    distance: f32,
    _settings: &AppSettings,
) -> usize {
    (distance / max_interpolation_gap_px(kind)).ceil().max(1.0) as usize
}

fn input_spacing_px(kind: EditKind, settings: &AppSettings) -> f32 {
    if kind == EditKind::Fill {
        settings.fill_input_spacing_px
    } else {
        settings.brush_input_spacing_px
    }
}

fn max_interpolation_gap_px(kind: EditKind) -> f32 {
    if kind == EditKind::Fill {
        MAX_FILL_INTERPOLATION_GAP_PX
    } else {
        MAX_BRUSH_INTERPOLATION_GAP_PX
    }
}

fn to_color32(color: Color) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r, color.g, color.b, color.a)
}

fn color_from_srgb(color: [u8; 3]) -> Color {
    Color::rgba(color[0], color[1], color[2], u8::MAX)
}

fn contrasting_text_color(color: Color) -> Color32 {
    let luminance =
        0.2126 * f32::from(color.r) + 0.7152 * f32::from(color.g) + 0.0722 * f32::from(color.b);
    if luminance > 140.0 {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

fn bookmark_edit_names(bookmarks: &[Bookmark]) -> HashMap<Uuid, String> {
    bookmarks
        .iter()
        .map(|bookmark| (bookmark.id, bookmark.name.clone()))
        .collect()
}

fn layer_operation_counts(operations: &[EditOperation]) -> HashMap<Uuid, usize> {
    let mut counts = HashMap::new();
    for operation in operations {
        *counts.entry(operation.layer_id).or_insert(0) += 1;
    }
    counts
}

fn selection_point(position: Pos2) -> SelectionPoint {
    SelectionPoint::new(position.x as f64, position.y as f64)
}

fn canvas_context_menu_kind(tool: ToolKind) -> Option<CanvasContextMenuKind> {
    match tool {
        ToolKind::Brush => Some(CanvasContextMenuKind::Brush),
        ToolKind::LassoFill => Some(CanvasContextMenuKind::Fill),
        ToolKind::Selection => Some(CanvasContextMenuKind::Selection),
        ToolKind::Eraser | ToolKind::Eyedropper | ToolKind::EraserLasso => None,
    }
}

fn selection_scale_handle_at(bounds: Rect, position: Pos2) -> Option<SelectionScaleHandle> {
    SelectionScaleHandle::ALL
        .into_iter()
        .find(|handle| handle.position(bounds).distance(position) <= SELECTION_HANDLE_HIT_RADIUS_PX)
}

fn selection_rotate_handle_position(bounds: Rect) -> Pos2 {
    bounds.center_top() + egui::vec2(0.0, -SELECTION_ROTATE_HANDLE_OFFSET_PX)
}

fn selection_rotate_handle_at(bounds: Rect, position: Pos2) -> bool {
    selection_rotate_handle_position(bounds).distance(position)
        <= SELECTION_ROTATE_HANDLE_RADIUS_PX + 3.0
}

fn selection_scale_factor(bounds: Rect, handle: SelectionScaleHandle, dragged_handle: Pos2) -> f32 {
    let pivot = handle.pivot(bounds);
    let original = handle.position(bounds) - pivot;
    let current = dragged_handle - pivot;
    let denominator = original.length_sq();
    if !denominator.is_finite() || denominator <= f32::EPSILON {
        return 1.0;
    }
    let scale = current.dot(original) / denominator;
    if scale.is_finite() {
        scale.clamp(MIN_SELECTION_SCALE, MAX_SELECTION_SCALE)
    } else {
        1.0
    }
}

fn rotate_position_around(position: Pos2, pivot: Pos2, angle: f32) -> Pos2 {
    let (sin, cos) = angle.sin_cos();
    let relative = position - pivot;
    Pos2::new(
        pivot.x + relative.x * cos - relative.y * sin,
        pivot.y + relative.x * sin + relative.y * cos,
    )
}

fn transformed_rect_bounds(bounds: Rect, mut transform: impl FnMut(Pos2) -> Pos2) -> Rect {
    let corners = [
        bounds.left_top(),
        bounds.right_top(),
        bounds.right_bottom(),
        bounds.left_bottom(),
    ];
    let mut min = Pos2::new(f32::INFINITY, f32::INFINITY);
    let mut max = Pos2::new(f32::NEG_INFINITY, f32::NEG_INFINITY);
    for corner in corners {
        let transformed = transform(corner);
        min.x = min.x.min(transformed.x);
        min.y = min.y.min(transformed.y);
        max.x = max.x.max(transformed.x);
        max.y = max.y.max(transformed.y);
    }
    Rect::from_min_max(min, max)
}

fn operation_intersects_selection(
    kind: EditKind,
    points: &[SelectionPoint],
    width: f64,
    selection: &SelectionShape,
) -> bool {
    match kind {
        EditKind::Paint | EditKind::Erase => {
            selection.intersects_polyline(points, width.max(0.0) * 0.5)
        }
        EditKind::Fill | EditKind::EraseArea | EditKind::CompactBlock => {
            selection.intersects_polygon(points)
        }
    }
}

fn fully_contained_object_operation_ids(
    operations: &[EditOperation],
    contained_operation_ids: &HashSet<Uuid>,
    layer_id: Uuid,
) -> HashSet<Uuid> {
    let mut total_counts = HashMap::<Uuid, usize>::new();
    let mut contained_counts = HashMap::<Uuid, usize>::new();
    for operation in operations
        .iter()
        .filter(|operation| operation.layer_id == layer_id)
    {
        let object_id = operation.object_group_id();
        *total_counts.entry(object_id).or_default() += 1;
        if contained_operation_ids.contains(&operation.id) {
            *contained_counts.entry(object_id).or_default() += 1;
        }
    }
    let fully_contained = contained_counts
        .into_iter()
        .filter_map(|(object_id, contained)| {
            (total_counts.get(&object_id) == Some(&contained)).then_some(object_id)
        })
        .collect::<HashSet<_>>();
    operations
        .iter()
        .filter(|operation| {
            operation.layer_id == layer_id && fully_contained.contains(&operation.object_group_id())
        })
        .map(|operation| operation.id)
        .collect()
}

fn operation_contained_by_rectangle(
    kind: EditKind,
    points: &[SelectionPoint],
    width: f64,
    selection: SelectionRect,
) -> bool {
    if points.is_empty() {
        return false;
    }
    let radius = if matches!(
        kind,
        EditKind::Fill | EditKind::EraseArea | EditKind::CompactBlock
    ) {
        0.0
    } else {
        width.max(0.0) * 0.5
    };
    points.iter().all(|point| {
        point.x - radius >= selection.min.x
            && point.x + radius <= selection.max.x
            && point.y - radius >= selection.min.y
            && point.y + radius <= selection.max.y
    })
}

fn operation_click_score(
    kind: EditKind,
    points: &[SelectionPoint],
    width: f64,
    click: SelectionPoint,
) -> Option<(f64, f64)> {
    let first = *points.first()?;
    let mut min = first;
    let mut max = first;
    for point in &points[1..] {
        min.x = min.x.min(point.x);
        min.y = min.y.min(point.y);
        max.x = max.x.max(point.x);
        max.y = max.y.max(point.y);
    }

    let radius = if matches!(
        kind,
        EditKind::Fill | EditKind::EraseArea | EditKind::CompactBlock
    ) {
        0.0
    } else {
        width.max(0.0) * 0.5
    };
    let centerline_distance = if matches!(
        kind,
        EditKind::Fill | EditKind::EraseArea | EditKind::CompactBlock
    ) {
        let polygon = SelectionShape::Lasso(points.to_vec());
        if polygon.contains_point(click) {
            0.0
        } else {
            closed_polyline_distance(click, points)
        }
    } else {
        polyline_distance(click, points)
    };
    let effective_distance = (centerline_distance - radius).max(0.0);
    if effective_distance > CLICK_SELECTION_TOLERANCE_PX {
        return None;
    }

    let width = (max.x - min.x + radius * 2.0).max(1.0);
    let height = (max.y - min.y + radius * 2.0).max(1.0);
    Some((effective_distance, width * height))
}

fn operation_pick_hit(kind: EditKind, points: &[Pos2], width: f32, position: Pos2) -> bool {
    if points.is_empty() {
        return false;
    }
    let points = points
        .iter()
        .copied()
        .map(selection_point)
        .collect::<Vec<_>>();
    let click = selection_point(position);
    match kind {
        EditKind::Paint | EditKind::Erase => {
            polyline_distance(click, &points) <= f64::from(width.max(1.0)) * 0.5
        }
        EditKind::Fill | EditKind::EraseArea | EditKind::CompactBlock => {
            points.len() >= 3 && SelectionShape::Lasso(points).contains_point(click)
        }
    }
}

fn polyline_distance(point: SelectionPoint, points: &[SelectionPoint]) -> f64 {
    if points.len() == 1 {
        return point_distance(point, points[0]);
    }
    points
        .windows(2)
        .map(|segment| point_segment_distance(point, segment[0], segment[1]))
        .fold(f64::INFINITY, f64::min)
}

fn closed_polyline_distance(point: SelectionPoint, points: &[SelectionPoint]) -> f64 {
    let open_distance = polyline_distance(point, points);
    if points.len() < 2 {
        return open_distance;
    }
    open_distance.min(point_segment_distance(
        point,
        *points.last().unwrap(),
        points[0],
    ))
}

fn point_distance(first: SelectionPoint, second: SelectionPoint) -> f64 {
    ((first.x - second.x).powi(2) + (first.y - second.y).powi(2)).sqrt()
}

fn point_segment_distance(
    point: SelectionPoint,
    start: SelectionPoint,
    end: SelectionPoint,
) -> f64 {
    let delta_x = end.x - start.x;
    let delta_y = end.y - start.y;
    let length_squared = delta_x * delta_x + delta_y * delta_y;
    if length_squared <= f64::EPSILON {
        return point_distance(point, start);
    }
    let projection = (((point.x - start.x) * delta_x + (point.y - start.y) * delta_y)
        / length_squared)
        .clamp(0.0, 1.0);
    point_distance(
        point,
        SelectionPoint::new(
            start.x + projection * delta_x,
            start.y + projection * delta_y,
        ),
    )
}

#[cfg(test)]
fn subtract_paint_sources<Mask>(
    sources: Vec<EditOperation>,
    masks: &[Mask],
    subtract: impl Fn(&EditOperation, &Mask) -> Option<Vec<Vec<CanvasPoint>>>,
) -> Option<Vec<(Uuid, Vec<Vec<CanvasPoint>>)>> {
    let mut changes = Vec::with_capacity(sources.len());
    for source in sources {
        let mut runs = vec![source.points.clone()];
        for mask in masks {
            let mut next = Vec::new();
            for points in runs {
                let mut fragment = source.clone();
                fragment.points = points;
                next.extend(subtract(&fragment, mask)?);
            }
            runs = next;
            if runs.is_empty() {
                break;
            }
        }
        changes.push((source.id, runs));
    }
    Some(changes)
}

fn subtract_eraser_sources(
    sources: &[EditOperation],
    fill_masks: &[Vec<CanvasPoint>],
    projection_camera: &CameraAddress,
    subtract_paint: impl Fn(&EditOperation) -> Result<Vec<Vec<CanvasPoint>>>,
) -> Result<Vec<(Uuid, Vec<EditOperation>)>> {
    let mut fill_mask_bounds = None::<Rect>;
    for mask in fill_masks {
        let Some(bounds) = canvas_points_screen_bounds(mask, projection_camera)? else {
            continue;
        };
        fill_mask_bounds = Some(fill_mask_bounds.map_or(bounds, |current| current.union(bounds)));
    }
    let Some(fill_mask_bounds) = fill_mask_bounds else {
        return Ok(Vec::new());
    };
    let mut results = Vec::with_capacity(sources.len());
    for source in sources {
        results.push((
            source,
            subtract_eraser_operation(
                source,
                fill_masks,
                fill_mask_bounds,
                projection_camera,
                &subtract_paint,
            )?,
        ));
    }
    let changed_fill_groups = results
        .iter()
        .filter(|(source, change)| source.kind == EditKind::Fill && change.is_some())
        .map(|(source, _)| source.object_group_id())
        .collect::<HashSet<_>>();
    let mut fill_group_ids = HashMap::new();
    let mut changes = Vec::new();
    for (source, change) in results {
        if change.is_none() && !changed_fill_groups.contains(&source.object_group_id()) {
            continue;
        }
        let mut replacements = change.unwrap_or_else(|| vec![source.clone()]);
        if source.kind == EditKind::Fill {
            let group_id = *fill_group_ids
                .entry(source.object_group_id())
                .or_insert_with(Uuid::new_v4);
            for replacement in &mut replacements {
                replacement.fill_group_id = Some(group_id);
            }
        }
        changes.push((source.id, replacements));
    }
    Ok(changes)
}

fn subtract_eraser_operation(
    source: &EditOperation,
    fill_masks: &[Vec<CanvasPoint>],
    fill_mask_bounds: Rect,
    projection_camera: &CameraAddress,
    subtract_paint: &impl Fn(&EditOperation) -> Result<Vec<Vec<CanvasPoint>>>,
) -> Result<Option<Vec<EditOperation>>> {
    if matches!(source.kind, EditKind::Paint | EditKind::Fill) {
        let Some(mut source_bounds) =
            canvas_points_screen_bounds(&source.points, projection_camera)?
        else {
            return Ok(None);
        };
        if source.kind == EditKind::Paint {
            source_bounds = source_bounds.expand(
                operation_width(source, projection_camera.depth, projection_camera.zoom) * 0.5,
            );
        }
        if !source_bounds.intersects(fill_mask_bounds) {
            return Ok(None);
        }
    }
    let fragments = match source.kind {
        EditKind::Paint => subtract_paint(source)?,
        EditKind::Fill => {
            subtract_fill_polygon_at_camera(&source.points, fill_masks, projection_camera)?
        }
        EditKind::CompactBlock => {
            let Some(compact_sources) = subtract_compact_sources(
                &source.compact_sources,
                fill_masks,
                fill_mask_bounds,
                projection_camera,
                subtract_paint,
            )?
            else {
                return Ok(None);
            };
            if compact_sources.is_empty() {
                return Ok(Some(Vec::new()));
            }
            let mut replacement = source.clone();
            replacement.compact_sources = compact_sources;
            recompute_compact_block_bounds(&mut replacement)?;
            return Ok(Some(vec![replacement]));
        }
        EditKind::Erase | EditKind::EraseArea => return Ok(None),
    };
    if fragments.len() == 1 && fragments[0] == source.points {
        return Ok(None);
    }
    Ok(Some(
        fragments
            .into_iter()
            .map(|points| {
                let mut replacement = source.clone();
                replacement.points = points;
                if replacement.kind == EditKind::Fill {
                    replacement.smooth_area = false;
                }
                replacement
            })
            .collect(),
    ))
}

fn subtract_compact_sources(
    sources: &[EditOperation],
    fill_masks: &[Vec<CanvasPoint>],
    fill_mask_bounds: Rect,
    projection_camera: &CameraAddress,
    subtract_paint: &impl Fn(&EditOperation) -> Result<Vec<Vec<CanvasPoint>>>,
) -> Result<Option<Vec<EditOperation>>> {
    let mut results = Vec::with_capacity(sources.len());
    for source in sources {
        results.push((
            source,
            subtract_eraser_operation(
                source,
                fill_masks,
                fill_mask_bounds,
                projection_camera,
                subtract_paint,
            )?,
        ));
    }
    if results.iter().all(|(_, change)| change.is_none()) {
        return Ok(None);
    }
    let changed_fill_groups = results
        .iter()
        .filter(|(source, change)| source.kind == EditKind::Fill && change.is_some())
        .map(|(source, _)| source.object_group_id())
        .collect::<HashSet<_>>();
    let mut fill_group_ids = HashMap::new();
    let mut replacements = Vec::new();
    for (source, change) in results {
        let mut source_replacements = change.unwrap_or_else(|| vec![source.clone()]);
        if source.kind == EditKind::Fill && changed_fill_groups.contains(&source.object_group_id())
        {
            let group_id = *fill_group_ids
                .entry(source.object_group_id())
                .or_insert_with(Uuid::new_v4);
            for replacement in &mut source_replacements {
                replacement.fill_group_id = Some(group_id);
            }
        }
        replacements.extend(source_replacements);
    }
    Ok(Some(replacements))
}

fn canvas_points_screen_bounds(
    points: &[CanvasPoint],
    camera: &CameraAddress,
) -> Result<Option<Rect>> {
    let projected = points
        .iter()
        .map(|point| {
            let (x, y) = camera
                .canvas_to_screen(point, 0.0, 0.0)
                .context("eraser source bounds are not representable")?;
            let position = Pos2::new(x as f32, y as f32);
            (position.x.is_finite() && position.y.is_finite())
                .then_some(position)
                .context("eraser source bounds are not finite")
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(pos2_bounds(&projected))
}

fn eraser_path_fill_masks(
    paths: &[(Vec<Pos2>, f32)],
    camera: &CameraAddress,
    rect: Rect,
) -> Result<Vec<Vec<CanvasPoint>>> {
    let mut masks = Vec::new();
    for (points, width) in paths {
        if points.is_empty() || !width.is_finite() || *width <= 0.0 {
            bail!("eraser path produced invalid Fill mask geometry");
        }
        let radius = *width * 0.5;
        let simplified = simplify_render_points_with_tolerance(
            &points
                .iter()
                .map(|point| (point.x, point.y))
                .collect::<Vec<_>>(),
            false,
            0.25,
        )
        .into_iter()
        .map(|(x, y)| Pos2::new(x, y))
        .collect::<Vec<_>>();
        let polygons = if simplified.len() == 1 {
            vec![circle_mask_polygon(simplified[0], radius)]
        } else {
            simplified
                .windows(2)
                .map(|segment| capsule_mask_polygon(segment[0], segment[1], radius))
                .collect()
        };
        masks.extend(polygons.into_iter().map(|polygon| {
            polygon
                .into_iter()
                .map(|point| screen_to_canvas_for_camera(camera, point, rect))
                .collect()
        }));
    }
    Ok(masks)
}

fn circle_mask_polygon(center: Pos2, radius: f32) -> Vec<Pos2> {
    const STEPS: usize = 16;
    (0..STEPS)
        .map(|step| {
            let angle = std::f32::consts::TAU * step as f32 / STEPS as f32;
            center + egui::vec2(angle.cos(), angle.sin()) * radius
        })
        .collect()
}

fn capsule_mask_polygon(start: Pos2, end: Pos2, radius: f32) -> Vec<Pos2> {
    const CAP_STEPS: usize = 8;
    let direction = end - start;
    let length = direction.length();
    if length <= 0.25 {
        // ponytail: at most 0.125 px conservative over-coverage avoids unstable capsule ears.
        return circle_mask_polygon(start.lerp(end, 0.5), radius + length * 0.5);
    }
    let angle = direction.y.atan2(direction.x);
    let mut polygon = Vec::with_capacity((CAP_STEPS + 1) * 2);
    for step in 0..=CAP_STEPS {
        let theta = angle - std::f32::consts::FRAC_PI_2
            + std::f32::consts::PI * step as f32 / CAP_STEPS as f32;
        polygon.push(end + egui::vec2(theta.cos(), theta.sin()) * radius);
    }
    for step in 0..=CAP_STEPS {
        let theta = angle
            + std::f32::consts::FRAC_PI_2
            + std::f32::consts::PI * step as f32 / CAP_STEPS as f32;
        polygon.push(start + egui::vec2(theta.cos(), theta.sin()) * radius);
    }
    polygon
}

pub fn subtract_paint_stroke_by_path(
    operation: &EditOperation,
    camera: &CameraAddress,
    rect: Rect,
    eraser_points: &[Pos2],
    eraser_width: f32,
) -> Option<Vec<Vec<CanvasPoint>>> {
    if eraser_points.is_empty()
        || !eraser_width.is_finite()
        || eraser_width <= 0.0
        || eraser_points
            .iter()
            .any(|point| !point.x.is_finite() || !point.y.is_finite())
    {
        return None;
    }
    let radius =
        f64::from(operation_width(operation, camera.depth, camera.zoom) + eraser_width) * 0.5;
    if !radius.is_finite() || radius <= 0.0 {
        return None;
    }
    subtract_paint_stroke_with(operation, camera, rect, |start, end| {
        paint_segment_outside_stroke_mask(start, end, eraser_points, radius)
    })
}

pub fn subtract_paint_stroke_by_polygon(
    operation: &EditOperation,
    camera: &CameraAddress,
    rect: Rect,
    polygon: &[Pos2],
) -> Option<Vec<Vec<CanvasPoint>>> {
    let polygon = normalized_pos_polygon(polygon);
    if polygon.len() < 3 || polygon_area_twice(&polygon).abs() < 1.0 {
        return None;
    }
    let radius = f64::from(operation_width(operation, camera.depth, camera.zoom)) * 0.5;
    if !radius.is_finite() || radius <= 0.0 {
        return None;
    }
    subtract_paint_stroke_with(operation, camera, rect, |start, end| {
        paint_segment_outside_polygon_mask(start, end, &polygon, radius)
    })
}

fn subtract_paint_stroke_with(
    operation: &EditOperation,
    camera: &CameraAddress,
    rect: Rect,
    outside_intervals: impl Fn(Pos2, Pos2) -> Vec<(f64, f64)>,
) -> Option<Vec<Vec<CanvasPoint>>> {
    if operation.kind != EditKind::Paint || operation.points.len() < 2 {
        return None;
    }
    // ponytail: subtract stored segments; persist flattened smoothing only if integration
    // shows a visible mismatch between replacement caps and rendered smoothing.
    let projected = operation
        .points
        .iter()
        .map(|point| canvas_point_to_position(camera, point, rect))
        .collect::<Option<Vec<_>>>()?;
    let mut unchanged = true;
    let mut runs = Vec::new();
    let mut current = Vec::new();
    let mut current_end = None;
    for (index, segment) in projected.windows(2).enumerate() {
        let intervals = outside_intervals(segment[0], segment[1]);
        unchanged &= intervals.len() == 1 && intervals[0] == (0.0, 1.0);
        for (start_t, end_t) in intervals {
            let start_position = segment[0].lerp(segment[1], start_t as f32);
            let end_position = segment[0].lerp(segment[1], end_t as f32);
            let start = canvas_point_on_projected_segment(
                &operation.points[index],
                &operation.points[index + 1],
                segment[0],
                segment[1],
                start_t,
                camera,
            )?;
            let end = canvas_point_on_projected_segment(
                &operation.points[index],
                &operation.points[index + 1],
                segment[0],
                segment[1],
                end_t,
                camera,
            )?;
            if current_end.is_some_and(|last: Pos2| last.distance(start_position) <= 0.01) {
                if current.last() != Some(&end) {
                    current.push(end);
                }
            } else {
                if current.len() >= 2 {
                    runs.push(std::mem::take(&mut current));
                }
                current.push(start);
                current.push(end);
            }
            current_end = Some(end_position);
        }
    }
    if unchanged {
        return Some(vec![operation.points.clone()]);
    }
    if current.len() >= 2 {
        runs.push(current);
    }
    Some(runs)
}

fn paint_segment_outside_stroke_mask(
    start: Pos2,
    end: Pos2,
    mask: &[Pos2],
    radius: f64,
) -> Vec<(f64, f64)> {
    let start = selection_point(start);
    let end = selection_point(end);
    let direction = SelectionPoint::new(end.x - start.x, end.y - start.y);
    let mut breaks = vec![0.0, 1.0];

    if mask.len() == 1 {
        add_circle_mask_breaks(
            &mut breaks,
            start,
            direction,
            selection_point(mask[0]),
            radius,
        );
    } else {
        for segment in mask.windows(2) {
            let mask_start = selection_point(segment[0]);
            let mask_end = selection_point(segment[1]);
            add_capsule_mask_breaks(&mut breaks, start, direction, mask_start, mask_end, radius);
        }
    }
    breaks.sort_by(f64::total_cmp);
    breaks.dedup_by(|left, right| (*left - *right).abs() <= 1.0e-9);

    let mask_points = mask
        .iter()
        .copied()
        .map(selection_point)
        .collect::<Vec<_>>();
    let mut outside = Vec::<(f64, f64)>::new();
    for interval in breaks.windows(2) {
        if interval[1] - interval[0] <= 1.0e-9 {
            continue;
        }
        let middle = (interval[0] + interval[1]) * 0.5;
        let point = SelectionPoint::new(
            start.x + direction.x * middle,
            start.y + direction.y * middle,
        );
        if polyline_distance(point, &mask_points) > radius {
            if let Some(last) = outside.last_mut()
                && (last.1 - interval[0]).abs() <= 1.0e-9
            {
                last.1 = interval[1];
            } else {
                outside.push((interval[0], interval[1]));
            }
        }
    }
    outside
}

fn paint_segment_outside_polygon_mask(
    start: Pos2,
    end: Pos2,
    polygon: &[Pos2],
    radius: f64,
) -> Vec<(f64, f64)> {
    let selection_start = selection_point(start);
    let selection_end = selection_point(end);
    let direction = SelectionPoint::new(
        selection_end.x - selection_start.x,
        selection_end.y - selection_start.y,
    );
    let mut breaks = vec![0.0, 1.0];
    let polygon_points = polygon
        .iter()
        .copied()
        .map(selection_point)
        .collect::<Vec<_>>();
    for (edge_start, edge_end) in polygon_edges_pos(polygon) {
        add_capsule_mask_breaks(
            &mut breaks,
            selection_start,
            direction,
            selection_point(edge_start),
            selection_point(edge_end),
            radius,
        );
    }
    breaks.sort_by(f64::total_cmp);
    breaks.dedup_by(|left, right| (*left - *right).abs() <= 1.0e-9);

    let mut outside = Vec::<(f64, f64)>::new();
    for interval in breaks.windows(2) {
        if interval[1] - interval[0] <= 1.0e-9 {
            continue;
        }
        let middle = (interval[0] + interval[1]) * 0.5;
        let point = SelectionPoint::new(
            selection_start.x + direction.x * middle,
            selection_start.y + direction.y * middle,
        );
        let screen_point = Pos2::new(point.x as f32, point.y as f32);
        let erased = point_in_pos_polygon(screen_point, polygon)
            || closed_polyline_distance(point, &polygon_points) <= radius;
        if !erased {
            if let Some(last) = outside.last_mut()
                && (last.1 - interval[0]).abs() <= 1.0e-9
            {
                last.1 = interval[1];
            } else {
                outside.push((interval[0], interval[1]));
            }
        }
    }
    outside
}

fn add_capsule_mask_breaks(
    breaks: &mut Vec<f64>,
    start: SelectionPoint,
    direction: SelectionPoint,
    mask_start: SelectionPoint,
    mask_end: SelectionPoint,
    radius: f64,
) {
    add_circle_mask_breaks(breaks, start, direction, mask_start, radius);
    add_circle_mask_breaks(breaks, start, direction, mask_end, radius);
    let mask_direction = SelectionPoint::new(mask_end.x - mask_start.x, mask_end.y - mask_start.y);
    let mask_length = point_distance(mask_start, mask_end);
    let slope = direction.x * mask_direction.y - direction.y * mask_direction.x;
    if mask_length > f64::EPSILON && slope.abs() > f64::EPSILON {
        let base = (start.x - mask_start.x) * mask_direction.y
            - (start.y - mask_start.y) * mask_direction.x;
        push_unit_break(breaks, (radius * mask_length - base) / slope);
        push_unit_break(breaks, (-radius * mask_length - base) / slope);
    }
}

fn add_circle_mask_breaks(
    breaks: &mut Vec<f64>,
    start: SelectionPoint,
    direction: SelectionPoint,
    center: SelectionPoint,
    radius: f64,
) {
    let offset = SelectionPoint::new(start.x - center.x, start.y - center.y);
    let a = direction.x * direction.x + direction.y * direction.y;
    if a <= f64::EPSILON {
        return;
    }
    let b = 2.0 * (offset.x * direction.x + offset.y * direction.y);
    let c = offset.x * offset.x + offset.y * offset.y - radius * radius;
    let discriminant = b * b - 4.0 * a * c;
    if discriminant < 0.0 {
        return;
    }
    let root = discriminant.sqrt();
    push_unit_break(breaks, (-b - root) / (2.0 * a));
    push_unit_break(breaks, (-b + root) / (2.0 * a));
}

fn push_unit_break(breaks: &mut Vec<f64>, value: f64) {
    if (-1.0e-9..=1.0 + 1.0e-9).contains(&value) {
        breaks.push(value.clamp(0.0, 1.0));
    }
}

fn canvas_point_on_projected_segment(
    start: &CanvasPoint,
    end: &CanvasPoint,
    start_position: Pos2,
    end_position: Pos2,
    t: f64,
    camera: &CameraAddress,
) -> Option<CanvasPoint> {
    if t <= 1.0e-9 {
        return Some(start.clone());
    }
    if t >= 1.0 - 1.0e-9 {
        return Some(end.clone());
    }
    start.translated_by_screen_delta(
        camera.depth,
        camera.zoom,
        f64::from(end_position.x - start_position.x) * t,
        f64::from(end_position.y - start_position.y) * t,
    )
}

fn wrapped_cycle_index(current: usize, length: usize, step: isize) -> usize {
    debug_assert!(length > 0);
    (current as isize + step).rem_euclid(length as isize) as usize
}

fn selection_wheel_steps(events: &[egui::Event]) -> Vec<isize> {
    let mut steps = Vec::new();
    for event in events {
        let egui::Event::MouseWheel {
            unit,
            delta,
            modifiers,
            ..
        } = event
        else {
            continue;
        };
        if !modifiers.alt || delta.y.abs() <= f32::EPSILON {
            continue;
        }
        let direction = if delta.y < 0.0 { 1 } else { -1 };
        let detents = match unit {
            egui::MouseWheelUnit::Line => delta.y.abs().round().clamp(1.0, 64.0) as usize,
            egui::MouseWheelUnit::Point | egui::MouseWheelUnit::Page => 1,
        };
        steps.extend(std::iter::repeat_n(direction, detents));
    }
    steps
}

fn raw_wheel_input_present(events: &[egui::Event]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            egui::Event::MouseWheel { delta, .. }
                if delta.x.abs() > f32::EPSILON || delta.y.abs() > f32::EPSILON
        )
    })
}

fn wheel_zoom_input_diagnostics(events: &[egui::Event]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| {
            let egui::Event::MouseWheel {
                unit,
                delta,
                phase,
                modifiers,
            } = event
            else {
                return None;
            };
            Some(json!({
                "unit": match unit {
                    egui::MouseWheelUnit::Line => "line",
                    egui::MouseWheelUnit::Point => "point",
                    egui::MouseWheelUnit::Page => "page",
                },
                "delta_x": delta.x,
                "delta_y": delta.y,
                "phase": match phase {
                    egui::TouchPhase::Start => "start",
                    egui::TouchPhase::Move => "move",
                    egui::TouchPhase::End => "end",
                    egui::TouchPhase::Cancel => "cancel",
                },
                "modifiers": {
                    "alt": modifiers.alt,
                    "ctrl": modifiers.ctrl,
                    "shift": modifiers.shift,
                    "command": modifiers.command,
                    "mac_cmd": modifiers.mac_cmd,
                },
            }))
        })
        .collect()
}

fn wheel_zoom_gesture_ended(events: &[egui::Event]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            egui::Event::MouseWheel {
                phase: egui::TouchPhase::End | egui::TouchPhase::Cancel,
                ..
            }
        )
    })
}

fn wheel_zoom_camera_diagnostics(camera: &CameraAddress) -> Value {
    json!({
        "depth": camera.depth,
        "tile_x": camera.tile_x.to_string(),
        "tile_y": camera.tile_y.to_string(),
        "local_x": camera.local_x,
        "local_y": camera.local_y,
        "zoom": camera.zoom,
    })
}

fn wheel_zoom_scrolls(
    events: &[egui::Event],
    point_remainder: &mut f32,
    page_scroll_points: f32,
) -> Vec<f32> {
    let mut scrolls = Vec::new();
    for event in events {
        let egui::Event::MouseWheel {
            unit, delta, phase, ..
        } = event
        else {
            continue;
        };
        if matches!(phase, egui::TouchPhase::End | egui::TouchPhase::Cancel) {
            *point_remainder = 0.0;
            continue;
        }
        if delta.y.abs() <= f32::EPSILON {
            continue;
        }
        match unit {
            egui::MouseWheelUnit::Line => {
                let direction = delta.y.signum();
                let detents = delta.y.abs().round().clamp(1.0, 64.0) as usize;
                scrolls.extend(std::iter::repeat_n(
                    direction * WHEEL_LINE_SCROLL_POINTS,
                    detents,
                ));
            }
            egui::MouseWheelUnit::Point => {
                *point_remainder += delta.y;
                let whole_points = point_remainder.trunc();
                if whole_points.abs() >= 1.0 {
                    scrolls.push(whole_points);
                    *point_remainder -= whole_points;
                }
            }
            egui::MouseWheelUnit::Page => {
                scrolls.push(delta.y * page_scroll_points);
            }
        }
    }
    scrolls
}

fn alt_vertical_drag_cycle_steps(start: Pos2, current: Pos2) -> isize {
    let delta_y = current.y - start.y;
    if delta_y.abs() < ALT_SELECTION_DRAG_STEP_PX {
        return 0;
    }
    let steps = (delta_y.abs() / ALT_SELECTION_DRAG_STEP_PX).floor() as isize;
    if delta_y > 0.0 { steps } else { -steps }
}

fn paint_order_wheel_steps(events: &[egui::Event]) -> Vec<isize> {
    let mut steps = Vec::new();
    for event in events {
        let egui::Event::MouseWheel {
            unit,
            delta,
            modifiers,
            ..
        } = event
        else {
            continue;
        };
        if !modifiers.ctrl || modifiers.alt || delta.y.abs() <= f32::EPSILON {
            continue;
        }
        let direction = if delta.y > 0.0 { 1 } else { -1 };
        let detents = match unit {
            egui::MouseWheelUnit::Line => delta.y.abs().round().clamp(1.0, 64.0) as usize,
            egui::MouseWheelUnit::Point | egui::MouseWheelUnit::Page => 1,
        };
        steps.extend(std::iter::repeat_n(direction, detents));
    }
    steps
}

fn append_preview_point(points: &mut Vec<Pos2>, point: Pos2, minimum_spacing: f32) {
    let Some(previous) = points.last().copied() else {
        points.push(point);
        return;
    };
    let distance = previous.distance(point);
    if !distance.is_finite() || distance < minimum_spacing {
        return;
    }
    let maximum_gap = MAX_LASSO_PREVIEW_GAP_PX.max(minimum_spacing);
    if distance <= maximum_gap {
        points.push(point);
        return;
    }
    let step_count =
        ((distance / maximum_gap).ceil() as usize).clamp(1, MAX_LASSO_PREVIEW_INTERPOLATION_STEPS);
    for step in 1..=step_count {
        points.push(previous.lerp(point, step as f32 / step_count as f32));
    }
}

fn area_rectangle_points(
    start: Pos2,
    end: Pos2,
    camera: &CameraAddress,
    rect: Rect,
) -> Vec<CanvasPoint> {
    let bounds = Rect::from_two_pos(start, end);
    [
        bounds.left_top(),
        bounds.right_top(),
        bounds.right_bottom(),
        bounds.left_bottom(),
    ]
    .into_iter()
    .map(|point| screen_to_canvas_for_camera(camera, point, rect))
    .collect()
}

fn area_lasso_points(
    points: &[Pos2],
    camera: &CameraAddress,
    rect: Rect,
) -> Option<Vec<CanvasPoint>> {
    if points.len() < 3
        || points
            .iter()
            .any(|point| !point.x.is_finite() || !point.y.is_finite())
        || polygon_area_twice(points).abs() < 1.0
    {
        return None;
    }
    Some(
        points
            .iter()
            .map(|point| screen_to_canvas_for_camera(camera, *point, rect))
            .collect(),
    )
}

pub fn subtract_fill_polygon(
    source: &[CanvasPoint],
    masks: &[Vec<CanvasPoint>],
) -> Result<Vec<Vec<CanvasPoint>>> {
    subtract_fill_polygon_with_limit(source, masks, MAX_FILL_DIFFERENCE_FRAGMENTS)
}

fn subtract_fill_polygon_with_limit(
    source: &[CanvasPoint],
    masks: &[Vec<CanvasPoint>],
    max_fragments: usize,
) -> Result<Vec<Vec<CanvasPoint>>> {
    let anchor = source.first().context("fill subtraction source is empty")?;
    let camera = CameraAddress {
        depth: anchor.depth,
        tile_x: anchor.tile_x.clone(),
        tile_y: anchor.tile_y.clone(),
        local_x: anchor.local_x,
        local_y: anchor.local_y,
        zoom: 1.0,
    };
    subtract_fill_polygon_at_camera_with_limit(source, masks, &camera, max_fragments)
}

fn subtract_fill_polygon_at_camera(
    source: &[CanvasPoint],
    masks: &[Vec<CanvasPoint>],
    projection_camera: &CameraAddress,
) -> Result<Vec<Vec<CanvasPoint>>> {
    subtract_fill_polygon_at_camera_with_limit(
        source,
        masks,
        projection_camera,
        MAX_FILL_DIFFERENCE_FRAGMENTS,
    )
}

fn subtract_fill_polygon_at_camera_with_limit(
    source: &[CanvasPoint],
    masks: &[Vec<CanvasPoint>],
    projection_camera: &CameraAddress,
    max_fragments: usize,
) -> Result<Vec<Vec<CanvasPoint>>> {
    let anchor = source.first().context("fill subtraction source is empty")?;
    if max_fragments == 0 {
        bail!("fill subtraction fragment limit is zero");
    }
    let mut output_camera = projection_camera.clone();
    let depth_delta: i32 = projection_camera
        .depth
        .saturating_sub(anchor.depth)
        .try_into()
        .context("fill subtraction depth span is not representable")?;
    output_camera.jump_to_depth(anchor.depth);
    output_camera.zoom *= (crate::coords::DEPTH_RATIO as f64).powi(depth_delta);
    if !output_camera.zoom.is_finite() || output_camera.zoom <= 0.0 {
        bail!("fill subtraction depth span is not representable");
    }
    let project = |points: &[CanvasPoint]| -> Result<Vec<Pos2>> {
        points
            .iter()
            .map(|point| {
                let (x, y) = projection_camera
                    .canvas_to_screen(point, 0.0, 0.0)
                    .context("fill subtraction point is not representable")?;
                let position = Pos2::new(x as f32, y as f32);
                (position.x.is_finite() && position.y.is_finite())
                    .then_some(position)
                    .context("fill subtraction point is not finite")
            })
            .collect()
    };

    let source_polygon = normalized_pos_polygon(&project(source)?);
    let Ok(source_triangles) = triangulate_fill_polygon(&source_polygon, "source") else {
        let projected_masks = masks
            .iter()
            .map(|mask| project(mask).map(|mask| normalized_pos_polygon(&mask)))
            .collect::<Result<Vec<_>>>()?;
        let fully_covered = !source_polygon.is_empty()
            && projected_masks.iter().any(|mask| {
                source_polygon
                    .iter()
                    .all(|point| point_in_pos_polygon(*point, mask))
                    && polygon_edges_pos(&source_polygon)
                        .into_iter()
                        .all(|(start, end)| {
                            paint_segment_outside_polygon_mask(start, end, mask, 0.0).is_empty()
                        })
            });
        return Ok(if fully_covered {
            Vec::new()
        } else {
            vec![source.to_vec()]
        });
    };
    if source_triangles.len() > max_fragments {
        bail!("fill subtraction exceeded the {max_fragments}-fragment limit");
    }

    let mut mask_triangles = Vec::new();
    for mask in masks {
        let polygon = normalized_pos_polygon(&project(mask)?);
        mask_triangles.extend(triangulate_fill_polygon(&polygon, "mask")?);
    }
    if mask_triangles.is_empty()
        || !source_triangles.iter().any(|source_triangle| {
            mask_triangles.iter().any(|mask_triangle| {
                fill_polygon_is_nondegenerate(&clip_polygon_to_convex_pos_polygon(
                    source_triangle,
                    mask_triangle,
                ))
            })
        })
    {
        return Ok(vec![source.to_vec()]);
    }

    let mut fragments = source_triangles;
    for mask_triangle in &mask_triangles {
        let mut next = Vec::new();
        for fragment in fragments {
            next.extend(subtract_convex_pos_polygon(&fragment, mask_triangle));
            if next.len() > max_fragments {
                bail!("fill subtraction exceeded the {max_fragments}-fragment limit");
            }
        }
        fragments = next;
        if fragments.is_empty() {
            break;
        }
    }

    fragments
        .into_iter()
        .map(|fragment| {
            let mut points = fragment
                .into_iter()
                .map(|point| {
                    output_camera.screen_to_canvas(point.x as f64, point.y as f64, 0.0, 0.0)
                })
                .collect::<Vec<_>>();
            if let Some(first) = points.first().cloned() {
                points.push(first);
            }
            Ok(points)
        })
        .collect()
}

fn triangulate_fill_polygon(polygon: &[Pos2], label: &str) -> Result<Vec<Vec<Pos2>>> {
    if !fill_polygon_is_nondegenerate(polygon) {
        bail!("fill subtraction {label} polygon is degenerate");
    }
    let triangles = triangulate_pos_polygon(polygon)
        .into_iter()
        .filter(|triangle| polygon_area_twice(triangle).abs() > MIN_FILL_GEOMETRY_AREA_TWICE)
        .collect::<Vec<_>>();
    let polygon_area = polygon_area_twice(polygon).abs();
    let triangle_area = triangles
        .iter()
        .map(|triangle| polygon_area_twice(triangle).abs())
        .sum::<f32>();
    let tolerance = (polygon_area * 1.0e-4).max(MIN_FILL_FRAGMENT_AREA_TWICE);
    if (triangle_area - polygon_area).abs() > tolerance {
        bail!("fill subtraction {label} polygon could not be triangulated");
    }
    Ok(triangles)
}

fn subtract_convex_pos_polygon(subject: &[Pos2], mask: &[Pos2]) -> Vec<Vec<Pos2>> {
    if !fill_polygon_is_nondegenerate(&clip_polygon_to_convex_pos_polygon(subject, mask)) {
        return vec![subject.to_vec()];
    }
    let mut inside = subject.to_vec();
    let mut fragments = Vec::new();
    for (edge_start, edge_end) in polygon_edges_pos(mask) {
        let outside = clip_pos_polygon_to_half_plane(&inside, edge_start, edge_end, false);
        if fill_fragment_is_valid(&outside) {
            fragments.push(outside);
        }
        inside = clip_pos_polygon_to_half_plane(&inside, edge_start, edge_end, true);
        if !fill_fragment_is_valid(&inside) {
            break;
        }
    }
    fragments
}

fn clip_pos_polygon_to_half_plane(
    subject: &[Pos2],
    edge_start: Pos2,
    edge_end: Pos2,
    keep_left: bool,
) -> Vec<Pos2> {
    let mut output = Vec::new();
    let Some(mut previous) = subject.last().copied() else {
        return output;
    };
    let is_inside = |point| {
        let cross = pos_cross(edge_start, edge_end, point);
        if keep_left {
            cross >= 0.0
        } else {
            cross <= 0.0
        }
    };
    let mut previous_inside = is_inside(previous);
    for current in subject.iter().copied() {
        let current_inside = is_inside(current);
        if current_inside != previous_inside
            && let Some(intersection) =
                clip_edge_line_intersection(previous, current, edge_start, edge_end)
        {
            output.push(intersection);
        }
        if current_inside {
            output.push(current);
        }
        previous = current;
        previous_inside = current_inside;
    }
    normalized_pos_polygon(&output)
}

fn fill_fragment_is_valid(points: &[Pos2]) -> bool {
    fill_polygon_is_nondegenerate(points)
        && polygon_area_twice(points).abs() >= MIN_FILL_FRAGMENT_AREA_TWICE
}

fn fill_polygon_is_nondegenerate(points: &[Pos2]) -> bool {
    points.len() >= 3
        && points
            .iter()
            .all(|point| point.x.is_finite() && point.y.is_finite())
        && polygon_area_twice(points).abs() >= MIN_FILL_GEOMETRY_AREA_TWICE
}

fn normalized_pos_polygon(points: &[Pos2]) -> Vec<Pos2> {
    let mut polygon = Vec::with_capacity(points.len());
    for point in points {
        if point.x.is_finite() && point.y.is_finite() && polygon.last() != Some(point) {
            polygon.push(*point);
        }
    }
    if polygon.len() > 1 && polygon.first() == polygon.last() {
        polygon.pop();
    }
    if polygon_area_twice(&polygon) < 0.0 {
        polygon.reverse();
    }
    polygon
}

fn same_pos_points(left: &[Pos2], right: &[Pos2]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.distance(*right) <= 0.01)
}

fn clip_pos_polyline_to_polygon(points: &[Pos2], polygon: &[Pos2], _radius: f32) -> Vec<Vec<Pos2>> {
    if points.len() < 2 || polygon.len() < 3 {
        return Vec::new();
    }
    if points
        .iter()
        .all(|point| point_in_pos_polygon(*point, polygon))
    {
        return vec![points.to_vec()];
    }

    let mut runs = Vec::new();
    let mut current: Vec<Pos2> = Vec::new();
    for segment in points.windows(2) {
        for (start, end) in clipped_segment_inside_polygon(segment[0], segment[1], polygon) {
            if current
                .last()
                .is_some_and(|last| last.distance(start) <= 0.01)
            {
                if current.last().is_none_or(|last| last.distance(end) > 0.01) {
                    current.push(end);
                }
            } else {
                if current.len() >= 2 {
                    runs.push(std::mem::take(&mut current));
                }
                current.push(start);
                if start.distance(end) > 0.01 {
                    current.push(end);
                }
            }
        }
        if !current.is_empty()
            && clipped_segment_inside_polygon(segment[0], segment[1], polygon).is_empty()
        {
            if current.len() >= 2 {
                runs.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    if current.len() >= 2 {
        runs.push(current);
    }
    runs
}

fn clipped_segment_inside_polygon(start: Pos2, end: Pos2, polygon: &[Pos2]) -> Vec<(Pos2, Pos2)> {
    let mut values = vec![0.0_f32, 1.0_f32];
    for (edge_start, edge_end) in polygon_edges_pos(polygon) {
        if let Some(t) = segment_intersection_t(start, end, edge_start, edge_end)
            && (-0.0001..=1.0001).contains(&t)
        {
            values.push(t.clamp(0.0, 1.0));
        }
    }
    values.sort_by(|left, right| left.total_cmp(right));
    values.dedup_by(|left, right| (*left - *right).abs() <= 0.0001);

    let mut segments = Vec::new();
    for interval in values.windows(2) {
        let a = interval[0];
        let b = interval[1];
        if b - a <= 0.0001 {
            continue;
        }
        let midpoint = start.lerp(end, (a + b) * 0.5);
        if point_in_pos_polygon(midpoint, polygon) {
            segments.push((start.lerp(end, a), start.lerp(end, b)));
        }
    }
    segments
}

fn clip_pos_area_operation_to_polygon(points: &[Pos2], clip_polygon: &[Pos2]) -> Vec<Vec<Pos2>> {
    let subject = normalized_pos_polygon(points);
    if subject.len() < 3 || clip_polygon.len() < 3 {
        return Vec::new();
    }
    if subject
        .iter()
        .all(|point| point_in_pos_polygon(*point, clip_polygon))
    {
        return vec![subject];
    }
    if clip_polygon
        .iter()
        .all(|point| point_in_pos_polygon(*point, &subject))
    {
        return vec![clip_polygon.to_vec()];
    }
    if pos_polygon_is_convex(clip_polygon) {
        let clipped = clip_polygon_to_convex_pos_polygon(&subject, clip_polygon);
        return (clipped.len() >= 3 && polygon_area_twice(&clipped).abs() >= 1.0)
            .then_some(clipped)
            .into_iter()
            .collect();
    }
    if pos_polygon_is_convex(&subject) {
        let clipped = clip_polygon_to_convex_pos_polygon(clip_polygon, &subject);
        return (clipped.len() >= 3 && polygon_area_twice(&clipped).abs() >= 1.0)
            .then_some(clipped)
            .into_iter()
            .collect();
    }

    let triangles = triangulate_pos_polygon(clip_polygon);
    let mut fragments = Vec::new();
    for triangle in triangles {
        let clipped = clip_polygon_to_convex_pos_polygon(&subject, &triangle);
        if clipped.len() >= 3 && polygon_area_twice(&clipped).abs() >= 1.0 {
            fragments.push(clipped);
        }
    }
    fragments
}

fn pos_polygon_is_convex(points: &[Pos2]) -> bool {
    let polygon = normalized_pos_polygon(points);
    if polygon.len() < 3 {
        return false;
    }
    let mut direction = 0.0_f32;
    for index in 0..polygon.len() {
        let cross = pos_cross(
            polygon[index],
            polygon[(index + 1) % polygon.len()],
            polygon[(index + 2) % polygon.len()],
        );
        if cross.abs() <= 0.0001 {
            continue;
        }
        if direction == 0.0 {
            direction = cross.signum();
        } else if cross.signum() != direction {
            return false;
        }
    }
    direction != 0.0
}

fn triangulate_pos_polygon(polygon: &[Pos2]) -> Vec<Vec<Pos2>> {
    let mut remaining = normalized_pos_polygon(polygon);
    if remaining.len() < 3 {
        return Vec::new();
    }
    if remaining.len() == 3 {
        return vec![remaining];
    }

    let mut triangles = Vec::new();
    let mut guard = 0;
    while remaining.len() > 3 && guard < polygon.len().saturating_mul(polygon.len()) {
        guard += 1;
        let len = remaining.len();
        let Some(index) = (0..len).find(|index| {
            let previous = remaining[(index + len - 1) % len];
            let current = remaining[*index];
            let next = remaining[(index + 1) % len];
            pos_cross(previous, current, next) > 0.0001
                && !remaining
                    .iter()
                    .enumerate()
                    .any(|(candidate_index, point)| {
                        candidate_index != (index + len - 1) % len
                            && candidate_index != *index
                            && candidate_index != (index + 1) % len
                            && point_in_triangle(*point, previous, current, next)
                    })
        }) else {
            break;
        };
        let len = remaining.len();
        triangles.push(vec![
            remaining[(index + len - 1) % len],
            remaining[index],
            remaining[(index + 1) % len],
        ]);
        remaining.remove(index);
    }
    if remaining.len() == 3 {
        triangles.push(remaining);
    }
    triangles
}

fn clip_polygon_to_convex_pos_polygon(subject: &[Pos2], clip_polygon: &[Pos2]) -> Vec<Pos2> {
    let mut output = normalized_pos_polygon(subject);
    let clip_polygon = normalized_pos_polygon(clip_polygon);
    for (edge_start, edge_end) in polygon_edges_pos(&clip_polygon) {
        if output.is_empty() {
            break;
        }
        let input = std::mem::take(&mut output);
        let mut previous = *input.last().expect("non-empty polygon");
        let mut previous_inside = point_left_of_edge(previous, edge_start, edge_end);
        for current in input {
            let current_inside = point_left_of_edge(current, edge_start, edge_end);
            if current_inside != previous_inside
                && let Some(intersection) =
                    clip_edge_line_intersection(previous, current, edge_start, edge_end)
            {
                output.push(intersection);
            }
            if current_inside {
                output.push(current);
            }
            previous = current;
            previous_inside = current_inside;
        }
    }
    normalized_pos_polygon(&output)
}

fn polygon_edges_pos(points: &[Pos2]) -> Vec<(Pos2, Pos2)> {
    if points.len() < 2 {
        return Vec::new();
    }
    (0..points.len())
        .map(|index| (points[index], points[(index + 1) % points.len()]))
        .collect()
}

fn point_in_pos_polygon(point: Pos2, polygon: &[Pos2]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    if polygon_edges_pos(polygon)
        .iter()
        .any(|(start, end)| distance_to_pos_segment(point, *start, *end) <= 0.01)
    {
        return true;
    }
    let mut inside = false;
    let mut previous = *polygon.last().expect("polygon has points");
    for current in polygon {
        if ((current.y > point.y) != (previous.y > point.y))
            && point.x
                < (previous.x - current.x) * (point.y - current.y)
                    / (previous.y - current.y + f32::EPSILON)
                    + current.x
        {
            inside = !inside;
        }
        previous = *current;
    }
    inside
}

fn point_in_triangle(point: Pos2, a: Pos2, b: Pos2, c: Pos2) -> bool {
    point_left_of_edge(point, a, b)
        && point_left_of_edge(point, b, c)
        && point_left_of_edge(point, c, a)
}

fn point_left_of_edge(point: Pos2, start: Pos2, end: Pos2) -> bool {
    ((end.x - start.x) * (point.y - start.y) - (end.y - start.y) * (point.x - start.x)) >= -0.01
}

fn pos_cross(a: Pos2, b: Pos2, c: Pos2) -> f32 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

fn segment_intersection_t(start: Pos2, end: Pos2, edge_start: Pos2, edge_end: Pos2) -> Option<f32> {
    let direction = end - start;
    let edge_direction = edge_end - edge_start;
    let denominator = direction.x * edge_direction.y - direction.y * edge_direction.x;
    if denominator.abs() <= f32::EPSILON {
        return None;
    }
    let delta = edge_start - start;
    let t = (delta.x * edge_direction.y - delta.y * edge_direction.x) / denominator;
    let u = (delta.x * direction.y - delta.y * direction.x) / denominator;
    ((-0.0001..=1.0001).contains(&t) && (-0.0001..=1.0001).contains(&u)).then_some(t)
}

fn clip_edge_line_intersection(
    start: Pos2,
    end: Pos2,
    edge_start: Pos2,
    edge_end: Pos2,
) -> Option<Pos2> {
    let direction = end - start;
    let edge_direction = edge_end - edge_start;
    let denominator = direction.x * edge_direction.y - direction.y * edge_direction.x;
    if denominator.abs() <= f32::EPSILON {
        return None;
    }
    let delta = edge_start - start;
    let t = (delta.x * edge_direction.y - delta.y * edge_direction.x) / denominator;
    Some(start.lerp(end, t))
}

fn distance_to_pos_segment(point: Pos2, start: Pos2, end: Pos2) -> f32 {
    let segment = end - start;
    let length_squared = segment.length_sq();
    if length_squared <= f32::EPSILON {
        return point.distance(start);
    }
    let t = ((point - start).dot(segment) / length_squared).clamp(0.0, 1.0);
    point.distance(start + segment * t)
}

fn eraser_lasso_operation(
    points: &[Pos2],
    camera: &CameraAddress,
    rect: Rect,
    layer_id: Uuid,
) -> Option<EditOperation> {
    if points.len() < 3
        || points
            .iter()
            .any(|point| !point.x.is_finite() || !point.y.is_finite())
        || polygon_area_twice(points).abs() < 1.0
    {
        return None;
    }
    let mut canvas_points: Vec<_> = points
        .iter()
        .map(|point| screen_to_canvas_for_camera(camera, *point, rect))
        .collect();
    canvas_points.push(canvas_points[0].clone());
    let mut operation = EditOperation::draft(
        EditKind::EraseArea,
        camera.depth,
        camera.zoom,
        canvas_points,
        BACKGROUND,
        0.0,
    );
    operation.layer_id = layer_id;
    Some(operation)
}

fn polygon_area_twice(points: &[Pos2]) -> f32 {
    if points.len() < 3 {
        return 0.0;
    }
    let mut area = 0.0;
    let mut previous = *points.last().unwrap();
    for point in points {
        area += previous.x * point.y - point.x * previous.y;
        previous = *point;
    }
    area
}

fn paint_rect_outline(painter: &Painter, rect: Rect, stroke: Stroke) {
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    for index in 0..corners.len() {
        painter.line_segment(
            [corners[index], corners[(index + 1) % corners.len()]],
            stroke,
        );
    }
}

fn paint_closed_outline(painter: &Painter, points: &[Pos2], stroke: Stroke) {
    if points.len() < 2 {
        return;
    }
    painter.add(egui::Shape::line(points.to_vec(), stroke));
    painter.line_segment([*points.last().unwrap(), points[0]], stroke);
}

fn paint_dashed_closed_outline(painter: &Painter, points: &[Pos2], stroke: Stroke) {
    if points.len() < 2 {
        return;
    }
    let dash = 8.0;
    let gap = 5.0;
    for index in 0..points.len() {
        paint_dashed_segment(
            painter,
            points[index],
            points[(index + 1) % points.len()],
            stroke,
            dash,
            gap,
        );
    }
}

fn paint_dashed_segment(
    painter: &Painter,
    start: Pos2,
    end: Pos2,
    stroke: Stroke,
    dash: f32,
    gap: f32,
) {
    let delta = end - start;
    let length = delta.length();
    if !length.is_finite() || length <= f32::EPSILON {
        return;
    }
    let direction = delta / length;
    let mut offset = 0.0;
    while offset < length {
        let segment_end = (offset + dash).min(length);
        painter.line_segment(
            [start + direction * offset, start + direction * segment_end],
            stroke,
        );
        offset += dash + gap;
    }
}

fn pos2_bounds(points: &[Pos2]) -> Option<Rect> {
    let first = *points.first()?;
    let mut min = first;
    let mut max = first;
    for point in &points[1..] {
        if !point.x.is_finite() || !point.y.is_finite() {
            return None;
        }
        min.x = min.x.min(point.x);
        min.y = min.y.min(point.y);
        max.x = max.x.max(point.x);
        max.y = max.y.max(point.y);
    }
    Some(Rect::from_min_max(min, max))
}

fn selection_highlight_render_points(
    operation: &EditOperation,
    points: Vec<Pos2>,
    smoothing_passes: usize,
    fill_fallback_max_points: usize,
) -> Vec<Pos2> {
    if operation.is_compact_block() {
        return points;
    }
    if operation.kind.is_area() {
        area_saved_fallback_render_points(
            &points,
            fill_fallback_max_points,
            if operation.smooth_area {
                smoothing_passes
            } else {
                0
            },
        )
    } else {
        smooth_pos2_saved_fallback(&points, smoothing_passes, false)
    }
}

fn paint_operation_highlight(
    painter: &Painter,
    kind: EditKind,
    points: &[Pos2],
    width: f32,
    color: Color32,
) {
    if matches!(
        kind,
        EditKind::Fill | EditKind::EraseArea | EditKind::CompactBlock
    ) {
        paint_closed_outline(painter, points, Stroke::new(3.0, color));
    } else if points.len() == 1 {
        painter.circle_stroke(points[0], 4.0, Stroke::new(2.0, color));
    } else if points.len() >= 2 {
        painter.add(egui::Shape::line(
            points.to_vec(),
            Stroke::new(width + 4.0, color),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AUTO_SEGMENTED_FALLBACK_POINTS, CanvasContextMenuKind, ClipboardCommand,
        ClipboardShortcutKeys, DepthRenderMode, FRAME_TIME_EMA_ALPHA, FallbackPaintOperation,
        FallbackRenderer, FrameRateTracker, MAX_BRUSH_SIZE, MAX_FALLBACK_SMOOTHING_INPUT_POINTS,
        MAX_LASSO_PREVIEW_GAP_PX, MAX_MEASURED_FRAME_TIME, MAX_SEGMENTED_FALLBACK_SHAPES_PER_FRAME,
        MAX_TILE_JOBS_QUEUED_PER_FRAME, MIN_BRUSH_SIZE, OperationBoundsCache, PointAppendDecision,
        QUALITY_SEGMENTED_FALLBACK_POINTS, RectangleSelectionMode, SavedFallbackRenderCache,
        SavedFallbackRenderFrameKey, SavedFallbackRenderOperationKey, SelectionClipboard,
        SelectionOverlayBoundsCache, SelectionRotateGesture, SelectionScaleGesture,
        SelectionScaleHandle, SelectionTargetMode, TILE_POLL_REPAINT_INTERVAL, TileFallbackMode,
        WHEEL_LINE_SCROLL_POINTS, WHEEL_ZOOM_DIRECTION_LATCH_TIMEOUT, WheelZoomDirectionLatch,
        adjusted_brush_size, alt_vertical_drag_cycle_steps, append_interpolated_points,
        append_preview_point, area_cycle_tool, area_lasso_points, area_rectangle_points,
        area_saved_fallback_render_points, auto_fast_stroke_fallback_active,
        canvas_context_menu_kind, canvas_overlay_text, capsule_mask_polygon, clamp_quick_depth,
        clip_polygon_to_convex_pos_polygon, clip_pos_area_operation_to_polygon,
        clip_pos_polyline_to_polygon, clip_pos2_polygon, clip_pos2_polyline,
        clipboard_command_from_events, closed_polyline_distance, color_from_srgb,
        consume_segmented_fallback_budget, depth_render_mode, draft_is_committable,
        eraser_lasso_operation, eraser_path_fill_masks, fallback_operation_limit,
        fallback_operation_skip_count, fill_fallback_render_points, format_overlay_coordinate,
        fully_contained_object_operation_ids, incremental_paint_operations,
        interpolation_step_count, large_selection_fast_overlay_active, layer_operation_counts,
        normalized_pos_polygon, operation_click_score, operation_contained_by_rectangle,
        operation_intersects_selection, operation_pick_hit, paint_fill_group_scanlines,
        paint_order_wheel_steps, parse_lateral_coordinate, point_append_decision,
        point_in_pos_polygon, polygon_area_twice, prepare_draft_for_commit,
        primary_pointer_positions, raw_wheel_input_present, rectangle_selection_mode_cycle,
        redo_shortcuts, repaint_interval_for_work, saved_fallback_operation_key,
        selection_bounds_signature, selection_highlight_render_points,
        selection_overlay_width_expansion, selection_rotate_handle_at,
        selection_rotate_handle_position, selection_scale_factor, selection_scale_handle_at,
        selection_target_cycle, selection_wheel_steps, set_straight_draft_endpoint,
        should_paint_fill_fallback, should_paint_live_draft, smooth_pos2_draft,
        smooth_pos2_saved_fallback, space_pan_requested, straight_line_requested,
        stroke_fallback_fast_endpoint_caps, stroke_fallback_segmented_point_limit,
        stroke_fallback_shapes, stroke_fallback_smoothing_passes, subtract_eraser_sources,
        subtract_fill_polygon, subtract_fill_polygon_with_limit, subtract_paint_sources,
        subtract_paint_stroke_by_path, subtract_paint_stroke_by_polygon,
        surrender_canvas_keyboard_focus, tile_display_rects, tile_fallback_mode,
        tile_generation_is_paused, tile_keys_for_view, tile_rebuild_deferred_since,
        tile_request_batch, tool_shortcut_allowed, triangulate_fill_polygon,
        wheel_zoom_gesture_ended, wheel_zoom_input_diagnostics, wheel_zoom_scrolls,
        wrapped_cycle_index, z_drag_zoom_factor, z_zoom_requested,
        zoom_tile_requests_deferred_since,
    };
    use crate::coords::{CameraAddress, CanvasPoint, ScreenAffine};
    use crate::document::{CanvasDocument, VisibleDepthMode};
    use crate::model::{Color, DEFAULT_LAYER_ID, EditKind, EditOperation, ToolKind};
    use crate::projection_cache::ProjectedGeometryCache;
    use crate::raster::{RasterOptions, render_tile_with_options};
    use crate::selection_geometry::{Point2, Rect2, SelectionShape};
    use crate::settings::{
        AppSettings, OverlayProfile, PerformanceProfile, SmoothingLevel, StrokeFallbackJoinMode,
    };
    use crate::spatial::{OperationIndex, operation_bounds};
    use crate::tile_cache::{
        TILE_BLEED, TILE_SIZE, TileKey, tile_lod_for_resolution, tile_resolution,
    };
    use anyhow::Context;
    use eframe::egui::{
        self, Color32, Event, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect, Shape,
        TouchPhase,
    };
    use num_bigint::BigInt;
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tempfile::TempDir;
    use uuid::Uuid;

    #[test]
    fn canvas_context_menu_routes_only_supported_tools() {
        assert_eq!(
            canvas_context_menu_kind(ToolKind::Brush),
            Some(CanvasContextMenuKind::Brush)
        );
        assert_eq!(
            canvas_context_menu_kind(ToolKind::LassoFill),
            Some(CanvasContextMenuKind::Fill)
        );
        assert_eq!(
            canvas_context_menu_kind(ToolKind::Selection),
            Some(CanvasContextMenuKind::Selection)
        );
        for tool in [
            ToolKind::Eraser,
            ToolKind::Eyedropper,
            ToolKind::EraserLasso,
        ] {
            assert_eq!(canvas_context_menu_kind(tool), None);
        }
    }

    #[test]
    fn selection_hit_test_uses_stroke_radius_and_fill_area() {
        let selection = SelectionShape::Rectangle(Rect2::from_points(
            Point2::new(5.0, 5.0),
            Point2::new(15.0, 15.0),
        ));
        let nearby_stroke = [Point2::new(0.0, 3.0), Point2::new(20.0, 3.0)];
        let fill = [
            Point2::new(8.0, 8.0),
            Point2::new(12.0, 8.0),
            Point2::new(10.0, 12.0),
        ];

        assert!(!operation_intersects_selection(
            EditKind::Paint,
            &nearby_stroke,
            3.0,
            &selection
        ));
        assert!(operation_intersects_selection(
            EditKind::Paint,
            &nearby_stroke,
            4.0,
            &selection
        ));
        assert!(operation_intersects_selection(
            EditKind::Fill,
            &fill,
            0.0,
            &selection
        ));
    }

    #[test]
    fn eyedropper_hit_test_uses_stroke_radius_and_fill_area() {
        let stroke = [Pos2::new(0.0, 10.0), Pos2::new(20.0, 10.0)];
        assert!(operation_pick_hit(
            EditKind::Paint,
            &stroke,
            6.0,
            Pos2::new(10.0, 12.9)
        ));
        assert!(!operation_pick_hit(
            EditKind::Paint,
            &stroke,
            6.0,
            Pos2::new(10.0, 14.1)
        ));

        let fill = [
            Pos2::new(0.0, 0.0),
            Pos2::new(20.0, 0.0),
            Pos2::new(20.0, 20.0),
            Pos2::new(0.0, 20.0),
        ];
        assert!(operation_pick_hit(
            EditKind::Fill,
            &fill,
            1.0,
            Pos2::new(10.0, 10.0)
        ));
        assert!(!operation_pick_hit(
            EditKind::Fill,
            &fill,
            1.0,
            Pos2::new(30.0, 10.0)
        ));
    }

    #[test]
    fn selection_scale_handles_hit_corners_and_preserve_click_offset() {
        let bounds = Rect::from_min_max(Pos2::new(10.0, 20.0), Pos2::new(110.0, 70.0));
        assert_eq!(
            selection_scale_handle_at(bounds, Pos2::new(114.0, 74.0)),
            Some(SelectionScaleHandle::BottomRight)
        );
        assert_eq!(selection_scale_handle_at(bounds, bounds.center()), None);

        let gesture = SelectionScaleGesture {
            handle: SelectionScaleHandle::BottomRight,
            bounds,
            pointer_start: Pos2::new(114.0, 74.0),
            current: Pos2::new(114.0, 74.0),
        };
        assert!((gesture.scale() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn selection_corner_scale_is_uniform_and_clamped_above_zero() {
        let bounds = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(100.0, 50.0));
        assert!(
            (selection_scale_factor(
                bounds,
                SelectionScaleHandle::BottomRight,
                Pos2::new(200.0, 100.0),
            ) - 2.0)
                .abs()
                < f32::EPSILON
        );
        assert!(
            (selection_scale_factor(
                bounds,
                SelectionScaleHandle::TopLeft,
                Pos2::new(-100.0, -50.0),
            ) - 2.0)
                .abs()
                < f32::EPSILON
        );
        assert_eq!(
            selection_scale_factor(
                bounds,
                SelectionScaleHandle::BottomRight,
                Pos2::new(-10.0, -10.0),
            ),
            super::MIN_SELECTION_SCALE
        );
    }

    #[test]
    fn selection_rotate_handle_tracks_angle_snap_and_bounds() {
        let bounds = Rect::from_min_max(Pos2::new(100.0, 100.0), Pos2::new(200.0, 160.0));
        let handle = selection_rotate_handle_position(bounds);
        assert!(selection_rotate_handle_at(
            bounds,
            handle + egui::vec2(2.0, -1.0)
        ));
        assert!(!selection_rotate_handle_at(bounds, bounds.center()));

        let free = SelectionRotateGesture {
            bounds,
            pointer_start: handle,
            current: bounds.center() + egui::vec2(60.0, 0.0),
            snap: false,
        };
        assert!((free.angle() - std::f32::consts::FRAC_PI_2).abs() < 0.001);
        let rotated = free.transform_position(Pos2::new(200.0, 130.0));
        assert!((rotated.x - 150.0).abs() < 0.001);
        assert!((rotated.y - 180.0).abs() < 0.001);
        let transformed_bounds = free.transformed_bounds();
        assert!((transformed_bounds.width() - bounds.height()).abs() < 0.001);
        assert!((transformed_bounds.height() - bounds.width()).abs() < 0.001);

        let snapped = SelectionRotateGesture {
            current: bounds.center() + egui::vec2(60.0, 3.0),
            snap: true,
            ..free
        };
        assert!((snapped.angle() - std::f32::consts::FRAC_PI_2).abs() < 0.001);
    }

    #[test]
    fn click_selection_prefers_the_smaller_overlapping_operation() {
        let click = Point2::new(5.0, 5.0);
        let long = [Point2::new(-100.0, 5.0), Point2::new(100.0, 5.0)];
        let small = [Point2::new(4.0, 5.0), Point2::new(6.0, 5.0)];

        let long_score =
            operation_click_score(EditKind::Paint, &long, 2.0, click).expect("long line hit");
        let small_score =
            operation_click_score(EditKind::Paint, &small, 2.0, click).expect("small line hit");

        assert_eq!(long_score.0, small_score.0);
        assert!(small_score.1 < long_score.1);
    }

    #[test]
    fn click_selection_hits_fill_interior_and_rejects_distant_geometry() {
        let fill = [
            Point2::new(0.0, 0.0),
            Point2::new(10.0, 0.0),
            Point2::new(10.0, 10.0),
            Point2::new(0.0, 10.0),
        ];

        assert!(operation_click_score(EditKind::Fill, &fill, 0.0, Point2::new(5.0, 5.0)).is_some());
        assert!(
            operation_click_score(EditKind::Fill, &fill, 0.0, Point2::new(30.0, 30.0)).is_none()
        );
    }

    #[test]
    fn inside_rectangle_rejects_crossing_strokes_and_respects_width() {
        let rectangle = Rect2::from_points(Point2::new(0.0, 0.0), Point2::new(10.0, 10.0));
        let inside = [Point2::new(2.0, 5.0), Point2::new(8.0, 5.0)];
        let crossing = [Point2::new(-1.0, 5.0), Point2::new(8.0, 5.0)];
        let touches_edge = [Point2::new(0.5, 5.0), Point2::new(8.0, 5.0)];

        assert!(operation_contained_by_rectangle(
            EditKind::Paint,
            &inside,
            2.0,
            rectangle
        ));
        assert!(!operation_contained_by_rectangle(
            EditKind::Paint,
            &crossing,
            2.0,
            rectangle
        ));
        assert!(!operation_contained_by_rectangle(
            EditKind::Paint,
            &touches_edge,
            2.0,
            rectangle
        ));
    }

    #[test]
    fn overlap_cycle_wraps_in_both_wheel_directions() {
        assert_eq!(wrapped_cycle_index(0, 3, 1), 1);
        assert_eq!(wrapped_cycle_index(2, 3, 1), 0);
        assert_eq!(wrapped_cycle_index(0, 3, -1), 2);
        assert_eq!(wrapped_cycle_index(2, 3, -1), 1);
    }

    #[test]
    fn wheel_cycle_counts_each_raw_detent_once_even_in_one_frame() {
        let wheel = |delta_y| Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            delta: egui::vec2(0.0, delta_y),
            phase: TouchPhase::Move,
            modifiers: Modifiers::ALT,
        };
        let events = [wheel(-1.0), wheel(-2.0), wheel(1.0)];

        assert_eq!(selection_wheel_steps(&events), vec![1, 1, 1, -1]);
    }

    #[test]
    fn alt_vertical_drag_cycles_by_threshold_and_direction() {
        let start = Pos2::new(100.0, 100.0);

        assert_eq!(
            alt_vertical_drag_cycle_steps(start, Pos2::new(100.0, 127.0)),
            0
        );
        assert_eq!(
            alt_vertical_drag_cycle_steps(start, Pos2::new(100.0, 128.0)),
            1
        );
        assert_eq!(
            alt_vertical_drag_cycle_steps(start, Pos2::new(100.0, 160.0)),
            2
        );
        assert_eq!(
            alt_vertical_drag_cycle_steps(start, Pos2::new(100.0, 72.0)),
            -1
        );
    }

    #[test]
    fn paint_order_wheel_counts_ctrl_detents_and_ignores_other_modifiers() {
        let wheel = |delta_y, modifiers| Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            delta: egui::vec2(0.0, delta_y),
            phase: TouchPhase::Move,
            modifiers,
        };
        let events = [
            wheel(1.0, Modifiers::CTRL),
            wheel(-2.0, Modifiers::CTRL),
            wheel(1.0, Modifiers::ALT),
            wheel(1.0, Modifiers::CTRL | Modifiers::ALT),
            wheel(1.0, Modifiers::NONE),
        ];

        assert_eq!(paint_order_wheel_steps(&events), vec![1, -1, -1]);
    }

    #[test]
    fn wheel_zoom_counts_line_detents_in_event_order() {
        let wheel = |delta_y| Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            delta: egui::vec2(0.0, delta_y),
            phase: TouchPhase::Move,
            modifiers: Modifiers::NONE,
        };
        let mut remainder = 0.0;

        let scrolls = wheel_zoom_scrolls(
            &[wheel(1.0), wheel(-2.0), wheel(1.0)],
            &mut remainder,
            720.0,
        );

        assert_eq!(
            scrolls,
            vec![
                WHEEL_LINE_SCROLL_POINTS,
                -WHEEL_LINE_SCROLL_POINTS,
                -WHEEL_LINE_SCROLL_POINTS,
                WHEEL_LINE_SCROLL_POINTS,
            ]
        );
        assert_eq!(remainder, 0.0);
    }

    #[test]
    fn wheel_zoom_diagnostics_preserve_raw_order_sign_unit_and_phase() {
        let events = [
            Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: egui::vec2(0.0, 1.0),
                phase: TouchPhase::Start,
                modifiers: Modifiers::NONE,
            },
            Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: egui::vec2(0.25, -0.75),
                phase: TouchPhase::Move,
                modifiers: Modifiers::SHIFT,
            },
            Event::MouseWheel {
                unit: MouseWheelUnit::Page,
                delta: egui::vec2(0.0, 0.0),
                phase: TouchPhase::End,
                modifiers: Modifiers::CTRL,
            },
        ];

        let diagnostics = wheel_zoom_input_diagnostics(&events);

        assert_eq!(diagnostics.len(), 3);
        assert_eq!(diagnostics[0]["unit"], "line");
        assert_eq!(diagnostics[0]["delta_y"], 1.0);
        assert_eq!(diagnostics[0]["phase"], "start");
        assert_eq!(diagnostics[1]["unit"], "point");
        assert_eq!(diagnostics[1]["delta_x"], 0.25);
        assert_eq!(diagnostics[1]["delta_y"], -0.75);
        assert_eq!(diagnostics[1]["phase"], "move");
        assert_eq!(diagnostics[1]["modifiers"]["shift"], true);
        assert_eq!(diagnostics[2]["unit"], "page");
        assert_eq!(diagnostics[2]["phase"], "end");
        assert_eq!(diagnostics[2]["modifiers"]["ctrl"], true);
    }

    #[test]
    fn wheel_zoom_direction_latch_suppresses_short_opposite_impulses() {
        let now = Instant::now();
        let mut latch = WheelZoomDirectionLatch::default();

        let initial = latch.apply(&[-40.0; 14], false, now);
        assert_eq!(initial.applied_scrolls, vec![-40.0; 14]);
        assert_eq!(initial.suppressed_scroll_count, 0);
        assert_eq!(initial.direction_used, Some(-1.0));

        let chatter = latch.apply(&[40.0], false, now + Duration::from_millis(100));
        assert!(chatter.applied_scrolls.is_empty());
        assert_eq!(chatter.suppressed_scroll_count, 1);
        assert_eq!(chatter.direction_before, Some(-1.0));
        assert_eq!(chatter.direction_after, Some(-1.0));

        let continued = latch.apply(&[-40.0], false, now + Duration::from_millis(150));
        assert_eq!(continued.applied_scrolls, vec![-40.0]);
    }

    #[test]
    fn wheel_zoom_direction_latch_accepts_a_new_direction_after_quiet() {
        let now = Instant::now();
        let mut latch = WheelZoomDirectionLatch::default();
        assert_eq!(latch.apply(&[40.0], false, now).applied_scrolls, [40.0]);

        let changed = latch.apply(&[-40.0], false, now + WHEEL_ZOOM_DIRECTION_LATCH_TIMEOUT);
        assert_eq!(changed.applied_scrolls, [-40.0]);
        assert_eq!(changed.direction_before, None);
        assert_eq!(changed.direction_used, Some(-1.0));
    }

    #[test]
    fn wheel_zoom_direction_latch_uses_batch_majority_and_resets_on_end() {
        let now = Instant::now();
        let mut latch = WheelZoomDirectionLatch::default();

        let mixed = latch.apply(&[-40.0, -40.0, 40.0], false, now);
        assert_eq!(mixed.applied_scrolls, [-40.0, -40.0]);
        assert_eq!(mixed.suppressed_scroll_count, 1);

        let ended = latch.apply(&[], true, now + Duration::from_millis(1));
        assert_eq!(ended.direction_before, Some(-1.0));
        assert_eq!(ended.direction_after, None);

        let opposite = latch.apply(&[40.0], false, now + Duration::from_millis(2));
        assert_eq!(opposite.applied_scrolls, [40.0]);

        let end_event = Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: egui::Vec2::ZERO,
            phase: TouchPhase::End,
            modifiers: Modifiers::NONE,
        };
        assert!(wheel_zoom_gesture_ended(&[end_event]));
    }

    #[test]
    fn wheel_zoom_accumulates_fractional_point_scroll() {
        let point = |delta_y, phase| Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: egui::vec2(0.0, delta_y),
            phase,
            modifiers: Modifiers::NONE,
        };
        let mut remainder = 0.0;

        assert_eq!(
            wheel_zoom_scrolls(&[point(0.4, TouchPhase::Move)], &mut remainder, 720.0),
            Vec::<f32>::new()
        );
        assert!((remainder - 0.4).abs() <= f32::EPSILON);
        assert_eq!(
            wheel_zoom_scrolls(&[point(0.7, TouchPhase::Move)], &mut remainder, 720.0),
            vec![1.0]
        );
        assert!((remainder - 0.1).abs() <= f32::EPSILON);
        assert_eq!(
            wheel_zoom_scrolls(&[point(0.0, TouchPhase::End)], &mut remainder, 720.0),
            Vec::<f32>::new()
        );
        assert_eq!(remainder, 0.0);
    }

    #[test]
    fn raw_wheel_presence_ignores_zero_delta_events() {
        let event = Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: egui::vec2(0.0, 0.0),
            phase: TouchPhase::Move,
            modifiers: Modifiers::NONE,
        };

        assert!(!raw_wheel_input_present(std::slice::from_ref(&event)));
        assert!(raw_wheel_input_present(&[Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: egui::vec2(0.0, 0.5),
            phase: TouchPhase::Move,
            modifiers: Modifiers::NONE,
        }]));
    }

    #[test]
    fn lasso_preview_skips_points_below_minimum_spacing() {
        let mut points = vec![Pos2::new(0.0, 0.0)];

        append_preview_point(&mut points, Pos2::new(1.0, 0.0), 2.0);
        append_preview_point(&mut points, Pos2::new(2.0, 0.0), 2.0);

        assert_eq!(points, vec![Pos2::new(0.0, 0.0), Pos2::new(2.0, 0.0)]);
    }

    #[test]
    fn lasso_preview_interpolates_large_pointer_gaps() {
        let mut points = vec![Pos2::new(0.0, 0.0)];

        append_preview_point(&mut points, Pos2::new(10.0, 0.0), 2.0);

        assert_eq!(points.last(), Some(&Pos2::new(10.0, 0.0)));
        assert!(points.len() > 2);
        assert!(points.windows(2).all(|segment| {
            segment[0].distance(segment[1]) <= MAX_LASSO_PREVIEW_GAP_PX + f32::EPSILON
        }));
    }

    #[test]
    fn area_rectangle_is_stored_as_canvas_corners() {
        let camera = CameraAddress::default();
        let rect = Rect::from_min_size(Pos2::new(10.0, 20.0), egui::vec2(512.0, 512.0));
        let points = area_rectangle_points(
            Pos2::new(110.0, 120.0),
            Pos2::new(210.0, 220.0),
            &camera,
            rect,
        );

        assert_eq!(points.len(), 4);
        assert_eq!(
            points[0],
            CanvasPoint::new(0, 0.into(), 0.into(), 0.1953125, 0.1953125)
        );
        assert_eq!(
            points[2],
            CanvasPoint::new(0, 0.into(), 0.into(), 0.390625, 0.390625)
        );
    }

    #[test]
    fn area_lasso_rejects_degenerate_and_stores_canvas_points() {
        let camera = CameraAddress::default();
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(512.0, 512.0));
        assert!(
            area_lasso_points(&[Pos2::new(1.0, 1.0), Pos2::new(2.0, 2.0)], &camera, rect).is_none()
        );

        let points = area_lasso_points(
            &[
                Pos2::new(128.0, 128.0),
                Pos2::new(384.0, 128.0),
                Pos2::new(384.0, 384.0),
            ],
            &camera,
            rect,
        )
        .expect("valid area lasso");
        assert_eq!(points.len(), 3);
        assert_eq!(
            points[0],
            CanvasPoint::new(0, 0.into(), 0.into(), 0.25, 0.25)
        );
        assert_eq!(
            points[2],
            CanvasPoint::new(0, 0.into(), 0.into(), 0.75, 0.75)
        );
    }

    #[test]
    fn area_polygon_clips_polyline_into_visible_runs() {
        let area = normalized_pos_polygon(&[
            Pos2::new(0.0, 0.0),
            Pos2::new(10.0, 0.0),
            Pos2::new(10.0, 10.0),
            Pos2::new(0.0, 10.0),
        ]);
        let runs =
            clip_pos_polyline_to_polygon(&[Pos2::new(-5.0, 5.0), Pos2::new(15.0, 5.0)], &area, 0.0);

        assert_eq!(runs.len(), 1);
        assert!((runs[0][0].x - 0.0).abs() <= 0.01);
        assert!((runs[0][1].x - 10.0).abs() <= 0.01);
        assert_eq!(runs[0][0].y, 5.0);
        assert_eq!(runs[0][1].y, 5.0);
    }

    #[test]
    fn paint_stroke_subtraction_handles_width_crossing_tangent_and_full_removal() {
        let camera = CameraAddress {
            depth: 12,
            ..CameraAddress::default()
        };
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(512.0, 512.0));
        let paint = EditOperation::draft(
            EditKind::Paint,
            camera.depth,
            camera.zoom,
            vec![
                camera.screen_to_canvas(100.0, 256.0, 512.0, 512.0),
                camera.screen_to_canvas(400.0, 256.0, 512.0, 512.0),
            ],
            Color::BLACK,
            10.0,
        );
        let project_x = |point: &CanvasPoint| {
            camera
                .canvas_to_screen(point, 512.0, 512.0)
                .expect("result projects")
                .0
        };

        let outside =
            subtract_paint_stroke_by_path(&paint, &camera, rect, &[Pos2::new(250.0, 50.0)], 20.0)
                .expect("valid subtraction");
        assert_eq!(outside, vec![paint.points.clone()]);

        let crossing = subtract_paint_stroke_by_path(
            &paint,
            &camera,
            rect,
            &[Pos2::new(250.0, 100.0), Pos2::new(250.0, 400.0)],
            20.0,
        )
        .expect("valid subtraction");
        assert_eq!(crossing.len(), 2);
        assert!((project_x(crossing[0].last().unwrap()) - 235.0).abs() < 0.01);
        assert!((project_x(&crossing[1][0]) - 265.0).abs() < 0.01);
        assert_eq!(crossing[0].last().unwrap().depth, camera.depth);

        let tangent =
            subtract_paint_stroke_by_path(&paint, &camera, rect, &[Pos2::new(250.0, 271.0)], 20.0)
                .expect("valid subtraction");
        assert_eq!(tangent, vec![paint.points.clone()]);

        let endpoint =
            subtract_paint_stroke_by_path(&paint, &camera, rect, &[Pos2::new(100.0, 256.0)], 20.0)
                .expect("valid subtraction");
        assert_eq!(endpoint.len(), 1);
        assert!((project_x(&endpoint[0][0]) - 115.0).abs() < 0.01);
        assert!((project_x(endpoint[0].last().unwrap()) - 400.0).abs() < 0.01);

        let removed = subtract_paint_stroke_by_path(
            &paint,
            &camera,
            rect,
            &[Pos2::new(100.0, 256.0), Pos2::new(400.0, 256.0)],
            20.0,
        )
        .expect("valid subtraction");
        assert!(removed.is_empty());
    }

    #[test]
    fn paint_stroke_polygon_subtraction_handles_outside_boundary_and_full_removal() {
        let camera = CameraAddress::default();
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(512.0, 512.0));
        let paint = EditOperation::draft(
            EditKind::Paint,
            camera.depth,
            camera.zoom,
            vec![
                camera.screen_to_canvas(100.0, 256.0, 512.0, 512.0),
                camera.screen_to_canvas(400.0, 256.0, 512.0, 512.0),
            ],
            Color::BLACK,
            10.0,
        );
        let project_x = |point: &CanvasPoint| {
            camera
                .canvas_to_screen(point, 512.0, 512.0)
                .expect("projected point")
                .0
        };
        let rectangle = |left: f32, top: f32, right: f32, bottom: f32| {
            [
                Pos2::new(left, top),
                Pos2::new(right, top),
                Pos2::new(right, bottom),
                Pos2::new(left, bottom),
            ]
        };

        let outside = subtract_paint_stroke_by_polygon(
            &paint,
            &camera,
            rect,
            &rectangle(200.0, 100.0, 300.0, 150.0),
        )
        .expect("valid outside polygon");
        assert_eq!(outside, vec![paint.points.clone()]);

        let crossing = subtract_paint_stroke_by_polygon(
            &paint,
            &camera,
            rect,
            &rectangle(240.0, 200.0, 260.0, 300.0),
        )
        .expect("valid crossing polygon");
        assert_eq!(crossing.len(), 2);
        assert!((project_x(crossing[0].last().unwrap()) - 235.0).abs() < 0.01);
        assert!((project_x(&crossing[1][0]) - 265.0).abs() < 0.01);

        let endpoint = subtract_paint_stroke_by_polygon(
            &paint,
            &camera,
            rect,
            &rectangle(50.0, 200.0, 200.0, 300.0),
        )
        .expect("valid endpoint polygon");
        assert_eq!(endpoint.len(), 1);
        assert!((project_x(&endpoint[0][0]) - 205.0).abs() < 0.01);

        let boundary = subtract_paint_stroke_by_polygon(
            &paint,
            &camera,
            rect,
            &rectangle(240.0, 200.0, 260.0, 252.0),
        )
        .expect("valid boundary polygon");
        assert_eq!(boundary.len(), 2);
        assert!((project_x(boundary[0].last().unwrap()) - 237.0).abs() < 0.01);
        assert!((project_x(&boundary[1][0]) - 263.0).abs() < 0.01);

        let removed = subtract_paint_stroke_by_polygon(
            &paint,
            &camera,
            rect,
            &rectangle(50.0, 200.0, 450.0, 300.0),
        )
        .expect("valid containing polygon");
        assert!(removed.is_empty());
    }

    #[test]
    fn vector_eraser_lasso_pipeline_commits_fragments_once_without_erase_area() -> anyhow::Result<()>
    {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let camera = CameraAddress::default();
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(512.0, 512.0));
        let paint = EditOperation::draft(
            EditKind::Paint,
            camera.depth,
            camera.zoom,
            vec![
                camera.screen_to_canvas(100.0, 256.0, 512.0, 512.0),
                camera.screen_to_canvas(400.0, 256.0, 512.0, 512.0),
            ],
            Color::BLACK,
            10.0,
        );
        let source_id = paint.id;
        document.commit(paint.clone())?;
        let polygons = vec![
            vec![
                Pos2::new(180.0, 200.0),
                Pos2::new(200.0, 200.0),
                Pos2::new(200.0, 300.0),
                Pos2::new(180.0, 300.0),
            ],
            vec![
                Pos2::new(280.0, 200.0),
                Pos2::new(300.0, 200.0),
                Pos2::new(300.0, 300.0),
                Pos2::new(280.0, 300.0),
            ],
        ];
        let changes = subtract_paint_sources(vec![paint], &polygons, |source, polygon| {
            subtract_paint_stroke_by_polygon(source, &camera, rect, polygon)
        })
        .expect("valid polygon subtraction");
        let replacement_ids = document
            .commit_paint_subtraction(changes, DEFAULT_LAYER_ID)?
            .expect("lasso changes Paint");

        assert_eq!(replacement_ids.len(), 3);
        assert_eq!(document.revision(), 2);
        assert!(
            document.operations().iter().all(|operation| {
                operation.kind == EditKind::Paint && operation.id != source_id
            })
        );
        assert!(document.undo()?);
        assert_eq!(document.operations()[0].id, source_id);
        Ok(())
    }

    #[test]
    fn vector_eraser_pipeline_commits_only_paint_replacements() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let camera = CameraAddress::default();
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(512.0, 512.0));
        let paint = EditOperation::draft(
            EditKind::Paint,
            camera.depth,
            camera.zoom,
            vec![
                camera.screen_to_canvas(100.0, 256.0, 512.0, 512.0),
                camera.screen_to_canvas(400.0, 256.0, 512.0, 512.0),
            ],
            Color::BLACK,
            10.0,
        );
        let source_id = paint.id;
        document.commit(paint.clone())?;

        let outside =
            subtract_paint_stroke_by_path(&paint, &camera, rect, &[Pos2::new(250.0, 50.0)], 20.0)
                .expect("valid outside mask");
        assert!(
            document
                .commit_paint_subtraction(
                    vec![(source_id, outside)],
                    crate::model::DEFAULT_LAYER_ID,
                )?
                .is_none()
        );
        assert_eq!(document.revision(), 1);

        let crossing = subtract_paint_stroke_by_path(
            &paint,
            &camera,
            rect,
            &[Pos2::new(250.0, 100.0), Pos2::new(250.0, 400.0)],
            20.0,
        )
        .expect("valid crossing mask");
        let replacement_ids = document
            .commit_paint_subtraction(vec![(source_id, crossing)], crate::model::DEFAULT_LAYER_ID)?
            .expect("crossing changes Paint");

        assert_eq!(replacement_ids.len(), 2);
        assert_eq!(document.revision(), 2);
        assert!(
            document
                .operations()
                .iter()
                .all(|operation| operation.kind == EditKind::Paint)
        );
        assert!(document.undo()?);
        assert_eq!(document.operations()[0].id, source_id);
        Ok(())
    }

    #[test]
    fn mixed_eraser_subtracts_paint_fill_and_compact_block_in_one_history_step()
    -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        let camera = CameraAddress::default();
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(512.0, 512.0));
        let paint = EditOperation::draft(
            EditKind::Paint,
            camera.depth,
            camera.zoom,
            vec![
                camera.screen_to_canvas(100.0, 256.0, 512.0, 512.0),
                camera.screen_to_canvas(400.0, 256.0, 512.0, 512.0),
            ],
            Color::BLACK,
            10.0,
        );
        let fill_points = [
            (150.0, 180.0),
            (350.0, 180.0),
            (350.0, 330.0),
            (150.0, 330.0),
            (150.0, 180.0),
        ]
        .into_iter()
        .map(|(x, y)| camera.screen_to_canvas(x, y, 512.0, 512.0))
        .collect::<Vec<_>>();
        let mut fill = EditOperation::draft(
            EditKind::Fill,
            camera.depth,
            camera.zoom,
            fill_points.clone(),
            Color::rgba(40, 80, 160, 255),
            0.0,
        );
        fill.smooth_area = true;
        let inner_paint = EditOperation::draft(
            EditKind::Paint,
            camera.depth,
            camera.zoom,
            vec![
                camera.screen_to_canvas(100.0, 300.0, 512.0, 512.0),
                camera.screen_to_canvas(400.0, 300.0, 512.0, 512.0),
            ],
            Color::rgba(180, 30, 50, 255),
            8.0,
        );
        let mut inner_fill = EditOperation::draft(
            EditKind::Fill,
            camera.depth,
            camera.zoom,
            fill_points.clone(),
            Color::rgba(20, 150, 60, 255),
            0.0,
        );
        inner_fill.smooth_area = true;
        let legacy_erase = EditOperation::draft(
            EditKind::EraseArea,
            camera.depth,
            camera.zoom,
            fill_points.clone(),
            Color::WHITE,
            0.0,
        );
        let block = EditOperation::compact_block(
            DEFAULT_LAYER_ID,
            fill_points[..4].to_vec(),
            vec![inner_paint, inner_fill, legacy_erase],
        );
        let original_ids = HashSet::from([paint.id, fill.id, block.id]);
        document.commit(paint)?;
        document.commit(fill)?;
        document.commit(block)?;

        let paths = vec![(vec![Pos2::new(250.0, 100.0), Pos2::new(250.0, 400.0)], 20.0)];
        let fill_masks = eraser_path_fill_masks(&paths, &camera, rect)?;
        let sources = document.operations().to_vec();
        let changes = subtract_eraser_sources(&sources, &fill_masks, &camera, |source| {
            subtract_paint_stroke_by_path(source, &camera, rect, &paths[0].0, paths[0].1)
                .context("valid Paint subtraction")
        })?;
        assert_eq!(changes.len(), 3);
        let replacement_ids = document
            .commit_eraser_subtraction(changes, DEFAULT_LAYER_ID)?
            .expect("mixed eraser changes all sources");
        assert!(replacement_ids.len() > 3);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.transaction_id)
                .collect::<HashSet<_>>()
                .len(),
            1
        );
        let compact = document
            .operations()
            .iter()
            .find(|operation| operation.is_compact_block())
            .expect("CompactBlock replacement");
        assert!(compact.compact_sources.len() > 3);
        assert!(
            compact
                .compact_sources
                .iter()
                .any(|source| { source.kind == EditKind::Fill && source.fill_group_id.is_some() })
        );
        assert!(
            document
                .operations()
                .iter()
                .filter(|source| source.kind == EditKind::Fill)
                .all(|source| !source.smooth_area)
        );
        assert!(
            compact
                .compact_sources
                .iter()
                .filter(|source| source.kind == EditKind::Fill)
                .all(|source| !source.smooth_area)
        );
        assert!(
            compact
                .compact_sources
                .iter()
                .any(|source| source.kind == EditKind::EraseArea)
        );
        assert!(document.undo()?);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<HashSet<_>>(),
            original_ids
        );
        assert!(document.redo()?);
        drop(document);

        let document = CanvasDocument::open(&root)?;
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<HashSet<_>>(),
            replacement_ids.into_iter().collect()
        );
        Ok(())
    }

    #[test]
    fn mixed_eraser_skips_a_disjoint_untriangulatable_fill_inside_compact_block()
    -> anyhow::Result<()> {
        let camera = CameraAddress::default();
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(512.0, 512.0));
        let paint = EditOperation::draft(
            EditKind::Paint,
            camera.depth,
            camera.zoom,
            vec![
                camera.screen_to_canvas(100.0, 256.0, 512.0, 512.0),
                camera.screen_to_canvas(400.0, 256.0, 512.0, 512.0),
            ],
            Color::BLACK,
            10.0,
        );
        let invalid_fill = EditOperation::draft(
            EditKind::Fill,
            camera.depth,
            camera.zoom,
            [
                (5_000.0, 0.0),
                (5_100.0, 100.0),
                (5_000.0, 100.0),
                (5_100.0, 0.0),
                (5_000.0, 0.0),
            ]
            .into_iter()
            .map(|(x, y)| camera.screen_to_canvas(x, y, 512.0, 512.0))
            .collect(),
            Color::BLACK,
            0.0,
        );
        let invalid_fill_id = invalid_fill.id;
        let block = EditOperation::compact_block(
            DEFAULT_LAYER_ID,
            paint.points.clone(),
            vec![paint, invalid_fill],
        );
        let path = vec![Pos2::new(250.0, 100.0), Pos2::new(250.0, 400.0)];
        let fill_masks = eraser_path_fill_masks(&[(path.clone(), 20.0)], &camera, rect)?;

        let changes = subtract_eraser_sources(&[block], &fill_masks, &camera, |source| {
            subtract_paint_stroke_by_path(source, &camera, rect, &path, 20.0)
                .context("valid Paint subtraction")
        })?;

        assert_eq!(changes.len(), 1);
        assert!(
            changes[0].1[0]
                .compact_sources
                .iter()
                .any(|source| { source.id == invalid_fill_id && source.kind == EditKind::Fill })
        );
        assert!(
            changes[0].1[0]
                .compact_sources
                .iter()
                .filter(|source| source.kind == EditKind::Paint)
                .count()
                > 1
        );
        Ok(())
    }

    #[test]
    fn mixed_erasers_preserve_partially_intersecting_legacy_invalid_fills() -> anyhow::Result<()> {
        for lasso in [false, true] {
            let temporary = TempDir::new()?;
            let root = temporary.path().join("test.esketch");
            let mut document = CanvasDocument::open(&root)?;
            let camera = CameraAddress::default();
            let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(512.0, 512.0));
            let paint = EditOperation::draft(
                EditKind::Paint,
                camera.depth,
                camera.zoom,
                vec![
                    camera.screen_to_canvas(100.0, 256.0, 512.0, 512.0),
                    camera.screen_to_canvas(400.0, 256.0, 512.0, 512.0),
                ],
                Color::BLACK,
                10.0,
            );
            let degenerate_points = [150.0, 200.0, 350.0, 150.0]
                .into_iter()
                .map(|y| camera.screen_to_canvas(250.0, y, 512.0, 512.0))
                .collect::<Vec<_>>();
            let malformed_points = [180.0, 250.0]
                .into_iter()
                .map(|y| camera.screen_to_canvas(250.0, y, 512.0, 512.0))
                .collect::<Vec<_>>();
            let degenerate_fill = EditOperation::draft(
                EditKind::Fill,
                camera.depth,
                camera.zoom,
                degenerate_points.clone(),
                Color::BLACK,
                0.0,
            );
            let malformed_fill = EditOperation::draft(
                EditKind::Fill,
                camera.depth,
                camera.zoom,
                malformed_points.clone(),
                Color::BLACK,
                0.0,
            );
            let block = EditOperation::compact_block(
                DEFAULT_LAYER_ID,
                paint.points.clone(),
                vec![paint, degenerate_fill, malformed_fill],
            );
            let block_id = block.id;
            document.commit(block)?;

            let path = vec![Pos2::new(250.0, 175.0), Pos2::new(250.0, 300.0)];
            let polygon = vec![
                Pos2::new(240.0, 175.0),
                Pos2::new(260.0, 175.0),
                Pos2::new(260.0, 300.0),
                Pos2::new(240.0, 300.0),
            ];
            let fill_masks = if lasso {
                vec![
                    polygon
                        .iter()
                        .map(|point| {
                            camera.screen_to_canvas(point.x as f64, point.y as f64, 512.0, 512.0)
                        })
                        .collect(),
                ]
            } else {
                eraser_path_fill_masks(&[(path.clone(), 20.0)], &camera, rect)?
            };
            let changes =
                subtract_eraser_sources(document.operations(), &fill_masks, &camera, |source| {
                    if lasso {
                        subtract_paint_stroke_by_polygon(source, &camera, rect, &polygon)
                    } else {
                        subtract_paint_stroke_by_path(source, &camera, rect, &path, 20.0)
                    }
                    .context("valid Paint subtraction")
                })?;

            document
                .commit_eraser_subtraction(changes, DEFAULT_LAYER_ID)?
                .expect("Paint and intersecting legacy Fill geometry change together");
            let compact = &document.operations()[0];
            assert!(
                compact
                    .compact_sources
                    .iter()
                    .any(|source| source.points == degenerate_points)
            );
            assert!(
                compact
                    .compact_sources
                    .iter()
                    .all(|source| source.points != malformed_points)
            );
            assert!(document.undo()?);
            assert_eq!(document.operations()[0].id, block_id);
            assert!(document.redo()?);
            drop(document);
            let reopened = CanvasDocument::open(&root)?;
            assert_eq!(reopened.operations().len(), 1);
            assert!(
                reopened.operations()[0]
                    .compact_sources
                    .iter()
                    .any(|source| source.points == degenerate_points)
            );
        }
        Ok(())
    }

    #[test]
    fn mixed_eraser_lasso_subtracts_paint_and_fill_together() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let camera = CameraAddress::default();
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(512.0, 512.0));
        let paint = EditOperation::draft(
            EditKind::Paint,
            camera.depth,
            camera.zoom,
            vec![
                camera.screen_to_canvas(100.0, 256.0, 512.0, 512.0),
                camera.screen_to_canvas(400.0, 256.0, 512.0, 512.0),
            ],
            Color::BLACK,
            10.0,
        );
        let fill = EditOperation::draft(
            EditKind::Fill,
            camera.depth,
            camera.zoom,
            [
                (150.0, 180.0),
                (350.0, 180.0),
                (350.0, 330.0),
                (150.0, 330.0),
                (150.0, 180.0),
            ]
            .into_iter()
            .map(|(x, y)| camera.screen_to_canvas(x, y, 512.0, 512.0))
            .collect(),
            Color::rgba(40, 80, 160, 255),
            0.0,
        );
        let original_ids = HashSet::from([paint.id, fill.id]);
        document.commit(paint)?;
        document.commit(fill)?;
        let polygon = vec![
            Pos2::new(230.0, 100.0),
            Pos2::new(270.0, 100.0),
            Pos2::new(270.0, 400.0),
            Pos2::new(230.0, 400.0),
        ];
        let fill_masks = vec![
            polygon
                .iter()
                .map(|point| camera.screen_to_canvas(point.x as f64, point.y as f64, 512.0, 512.0))
                .collect::<Vec<_>>(),
        ];
        let changes =
            subtract_eraser_sources(document.operations(), &fill_masks, &camera, |source| {
                subtract_paint_stroke_by_polygon(source, &camera, rect, &polygon)
                    .context("valid Paint subtraction")
            })?;
        assert_eq!(changes.len(), 2);
        document
            .commit_eraser_subtraction(changes, DEFAULT_LAYER_ID)?
            .expect("lasso changes Paint and Fill");
        assert!(document.undo()?);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<HashSet<_>>(),
            original_ids
        );
        Ok(())
    }

    #[test]
    fn fill_eraser_mask_simplifies_a_dense_straight_path_to_two_capsules_at_most()
    -> anyhow::Result<()> {
        let camera = CameraAddress::default();
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(512.0, 512.0));
        let path = (0..200)
            .map(|step| Pos2::new(100.0 + step as f32, 256.0))
            .collect::<Vec<_>>();
        let masks = eraser_path_fill_masks(&[(path, 20.0)], &camera, rect)?;

        assert!(masks.len() <= 2, "masks={}", masks.len());
        assert!(masks.iter().all(|mask| mask.len() == 18));
        Ok(())
    }

    #[test]
    fn generated_eraser_capsules_are_triangulatable() {
        for radius in [0.5, 1.0, 10.0, 250.0] {
            for length in [0.0001, 0.001, 0.01, 0.1, 0.24, 0.25, 1.0, 100.0] {
                for angle in [0.0_f32, 0.3, 1.0, 2.4] {
                    let end = Pos2::new(angle.cos() * length, angle.sin() * length);
                    let polygon = capsule_mask_polygon(Pos2::ZERO, end, radius);
                    triangulate_fill_polygon(&polygon, "mask").unwrap_or_else(|error| {
                        panic!("radius={radius} length={length} angle={angle}: {error:#}")
                    });
                }
            }
        }
    }

    #[test]
    fn mixed_eraser_noop_error_and_full_compact_delete_are_atomic() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let camera = CameraAddress::default();
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(512.0, 512.0));
        let paint = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![
                camera.screen_to_canvas(100.0, 256.0, 512.0, 512.0),
                camera.screen_to_canvas(400.0, 256.0, 512.0, 512.0),
            ],
            Color::BLACK,
            10.0,
        );
        let fill_points = [
            (100.0, 180.0),
            (400.0, 180.0),
            (400.0, 330.0),
            (100.0, 330.0),
            (100.0, 180.0),
        ]
        .into_iter()
        .map(|(x, y)| camera.screen_to_canvas(x, y, 512.0, 512.0))
        .collect::<Vec<_>>();
        let fill = EditOperation::draft(
            EditKind::Fill,
            0,
            1.0,
            fill_points.clone(),
            Color::BLACK,
            0.0,
        );
        let block = EditOperation::compact_block(
            DEFAULT_LAYER_ID,
            fill_points[..4].to_vec(),
            vec![paint, fill],
        );
        let block_id = block.id;
        document.commit(block)?;
        let revision = document.revision();

        let outside = vec![
            Pos2::new(450.0, 20.0),
            Pos2::new(500.0, 20.0),
            Pos2::new(500.0, 80.0),
            Pos2::new(450.0, 80.0),
        ];
        let outside_mask = vec![
            outside
                .iter()
                .map(|point| camera.screen_to_canvas(point.x as f64, point.y as f64, 512.0, 512.0))
                .collect::<Vec<_>>(),
        ];
        let changes =
            subtract_eraser_sources(document.operations(), &outside_mask, &camera, |source| {
                subtract_paint_stroke_by_polygon(source, &camera, rect, &outside)
                    .context("valid outside mask")
            })?;
        assert!(changes.is_empty());
        assert!(
            document
                .commit_eraser_subtraction(changes, DEFAULT_LAYER_ID)?
                .is_none()
        );
        assert_eq!(document.revision(), revision);

        let mut invalid_mask = outside_mask[0].clone();
        invalid_mask[0].local_x = f64::NAN;
        assert!(
            subtract_eraser_sources(document.operations(), &[invalid_mask], &camera, |source| {
                subtract_paint_stroke_by_polygon(source, &camera, rect, &outside)
                    .context("valid Paint mask")
            })
            .is_err()
        );
        assert_eq!(document.revision(), revision);
        assert_eq!(document.operations()[0].id, block_id);

        let covering = vec![
            Pos2::new(0.0, 0.0),
            Pos2::new(512.0, 0.0),
            Pos2::new(512.0, 512.0),
            Pos2::new(0.0, 512.0),
        ];
        let covering_mask = vec![
            covering
                .iter()
                .map(|point| camera.screen_to_canvas(point.x as f64, point.y as f64, 512.0, 512.0))
                .collect::<Vec<_>>(),
        ];
        let changes =
            subtract_eraser_sources(document.operations(), &covering_mask, &camera, |source| {
                subtract_paint_stroke_by_polygon(source, &camera, rect, &covering)
                    .context("valid covering mask")
            })?;
        let replacements = document
            .commit_eraser_subtraction(changes, DEFAULT_LAYER_ID)?
            .expect("covering mask deletes CompactBlock");
        assert!(replacements.is_empty());
        assert!(document.operations().is_empty());
        assert!(document.undo()?);
        assert_eq!(document.operations()[0].id, block_id);
        Ok(())
    }

    #[test]
    fn fill_group_subtraction_replaces_untouched_siblings_atomically() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let camera = CameraAddress::default();
        let group_id = Uuid::new_v4();
        let transaction_id = Uuid::new_v4();
        let fill = |coordinates: &[(f64, f64)]| {
            let mut operation = EditOperation::draft(
                EditKind::Fill,
                0,
                1.0,
                coordinates
                    .iter()
                    .map(|(x, y)| camera.screen_to_canvas(*x, *y, 512.0, 512.0))
                    .collect(),
                Color::rgba(50, 100, 180, 255),
                0.0,
            );
            operation.paint_order = Some(1);
            operation.fill_group_id = Some(group_id);
            operation.transaction_id = transaction_id;
            operation
        };
        let left = fill(&[
            (100.0, 180.0),
            (250.0, 180.0),
            (250.0, 330.0),
            (100.0, 330.0),
            (100.0, 180.0),
        ]);
        let right = fill(&[
            (250.0, 180.0),
            (400.0, 180.0),
            (400.0, 330.0),
            (250.0, 330.0),
            (250.0, 180.0),
        ]);
        let original_ids = HashSet::from([left.id, right.id]);
        let right_points = right.points.clone();
        document.commit_group(vec![left, right])?;
        let mask = vec![
            camera.screen_to_canvas(120.0, 200.0, 512.0, 512.0),
            camera.screen_to_canvas(180.0, 200.0, 512.0, 512.0),
            camera.screen_to_canvas(180.0, 300.0, 512.0, 512.0),
            camera.screen_to_canvas(120.0, 300.0, 512.0, 512.0),
        ];
        let changes =
            subtract_eraser_sources(document.operations(), &[mask], &camera, |_| Ok(Vec::new()))?;
        assert_eq!(changes.len(), 2);
        assert!(changes.iter().any(|(_, replacements)| {
            replacements.len() == 1 && replacements[0].points == right_points
        }));
        document
            .commit_eraser_subtraction(changes, DEFAULT_LAYER_ID)?
            .expect("one changed member replaces the whole Fill group");
        let replacement_groups = document
            .operations()
            .iter()
            .filter_map(|operation| operation.fill_group_id)
            .collect::<HashSet<_>>();
        assert_eq!(replacement_groups.len(), 1);
        assert!(!replacement_groups.contains(&group_id));
        assert!(document.undo()?);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<HashSet<_>>(),
            original_ids
        );
        Ok(())
    }

    fn test_canvas_polygon(depth: i64, coordinates: &[(f64, f64)]) -> Vec<CanvasPoint> {
        test_canvas_polygon_at(depth, BigInt::from(0), coordinates)
    }

    fn test_canvas_polygon_at(
        depth: i64,
        tile: BigInt,
        coordinates: &[(f64, f64)],
    ) -> Vec<CanvasPoint> {
        let mut points = coordinates
            .iter()
            .map(|(x, y)| CanvasPoint::new(depth, tile.clone(), tile.clone(), *x, *y))
            .collect::<Vec<_>>();
        if let Some(first) = points.first().cloned() {
            points.push(first);
        }
        points
    }

    fn test_fill_fragment_operations(
        source: &EditOperation,
        masks: &[EditOperation],
    ) -> anyhow::Result<Vec<EditOperation>> {
        let fill_group_id = Uuid::new_v4();
        let mask_points = masks
            .iter()
            .map(|mask| mask.points.clone())
            .collect::<Vec<_>>();
        subtract_fill_polygon(&source.points, &mask_points).map(|fragments| {
            fragments
                .into_iter()
                .map(|points| {
                    let mut fragment = source.clone();
                    fragment.id = Uuid::new_v4();
                    fragment.fill_group_id = Some(fill_group_id);
                    fragment.points = points;
                    fragment
                })
                .collect()
        })
    }

    fn test_fallback_fill_pixels(
        operations: &[(Vec<Vec<Pos2>>, Color32)],
        size: usize,
    ) -> Vec<Color32> {
        let context = egui::Context::default();
        let clip_rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(size as f32, size as f32));
        let output = context.run_ui(egui::RawInput::default(), |ui| {
            let painter = ui.painter().with_clip_rect(clip_rect);
            for (polygons, color) in operations {
                let polygon_slices = polygons.iter().map(Vec::as_slice).collect::<Vec<_>>();
                paint_fill_group_scanlines(&painter, clip_rect, &polygon_slices, *color);
            }
        });
        let mut pixels = vec![Color32::from_rgb(250, 250, 248); size * size];
        for clipped_shape in output.shapes {
            let Shape::Rect(shape) = clipped_shape.shape else {
                panic!("fallback Fill emitted a non-rectangle shape");
            };
            let painted = shape.rect.intersect(clipped_shape.clip_rect);
            for y in 0..size {
                for x in 0..size {
                    if painted.contains(Pos2::new(x as f32 + 0.5, y as f32 + 0.5)) {
                        pixels[y * size + x] = shape.fill;
                    }
                }
            }
        }
        pixels
    }

    fn test_projected_polygon(points: &[CanvasPoint], depth: i64) -> Vec<Pos2> {
        let camera = CameraAddress {
            depth,
            tile_x: 0.into(),
            tile_y: 0.into(),
            local_x: 0.0,
            local_y: 0.0,
            zoom: 1.0,
        };
        normalized_pos_polygon(
            &points
                .iter()
                .map(|point| {
                    let (x, y) = camera
                        .canvas_to_screen(point, 0.0, 0.0)
                        .expect("test point is representable");
                    Pos2::new(x as f32, y as f32)
                })
                .collect::<Vec<_>>(),
        )
    }

    fn test_fill_area(fragments: &[Vec<CanvasPoint>], depth: i64) -> f32 {
        fragments
            .iter()
            .map(|fragment| {
                polygon_area_twice(&test_projected_polygon(fragment, depth)).abs() * 0.5
            })
            .sum()
    }

    fn assert_test_fill_area(fragments: &[Vec<CanvasPoint>], depth: i64, local_area: f32) {
        let expected = local_area * 512.0 * 512.0;
        let actual = test_fill_area(fragments, depth);
        assert!(
            (actual - expected).abs() <= expected.abs().max(1.0) * 1.0e-4,
            "actual={actual} expected={expected} fragments={}",
            fragments.len()
        );
    }

    fn assert_test_fill_fragments_do_not_overlap(fragments: &[Vec<CanvasPoint>], depth: i64) {
        let projected = fragments
            .iter()
            .map(|fragment| test_projected_polygon(fragment, depth))
            .collect::<Vec<_>>();
        for left in 0..projected.len() {
            for right in left + 1..projected.len() {
                let overlap =
                    clip_polygon_to_convex_pos_polygon(&projected[left], &projected[right]);
                assert!(
                    polygon_area_twice(&overlap).abs() < 1.0,
                    "fragments {left} and {right} overlap"
                );
            }
        }
    }

    #[test]
    fn fill_polygon_difference_preserves_exact_no_op_depth_and_winding() -> anyhow::Result<()> {
        let depth = 37;
        let source = test_canvas_polygon(depth, &[(0.1, 0.1), (0.9, 0.1), (0.9, 0.9), (0.1, 0.9)]);
        let disjoint_reversed =
            test_canvas_polygon(depth, &[(1.2, 0.8), (1.4, 0.8), (1.4, 0.2), (1.2, 0.2)]);
        let tangent = test_canvas_polygon(depth, &[(0.9, 0.3), (1.0, 0.3), (1.0, 0.7), (0.9, 0.7)]);

        let fragments = subtract_fill_polygon(&source, &[disjoint_reversed, tangent])?;

        assert_eq!(fragments, vec![source]);
        assert!(fragments.iter().flatten().all(|point| point.depth == depth));
        Ok(())
    }

    #[test]
    fn mixed_eraser_keeps_a_visible_cross_depth_fill_mask_representable() -> anyhow::Result<()> {
        let view = CameraAddress {
            depth: -1,
            tile_x: BigInt::from(0),
            tile_y: BigInt::from(0),
            local_x: 0.5,
            local_y: 0.5,
            zoom: 3.0,
        };
        let mut source_camera = view.clone();
        source_camera.jump_to_depth(-5);
        source_camera.zoom *= (crate::coords::DEPTH_RATIO as f64).powi(4);
        let polygon = |camera: &CameraAddress, min: f64, max: f64| {
            [(min, min), (max, min), (max, max), (min, max), (min, min)]
                .into_iter()
                .map(|(x, y)| camera.screen_to_canvas(x, y, 512.0, 512.0))
                .collect::<Vec<_>>()
        };
        let source = polygon(&source_camera, -6_000.0, 6_500.0);
        let mask = polygon(&view, 246.0, 266.0);
        let fill = EditOperation::draft(
            EditKind::Fill,
            -5,
            source_camera.zoom,
            source.clone(),
            Color::BLACK,
            0.0,
        );

        let changes = subtract_eraser_sources(&[fill], &[mask], &view, |_| Ok(Vec::new()))?;

        assert_eq!(changes.len(), 1);
        assert_ne!(changes[0].1[0].points, source);
        assert!(
            changes[0]
                .1
                .iter()
                .flat_map(|fragment| &fragment.points)
                .all(|point| point.depth == -5)
        );
        let projected = changes[0]
            .1
            .iter()
            .map(|fragment| {
                fragment
                    .points
                    .iter()
                    .map(|point| {
                        let (x, y) = view
                            .canvas_to_screen(point, 512.0, 512.0)
                            .expect("replacement remains visible at the erase camera");
                        Pos2::new(x as f32, y as f32)
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert!(
            !projected
                .iter()
                .any(|fragment| point_in_pos_polygon(Pos2::new(256.0, 256.0), fragment))
        );
        assert!(
            projected
                .iter()
                .any(|fragment| point_in_pos_polygon(Pos2::new(200.0, 200.0), fragment))
        );
        Ok(())
    }

    #[test]
    fn fill_polygon_difference_handles_crossing_containment_and_hole() -> anyhow::Result<()> {
        let depth = -12;
        let source = test_canvas_polygon(depth, &[(0.1, 0.1), (0.9, 0.1), (0.9, 0.9), (0.1, 0.9)]);
        let crossing =
            test_canvas_polygon(depth, &[(0.45, 0.0), (0.55, 0.0), (0.55, 1.0), (0.45, 1.0)]);
        let crossed = subtract_fill_polygon(&source, &[crossing])?;
        assert_test_fill_area(&crossed, depth, 0.56);
        assert!(crossed.len() >= 2);

        let hole = test_canvas_polygon(depth, &[(0.4, 0.4), (0.6, 0.4), (0.6, 0.6), (0.4, 0.6)]);
        let holed = subtract_fill_polygon(&source, &[hole])?;
        assert_test_fill_area(&holed, depth, 0.60);
        assert_test_fill_fragments_do_not_overlap(&holed, depth);
        assert!(holed.len() > 1);
        assert!(holed.iter().all(|fragment| {
            fragment.first() == fragment.last() && fragment.iter().all(|point| point.depth == depth)
        }));

        let containing_reversed =
            test_canvas_polygon(depth, &[(0.0, 1.0), (1.0, 1.0), (1.0, 0.0), (0.0, 0.0)]);
        assert!(subtract_fill_polygon(&source, &[containing_reversed])?.is_empty());
        Ok(())
    }

    #[test]
    fn fill_polygon_difference_handles_concave_source_mask_and_multiple_masks() -> anyhow::Result<()>
    {
        let depth = 5;
        let source = test_canvas_polygon(
            depth,
            &[
                (0.1, 0.1),
                (0.9, 0.1),
                (0.9, 0.4),
                (0.4, 0.4),
                (0.4, 0.9),
                (0.1, 0.9),
            ],
        );
        let concave_mask = test_canvas_polygon(
            depth,
            &[
                (0.2, 0.15),
                (0.8, 0.15),
                (0.8, 0.25),
                (0.4, 0.25),
                (0.4, 0.35),
                (0.2, 0.35),
            ],
        );
        let second_mask =
            test_canvas_polygon(depth, &[(0.15, 0.6), (0.25, 0.6), (0.25, 0.8), (0.15, 0.8)]);

        let fragments = subtract_fill_polygon(&source, &[concave_mask, second_mask])?;

        assert_test_fill_area(&fragments, depth, 0.29);
        assert_test_fill_fragments_do_not_overlap(&fragments, depth);
        assert!(fragments.len() > 2);
        Ok(())
    }

    #[test]
    fn fill_polygon_difference_preserves_degenerate_source_and_rejects_invalid_masks() {
        let depth = 0;
        let triangle = test_canvas_polygon(depth, &[(0.1, 0.1), (0.9, 0.1), (0.5, 0.9)]);
        let crossing =
            test_canvas_polygon(depth, &[(0.45, 0.0), (0.55, 0.0), (0.55, 1.0), (0.45, 1.0)]);
        let degenerate = test_canvas_polygon(depth, &[(0.1, 0.1), (0.2, 0.2), (0.3, 0.3)]);
        assert_eq!(
            subtract_fill_polygon(&degenerate, &[]).unwrap(),
            vec![degenerate.clone()]
        );
        assert!(subtract_fill_polygon(&triangle, &[degenerate]).is_err());

        let mut nonfinite = triangle.clone();
        nonfinite[1].local_x = f64::NAN;
        assert!(subtract_fill_polygon(&nonfinite, &[]).is_err());

        let error = subtract_fill_polygon_with_limit(&triangle, &[crossing], 1)
            .expect_err("crossing must exceed one fragment");
        assert!(error.to_string().contains("1-fragment limit"));
    }

    #[test]
    fn fill_polygon_difference_erases_subpixel_fragments_that_can_be_zoomed_later() {
        let source =
            test_canvas_polygon(0, &[(0.0, 0.0), (0.001, 0.0), (0.001, 0.001), (0.0, 0.001)]);
        let mask = test_canvas_polygon(
            0,
            &[
                (-0.001, -0.001),
                (0.002, -0.001),
                (0.002, 0.002),
                (-0.001, 0.002),
            ],
        );

        assert!(subtract_fill_polygon(&source, &[mask]).unwrap().is_empty());
    }

    #[test]
    fn fill_polygon_difference_stress_stays_below_fixed_fragment_limit() -> anyhow::Result<()> {
        let depth = 0;
        let source = test_canvas_polygon(
            depth,
            &[(0.05, 0.05), (0.95, 0.05), (0.95, 0.95), (0.05, 0.95)],
        );
        let masks = (0..48)
            .map(|index| {
                let left = 0.06 + index as f64 * 0.018;
                test_canvas_polygon(
                    depth,
                    &[
                        (left, 0.04),
                        (left + 0.006, 0.04),
                        (left + 0.006, 0.96),
                        (left, 0.96),
                    ],
                )
            })
            .collect::<Vec<_>>();

        let fragments = subtract_fill_polygon(&source, &masks)?;

        assert!(fragments.len() <= 256, "fragments={}", fragments.len());
        assert_eq!(fragments.len(), 98);
        assert_test_fill_fragments_do_not_overlap(&fragments, depth);
        assert_test_fill_area(&fragments, depth, 0.81 - 48.0 * 0.006 * 0.9);
        Ok(())
    }

    #[test]
    fn fill_group_render_removes_raster_seams_and_preserves_fallback_edges() -> anyhow::Result<()> {
        let depth = 0;
        let mut source = EditOperation::draft(
            EditKind::Fill,
            depth,
            1.0,
            test_canvas_polygon(depth, &[(0.1, 0.1), (0.9, 0.1), (0.9, 0.9), (0.1, 0.9)]),
            Color::BLACK,
            0.0,
        );
        source.sequence = 10;
        source.paint_order = Some(10);
        let mut mask = EditOperation::draft(
            EditKind::EraseArea,
            depth,
            1.0,
            test_canvas_polygon(
                depth,
                &[
                    (0.3, 0.3),
                    (0.7, 0.3),
                    (0.7, 0.45),
                    (0.5, 0.45),
                    (0.5, 0.7),
                    (0.3, 0.7),
                ],
            ),
            Color::WHITE,
            0.0,
        );
        mask.sequence = 11;
        mask.paint_order = Some(11);
        let mut later = EditOperation::draft(
            EditKind::Fill,
            depth,
            1.0,
            test_canvas_polygon(depth, &[(0.42, 0.5), (0.58, 0.5), (0.58, 0.6), (0.42, 0.6)]),
            Color::rgba(220, 30, 30, 255),
            0.0,
        );
        later.sequence = 12;
        later.paint_order = Some(12);

        let mut fragment_operations = test_fill_fragment_operations(&source, &[mask.clone()])?;
        assert!(fragment_operations.iter().all(|fragment| {
            fragment.kind == EditKind::Fill
                && fragment.color == source.color
                && fragment.paint_order == source.paint_order
        }));
        fragment_operations.push(later.clone());
        let legacy_operations = vec![source, mask.clone(), later];
        let fallback_operations = |operations: &[EditOperation]| {
            let mut groups = Vec::<(Vec<Vec<Pos2>>, Color32)>::new();
            for (index, operation) in operations.iter().enumerate() {
                let color = operation.opaque_visible_color(Color::WHITE);
                let points = test_projected_polygon(&operation.points, depth)
                    .into_iter()
                    .map(|point| Pos2::new(point.x * 0.25, point.y * 0.25))
                    .collect();
                if index > 0 && operations[index - 1].shares_fill_group_with(operation) {
                    groups
                        .last_mut()
                        .expect("previous fallback group")
                        .0
                        .push(points);
                } else {
                    groups.push((
                        vec![points],
                        Color32::from_rgba_unmultiplied(color.r, color.g, color.b, u8::MAX),
                    ));
                }
            }
            groups
        };
        let fragment_fallback =
            test_fallback_fill_pixels(&fallback_operations(&fragment_operations), 128);
        let mut ungrouped_fragment_operations = fragment_operations.clone();
        for operation in &mut ungrouped_fragment_operations {
            operation.fill_group_id = None;
        }
        let ungrouped_fragment_fallback =
            test_fallback_fill_pixels(&fallback_operations(&ungrouped_fragment_operations), 128);
        assert_eq!(fragment_fallback, ungrouped_fragment_fallback);
        let legacy_fallback =
            test_fallback_fill_pixels(&fallback_operations(&legacy_operations), 128);
        let fallback_differences = fragment_fallback
            .iter()
            .zip(&legacy_fallback)
            .enumerate()
            .filter_map(|(index, (fragment, legacy))| {
                (fragment != legacy).then_some((index, *fragment, *legacy))
            })
            .collect::<Vec<_>>();
        let mask_outline = test_projected_polygon(&mask.points, depth)
            .into_iter()
            .map(|point| Point2::new(f64::from(point.x * 0.25), f64::from(point.y * 0.25)))
            .collect::<Vec<_>>();
        assert_eq!(fallback_differences.len(), 208);
        assert!(fallback_differences.iter().all(|(index, _, _)| {
            let position = Point2::new((index % 128) as f64 + 0.5, (index / 128) as f64 + 0.5);
            closed_polyline_distance(position, &mask_outline) <= 1.5
        }));
        let key = TileKey {
            depth,
            x: 0.into(),
            y: 0.into(),
            lod: tile_lod_for_resolution(128),
        };

        let mut differing_pixel_counts = Vec::new();
        for edge_quality in 0..=2 {
            let options = RasterOptions::new(edge_quality, 0);
            let legacy = render_tile_with_options(&legacy_operations, &key, Color::WHITE, options);
            let fragments =
                render_tile_with_options(&fragment_operations, &key, Color::WHITE, options);
            let differences = fragments
                .as_raw()
                .iter()
                .zip(legacy.as_raw())
                .enumerate()
                .filter_map(|(index, (fragment, legacy))| {
                    (fragment != legacy).then_some((index, *fragment, *legacy))
                })
                .collect::<Vec<_>>();
            let width = fragments.width() as usize;
            let differing_pixels = differences
                .iter()
                .map(|(index, _, _)| index / 4)
                .collect::<HashSet<_>>();
            differing_pixel_counts.push(differing_pixels.len());
            assert!(
                differing_pixels.len() < width * width / 50,
                "edge_quality={edge_quality} differences={} first={:?}",
                differences.len(),
                differences.first()
            );
        }
        assert_eq!(differing_pixel_counts, [0, 0, 0]);
        Ok(())
    }

    #[test]
    fn fill_group_fragments_keep_object_payload_semantics_at_extreme_coordinates()
    -> anyhow::Result<()> {
        let depth = 1_200;
        let tile = BigInt::from(10u8).pow(200);
        let source = EditOperation::draft(
            EditKind::Fill,
            depth,
            1.0,
            test_canvas_polygon_at(
                depth,
                tile.clone(),
                &[(0.1, 0.1), (0.9, 0.1), (0.9, 0.9), (0.1, 0.9)],
            ),
            Color::rgba(20, 40, 60, 255),
            0.0,
        );
        let mask = EditOperation::draft(
            EditKind::EraseArea,
            depth,
            1.0,
            test_canvas_polygon_at(
                depth,
                tile.clone(),
                &[(0.4, 0.4), (0.6, 0.4), (0.6, 0.6), (0.4, 0.6)],
            ),
            Color::WHITE,
            0.0,
        );
        let fragments = test_fill_fragment_operations(&source, &[mask])?;

        assert!(fragments.len() > 1);
        assert_eq!(
            fragments
                .iter()
                .filter_map(|fragment| fragment.fill_group_id)
                .collect::<HashSet<_>>()
                .len(),
            1
        );
        assert!(
            fully_contained_object_operation_ids(
                &fragments,
                &HashSet::from([fragments[0].id]),
                DEFAULT_LAYER_ID,
            )
            .is_empty()
        );
        assert_eq!(
            fully_contained_object_operation_ids(
                &fragments,
                &fragments.iter().map(|fragment| fragment.id).collect(),
                DEFAULT_LAYER_ID,
            )
            .len(),
            fragments.len()
        );
        assert_eq!(
            fragments
                .iter()
                .map(|fragment| fragment.id)
                .collect::<HashSet<_>>()
                .len(),
            fragments.len()
        );
        assert!(fragments.iter().all(|fragment| {
            operation_bounds(fragment).is_some_and(|bounds| {
                bounds.depth == depth
                    && bounds.min_x == tile
                    && bounds.max_x == tile
                    && bounds.min_y == tile
                    && bounds.max_y == tile
            })
        }));

        let key = TileKey {
            depth,
            x: tile.clone(),
            y: tile.clone(),
            lod: tile_lod_for_resolution(128),
        };
        assert_eq!(
            OperationIndex::build(&fragments).query(&key).len(),
            fragments.len()
        );

        let camera = CameraAddress {
            depth,
            tile_x: tile.clone(),
            tile_y: tile.clone(),
            local_x: 0.5,
            local_y: 0.5,
            zoom: 1.0,
        };
        let projected = fragments
            .iter()
            .map(|fragment| {
                fragment
                    .points
                    .iter()
                    .map(|point| {
                        let (x, y) = camera
                            .canvas_to_screen(point, 512.0, 512.0)
                            .expect("same-depth extreme coordinate is representable");
                        Pos2::new(x as f32, y as f32)
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let retained_click = Pos2::new(102.4, 76.8);
        let hit_indices = projected
            .iter()
            .enumerate()
            .filter_map(|(index, points)| {
                operation_pick_hit(EditKind::Fill, points, 0.0, retained_click).then_some(index)
            })
            .collect::<Vec<_>>();
        assert_eq!(hit_indices.len(), 1, "hit_indices={hit_indices:?}");
        assert!(projected.iter().all(|points| !operation_pick_hit(
            EditKind::Fill,
            points,
            0.0,
            Pos2::new(256.0, 256.0)
        )));

        let retained_selection = SelectionShape::Rectangle(Rect2::from_points(
            Point2::new(98.0, 72.0),
            Point2::new(106.0, 80.0),
        ));
        let hole_selection = SelectionShape::Rectangle(Rect2::from_points(
            Point2::new(240.0, 240.0),
            Point2::new(272.0, 272.0),
        ));
        let selection_points = projected
            .iter()
            .map(|points| {
                points
                    .iter()
                    .map(|point| Point2::new(f64::from(point.x), f64::from(point.y)))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            selection_points
                .iter()
                .filter(|points| {
                    operation_intersects_selection(EditKind::Fill, points, 0.0, &retained_selection)
                })
                .count(),
            1
        );
        assert!(selection_points.iter().all(|points| {
            !operation_intersects_selection(EditKind::Fill, points, 0.0, &hole_selection)
                && operation_contained_by_rectangle(
                    EditKind::Fill,
                    points,
                    0.0,
                    Rect2::from_points(Point2::new(0.0, 0.0), Point2::new(512.0, 512.0)),
                )
        }));

        let transforms = [
            ScreenAffine::uniform_scale(256.0, 256.0, 0.9).unwrap(),
            ScreenAffine::rotation(256.0, 256.0, 0.2).unwrap(),
            ScreenAffine::flip_horizontal(256.0, 256.0).unwrap(),
            ScreenAffine::flip_vertical(256.0, 256.0).unwrap(),
        ];
        for transform in transforms {
            for point in fragments.iter().flat_map(|fragment| &fragment.points) {
                let transformed = transform
                    .transform_canvas_point(point, &camera, 512.0, 512.0)
                    .expect("fragment transform remains representable");
                assert_eq!(transformed.depth, depth);
                assert_eq!(transformed.tile_x, tile);
                assert_eq!(transformed.tile_y, tile);
            }
        }

        let clipboard = SelectionClipboard {
            operations: fragments.clone(),
            paste_count: 0,
        };
        let recolored = clipboard
            .operations
            .iter()
            .cloned()
            .map(|mut operation| {
                operation.color = Color::rgba(200, 100, 50, 255);
                operation
            })
            .collect::<Vec<_>>();
        assert!(
            recolored
                .iter()
                .zip(&fragments)
                .all(|(recolored, source)| recolored.points == source.points)
        );
        assert_eq!(clipboard.paste_count, 0);
        Ok(())
    }

    #[test]
    fn area_polygon_clips_fill_covering_area_into_one_selected_polygon() {
        let area = normalized_pos_polygon(&[
            Pos2::new(0.0, 0.0),
            Pos2::new(10.0, 0.0),
            Pos2::new(10.0, 10.0),
            Pos2::new(0.0, 10.0),
        ]);
        let subject = [
            Pos2::new(-5.0, -5.0),
            Pos2::new(15.0, -5.0),
            Pos2::new(15.0, 15.0),
            Pos2::new(-5.0, 15.0),
        ];

        let fragments = clip_pos_area_operation_to_polygon(&subject, &area);

        assert_eq!(fragments, vec![area]);
    }

    #[test]
    fn area_polygon_clips_partial_fill_to_single_convex_result() {
        let area = normalized_pos_polygon(&[
            Pos2::new(0.0, 0.0),
            Pos2::new(10.0, 0.0),
            Pos2::new(10.0, 10.0),
            Pos2::new(0.0, 10.0),
        ]);
        let subject = [
            Pos2::new(5.0, -5.0),
            Pos2::new(15.0, -5.0),
            Pos2::new(15.0, 15.0),
            Pos2::new(5.0, 15.0),
        ];

        let fragments = clip_pos_area_operation_to_polygon(&subject, &area);

        assert_eq!(fragments.len(), 1);
        assert!(fragments[0].iter().all(|point| point.x >= 4.99
            && point.x <= 10.01
            && point.y >= -0.01
            && point.y <= 10.01));
    }

    #[test]
    fn erase_area_prepare_closes_clipped_polygon() {
        let mut operation = EditOperation::draft(
            EditKind::EraseArea,
            0,
            1.0,
            vec![
                CanvasPoint::new(0, 0.into(), 0.into(), 0.0, 0.0),
                CanvasPoint::new(0, 0.into(), 0.into(), 1.0, 0.0),
                CanvasPoint::new(0, 0.into(), 0.into(), 1.0, 1.0),
            ],
            Color::WHITE,
            0.0,
        );

        prepare_draft_for_commit(&mut operation);

        assert_eq!(operation.points.first(), operation.points.last());
        assert!(draft_is_committable(&operation));
    }

    #[test]
    fn eraser_lasso_builds_one_closed_area_operation_on_the_active_layer() {
        let camera = CameraAddress::default();
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(512.0, 512.0));
        let layer_id = Uuid::new_v4();
        let points = [
            Pos2::new(100.0, 100.0),
            Pos2::new(300.0, 100.0),
            Pos2::new(200.0, 300.0),
        ];

        let operation =
            eraser_lasso_operation(&points, &camera, rect, layer_id).expect("valid lasso");

        assert_eq!(operation.kind, EditKind::EraseArea);
        assert_eq!(operation.layer_id, layer_id);
        assert!(operation.destructive);
        assert_eq!(operation.points.len(), 4);
        assert_eq!(operation.points.first(), operation.points.last());
        assert!(
            eraser_lasso_operation(
                &[
                    Pos2::new(10.0, 10.0),
                    Pos2::new(20.0, 20.0),
                    Pos2::new(30.0, 30.0)
                ],
                &camera,
                rect,
                layer_id
            )
            .is_none()
        );
    }

    #[test]
    fn layer_panel_counts_operations_by_layer() {
        let point = CanvasPoint::new(0, 0.into(), 0.into(), 0.5, 0.5);
        let mut first = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![point.clone(), point.clone()],
            Color::BLACK,
            5.0,
        );
        let mut second = first.clone();
        let other_layer = Uuid::from_u128(2);
        first.layer_id = DEFAULT_LAYER_ID;
        second.layer_id = other_layer;

        let counts = layer_operation_counts(&[first, second]);

        assert_eq!(counts.get(&DEFAULT_LAYER_ID), Some(&1));
        assert_eq!(counts.get(&other_layer), Some(&1));
    }

    #[test]
    fn tile_display_uses_content_rect_and_source_bleed_crop() {
        let lod = tile_lod_for_resolution(128);
        let (destination, source) = tile_display_rects(Pos2::new(10.0, 20.0), 1.25, lod);
        let texture_pixels = (tile_resolution(lod) + TILE_BLEED * 2) as f32;
        let bleed_uv = TILE_BLEED as f32 / texture_pixels;

        assert_eq!(destination.min, Pos2::new(10.0, 20.0));
        assert_eq!(destination.width(), TILE_SIZE as f32 * 1.25);
        assert_eq!(destination.height(), TILE_SIZE as f32 * 1.25);
        assert_eq!(source.min, Pos2::new(bleed_uv, bleed_uv));
        assert_eq!(source.max, Pos2::new(1.0 - bleed_uv, 1.0 - bleed_uv));
    }

    #[test]
    fn adjacent_tile_destinations_share_their_content_edge() {
        let zoom = 1.125;
        let lod = tile_lod_for_resolution(256);
        let (left, _) = tile_display_rects(Pos2::new(5.0, 0.0), zoom, lod);
        let (right, _) =
            tile_display_rects(Pos2::new(5.0 + TILE_SIZE as f32 * zoom, 0.0), zoom, lod);

        assert_eq!(left.max.x, right.min.x);
    }

    #[test]
    fn tile_fallback_uses_only_operations_newer_than_the_oldest_retained_tile() {
        assert_eq!(
            tile_fallback_mode(8, [Some((8, 12)), Some((8, 12))]),
            TileFallbackMode::Current
        );
        assert_eq!(
            tile_fallback_mode(8, [Some((8, 12)), Some((7, 10)), Some((6, 7))]),
            TileFallbackMode::OverlayAfter(7)
        );
        assert_eq!(
            tile_fallback_mode(8, [Some((8, 12)), None]),
            TileFallbackMode::Full
        );
        assert_eq!(
            tile_fallback_mode(8, [Some((9, 12))]),
            TileFallbackMode::Full
        );
    }

    #[test]
    fn distant_tiles_gate_only_distant_non_empty_views() {
        assert_eq!(
            depth_render_mode(VisibleDepthMode::Empty, true),
            DepthRenderMode::Empty
        );
        assert_eq!(
            depth_render_mode(VisibleDepthMode::Near, true),
            DepthRenderMode::VectorsNear
        );
        assert_eq!(
            depth_render_mode(VisibleDepthMode::Distant, true),
            DepthRenderMode::DistantTile
        );
        assert_eq!(
            depth_render_mode(VisibleDepthMode::Distant, false),
            DepthRenderMode::VectorsNear
        );
    }

    #[test]
    fn incremental_tile_updates_accept_only_new_opaque_paint_operations() {
        let point = CanvasPoint::new(0, 0.into(), 0.into(), 0.5, 0.5);
        let operation = |kind, color, sequence| {
            let mut operation =
                EditOperation::draft(kind, 0, 1.0, vec![point.clone(), point.clone()], color, 8.0);
            operation.sequence = sequence;
            operation
        };
        let old = operation(EditKind::Paint, Color::BLACK, 1);
        let first = operation(EditKind::Paint, Color::BLACK, 2);
        let second = operation(EditKind::Paint, Color::rgba(200, 20, 20, 255), 3);

        let incremental =
            incremental_paint_operations(1, &[old.clone(), first.clone(), second.clone()])
                .expect("opaque Paint delta");
        assert_eq!(incremental.len(), 2);
        assert_eq!(incremental[0].sequence, 2);
        assert_eq!(incremental[1].sequence, 3);
        assert!(
            incremental_paint_operations(3, &[old.clone(), first.clone(), second.clone()])
                .expect("empty tile-local delta")
                .is_empty()
        );
        let new_lower_layer = operation(EditKind::Paint, Color::BLACK, 4);
        let old_upper_layer = operation(EditKind::Paint, Color::BLACK, 3);
        assert!(
            incremental_paint_operations(3, &[new_lower_layer, old_upper_layer]).is_none(),
            "a new lower-layer operation is not a render-order suffix"
        );

        let transparent = operation(EditKind::Paint, Color::rgba(20, 20, 20, 128), 4);
        let erase = operation(EditKind::Erase, Color::WHITE, 4);
        let fill = operation(EditKind::Fill, Color::BLACK, 4);
        assert!(incremental_paint_operations(3, &[transparent]).is_none());
        assert!(incremental_paint_operations(3, &[erase]).is_none());
        assert!(incremental_paint_operations(3, &[fill]).is_none());
    }

    #[test]
    fn frame_rate_tracker_measures_active_interactions_and_resets_after_idle() {
        let mut tracker = FrameRateTracker::default();

        assert_eq!(tracker.update(1.0 / 20.0, false), None);
        assert_eq!(tracker.metrics(), None);

        let measured = tracker
            .update(1.0 / 60.0, true)
            .expect("active frame measurement");
        assert!((measured - 1000.0 / 60.0).abs() < 0.01);
        let (fps, frame_time) = tracker.metrics().expect("active measurement");
        assert!((fps - 60.0).abs() < 0.01);
        assert!((frame_time - 1000.0 / 60.0).abs() < 0.01);

        let measured = tracker
            .update(1.0 / 30.0, true)
            .expect("continued active frame measurement");
        assert!((measured - 1000.0 / 30.0).abs() < 0.01);
        let expected = 1.0 / 60.0 + (1.0 / 30.0 - 1.0 / 60.0) * FRAME_TIME_EMA_ALPHA;
        assert!(
            (tracker.metrics().expect("averaged measurement").1 - expected * 1000.0).abs() < 0.01
        );

        assert_eq!(tracker.update(1.0 / 20.0, false), None);
        assert_eq!(tracker.update(1.0 / 40.0, true), Some(25.0));
        let (fps, _) = tracker.metrics().expect("reset measurement");
        assert!((fps - 40.0).abs() < 0.01);
    }

    #[test]
    fn frame_rate_tracker_rejects_invalid_frame_times() {
        let mut tracker = FrameRateTracker::default();

        assert_eq!(tracker.update(f32::NAN, true), None);
        assert_eq!(tracker.update(0.0, true), None);
        assert_eq!(tracker.update(1.0, true), None);

        assert_eq!(tracker.metrics(), None);

        assert_eq!(
            tracker.update(MAX_MEASURED_FRAME_TIME, true),
            Some(MAX_MEASURED_FRAME_TIME * 1_000.0)
        );
    }

    #[test]
    fn manual_and_drawing_tile_pauses_are_independent() {
        assert!(!tile_generation_is_paused(false, false));
        assert!(tile_generation_is_paused(true, false));
        assert!(tile_generation_is_paused(false, true));
        assert!(tile_generation_is_paused(true, true));
    }

    #[test]
    fn deferred_drawing_preview_skips_only_live_draft_painting() {
        assert!(should_paint_live_draft(false));
        assert!(!should_paint_live_draft(true));
    }

    #[test]
    fn primary_pointer_positions_preserve_all_drag_events_in_order() {
        let events = vec![
            Event::PointerMoved(Pos2::new(0.0, 0.0)),
            Event::PointerButton {
                pos: Pos2::new(1.0, 1.0),
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            Event::PointerMoved(Pos2::new(2.0, 2.0)),
            Event::PointerMoved(Pos2::new(3.0, 3.0)),
            Event::PointerButton {
                pos: Pos2::new(4.0, 4.0),
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
            Event::PointerMoved(Pos2::new(5.0, 5.0)),
        ];

        let (press, positions, touch_history) = primary_pointer_positions(&events, false);

        assert_eq!(press, Some(Pos2::new(1.0, 1.0)));
        assert!(!touch_history);
        assert_eq!(
            positions,
            vec![
                Pos2::new(2.0, 2.0),
                Pos2::new(3.0, 3.0),
                Pos2::new(4.0, 4.0)
            ]
        );
    }

    #[test]
    fn primary_pointer_positions_restore_down_state_from_release_event() {
        let events = vec![
            Event::PointerMoved(Pos2::new(6.0, 6.0)),
            Event::PointerButton {
                pos: Pos2::new(7.0, 7.0),
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ];

        let (press, positions, touch_history) = primary_pointer_positions(&events, false);

        assert_eq!(press, None);
        assert!(!touch_history);
        assert_eq!(positions, vec![Pos2::new(6.0, 6.0), Pos2::new(7.0, 7.0)]);
    }

    #[test]
    fn primary_pointer_positions_prefer_ordered_pen_touch_history() {
        let touch = |phase, x| Event::Touch {
            device_id: egui::TouchDeviceId(1),
            id: egui::TouchId(7),
            phase,
            pos: Pos2::new(x, x),
            force: Some(0.5),
        };
        let events = vec![
            touch(egui::TouchPhase::Start, 1.0),
            Event::PointerButton {
                pos: Pos2::new(1.0, 1.0),
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            touch(egui::TouchPhase::Move, 2.0),
            Event::PointerMoved(Pos2::new(2.0, 2.0)),
            touch(egui::TouchPhase::Move, 3.0),
            Event::PointerMoved(Pos2::new(3.0, 3.0)),
            touch(egui::TouchPhase::End, 4.0),
            Event::PointerButton {
                pos: Pos2::new(3.0, 3.0),
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ];

        let (press, positions, touch_history) = primary_pointer_positions(&events, false);

        assert_eq!(press, Some(Pos2::new(1.0, 1.0)));
        assert_eq!(
            positions,
            vec![
                Pos2::new(2.0, 2.0),
                Pos2::new(3.0, 3.0),
                Pos2::new(4.0, 4.0)
            ]
        );
        assert!(touch_history);
    }

    #[test]
    fn drawing_interpolates_large_pointer_jumps() {
        let settings = AppSettings::default();

        assert_eq!(
            point_append_decision(EditKind::Paint, 1.0, &settings),
            PointAppendDecision::Skip
        );
        assert_eq!(
            point_append_decision(EditKind::Paint, 4.0, &settings),
            PointAppendDecision::Append
        );
        assert_eq!(
            point_append_decision(EditKind::Paint, 9.0, &settings),
            PointAppendDecision::Interpolate
        );
        assert_eq!(
            point_append_decision(EditKind::Fill, 1.5, &settings),
            PointAppendDecision::Skip
        );
        assert_eq!(
            point_append_decision(EditKind::Fill, 3.0, &settings),
            PointAppendDecision::Append
        );
        assert_eq!(
            point_append_decision(EditKind::Fill, 5.0, &settings),
            PointAppendDecision::Interpolate
        );
    }

    #[test]
    fn fill_interpolation_is_not_bounded_by_the_fallback_point_limit() {
        let settings = AppSettings::default();
        let max_fill_draft_points = settings.fill_draft_max_points();

        assert_eq!(
            interpolation_step_count(EditKind::Fill, 10, 128.0, &settings),
            32
        );
        assert_eq!(
            interpolation_step_count(
                EditKind::Fill,
                max_fill_draft_points - 1,
                10_000.0,
                &settings
            ),
            2500
        );
        assert_eq!(
            interpolation_step_count(EditKind::Fill, max_fill_draft_points, 10_000.0, &settings),
            2500
        );
        assert!(
            interpolation_step_count(EditKind::Paint, max_fill_draft_points, 10_000.0, &settings)
                > 1
        );
    }

    #[test]
    fn full_fill_draft_continues_appending_raw_live_points() {
        let settings = AppSettings::default();
        let max_fill_draft_points = settings.fill_draft_max_points();
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(512.0, 512.0));
        let camera = CameraAddress::default();
        let start = CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.5, 0.5);
        let mut draft = EditOperation::draft(
            EditKind::Fill,
            0,
            1.0,
            vec![start.clone(); max_fill_draft_points],
            Color::BLACK,
            8.0,
        );

        append_interpolated_points(
            &mut draft,
            &camera,
            Pos2::new(276.0, 256.0),
            rect,
            &settings,
        );

        assert!(draft.points.len() > max_fill_draft_points);
        assert_eq!(draft.points.first(), Some(&start));
    }

    #[test]
    fn input_density_is_independent_from_interpolation_and_fill_limits() {
        let dense_settings = AppSettings {
            brush_input_spacing_px: 0.75,
            fill_input_spacing_px: 1.0,
            ..AppSettings::default()
        }
        .normalized();
        let sparse_settings = AppSettings {
            brush_input_spacing_px: 8.0,
            fill_input_spacing_px: 4.0,
            ..AppSettings::default()
        }
        .normalized();
        let fill_limited_settings = AppSettings {
            fill_fallback_max_points: 2,
            fill_fallback_max_depth_delta: 0,
            ..AppSettings::default()
        }
        .normalized();
        let fill = EditOperation::draft(
            EditKind::Fill,
            0,
            1.0,
            vec![
                CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.25, 0.25),
                CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.75, 0.25),
                CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.75, 0.75),
            ],
            Color::BLACK,
            8.0,
        );

        assert_eq!(
            point_append_decision(EditKind::Paint, 1.0, &dense_settings),
            PointAppendDecision::Append
        );
        assert_eq!(
            point_append_decision(EditKind::Paint, 1.0, &sparse_settings),
            PointAppendDecision::Skip
        );
        assert_eq!(
            point_append_decision(EditKind::Paint, 10.0, &dense_settings),
            PointAppendDecision::Interpolate
        );
        assert_eq!(
            point_append_decision(EditKind::Paint, 10.0, &sparse_settings),
            PointAppendDecision::Interpolate
        );
        assert_eq!(
            interpolation_step_count(EditKind::Paint, 0, 128.0, &dense_settings),
            interpolation_step_count(EditKind::Paint, 0, 128.0, &sparse_settings)
        );
        assert_eq!(
            point_append_decision(EditKind::Fill, 3.0, &dense_settings),
            PointAppendDecision::Append
        );
        assert_eq!(
            point_append_decision(EditKind::Fill, 3.0, &sparse_settings),
            PointAppendDecision::Skip
        );
        assert_eq!(
            point_append_decision(EditKind::Fill, 5.0, &sparse_settings),
            PointAppendDecision::Interpolate
        );
        assert!(!should_paint_fill_fallback(
            &fill,
            0,
            129,
            &fill_limited_settings
        ));
    }

    #[test]
    fn final_endpoint_bypasses_sparse_input_threshold() {
        let camera = CameraAddress::default();
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(512.0, 512.0));
        let start = camera.screen_to_canvas(256.0, 256.0, 512.0, 512.0);
        let mut draft =
            EditOperation::draft(EditKind::Paint, 0, 1.0, vec![start], Color::BLACK, 8.0);
        let settings = AppSettings {
            brush_input_spacing_px: 8.0,
            ..AppSettings::default()
        };

        append_interpolated_points(
            &mut draft,
            &camera,
            Pos2::new(261.0, 256.0),
            rect,
            &settings,
        );

        assert_eq!(draft.points.len(), 2);
        let (x, y) = camera
            .canvas_to_screen(&draft.points[1], 512.0, 512.0)
            .expect("endpoint projects");
        assert!((x - 261.0).abs() <= f64::EPSILON);
        assert!((y - 256.0).abs() <= f64::EPSILON);
    }

    #[test]
    fn brush_single_click_becomes_a_visible_dot_operation() {
        let point = CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.5, 0.5);
        let mut paint = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![point.clone()],
            Color::BLACK,
            12.0,
        );
        prepare_draft_for_commit(&mut paint);

        assert_eq!(paint.points.len(), 2);
        assert_eq!(paint.points[0].local_x, point.local_x);
        assert_eq!(paint.points[1].local_x, point.local_x);
        assert_eq!(paint.points[0].local_y, point.local_y);
        assert_eq!(paint.points[1].local_y, point.local_y);
    }

    #[test]
    fn click_to_dot_does_not_apply_to_eraser_or_fill() {
        let point = CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.5, 0.5);
        let mut erase = EditOperation::draft(
            EditKind::Erase,
            0,
            1.0,
            vec![point.clone()],
            Color::WHITE,
            12.0,
        );
        let mut fill =
            EditOperation::draft(EditKind::Fill, 0, 1.0, vec![point], Color::BLACK, 12.0);

        prepare_draft_for_commit(&mut erase);
        prepare_draft_for_commit(&mut fill);

        assert_eq!(erase.points.len(), 1);
        assert_eq!(fill.points.len(), 1);
        assert!(!draft_is_committable(&erase));
        assert!(!draft_is_committable(&fill));
    }

    #[test]
    fn fill_requires_three_distinct_points_before_closure() {
        let points = [
            CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.1, 0.1),
            CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.2, 0.1),
            CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.2, 0.2),
        ];
        let mut too_short = EditOperation::draft(
            EditKind::Fill,
            0,
            1.0,
            points[..2].to_vec(),
            Color::BLACK,
            8.0,
        );
        let mut valid =
            EditOperation::draft(EditKind::Fill, 0, 1.0, points.to_vec(), Color::BLACK, 8.0);

        prepare_draft_for_commit(&mut too_short);
        prepare_draft_for_commit(&mut valid);

        assert!(!draft_is_committable(&too_short));
        assert!(draft_is_committable(&valid));
        assert_eq!(valid.points.first(), valid.points.last());
    }

    #[test]
    fn shift_straight_lines_are_limited_to_brush_and_eraser() {
        assert!(straight_line_requested(EditKind::Paint, true));
        assert!(straight_line_requested(EditKind::Erase, true));
        assert!(!straight_line_requested(EditKind::Fill, true));
        assert!(!straight_line_requested(EditKind::Paint, false));
    }

    #[test]
    fn straight_line_endpoint_collapses_draft_to_start_and_current_point() {
        let start = CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.1, 0.1);
        let middle = CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.3, 0.4);
        let old_end = CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.5, 0.6);
        let new_end = CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.8, 0.9);
        let mut paint = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![start.clone(), middle, old_end],
            Color::BLACK,
            12.0,
        );

        set_straight_draft_endpoint(&mut paint, new_end.clone());

        assert_eq!(paint.points, vec![start, new_end]);
    }

    #[test]
    fn brush_size_adjustment_is_clamped_to_toolbar_range() {
        assert_eq!(adjusted_brush_size(10.0, 5.0), 15.0);
        assert_eq!(adjusted_brush_size(10.0, -20.0), MIN_BRUSH_SIZE);
        assert_eq!(adjusted_brush_size(99.0, 20.0), MAX_BRUSH_SIZE);
    }

    #[test]
    fn quick_depth_target_is_bounded_for_interactive_navigation() {
        assert_eq!(clamp_quick_depth(i64::MIN), -10_000);
        assert_eq!(clamp_quick_depth(-100), -100);
        assert_eq!(clamp_quick_depth(100), 100);
        assert_eq!(clamp_quick_depth(i64::MAX), 10_000);
    }

    #[test]
    fn redo_shortcuts_match_shift_z_before_ctrl_y() {
        let shortcuts = redo_shortcuts();

        assert_eq!(shortcuts[0].logical_key, egui::Key::Z);
        assert!(shortcuts[0].modifiers.ctrl);
        assert!(shortcuts[0].modifiers.shift);
        assert_eq!(shortcuts[1].logical_key, egui::Key::Y);
        assert!(shortcuts[1].modifiers.ctrl);
        assert!(!shortcuts[1].modifiers.shift);
    }

    #[test]
    fn tool_shortcuts_do_not_intercept_clipboard_modifiers() {
        assert!(tool_shortcut_allowed(Modifiers::NONE));
        assert!(!tool_shortcut_allowed(Modifiers::CTRL));
        assert!(!tool_shortcut_allowed(Modifiers::COMMAND));
        assert!(!tool_shortcut_allowed(Modifiers::ALT));
    }

    #[test]
    fn x_shortcut_cycles_area_fill_and_erase() {
        assert_eq!(area_cycle_tool(ToolKind::Brush), ToolKind::LassoFill);
        assert_eq!(area_cycle_tool(ToolKind::Eraser), ToolKind::LassoFill);
        assert_eq!(area_cycle_tool(ToolKind::Eyedropper), ToolKind::LassoFill);
        assert_eq!(area_cycle_tool(ToolKind::Selection), ToolKind::LassoFill);
        assert_eq!(area_cycle_tool(ToolKind::LassoFill), ToolKind::EraserLasso);
        assert_eq!(area_cycle_tool(ToolKind::EraserLasso), ToolKind::LassoFill);
    }

    #[test]
    fn s_shortcut_cycles_selection_inside_and_crossing() {
        assert_eq!(
            rectangle_selection_mode_cycle(RectangleSelectionMode::Inside),
            RectangleSelectionMode::Crossing
        );
        assert_eq!(
            rectangle_selection_mode_cycle(RectangleSelectionMode::Crossing),
            RectangleSelectionMode::Inside
        );
    }

    #[test]
    fn q_shortcut_cycles_selection_object_and_area() {
        assert_eq!(
            selection_target_cycle(SelectionTargetMode::Object),
            SelectionTargetMode::Area
        );
        assert_eq!(
            selection_target_cycle(SelectionTargetMode::Area),
            SelectionTargetMode::Object
        );
    }

    #[test]
    fn canvas_pointer_press_surrenders_stale_keyboard_focus() {
        let context = egui::Context::default();
        let text_edit_id = egui::Id::new("stale text edit");
        context.memory_mut(|memory| memory.request_focus(text_edit_id));
        assert!(context.egui_wants_keyboard_input());

        surrender_canvas_keyboard_focus(&context, true, true);

        assert!(!context.egui_wants_keyboard_input());
    }

    #[test]
    fn clipboard_events_map_to_internal_commands() {
        assert_eq!(
            clipboard_command_from_events(&[Event::Copy], Modifiers::CTRL),
            Some(ClipboardCommand::Copy)
        );
        assert_eq!(
            clipboard_command_from_events(&[Event::Cut], Modifiers::CTRL),
            Some(ClipboardCommand::Cut)
        );
        assert_eq!(
            clipboard_command_from_events(&[Event::Paste("ignored".to_owned())], Modifiers::CTRL),
            Some(ClipboardCommand::Paste)
        );
        assert_eq!(
            clipboard_command_from_events(
                &[Event::Paste("ignored".to_owned())],
                Modifiers::CTRL | Modifiers::SHIFT,
            ),
            Some(ClipboardCommand::PasteInPlace)
        );
    }

    #[test]
    fn native_clipboard_keys_trigger_only_on_press_edges() {
        let mut keys = ClipboardShortcutKeys::default();
        assert_eq!(
            keys.update(true, false, true, false, false),
            Some(ClipboardCommand::Copy)
        );
        assert_eq!(keys.update(true, false, true, false, false), None);
        assert_eq!(keys.update(true, false, false, false, false), None);
        assert_eq!(
            keys.update(true, false, true, false, false),
            Some(ClipboardCommand::Copy)
        );
        assert_eq!(
            keys.update(true, true, false, false, true),
            Some(ClipboardCommand::PasteInPlace)
        );
    }

    #[test]
    fn lateral_coordinate_parser_accepts_bounded_decimal_and_scientific_integers() {
        assert_eq!(parse_lateral_coordinate("0"), Some(BigInt::from(0)));
        assert_eq!(parse_lateral_coordinate("-123"), Some(BigInt::from(-123)));
        assert_eq!(
            parse_lateral_coordinate("1e100"),
            Some(BigInt::from(10u8).pow(100))
        );
        assert_eq!(
            parse_lateral_coordinate("-25E+3"),
            Some(BigInt::from(-25_000))
        );
        assert!(parse_lateral_coordinate("1.5e3").is_none());
        assert!(parse_lateral_coordinate("1e-3").is_none());
        assert!(parse_lateral_coordinate("1e10000").is_none());
        assert!(parse_lateral_coordinate("1e2e3").is_none());
    }

    #[test]
    fn overlay_coordinate_formatter_bounds_extreme_bigints() {
        assert_eq!(
            format_overlay_coordinate(&BigInt::from(-123_456)),
            "-123456"
        );
        assert_eq!(
            format_overlay_coordinate(&BigInt::from(10u8).pow(100)),
            "1e100"
        );
        assert_eq!(
            format_overlay_coordinate(&-BigInt::from(10u8).pow(100)),
            "-1e100"
        );
        let arbitrary = "123456789012345678901234567890".parse::<BigInt>().unwrap();
        assert_eq!(
            format_overlay_coordinate(&arbitrary),
            "12345678...34567890 (30d)"
        );
    }

    #[test]
    fn standard_overlay_matches_the_post_navigation_display() {
        let settings = AppSettings::default();
        let camera = CameraAddress::default();
        let text = canvas_overlay_text(
            &settings,
            &camera,
            42,
            "fps 60.0 (16.7 ms)",
            "Ready",
            "tiles paused",
            "123",
            "456",
        )
        .expect("standard overlay");

        assert!(text.contains("zoom 1.000x"));
        assert!(text.contains("42 ops"));
        assert!(text.contains("fps 60.0 (16.7 ms)"));
        assert!(text.contains("Ready"));
        assert!(text.contains("tiles paused"));
        assert!(!text.contains("depth"));
        assert!(!text.contains("tile X"));
        assert!(!text.contains("local X"));
    }

    #[test]
    fn diagnostic_overlay_includes_depth_and_coordinates() {
        let mut settings = AppSettings::default();
        settings.apply_overlay_profile(OverlayProfile::Diagnostics);
        let camera = CameraAddress {
            depth: 12,
            local_x: 0.25,
            local_y: -0.5,
            ..CameraAddress::default()
        };
        let text = canvas_overlay_text(&settings, &camera, 7, "fps --", "Ready", "", "1e100", "-5")
            .expect("diagnostic overlay");

        assert!(text.contains("depth 12"));
        assert!(text.contains("tile X 1e100"));
        assert!(text.contains("tile Y -5"));
        assert!(text.contains("local X 0.250000   Y -0.500000"));
    }

    #[test]
    fn disabled_or_empty_custom_overlay_renders_nothing() {
        let mut settings = AppSettings {
            overlay_enabled: false,
            ..AppSettings::default()
        };
        assert!(
            canvas_overlay_text(
                &settings,
                &CameraAddress::default(),
                0,
                "fps --",
                "Ready",
                "",
                "0",
                "0",
            )
            .is_none()
        );

        settings.overlay_enabled = true;
        settings.overlay_show_depth = false;
        settings.overlay_show_zoom = false;
        settings.overlay_show_tile_coordinates = false;
        settings.overlay_show_local_coordinates = false;
        settings.overlay_show_operation_count = false;
        settings.overlay_show_performance = false;
        settings.overlay_show_status = false;
        settings.overlay_show_tile_state = false;
        assert!(
            canvas_overlay_text(
                &settings,
                &CameraAddress::default(),
                0,
                "fps --",
                "Ready",
                "",
                "0",
                "0",
            )
            .is_none()
        );
    }

    #[test]
    fn rgb_color_edit_produces_opaque_drawing_color() {
        assert_eq!(
            color_from_srgb([250, 240, 40]),
            Color::rgba(250, 240, 40, 255)
        );
    }

    #[test]
    fn dense_stroke_fallback_keeps_raw_points_and_uses_fast_shapes() {
        let points: Vec<_> = (0..QUALITY_SEGMENTED_FALLBACK_POINTS + 1)
            .map(|index| Pos2::new(index as f32, (index % 7) as f32))
            .collect();
        let expected_points = points.clone();

        let (shapes, stats) = stroke_fallback_shapes(
            points.as_slice(),
            8.0,
            Color32::BLACK,
            stroke_fallback_segmented_point_limit(StrokeFallbackJoinMode::Quality, false),
            stroke_fallback_fast_endpoint_caps(StrokeFallbackJoinMode::Quality, false),
        );

        assert_eq!(shapes.len(), 3);
        assert_eq!(stats.stroke_shape_count, 3);
        assert_eq!(stats.segmented_strokes, 0);
        assert_eq!(stats.fast_strokes, 1);
        let Shape::Path(path) = &shapes[0] else {
            panic!("first fallback shape must be one polyline");
        };
        assert_eq!(path.points, expected_points);
        assert!(matches!(shapes[1], Shape::Circle(_)));
        assert!(matches!(shapes[2], Shape::Circle(_)));
    }

    #[test]
    fn segmented_fallback_budget_downgrades_over_budget_runs() {
        let limit = stroke_fallback_segmented_point_limit(StrokeFallbackJoinMode::Quality, false);
        let mut budget = 7;

        let (first_limit, first_downgraded) =
            consume_segmented_fallback_budget(4, limit, &mut budget);
        assert_eq!(first_limit, limit);
        assert!(!first_downgraded);
        assert_eq!(budget, 0);

        let (second_limit, second_downgraded) =
            consume_segmented_fallback_budget(3, limit, &mut budget);
        assert_eq!(second_limit, None);
        assert!(second_downgraded);

        let (dense_limit, dense_downgraded) = consume_segmented_fallback_budget(
            QUALITY_SEGMENTED_FALLBACK_POINTS + 1,
            limit,
            &mut budget,
        );
        assert_eq!(dense_limit, limit);
        assert!(!dense_downgraded);
    }

    #[test]
    fn large_selection_overlay_simplifies_only_during_interaction() {
        assert!(!large_selection_fast_overlay_active(511, true, false));
        assert!(!large_selection_fast_overlay_active(512, false, false));
        assert!(!large_selection_fast_overlay_active(512, true, true));
        assert!(large_selection_fast_overlay_active(512, true, false));
        assert!(large_selection_fast_overlay_active(900, true, false));
    }

    #[test]
    fn selection_bounds_signature_changes_for_same_size_different_ids() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let first = [a, b].into_iter().collect::<HashSet<_>>();
        let second = [a, c].into_iter().collect::<HashSet<_>>();

        assert_ne!(
            selection_bounds_signature(&first),
            selection_bounds_signature(&second)
        );
    }

    #[test]
    fn selection_overlay_bounds_cache_rebuilds_and_projects_selected_bounds() {
        let mut first = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![
                CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.25, 0.25),
                CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.75, 0.75),
            ],
            Color::BLACK,
            12.0,
        );
        first.id = Uuid::from_u128(10);
        let mut second = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![
                CanvasPoint::new(0, BigInt::from(1), BigInt::from(1), 0.25, 0.25),
                CanvasPoint::new(0, BigInt::from(1), BigInt::from(1), 0.75, 0.75),
            ],
            Color::BLACK,
            4.0,
        );
        second.id = Uuid::from_u128(20);
        let operations = vec![first, second];
        let camera = CameraAddress::default();
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(512.0, 512.0));
        let mut cache = SelectionOverlayBoundsCache::default();

        let selected_first = [Uuid::from_u128(10)].into_iter().collect::<HashSet<_>>();
        let first_bounds = {
            let (bounds, widths) = cache.bounds_for(1, &operations, &selected_first);
            assert_eq!(widths.len(), 1);
            bounds[0].project(&camera, rect).expect("projected first")
        };
        assert!((first_bounds.min.x - 128.0).abs() < 0.001);
        assert!((first_bounds.max.x - 384.0).abs() < 0.001);

        let selected_second = [Uuid::from_u128(20)].into_iter().collect::<HashSet<_>>();
        let second_bounds = {
            let (bounds, widths) = cache.bounds_for(1, &operations, &selected_second);
            assert_eq!(widths.len(), 1);
            bounds[0].project(&camera, rect).expect("projected second")
        };
        assert!(second_bounds.min.x > first_bounds.max.x);
        assert_eq!(
            selection_overlay_width_expansion(
                cache.bounds_for(1, &operations, &selected_second).1,
                0,
                1.0,
            ),
            4.0
        );
    }

    #[test]
    fn auto_saved_stroke_fallback_uses_segment_capsule_shapes_without_active_draft() {
        let points = vec![
            Pos2::new(0.0, 0.0),
            Pos2::new(12.0, 16.0),
            Pos2::new(28.0, 3.0),
        ];

        let (shapes, stats) = stroke_fallback_shapes(
            points.as_slice(),
            8.0,
            Color32::BLACK,
            stroke_fallback_segmented_point_limit(StrokeFallbackJoinMode::Auto, false),
            stroke_fallback_fast_endpoint_caps(StrokeFallbackJoinMode::Auto, false),
        );

        assert_eq!(shapes.len(), 5);
        assert_eq!(stats.stroke_shape_count, 5);
        assert_eq!(stats.segmented_strokes, 1);
        assert_eq!(stats.fast_strokes, 0);
        assert!(matches!(shapes[0], Shape::LineSegment { .. }));
        assert!(matches!(shapes[1], Shape::LineSegment { .. }));
        assert!(matches!(shapes[2], Shape::Circle(_)));
        assert!(matches!(shapes[3], Shape::Circle(_)));
        assert!(matches!(shapes[4], Shape::Circle(_)));
    }

    #[test]
    fn auto_saved_stroke_fallback_uses_fast_shapes_with_active_draft() {
        let points = vec![
            Pos2::new(0.0, 0.0),
            Pos2::new(12.0, 16.0),
            Pos2::new(28.0, 3.0),
        ];

        let (shapes, stats) = stroke_fallback_shapes(
            points.as_slice(),
            8.0,
            Color32::BLACK,
            stroke_fallback_segmented_point_limit(StrokeFallbackJoinMode::Auto, true),
            stroke_fallback_fast_endpoint_caps(StrokeFallbackJoinMode::Auto, true),
        );

        assert_eq!(shapes.len(), 1);
        assert_eq!(stats.stroke_shape_count, 1);
        assert_eq!(stats.segmented_strokes, 0);
        assert_eq!(stats.fast_strokes, 1);
        assert!(matches!(shapes[0], Shape::Path(_)));
    }

    #[test]
    fn auto_saved_stroke_fallback_uses_fast_shapes_during_interaction() {
        let points = vec![
            Pos2::new(0.0, 0.0),
            Pos2::new(12.0, 16.0),
            Pos2::new(28.0, 3.0),
        ];
        let auto_fast_path = auto_fast_stroke_fallback_active(true, false, false);

        let (shapes, stats) = stroke_fallback_shapes(
            points.as_slice(),
            8.0,
            Color32::BLACK,
            stroke_fallback_segmented_point_limit(StrokeFallbackJoinMode::Auto, auto_fast_path),
            stroke_fallback_fast_endpoint_caps(StrokeFallbackJoinMode::Auto, auto_fast_path),
        );

        assert_eq!(shapes.len(), 1);
        assert_eq!(stats.stroke_shape_count, 1);
        assert_eq!(stats.segmented_strokes, 0);
        assert_eq!(stats.fast_strokes, 1);
        assert!(matches!(shapes[0], Shape::Path(_)));
    }

    #[test]
    fn auto_saved_stroke_fallback_uses_fast_shapes_while_tiles_are_pending() {
        let points = vec![
            Pos2::new(0.0, 0.0),
            Pos2::new(12.0, 16.0),
            Pos2::new(28.0, 3.0),
        ];
        let auto_fast_path = auto_fast_stroke_fallback_active(false, true, false);

        let (shapes, stats) = stroke_fallback_shapes(
            points.as_slice(),
            8.0,
            Color32::BLACK,
            stroke_fallback_segmented_point_limit(StrokeFallbackJoinMode::Auto, auto_fast_path),
            stroke_fallback_fast_endpoint_caps(StrokeFallbackJoinMode::Auto, auto_fast_path),
        );

        assert_eq!(shapes.len(), 1);
        assert_eq!(stats.stroke_shape_count, 1);
        assert_eq!(stats.segmented_strokes, 0);
        assert_eq!(stats.fast_strokes, 1);
        assert!(matches!(shapes[0], Shape::Path(_)));
    }

    #[test]
    fn manual_tile_pause_keeps_idle_saved_stroke_smoothing() {
        let auto_fast_path = auto_fast_stroke_fallback_active(false, false, true);

        assert!(!auto_fast_path);
        assert_eq!(
            stroke_fallback_smoothing_passes(StrokeFallbackJoinMode::Auto, auto_fast_path, 3),
            3
        );
    }

    #[test]
    fn fallback_operation_budget_uses_visible_setting() {
        let mut settings = AppSettings::default();

        assert_eq!(
            fallback_operation_limit(&settings, TileFallbackMode::Full),
            None
        );

        settings.apply_performance_profile(PerformanceProfile::Performance);
        assert_eq!(
            fallback_operation_limit(&settings, TileFallbackMode::Full),
            Some(1_500)
        );
        assert_eq!(
            fallback_operation_limit(&settings, TileFallbackMode::Current),
            None
        );

        settings.apply_performance_profile(PerformanceProfile::Balanced);
        assert_eq!(
            fallback_operation_limit(&settings, TileFallbackMode::Full),
            None
        );

        settings.saved_fallback_operation_limit = 777;
        assert_eq!(
            fallback_operation_limit(&settings, TileFallbackMode::OverlayAfter(10)),
            Some(777)
        );
    }

    #[test]
    fn fallback_operation_budget_skips_oldest_visible_operations() {
        assert_eq!(fallback_operation_skip_count(10, None), 0);
        assert_eq!(fallback_operation_skip_count(10, Some(12)), 0);
        assert_eq!(fallback_operation_skip_count(10, Some(4)), 6);
    }

    #[test]
    fn fallback_operation_budget_never_splits_a_fill_group() {
        let settings = AppSettings::default();
        let camera = CameraAddress::default();
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(512.0, 512.0));
        let mut older = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![camera.center_point(), camera.center_point()],
            Color::BLACK,
            4.0,
        );
        older.sequence = 1;
        let source = EditOperation::draft(
            EditKind::Fill,
            0,
            1.0,
            test_canvas_polygon(0, &[(0.1, 0.1), (0.9, 0.1), (0.5, 0.9)]),
            Color::BLACK,
            0.0,
        );
        let mask = EditOperation::draft(
            EditKind::EraseArea,
            0,
            1.0,
            test_canvas_polygon(0, &[(0.45, 0.0), (0.55, 0.0), (0.55, 1.0), (0.45, 1.0)]),
            Color::WHITE,
            0.0,
        );
        let mut fragments = test_fill_fragment_operations(&source, &[mask]).unwrap();
        for (offset, fragment) in fragments.iter_mut().enumerate() {
            fragment.sequence = offset as i64 + 2;
            fragment.paint_order = Some(2);
        }
        let operations = std::iter::once(older)
            .chain(fragments)
            .map(Arc::new)
            .collect::<Vec<_>>();
        let mut renderer = FallbackRenderer::default();

        let (projected, skipped, _) = renderer.project_operations(
            &operations,
            &camera,
            rect,
            None,
            Some(1),
            settings.stroke_fallback_joins,
            false,
            settings.smoothing,
            Instant::now(),
        );

        assert_eq!(skipped, 1);
        assert_eq!(projected.len(), operations.len() - 1);
    }

    #[test]
    fn operation_screen_cull_rejects_definitely_offscreen_fallback_operations() {
        let camera = CameraAddress::default();
        let viewport = Rect::from_min_max(Pos2::ZERO, Pos2::new(1280.0, 720.0));
        let center = camera.center_point();
        let visible = EditOperation::draft(
            EditKind::Paint,
            camera.depth,
            camera.zoom,
            vec![center.clone(), center],
            Color::BLACK,
            8.0,
        );
        let mut cache = OperationBoundsCache::default();
        let visible_lookup =
            cache.operation_may_intersect_screen_rect(&visible, &camera, viewport, 8.0);
        assert!(!visible_lookup.cache_hit);
        assert!(visible_lookup.may_intersect);
        assert_eq!(cache.len(), 1);

        let repeated_lookup =
            cache.operation_may_intersect_screen_rect(&visible, &camera, viewport, 8.0);
        assert!(repeated_lookup.cache_hit);
        assert!(repeated_lookup.may_intersect);

        let offscreen = EditOperation::draft(
            EditKind::Paint,
            camera.depth,
            camera.zoom,
            vec![
                CanvasPoint::new(camera.depth, 100.into(), 100.into(), 0.0, 0.0),
                CanvasPoint::new(camera.depth, 100.into(), 100.into(), 0.2, 0.2),
            ],
            Color::BLACK,
            8.0,
        );
        let compact = EditOperation::compact_block(
            DEFAULT_LAYER_ID,
            offscreen.points.clone(),
            vec![offscreen],
        );
        let compact_source = compact.compact_sources.first().expect("compact source");
        let compact_source_lookup =
            cache.operation_may_intersect_screen_rect(compact_source, &camera, viewport, 8.0);
        assert!(!compact_source_lookup.cache_hit);
        assert!(!compact_source_lookup.may_intersect);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn operation_bounds_cache_survives_navigation_anchor_reset_and_explicitly_clears() {
        let viewport = Rect::from_min_max(Pos2::ZERO, Pos2::new(1280.0, 720.0));
        let camera = CameraAddress::default();
        let center = camera.center_point();
        let operation = EditOperation::draft(
            EditKind::Paint,
            camera.depth,
            camera.zoom,
            vec![center.clone(), center],
            Color::BLACK,
            8.0,
        );
        let mut bounds_cache = OperationBoundsCache::default();
        let first =
            bounds_cache.operation_may_intersect_screen_rect(&operation, &camera, viewport, 8.0);
        assert!(!first.cache_hit);

        let mut projected_geometry = ProjectedGeometryCache::default();
        assert!(projected_geometry.begin_frame(&camera, viewport).is_some());
        let mut moved_camera = camera;
        moved_camera.tile_x = 1_000.into();
        moved_camera.zoom = 2.0;
        assert!(
            projected_geometry
                .begin_frame(&moved_camera, viewport)
                .is_some()
        );

        let after_anchor_reset = bounds_cache.operation_may_intersect_screen_rect(
            &operation,
            &moved_camera,
            viewport,
            8.0,
        );
        assert!(after_anchor_reset.cache_hit);
        assert_eq!(bounds_cache.len(), 1);

        bounds_cache.clear();
        assert_eq!(bounds_cache.len(), 0);
        let after_clear = bounds_cache.operation_may_intersect_screen_rect(
            &operation,
            &moved_camera,
            viewport,
            8.0,
        );
        assert!(!after_clear.cache_hit);
    }

    #[test]
    fn fallback_renderer_centralizes_frame_and_invalidation_lifecycle() {
        let settings = AppSettings::default();
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(512.0, 512.0));
        let camera = CameraAddress::default();
        let frame_key = SavedFallbackRenderFrameKey::new(1, &camera, rect, &settings, false);
        let operation_key = SavedFallbackRenderOperationKey {
            scope_id: Uuid::nil(),
            operation_id: Uuid::nil(),
            sequence: 1,
            render_variant: 0,
        };
        let center = camera.center_point();
        let operation = EditOperation::draft(
            EditKind::Paint,
            camera.depth,
            camera.zoom,
            vec![center.clone(), center],
            Color::BLACK,
            8.0,
        );
        let mut renderer = FallbackRenderer::default();
        let operations = vec![Arc::new(operation.clone())];

        renderer.begin_frame(frame_key.clone());
        renderer
            .saved_render_cache
            .insert(operation_key, vec![Pos2::ZERO, Pos2::new(1.0, 1.0)]);
        let bounds = renderer
            .operation_bounds_cache
            .operation_may_intersect_screen_rect(&operation, &camera, rect, 8.0);
        assert!(!bounds.cache_hit);
        renderer.segmented_shape_budget_remaining = 0;

        renderer.begin_frame(frame_key);
        assert_eq!(renderer.saved_render_cache.cached_operation_count(), 1);
        assert_eq!(renderer.operation_bounds_cache.len(), 1);
        assert_eq!(
            renderer.segmented_shape_budget_remaining,
            MAX_SEGMENTED_FALLBACK_SHAPES_PER_FRAME
        );

        let mut moved_camera = camera.clone();
        moved_camera.tile_x = 100.into();
        renderer.begin_frame(SavedFallbackRenderFrameKey::new(
            1,
            &moved_camera,
            rect,
            &settings,
            false,
        ));
        assert_eq!(renderer.saved_render_cache.cached_operation_count(), 0);
        assert_eq!(renderer.operation_bounds_cache.len(), 1);

        let visible_tiles = Vec::new();
        let (visible, first_visible_cache_hit) =
            renderer.visible_operations(1, &visible_tiles, || vec![0]);
        assert!(!first_visible_cache_hit);
        assert_eq!(visible.len(), 1);
        let (cached_visible, repeated_visible_cache_hit) =
            renderer.visible_operations(1, &visible_tiles, || {
                panic!("same visible-operation cache key should not query again")
            });
        assert!(repeated_visible_cache_hit);
        assert_eq!(cached_visible.len(), 1);

        renderer.begin_frame(SavedFallbackRenderFrameKey::new(
            2, &camera, rect, &settings, false,
        ));
        renderer.operation_bounds_cache.clear();
        let (projected, skipped, first_stats) = renderer.project_operations(
            &operations,
            &camera,
            rect,
            None,
            None,
            settings.stroke_fallback_joins,
            false,
            settings.smoothing,
            Instant::now(),
        );
        assert_eq!(projected.len(), 1);
        assert_eq!(skipped, 0);
        assert!(matches!(
            projected.first(),
            Some(FallbackPaintOperation::Projected { .. })
        ));
        assert_eq!(first_stats.projected_operations, 1);
        assert_eq!(first_stats.fallback_bounds_cache_hits, 0);
        assert_eq!(first_stats.fallback_bounds_cache_misses, 1);

        let derived_key = saved_fallback_operation_key(
            operation.id,
            &operation,
            settings.stroke_fallback_joins,
            false,
            settings.smoothing,
        );
        renderer
            .saved_render_cache
            .insert(derived_key, vec![Pos2::ZERO, Pos2::new(1.0, 1.0)]);
        let (derived, _, derived_stats) = renderer.project_operations(
            &operations,
            &camera,
            rect,
            None,
            None,
            settings.stroke_fallback_joins,
            false,
            settings.smoothing,
            Instant::now(),
        );
        assert!(matches!(
            derived.first(),
            Some(FallbackPaintOperation::CachedDerived { .. })
        ));
        assert_eq!(derived_stats.fallback_cache_hits, 1);
        assert_eq!(derived_stats.projected_operations, 0);
        assert_eq!(derived_stats.fallback_bounds_cache_hits, 0);
        assert_eq!(derived_stats.fallback_bounds_cache_misses, 0);

        let mut zoomed_camera = camera.clone();
        zoomed_camera.zoom = 1.1;
        renderer.begin_frame(SavedFallbackRenderFrameKey::new(
            3,
            &zoomed_camera,
            rect,
            &settings,
            false,
        ));
        let (_, _, warmed_stats) = renderer.project_operations(
            &operations,
            &zoomed_camera,
            rect,
            None,
            None,
            settings.stroke_fallback_joins,
            false,
            settings.smoothing,
            Instant::now(),
        );
        assert_eq!(warmed_stats.fallback_bounds_cache_hits, 1);
        assert_eq!(warmed_stats.fallback_bounds_cache_misses, 0);

        renderer.clear();
        assert_eq!(renderer.saved_render_cache.cached_operation_count(), 0);
        assert_eq!(renderer.operation_bounds_cache.len(), 0);
        assert_eq!(
            renderer.segmented_shape_budget_remaining,
            MAX_SEGMENTED_FALLBACK_SHAPES_PER_FRAME
        );
        let (visible, visible_cache_hit) = renderer.visible_operations(1, &visible_tiles, Vec::new);
        assert!(!visible_cache_hit);
        assert!(visible.is_empty());
    }

    #[test]
    fn saved_fallback_render_cache_keys_on_frame_identity() {
        let settings = AppSettings::default();
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(512.0, 512.0));
        let camera = CameraAddress::default();
        let frame_key = SavedFallbackRenderFrameKey::new(7, &camera, rect, &settings, false);
        let operation_key = SavedFallbackRenderOperationKey {
            scope_id: Uuid::nil(),
            operation_id: Uuid::nil(),
            sequence: 42,
            render_variant: 0,
        };
        let points = vec![Pos2::new(1.0, 2.0), Pos2::new(3.0, 4.0)];
        let mut cache = SavedFallbackRenderCache::default();

        cache.begin_frame(frame_key.clone());
        cache.insert(operation_key, points.clone());
        let clipped_runs = cache.insert_clipped_stroke_runs(operation_key, vec![points.clone()]);
        let clipped_area = cache.insert_clipped_area_polygon(operation_key, points.clone());
        let cached = cache.get(&operation_key).expect("cached points");
        assert_eq!(cached.as_ref(), points.as_slice());
        assert_eq!(
            clipped_runs.as_ref(),
            cache
                .get_clipped_stroke_runs(&operation_key)
                .expect("cached clipped stroke runs")
                .as_ref()
        );
        assert_eq!(
            clipped_area.as_ref(),
            cache
                .get_clipped_area_polygon(&operation_key)
                .expect("cached clipped area")
                .as_ref()
        );

        cache.begin_frame(frame_key);
        assert_eq!(cache.cached_operation_count(), 1);
        assert!(cache.get_clipped_stroke_runs(&operation_key).is_some());
        assert!(cache.get_clipped_area_polygon(&operation_key).is_some());

        let transformed_key = SavedFallbackRenderOperationKey {
            scope_id: Uuid::nil(),
            operation_id: Uuid::new_v4(),
            sequence: 43,
            render_variant: 0,
        };
        assert!(
            cache.insert_transformed_from(&operation_key, transformed_key, |point| Pos2::new(
                point.x + 10.0,
                point.y - 1.0
            ),)
        );
        assert_eq!(
            cache
                .get(&transformed_key)
                .expect("transformed cached points")
                .as_ref(),
            &[Pos2::new(11.0, 1.0), Pos2::new(13.0, 3.0)]
        );
        assert!(cache.get_clipped_stroke_runs(&transformed_key).is_none());
        assert!(cache.get_clipped_area_polygon(&transformed_key).is_none());

        cache.begin_frame(SavedFallbackRenderFrameKey::new(
            8, &camera, rect, &settings, false,
        ));
        assert_eq!(cache.cached_operation_count(), 2);
        assert_eq!(
            cache
                .get(&operation_key)
                .expect("revision-only hit")
                .as_ref(),
            points.as_slice()
        );
        assert!(cache.get_clipped_stroke_runs(&operation_key).is_some());
        assert!(cache.get_clipped_area_polygon(&operation_key).is_some());

        let mut moved_camera = camera;
        moved_camera.local_x += 0.01;
        cache.begin_frame(SavedFallbackRenderFrameKey::new(
            7,
            &moved_camera,
            rect,
            &settings,
            false,
        ));
        assert_eq!(cache.cached_operation_count(), 0);
        assert_eq!(cache.get(&operation_key), None);
        assert!(cache.get_clipped_stroke_runs(&operation_key).is_none());
        assert!(cache.get_clipped_area_polygon(&operation_key).is_none());
    }

    #[test]
    fn saved_fallback_fast_path_state_does_not_clear_frame_cache() {
        let mut settings = AppSettings {
            smoothing: SmoothingLevel::Strong,
            stroke_fallback_joins: StrokeFallbackJoinMode::Auto,
            ..AppSettings::default()
        };
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(512.0, 512.0));
        let camera = CameraAddress::default();
        let operation = EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![
                CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.1, 0.1),
                CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.2, 0.2),
            ],
            Color::BLACK,
            4.0,
        );
        let quality_key = saved_fallback_operation_key(
            operation.id,
            &operation,
            settings.stroke_fallback_joins,
            false,
            settings.smoothing,
        );
        let fast_key = saved_fallback_operation_key(
            operation.id,
            &operation,
            settings.stroke_fallback_joins,
            true,
            settings.smoothing,
        );
        assert_ne!(quality_key.render_variant, fast_key.render_variant);

        let mut cache = SavedFallbackRenderCache::default();
        cache.begin_frame(SavedFallbackRenderFrameKey::new(
            7, &camera, rect, &settings, false,
        ));
        cache.insert(quality_key, vec![Pos2::new(1.0, 2.0)]);
        cache.begin_frame(SavedFallbackRenderFrameKey::new(
            8, &camera, rect, &settings, true,
        ));
        assert_eq!(cache.cached_operation_count(), 1);
        assert!(cache.get(&quality_key).is_some());
        assert!(cache.get(&fast_key).is_none());

        settings.stroke_fallback_joins = StrokeFallbackJoinMode::Quality;
        cache.begin_frame(SavedFallbackRenderFrameKey::new(
            9, &camera, rect, &settings, true,
        ));
        assert_eq!(cache.cached_operation_count(), 0);
    }

    #[test]
    fn quality_saved_stroke_fallback_segments_up_to_quality_limit() {
        let points: Vec<_> = (0..QUALITY_SEGMENTED_FALLBACK_POINTS)
            .map(|index| Pos2::new(index as f32, (index % 7) as f32))
            .collect();

        let (shapes, stats) = stroke_fallback_shapes(
            points.as_slice(),
            8.0,
            Color32::BLACK,
            stroke_fallback_segmented_point_limit(StrokeFallbackJoinMode::Quality, true),
            stroke_fallback_fast_endpoint_caps(StrokeFallbackJoinMode::Quality, true),
        );

        assert_eq!(shapes.len(), QUALITY_SEGMENTED_FALLBACK_POINTS * 2 - 1);
        assert_eq!(stats.segmented_strokes, 1);
        assert_eq!(stats.fast_strokes, 0);
        assert!(matches!(shapes[0], Shape::LineSegment { .. }));
    }

    #[test]
    fn performance_saved_stroke_fallback_always_uses_fast_shapes() {
        let points: Vec<_> = (0..AUTO_SEGMENTED_FALLBACK_POINTS)
            .map(|index| Pos2::new(index as f32, (index % 7) as f32))
            .collect();

        let (shapes, stats) = stroke_fallback_shapes(
            points.as_slice(),
            8.0,
            Color32::BLACK,
            stroke_fallback_segmented_point_limit(StrokeFallbackJoinMode::Performance, false),
            stroke_fallback_fast_endpoint_caps(StrokeFallbackJoinMode::Performance, false),
        );

        assert_eq!(shapes.len(), 1);
        assert_eq!(stats.stroke_shape_count, 1);
        assert_eq!(stats.segmented_strokes, 0);
        assert_eq!(stats.fast_strokes, 1);
        assert!(matches!(shapes[0], Shape::Path(_)));
    }

    #[test]
    fn fast_saved_stroke_fallback_skips_smoothing_passes() {
        assert_eq!(
            stroke_fallback_smoothing_passes(StrokeFallbackJoinMode::Auto, false, 3),
            3
        );
        assert_eq!(
            stroke_fallback_smoothing_passes(StrokeFallbackJoinMode::Auto, true, 3),
            0
        );
        assert_eq!(
            stroke_fallback_smoothing_passes(StrokeFallbackJoinMode::Quality, true, 3),
            3
        );
        assert_eq!(
            stroke_fallback_smoothing_passes(StrokeFallbackJoinMode::Performance, false, 3),
            0
        );
    }

    #[test]
    fn saved_fallback_geometry_is_clipped_without_changing_source_points() {
        let dense: Vec<_> = (0..1000)
            .map(|index| Pos2::new(index as f32 * 0.1, 5.0))
            .collect();
        let smoothed = smooth_pos2_saved_fallback(&dense, 0, false);
        assert_eq!(smoothed, dense);

        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(10.0, 10.0));
        let line = [
            Pos2::new(-10.0, 5.0),
            Pos2::new(5.0, 5.0),
            Pos2::new(20.0, 5.0),
        ];
        let runs = clip_pos2_polyline(&line, rect);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].first(), Some(&Pos2::new(0.0, 5.0)));
        assert_eq!(runs[0].last(), Some(&Pos2::new(10.0, 5.0)));

        let polygon = [
            Pos2::new(-5.0, -5.0),
            Pos2::new(15.0, -5.0),
            Pos2::new(15.0, 15.0),
            Pos2::new(-5.0, 15.0),
        ];
        assert_eq!(clip_pos2_polygon(&polygon, rect).len(), 4);
    }

    #[test]
    fn live_draft_smoothing_curves_sparse_mouse_corners() {
        let points = [
            Pos2::new(0.0, 0.0),
            Pos2::new(10.0, 10.0),
            Pos2::new(20.0, 0.0),
        ];
        let smoothed = smooth_pos2_draft(&points, 2);

        assert_eq!(smoothed.first(), points.first());
        assert_eq!(smoothed.last(), points.last());
        assert!(smoothed.len() > points.len());
        assert!(!smoothed.contains(&points[1]));
        assert_eq!(smooth_pos2_draft(&points, 0), points);
    }

    #[test]
    fn saved_fallback_smoothing_is_curved_and_bounded() {
        let points = [
            Pos2::new(0.0, 0.0),
            Pos2::new(10.0, 10.0),
            Pos2::new(20.0, 0.0),
        ];
        let smoothed = smooth_pos2_saved_fallback(&points, 2, false);

        assert!(smoothed.len() > points.len());
        assert!(!smoothed.contains(&points[1]));
        assert_eq!(smooth_pos2_saved_fallback(&points, 0, false), points);

        let oversized: Vec<_> = (0..=MAX_FALLBACK_SMOOTHING_INPUT_POINTS)
            .map(|index| Pos2::new(index as f32, (index % 2) as f32))
            .collect();
        assert_eq!(smooth_pos2_saved_fallback(&oversized, 3, false), oversized);
    }

    #[test]
    fn selection_highlight_uses_saved_fallback_render_geometry() {
        let points = vec![
            Pos2::new(0.0, 0.0),
            Pos2::new(10.0, 10.0),
            Pos2::new(20.0, 0.0),
        ];
        let operation =
            EditOperation::draft(EditKind::Paint, 0, 1.0, Vec::new(), Color::BLACK, 5.0);

        let highlighted = selection_highlight_render_points(&operation, points.clone(), 2, 64);

        assert_eq!(highlighted, smooth_pos2_saved_fallback(&points, 2, false));
        assert_ne!(highlighted, points);
    }

    #[test]
    fn saved_area_fallback_preserves_sharp_polygon_corners() {
        let square = [
            Pos2::new(0.0, 0.0),
            Pos2::new(10.0, 0.0),
            Pos2::new(10.0, 10.0),
            Pos2::new(0.0, 10.0),
            Pos2::new(0.0, 0.0),
        ];

        let rendered = area_saved_fallback_render_points(&square, 64, 0);

        assert_eq!(rendered, square);
        assert!(smooth_pos2_saved_fallback(&rendered, 3, true).len() > rendered.len());
    }

    #[test]
    fn freehand_area_fallback_smooths_closed_corners() {
        let square = [
            Pos2::new(0.0, 0.0),
            Pos2::new(10.0, 0.0),
            Pos2::new(10.0, 10.0),
            Pos2::new(0.0, 10.0),
            Pos2::new(0.0, 0.0),
        ];

        let rendered = area_saved_fallback_render_points(&square, 64, 2);

        assert!(rendered.len() > square.len());
        assert!(!rendered.contains(&square[1]));
    }

    #[test]
    fn saved_area_fallback_samples_without_screen_space_simplification() {
        let points = [
            Pos2::new(0.0, 0.0),
            Pos2::new(0.05, 0.0),
            Pos2::new(0.10, 0.0),
            Pos2::new(0.15, 0.0),
            Pos2::new(1.0, 1.0),
            Pos2::new(2.0, 0.0),
            Pos2::new(0.0, 0.0),
        ];

        let rendered = area_saved_fallback_render_points(&points, 4, 0);

        assert_eq!(rendered, vec![points[0], points[1], points[3], points[5]]);
    }

    #[test]
    fn fill_fallback_render_points_are_derived_and_bounded() {
        let points: Vec<_> = (0..20).map(|index| Pos2::new(index as f32, 0.0)).collect();
        let rendered = fill_fallback_render_points(&points, 8);

        assert_eq!(points.len(), 20);
        assert!(rendered.len() <= 8);
        assert_eq!(rendered.first(), points.first());
        assert!(rendered.windows(2).all(|pair| pair[0] != pair[1]));
    }

    #[test]
    fn space_pan_requires_space_and_primary_pointer() {
        assert!(space_pan_requested(true, true));
        assert!(!space_pan_requested(true, false));
        assert!(!space_pan_requested(false, true));
        assert!(!space_pan_requested(false, false));
    }

    #[test]
    fn z_zoom_requires_z_and_primary_pointer() {
        assert!(z_zoom_requested(true, true));
        assert!(!z_zoom_requested(true, false));
        assert!(!z_zoom_requested(false, true));
        assert!(!z_zoom_requested(false, false));
    }

    #[test]
    fn z_zoom_drag_direction_is_stable() {
        assert!(z_drag_zoom_factor(-10.0) > 1.0);
        assert!(z_drag_zoom_factor(10.0) < 1.0);
        assert_eq!(z_drag_zoom_factor(0.0), 1.0);
        assert_eq!(z_drag_zoom_factor(-10_000.0), 2.0);
        assert_eq!(z_drag_zoom_factor(10_000.0), 0.5);
    }

    #[test]
    fn tile_requests_are_deferred_only_while_zoom_is_settling() {
        let now = Instant::now();
        let settle_interval = AppSettings::default().fast_zoom_tile_settle_interval();

        assert!(!zoom_tile_requests_deferred_since(
            None,
            now,
            settle_interval
        ));

        let recent_zoom = now
            .checked_sub(Duration::from_millis(
                settle_interval.as_millis() as u64 / 2,
            ))
            .expect("test interval should be representable");
        assert!(zoom_tile_requests_deferred_since(
            Some(recent_zoom),
            now,
            settle_interval
        ));

        let settled_zoom = now
            .checked_sub(settle_interval + Duration::from_millis(1))
            .expect("test interval should be representable");
        assert!(!zoom_tile_requests_deferred_since(
            Some(settled_zoom),
            now,
            settle_interval
        ));
    }

    #[test]
    fn repaint_interval_prioritizes_interaction_over_tile_polling() {
        let preview_interval = Duration::from_millis(33);
        assert_eq!(
            repaint_interval_for_work(false, false, false, false, preview_interval),
            None
        );
        assert_eq!(
            repaint_interval_for_work(true, false, false, false, preview_interval),
            Some(preview_interval)
        );
        assert_eq!(
            repaint_interval_for_work(false, true, false, false, preview_interval),
            Some(TILE_POLL_REPAINT_INTERVAL)
        );
        assert_eq!(
            repaint_interval_for_work(false, false, true, false, preview_interval),
            Some(preview_interval)
        );
        assert_eq!(
            repaint_interval_for_work(true, true, false, false, preview_interval),
            Some(preview_interval)
        );
        assert_eq!(
            repaint_interval_for_work(false, false, false, true, preview_interval),
            Some(preview_interval)
        );
        let slow_preview = Duration::from_millis(66);
        assert_eq!(
            repaint_interval_for_work(false, true, false, false, slow_preview),
            Some(slow_preview)
        );
    }

    #[test]
    fn rebuild_deferral_expires_after_idle_interval() {
        let now = Instant::now();
        let interval = Duration::from_millis(180);
        let recent = now
            .checked_sub(interval - Duration::from_millis(1))
            .expect("test interval should be representable");
        let settled = now
            .checked_sub(interval + Duration::from_millis(1))
            .expect("test interval should be representable");

        assert!(tile_rebuild_deferred_since(Some(recent), now, interval));
        assert!(!tile_rebuild_deferred_since(Some(settled), now, interval));
        assert!(!tile_rebuild_deferred_since(None, now, interval));
    }

    #[test]
    fn tile_requests_prioritize_visible_tiles_before_prefetch_rings() {
        let camera = CameraAddress::default();
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(512.0, 512.0));
        let lod = tile_lod_for_resolution(64);

        let visible = tile_keys_for_view(&camera, rect, lod, 0);
        let radius_one = tile_keys_for_view(&camera, rect, lod, 1);
        let radius_two = tile_keys_for_view(&camera, rect, lod, 2);

        assert_eq!(visible.len(), 4);
        assert_eq!(radius_one.len(), 16);
        assert_eq!(radius_two.len(), 36);
        assert_eq!(&radius_one[..visible.len()], visible.as_slice());
        assert_eq!(&radius_two[..visible.len()], visible.as_slice());
        assert_eq!(&radius_two[..radius_one.len()], radius_one.as_slice());
    }

    #[test]
    fn tile_request_batch_limits_jobs_without_reordering() {
        let camera = CameraAddress::default();
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(512.0, 512.0));
        let lod = tile_lod_for_resolution(64);
        let requested = tile_keys_for_view(&camera, rect, lod, 2);

        let batch = tile_request_batch(requested.clone());

        assert_eq!(batch.len(), MAX_TILE_JOBS_QUEUED_PER_FRAME);
        assert_eq!(
            batch.as_slice(),
            &requested[..MAX_TILE_JOBS_QUEUED_PER_FRAME]
        );
    }

    #[test]
    fn visible_tile_keys_preserve_extreme_camera_depths() {
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 720.0));
        let lod = tile_lod_for_resolution(512);

        for depth in [-1000, -100, 100, 1000] {
            let camera = CameraAddress {
                depth,
                ..CameraAddress::default()
            };
            let keys = tile_keys_for_view(&camera, rect, lod, 2);
            assert!(!keys.is_empty());
            assert!(keys.iter().all(|key| key.depth == depth));
            assert_eq!(
                keys.iter().collect::<HashSet<_>>().len(),
                keys.len(),
                "depth={depth}"
            );
        }
    }
    #[test]
    fn fill_fallback_is_disabled_far_below_native_depth() {
        let fill = EditOperation::draft(
            EditKind::Fill,
            0,
            1.0,
            vec![
                CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.25, 0.25),
                CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.75, 0.25),
                CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.75, 0.75),
            ],
            Color::BLACK,
            8.0,
        );

        let settings = AppSettings::default();

        assert!(should_paint_fill_fallback(
            &fill,
            6,
            fill.points.len(),
            &settings
        ));
        assert!(!should_paint_fill_fallback(
            &fill,
            7,
            fill.points.len(),
            &settings
        ));
    }

    #[test]
    fn oversized_fill_can_still_use_bounded_saved_fallback() {
        let settings = AppSettings {
            fill_fallback_max_points: 8,
            ..AppSettings::default()
        }
        .normalized();
        let raw_points: Vec<_> = (0..256).map(|index| Pos2::new(index as f32, 0.0)).collect();
        let fallback_points =
            fill_fallback_render_points(&raw_points, settings.fill_fallback_max_points);
        let fill = EditOperation::draft(
            EditKind::Fill,
            0,
            1.0,
            vec![
                CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.25, 0.25),
                CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.75, 0.25),
                CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.75, 0.75),
            ],
            Color::BLACK,
            8.0,
        );

        assert!(raw_points.len() > settings.fill_fallback_max_points);
        assert!(should_paint_fill_fallback(
            &fill,
            0,
            fallback_points.len(),
            &settings
        ));
    }
}
