use crate::coords::{CameraAddress, CanvasPoint};
use crate::document::CanvasDocument;
use crate::local_paths::{default_canvas_path, settings_path};
use crate::model::{Bookmark, Color, EditKind, EditOperation, ToolKind};
use crate::mouse_history::{MouseHistory, NativeWindow, native_window};
use crate::projection_cache::{ProjectedGeometryCache, VisibleOperationCache};
use crate::raster::operation_width;
use crate::settings::{
    AppSettings, EdgeQuality, MAX_BRUSH_INPUT_SPACING_PX, MAX_CACHE_SIZE_MIB,
    MAX_FILL_INPUT_SPACING_PX, MAX_PREVIEW_FPS, MAX_TILE_PREFETCH_RADIUS,
    MIN_BRUSH_INPUT_SPACING_PX, MIN_CACHE_SIZE_MIB, MIN_FILL_INPUT_SPACING_PX, MIN_PREVIEW_FPS,
    PerformanceProfile, PngCompression, SmoothingLevel, TileRebuildPolicy,
};
use crate::smoothing::{
    GeometryClipRect, clip_polygon_to_rect, clip_polyline_to_rect, simplify_render_points,
    smooth_closed_points, smooth_stroke_points,
};
use crate::tile_cache::{
    TILE_BLEED, TILE_RESOLUTIONS, TILE_SIZE, TileCache, TileKey, tile_lod_for_resolution,
    tile_resolution,
};
use crate::tile_scheduler::{IncrementalTileUpdate, TileJob, TileScheduler};
use anyhow::{Context, Result};
use eframe::egui::{self, Color32, Painter, PointerButton, Pos2, Rect, Sense, Stroke};
use num_bigint::BigInt;
use std::collections::{HashMap, HashSet};
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
const FRAME_TIME_EMA_ALPHA: f32 = 0.15;
const MAX_MEASURED_FRAME_TIME: f32 = 0.25;
const MAX_FALLBACK_SMOOTHING_INPUT_POINTS: usize = 4096;
const MAX_FALLBACK_SMOOTHING_OUTPUT_POINTS: usize = 8192;
const MIN_QUICK_DEPTH: i64 = -10_000;
const MAX_QUICK_DEPTH: i64 = 10_000;
const MAX_LATERAL_COORDINATE_DIGITS: usize = 10_000;
const MAX_EXACT_OVERLAY_COORDINATE_DIGITS: usize = 18;
const OVERLAY_COORDINATE_EDGE_DIGITS: usize = 8;

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
    fn update(&mut self, frame_time: f32, interaction_active: bool) {
        if !interaction_active {
            self.interaction_active_last_frame = false;
            return;
        }
        if !frame_time.is_finite()
            || frame_time <= f32::EPSILON
            || frame_time > MAX_MEASURED_FRAME_TIME
        {
            return;
        }

        if !self.interaction_active_last_frame || self.average_frame_time.is_none() {
            self.average_frame_time = Some(frame_time);
        } else if let Some(average) = self.average_frame_time.as_mut() {
            *average += (frame_time - *average) * FRAME_TIME_EMA_ALPHA;
        }
        self.interaction_active_last_frame = true;
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
}

impl CoordinateOverlayCache {
    fn update(&mut self, camera: &CameraAddress) {
        if self.tile_x.as_ref() != Some(&camera.tile_x) {
            self.formatted_x = format_overlay_coordinate(&camera.tile_x);
            self.tile_x = Some(camera.tile_x.clone());
        }
        if self.tile_y.as_ref() != Some(&camera.tile_y) {
            self.formatted_y = format_overlay_coordinate(&camera.tile_y);
            self.tile_y = Some(camera.tile_y.clone());
        }
    }
}

pub struct EndlessSketchApp {
    document: CanvasDocument,
    camera: CameraAddress,
    tool: ToolKind,
    color: Color,
    brush_size: f32,
    draft: Option<EditOperation>,
    last_draft_save: Instant,
    last_pointer_position: Option<Pos2>,
    status_message: String,
    tile_scheduler: TileScheduler,
    tile_textures: HashMap<TileKey, LoadedTileTexture>,
    pending_tiles: HashSet<(TileKey, u64)>,
    visible_tile_generation: u64,
    visible_tile_set: HashSet<TileKey>,
    last_zoom_change: Option<Instant>,
    last_tile_rebuild_interaction: Option<Instant>,
    settings: AppSettings,
    settings_path: PathBuf,
    bookmarks: Vec<Bookmark>,
    bookmark_edits: HashMap<Uuid, String>,
    show_bookmarks: bool,
    show_settings: bool,
    brush_sizing_drag_active: bool,
    frame_rate: FrameRateTracker,
    projected_geometry: ProjectedGeometryCache,
    visible_operations: VisibleOperationCache,
    automatic_tile_generation_pause: bool,
    mouse_history: MouseHistory,
    depth_jump_target: i64,
    lateral_jump_x: String,
    lateral_jump_y: String,
    coordinate_overlay: CoordinateOverlayCache,
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
        let document = CanvasDocument::open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        let tile_scheduler = TileScheduler::new(
            TileCache::with_options(document.root(), settings.tile_cache_options())?,
            settings.tile_worker_count,
        );
        let bookmarks = document.bookmarks()?;
        let bookmark_edits = bookmark_edit_names(&bookmarks);
        Ok(Self {
            document,
            camera: CameraAddress::default(),
            tool: ToolKind::Brush,
            color: Color::BLACK,
            brush_size: 5.0,
            draft: None,
            last_draft_save: Instant::now(),
            last_pointer_position: None,
            status_message,
            tile_scheduler,
            tile_textures: HashMap::new(),
            pending_tiles: HashSet::new(),
            visible_tile_generation: 0,
            visible_tile_set: HashSet::new(),
            last_zoom_change: None,
            last_tile_rebuild_interaction: None,
            settings,
            settings_path,
            bookmarks,
            bookmark_edits,
            show_bookmarks: false,
            show_settings: false,
            brush_sizing_drag_active: false,
            frame_rate: FrameRateTracker::default(),
            projected_geometry: ProjectedGeometryCache::default(),
            visible_operations: VisibleOperationCache::default(),
            automatic_tile_generation_pause: false,
            mouse_history: MouseHistory::default(),
            depth_jump_target: 0,
            lateral_jump_x: "0".to_owned(),
            lateral_jump_y: "0".to_owned(),
            coordinate_overlay: CoordinateOverlayCache::default(),
        })
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tool, ToolKind::Brush, "Brush (B)");
            ui.selectable_value(&mut self.tool, ToolKind::Eraser, "Eraser (E)");
            ui.selectable_value(&mut self.tool, ToolKind::LassoFill, "Fill (L)");
            ui.selectable_value(&mut self.tool, ToolKind::Eyedropper, "Picker (I)");
            ui.separator();
            ui.add(
                egui::Slider::new(&mut self.brush_size, MIN_BRUSH_SIZE..=MAX_BRUSH_SIZE)
                    .text("Size"),
            );
            let mut color = [self.color.r, self.color.g, self.color.b];
            if ui.color_edit_button_srgb(&mut color).changed() {
                self.color = color_from_srgb(color);
            }
            ui.separator();
            if ui.button("Undo").clicked() {
                self.run_undo();
            }
            if ui.button("Redo").clicked() {
                self.run_redo();
            }
            if ui.button("Open").clicked() {
                self.open_dialog();
            }
            if ui.button("New").clicked() {
                self.new_dialog();
            }
            if ui.button("Add bookmark").clicked() {
                let name = format!("Bookmark {}", self.bookmarks.len() + 1);
                match self.document.add_bookmark(&name, &self.camera) {
                    Ok(bookmark) => {
                        self.bookmark_edits
                            .insert(bookmark.id, bookmark.name.clone());
                        self.bookmarks.push(bookmark);
                        self.status_message = "Bookmark saved".to_owned();
                    }
                    Err(error) => self.status_message = format!("Bookmark failed: {error:#}"),
                }
            }
            if ui.button("Bookmarks").clicked() {
                self.show_bookmarks = true;
            }
            if ui.button("Settings").clicked() {
                self.show_settings = true;
            }
        });
        let mut jump_requested = false;
        ui.horizontal(|ui| {
            ui.label(format!("Current depth: {}", self.camera.depth));
            ui.label("Jump to");
            let response = ui.add(
                egui::DragValue::new(&mut self.depth_jump_target)
                    .range(MIN_QUICK_DEPTH..=MAX_QUICK_DEPTH)
                    .speed(1),
            );
            let submit_by_enter =
                response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            jump_requested = ui.button("Go").clicked() || submit_by_enter;
        });
        if jump_requested {
            self.jump_to_target_depth();
        }
        let mut lateral_jump_requested = false;
        let mut origin_requested = false;
        ui.horizontal(|ui| {
            ui.label("Tile jump");
            ui.label("X");
            let x_response = ui.add(
                egui::TextEdit::singleline(&mut self.lateral_jump_x)
                    .desired_width(150.0)
                    .char_limit(MAX_LATERAL_COORDINATE_DIGITS + 8),
            );
            ui.label("Y");
            let y_response = ui.add(
                egui::TextEdit::singleline(&mut self.lateral_jump_y)
                    .desired_width(150.0)
                    .char_limit(MAX_LATERAL_COORDINATE_DIGITS + 8),
            );
            let submit_by_enter = (x_response.lost_focus() || y_response.lost_focus())
                && ui.input(|input| input.key_pressed(egui::Key::Enter));
            lateral_jump_requested = ui.button("Go XY").clicked() || submit_by_enter;
            origin_requested = ui.button("Origin").clicked();
        });
        if lateral_jump_requested {
            self.jump_to_lateral_target();
        } else if origin_requested {
            self.jump_to_lateral_origin();
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
        let previous_edge_quality = self.settings.edge_quality;
        let previous_smoothing = self.settings.smoothing;
        let previous_cache_size_mib = self.settings.cache_size_mib;
        let previous_png_compression = self.settings.png_compression;

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
                changed |= ui
                    .add(
                        egui::Slider::new(
                            &mut self.settings.preview_fps,
                            MIN_PREVIEW_FPS..=MAX_PREVIEW_FPS,
                        )
                        .text("Preview FPS"),
                    )
                    .changed();
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
            let render_quality_changed = self.settings.edge_quality != previous_edge_quality
                || self.settings.smoothing != previous_smoothing;
            let cache_settings_changed = self.settings.cache_size_mib != previous_cache_size_mib
                || self.settings.png_compression != previous_png_compression;
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
            if rebuild_policy_changed || prefetch_radius_changed {
                self.apply_tile_request_settings();
            }
            self.save_settings();
        }
    }

    fn bookmark_window(&mut self, context: &egui::Context) {
        if !self.show_bookmarks {
            return;
        }

        let mut open = self.show_bookmarks;
        let mut close_requested = false;
        let mut selected_camera = None;
        let mut rename_request = None;
        let mut delete_request = None;
        let bookmarks = self.bookmarks.clone();

        egui::Window::new("Bookmarks")
            .open(&mut open)
            .default_width(520.0)
            .resizable(true)
            .collapsible(false)
            .show(context, |ui| {
                if bookmarks.is_empty() {
                    ui.label("No bookmarks");
                    return;
                }

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

                ui.separator();
                if ui.button("Close").clicked() {
                    close_requested = true;
                }
            });

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

    fn canvas(&mut self, ui: &mut egui::Ui, context: &egui::Context, window: Option<NativeWindow>) {
        let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        painter.rect_filled(response.rect, 0.0, to_color32(BACKGROUND));
        let frame_time = ui.input(|input| input.unstable_dt);

        self.handle_shortcuts(context);
        self.handle_navigation(ui, &response);
        self.handle_drawing(ui, &response, window);
        let pointer_down = ui.input(|input| input.pointer.any_down());
        let input_active = self.draft.is_some() || pointer_down;
        self.update_tile_rebuild_interaction(input_active);
        let interaction_active =
            input_active || self.zoom_tile_requests_deferred() || self.tile_rebuild_is_deferred();
        self.frame_rate.update(frame_time, interaction_active);
        match self.paint_cached_tiles(context, &painter, response.rect) {
            TileFallbackMode::Current => {}
            TileFallbackMode::OverlayAfter(sequence) => {
                self.paint_operations_after(&painter, response.rect, sequence);
            }
            TileFallbackMode::Full => self.paint_operations(&painter, response.rect),
        }
        self.paint_draft(&painter, response.rect);
        self.paint_overlay(&painter, response.rect);
    }

    fn handle_shortcuts(&mut self, context: &egui::Context) {
        context.input(|input| {
            if input.key_pressed(egui::Key::B) {
                self.tool = ToolKind::Brush;
            } else if input.key_pressed(egui::Key::E) {
                self.tool = ToolKind::Eraser;
            } else if input.key_pressed(egui::Key::L) {
                self.tool = ToolKind::LassoFill;
            } else if input.key_pressed(egui::Key::I) {
                self.tool = ToolKind::Eyedropper;
            }
        });
        let undo = context.input_mut(|input| {
            input.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::CTRL,
                egui::Key::Z,
            ))
        });
        let redo = context.input_mut(|input| {
            input.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::CTRL,
                egui::Key::Y,
            ))
        });
        if undo {
            self.run_undo();
        }
        if redo {
            self.run_redo();
        }
    }

    fn handle_navigation(&mut self, ui: &egui::Ui, response: &egui::Response) {
        if response.hovered() {
            let scroll = ui.input(|input| input.smooth_scroll_delta.y);
            if scroll.abs() > f32::EPSILON
                && let Some(position) = response.hover_pos()
            {
                let factor = (scroll as f64 * 0.002).exp();
                self.camera.zoom_at(
                    factor,
                    (position.x - response.rect.left()) as f64,
                    (position.y - response.rect.top()) as f64,
                    response.rect.width() as f64,
                    response.rect.height() as f64,
                );
                self.mark_zoom_changed();
            }
        }

        if is_space_pan_active(ui) {
            let delta = ui.input(|input| input.pointer.delta());
            if delta.x.abs() > f32::EPSILON || delta.y.abs() > f32::EPSILON {
                self.camera.pan_content_by(delta.x as f64, delta.y as f64);
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
            }
            return;
        }

        if ui.input(|input| input.pointer.button_down(PointerButton::Middle)) {
            let delta = ui.input(|input| input.pointer.delta());
            if delta.x.abs() > f32::EPSILON || delta.y.abs() > f32::EPSILON {
                self.camera.pan_content_by(delta.x as f64, delta.y as f64);
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
            return;
        }

        let pointer = ui.input(|input| input.pointer.clone());
        let pixels_per_point = ui.ctx().pixels_per_point();
        let position = pointer.interact_pos();
        let primary_pressed = pointer.button_pressed(PointerButton::Primary);
        let primary_down = pointer.button_down(PointerButton::Primary);
        let primary_released = pointer.button_released(PointerButton::Primary);
        let events = ui.input(|input| input.events.clone());
        let (event_press_position, mut drag_positions) =
            primary_pointer_positions(&events, primary_down);
        let shift_down = ui.input(|input| input.modifiers.shift);
        let ctrl_down = ui.input(|input| input.modifiers.ctrl);

        if primary_pressed && ctrl_down && response.hovered() {
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

        if primary_pressed && response.hovered() {
            let press_position = event_press_position.or(position);
            if self.tool == ToolKind::Eyedropper {
                self.pick_color_at(press_position, response.rect);
                return;
            }
            if let Some(position) = press_position {
                let point = self.position_to_canvas(position, response.rect);
                let kind = match self.tool {
                    ToolKind::Brush => EditKind::Paint,
                    ToolKind::Eraser => EditKind::Erase,
                    ToolKind::LassoFill => EditKind::Fill,
                    ToolKind::Eyedropper => return,
                };
                let color = if kind == EditKind::Erase {
                    BACKGROUND
                } else {
                    self.color
                };
                self.draft = Some(EditOperation::draft(
                    kind,
                    self.camera.depth,
                    self.camera.zoom,
                    vec![point],
                    color,
                    self.brush_size,
                ));
                self.mouse_history.begin(window, position, pixels_per_point);
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
            && (primary_down || primary_released)
            && let Some(position) = position
            && let Some(history_positions) =
                self.mouse_history
                    .positions_since(window, position, pixels_per_point)
            && history_positions.len() > drag_positions.len()
        {
            drag_positions = history_positions;
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
            self.finish_draft();
        }
        self.last_pointer_position = position;
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

    fn finish_draft(&mut self) {
        self.mouse_history.reset();
        let Some(mut draft) = self.draft.take() else {
            self.end_drawing_tile_pause();
            return;
        };
        prepare_draft_for_commit(&mut draft);
        if !draft_is_committable(&draft) {
            let _ = self.document.discard_draft(&draft);
            self.end_drawing_tile_pause();
            return;
        }
        match self.document.commit(draft) {
            Ok(()) => {
                self.begin_new_document_revision();
                self.status_message = "Saved".to_owned();
            }
            Err(error) => self.status_message = format!("Save failed: {error:#}"),
        }
        self.end_drawing_tile_pause();
    }

    fn paint_operations(&mut self, painter: &Painter, rect: Rect) {
        for (index, points) in self.projected_operations(rect, None) {
            if let Some(operation) = self.document.operations().get(index) {
                self.paint_operation_points(painter, rect, operation, false, points);
            }
        }
    }

    fn paint_operations_after(&mut self, painter: &Painter, rect: Rect, sequence: i64) {
        for (index, points) in self.projected_operations(rect, Some(sequence)) {
            if let Some(operation) = self.document.operations().get(index) {
                self.paint_operation_points(painter, rect, operation, false, points);
            }
        }
    }

    fn projected_operations(
        &mut self,
        rect: Rect,
        after_sequence: Option<i64>,
    ) -> Vec<(usize, Vec<Pos2>)> {
        let indices = self.visible_operation_indices(rect);
        let Some(frame) = self.projected_geometry.begin_frame(&self.camera, rect) else {
            return Vec::new();
        };
        let operations = self.document.operations();
        let mut projected = Vec::with_capacity(indices.len());
        for index in indices {
            let Some(operation) = operations.get(index) else {
                continue;
            };
            if after_sequence.is_some_and(|sequence| operation.sequence <= sequence) {
                continue;
            }
            let points = self.projected_geometry.project_operation(operation, frame);
            projected.push((index, points));
        }
        projected
    }

    fn visible_operation_indices(&mut self, rect: Rect) -> Vec<usize> {
        let lod = tile_lod_for_resolution(self.settings.tile_resolution_px);
        let visible_tiles = self.visible_tiles(rect, lod);
        let revision = self.document.revision();
        let document = &self.document;
        self.visible_operations
            .get_or_update(revision, &visible_tiles, || {
                document.operation_indices_for_tiles(&visible_tiles)
            })
            .to_vec()
    }

    fn paint_draft(&self, painter: &Painter, rect: Rect) {
        if !should_paint_live_draft(self.settings.deferred_drawing_preview) {
            return;
        }
        if let Some(operation) = &self.draft {
            self.paint_operation(painter, rect, operation, true);
        }
    }

    fn paint_operation(
        &self,
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
        self.paint_operation_points(painter, rect, operation, is_draft, points);
    }

    fn paint_operation_points(
        &self,
        painter: &Painter,
        rect: Rect,
        operation: &EditOperation,
        is_draft: bool,
        points: Vec<Pos2>,
    ) {
        if points.len() < 2 {
            return;
        }

        let width = operation_width(operation, self.camera.depth, self.camera.zoom);
        let color = to_color32(operation.opaque_visible_color(BACKGROUND));
        if operation.kind == EditKind::Fill {
            if is_draft {
                let points =
                    smooth_pos2_draft(&points, usize::from(self.settings.smoothing.passes()));
                painter.add(egui::Shape::line(points, Stroke::new(1.5, color)));
            } else {
                let points = simplify_pos2_render_points(&points, true);
                if should_paint_fill_fallback(
                    operation,
                    self.camera.depth,
                    points.len(),
                    &self.settings,
                ) {
                    let points = smooth_pos2_saved_fallback(
                        &points,
                        usize::from(self.settings.smoothing.passes()),
                        true,
                    );
                    let clipped = clip_pos2_polygon(&points, rect);
                    paint_fill_scanlines(painter, rect, &clipped, color);
                }
            }
        } else if is_draft {
            let points = smooth_pos2_draft(&points, usize::from(self.settings.smoothing.passes()));
            paint_stroke_fallback(painter, points, width, color);
        } else {
            let points = simplify_pos2_render_points(&points, false);
            let clip_rect = rect.expand(width * 0.5 + 1.0);
            for run in clip_pos2_polyline(&points, clip_rect) {
                let run = smooth_pos2_saved_fallback(
                    &run,
                    usize::from(self.settings.smoothing.passes()),
                    false,
                );
                paint_stroke_fallback(painter, run, width, color);
            }
        }
    }

    fn paint_overlay(&mut self, painter: &Painter, rect: Rect) {
        self.coordinate_overlay.update(&self.camera);
        let tile_state = if self.settings.pause_tile_generation {
            "   tiles paused"
        } else if self.automatic_tile_generation_pause {
            "   tiles paused: drawing"
        } else if self.tile_rebuild_is_deferred() {
            "   rebuild waiting"
        } else {
            ""
        };
        let performance = self.frame_rate.metrics().map_or_else(
            || "fps --".to_owned(),
            |(fps, frame_time)| format!("fps {fps:.1} ({frame_time:.1} ms)"),
        );
        let text = format!(
            "depth {}   zoom {:.3}×   {} ops   {}   {}{}\n\
             tile X {}\n\
             tile Y {}   local X {:.6}   Y {:.6}",
            self.camera.depth,
            self.camera.zoom,
            self.document.operations().len(),
            performance,
            self.status_message,
            tile_state,
            self.coordinate_overlay.formatted_x,
            self.coordinate_overlay.formatted_y,
            self.camera.local_x,
            self.camera.local_y
        );
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
    ) -> TileFallbackMode {
        self.collect_tile_results(context);
        let revision = self.document.revision();
        self.pending_tiles
            .retain(|(_, cached_revision)| *cached_revision == revision);

        if self.zoom_tile_requests_deferred() {
            return TileFallbackMode::Full;
        }

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
        }

        if !tile_generation_is_paused(
            self.settings.pause_tile_generation,
            self.automatic_tile_generation_pause,
        ) && !self.tile_rebuild_is_deferred()
        {
            let missing: Vec<TileKey> = requested
                .iter()
                .filter(|key| {
                    self.tile_textures
                        .get(*key)
                        .is_none_or(|loaded| loaded.revision != revision)
                        && !self.pending_tiles.contains(&((*key).clone(), revision))
                })
                .cloned()
                .collect();
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
                }
            }
        }

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
        tile_fallback_mode(revision, snapshots)
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

    fn collect_tile_results(&mut self, context: &egui::Context) {
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
    }

    fn visible_tiles(&self, rect: Rect, lod: u8) -> Vec<TileKey> {
        tile_keys_for_view(&self.camera, rect, lod, 0)
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
        let (x, y) =
            self.camera
                .canvas_to_screen(point, rect.width() as f64, rect.height() as f64)?;
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        Some(Pos2::new(rect.left() + x as f32, rect.top() + y as f32))
    }

    fn pick_color_at(&mut self, position: Option<Pos2>, rect: Rect) {
        let Some(position) = position else {
            return;
        };
        for operation in self.document.operations().iter().rev() {
            let color = operation.opaque_visible_color(BACKGROUND);
            if color == BACKGROUND {
                continue;
            }
            if operation.points.iter().any(|point| {
                self.point_to_position(point, rect)
                    .is_some_and(|candidate| candidate.distance(position) <= operation.width_px)
            }) {
                self.color = color;
                self.status_message = "Color picked".to_owned();
                return;
            }
        }
    }

    fn run_undo(&mut self) {
        match self.document.undo() {
            Ok(true) => {
                self.invalidate_tile_rendering();
                self.status_message = "Undone".to_owned();
            }
            Ok(false) => self.status_message = "Nothing to undo".to_owned(),
            Err(error) => self.status_message = format!("Undo failed: {error:#}"),
        }
    }

    fn run_redo(&mut self) {
        match self.document.redo() {
            Ok(true) => {
                self.invalidate_tile_rendering();
                self.status_message = "Redone".to_owned();
            }
            Ok(false) => self.status_message = "Nothing to redo".to_owned(),
            Err(error) => self.status_message = format!("Redo failed: {error:#}"),
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
                self.camera = CameraAddress::default();
                self.depth_jump_target = self.camera.depth;
                self.status_message = "Canvas opened".to_owned();
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
    }

    fn mark_zoom_changed(&mut self) {
        self.depth_jump_target = self.camera.depth;
        self.last_zoom_change = Some(Instant::now());
        self.visible_tile_generation = self.visible_tile_generation.saturating_add(1);
        self.tile_scheduler
            .set_generation(self.visible_tile_generation);
        self.pending_tiles.clear();
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
            Ok(()) => self.status_message = "Settings saved".to_owned(),
            Err(error) => self.status_message = format!("Settings failed: {error:#}"),
        }
    }

    fn recreate_tile_scheduler(&mut self) {
        match TileCache::with_options(self.document.root(), self.settings.tile_cache_options()) {
            Ok(cache) => {
                self.tile_scheduler = TileScheduler::new(cache, self.settings.tile_worker_count);
                self.invalidate_tile_rendering();
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
    }

    fn invalidate_projection_caches(&mut self) {
        self.projected_geometry.clear();
        self.visible_operations.clear();
    }

    fn begin_new_document_revision(&mut self) {
        self.pending_tiles.clear();
        self.visible_tile_generation = self.visible_tile_generation.saturating_add(1);
        self.tile_scheduler
            .set_generation(self.visible_tile_generation);
    }

    fn apply_tile_generation_pause(&mut self) {
        self.pending_tiles.clear();
        self.visible_tile_generation = self.visible_tile_generation.saturating_add(1);
        self.tile_scheduler
            .set_generation(self.visible_tile_generation);
        if self.settings.pause_tile_generation {
            self.status_message = "Tile generation paused".to_owned();
        } else if self.automatic_tile_generation_pause {
            self.status_message = "Tile generation paused while drawing".to_owned();
        } else {
            self.visible_tile_set.clear();
            self.status_message = "Tile generation resumed".to_owned();
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
    }

    fn end_drawing_tile_pause(&mut self) {
        if !self.automatic_tile_generation_pause {
            return;
        }
        self.automatic_tile_generation_pause = false;
        self.visible_tile_set.clear();
    }
}

fn tile_generation_is_paused(manual_pause: bool, drawing_pause: bool) -> bool {
    manual_pause || drawing_pause
}

fn simplify_pos2_render_points(points: &[Pos2], closed: bool) -> Vec<Pos2> {
    let tuples: Vec<_> = points.iter().map(|point| (point.x, point.y)).collect();
    simplify_render_points(&tuples, closed)
        .into_iter()
        .map(|(x, y)| Pos2::new(x, y))
        .collect()
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
        smooth_closed_points(&tuples, level)
    } else {
        smooth_stroke_points(&tuples, level)
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

fn paint_fill_scanlines(painter: &Painter, clip_rect: Rect, points: &[Pos2], color: Color32) {
    if points.len() < 3 {
        return;
    }
    let min_y = points
        .iter()
        .map(|point| point.y)
        .fold(f32::INFINITY, f32::min)
        .floor()
        .max(clip_rect.top())
        .max(i32::MIN as f32) as i32;
    let max_y = points
        .iter()
        .map(|point| point.y)
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil()
        .min(clip_rect.bottom())
        .min(i32::MAX as f32) as i32;
    if min_y > max_y {
        return;
    }

    let mut intersections = Vec::new();
    for y in min_y..=max_y {
        let scan_y = y as f32 + 0.5;
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

        for span in intersections.chunks_exact(2) {
            let left = span[0].max(clip_rect.left());
            let right = span[1].min(clip_rect.right());
            if right > left {
                painter.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(left, y as f32 - 0.5),
                        Pos2::new(right, y as f32 + 1.5),
                    ),
                    0.0,
                    color,
                );
            }
        }
    }
}

fn paint_stroke_fallback(painter: &Painter, points: Vec<Pos2>, width: f32, color: Color32) {
    painter.extend(fast_stroke_fallback_shapes(points, width, color));
}

fn fast_stroke_fallback_shapes(points: Vec<Pos2>, width: f32, color: Color32) -> Vec<egui::Shape> {
    if points.len() < 2 {
        return Vec::new();
    }
    debug_assert_eq!(color.a(), u8::MAX);
    let radius = width * 0.5;
    if points.len() == 2 && points[0].distance(points[1]) <= f32::EPSILON {
        return vec![egui::Shape::circle_filled(points[0], radius, color)];
    }
    let start = points[0];
    let end = *points.last().expect("stroke has at least two points");
    vec![
        egui::Shape::line(points, Stroke::new(width, color)),
        egui::Shape::circle_filled(start, radius, color),
        egui::Shape::circle_filled(end, radius, color),
    ]
}

impl eframe::App for EndlessSketchApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let context = ui.ctx().clone();
        let window = native_window(frame);
        egui::Panel::top("toolbar").show_inside(ui, |ui| self.toolbar(ui));
        self.bookmark_window(&context);
        self.settings_window(&context);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| self.canvas(ui, &context, window));
        if let Some(interval) = self.repaint_interval() {
            context.request_repaint_after(interval);
        }
    }

    fn on_exit(&mut self) {
        if let Some(draft) = &self.draft {
            let _ = self.document.save_draft(draft);
        }
        if let Err(error) = self.document.checkpoint() {
            log::error!("checkpoint failed during shutdown: {error:#}");
        }
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        if let Err(error) = self.document.checkpoint() {
            log::error!("periodic checkpoint failed: {error:#}");
        }
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
    let newer: Vec<_> = operations
        .iter()
        .filter(|operation| operation.sequence > snapshot_sequence)
        .collect();
    if newer
        .iter()
        .all(|operation| operation.kind == EditKind::Paint && operation.color.is_opaque())
    {
        Some(newer.into_iter().cloned().collect())
    } else {
        None
    }
}

fn primary_pointer_positions(
    events: &[egui::Event],
    primary_down_after_events: bool,
) -> (Option<Pos2>, Vec<Pos2>) {
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

    (press_position, drag_positions)
}

fn append_interpolated_points(
    draft: &mut EditOperation,
    camera: &CameraAddress,
    position: Pos2,
    rect: Rect,
    settings: &AppSettings,
) {
    if fill_draft_is_full(draft, settings) {
        return;
    }
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
    if draft.kind == EditKind::Fill && draft.points.len() >= 3 {
        draft.points.push(draft.points[0].clone());
    }
}

fn draft_is_committable(draft: &EditOperation) -> bool {
    if draft.kind == EditKind::Fill {
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

fn append_draft_point(draft: &mut EditOperation, point: CanvasPoint, settings: &AppSettings) {
    if fill_draft_is_full(draft, settings) {
        return;
    }
    draft.points.push(point);
}

fn fill_draft_is_full(draft: &EditOperation, settings: &AppSettings) -> bool {
    draft.kind == EditKind::Fill && draft.points.len() >= settings.fill_draft_max_points()
}

fn interpolation_step_count(
    kind: EditKind,
    existing_points: usize,
    distance: f32,
    settings: &AppSettings,
) -> usize {
    let desired = (distance / max_interpolation_gap_px(kind)).ceil().max(1.0) as usize;
    if kind == EditKind::Fill {
        let remaining = settings
            .fill_draft_max_points()
            .saturating_sub(existing_points);
        desired.min(remaining)
    } else {
        desired
    }
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

fn bookmark_edit_names(bookmarks: &[Bookmark]) -> HashMap<Uuid, String> {
    bookmarks
        .iter()
        .map(|bookmark| (bookmark.id, bookmark.name.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        FRAME_TIME_EMA_ALPHA, FrameRateTracker, MAX_BRUSH_SIZE,
        MAX_FALLBACK_SMOOTHING_INPUT_POINTS, MIN_BRUSH_SIZE, PointAppendDecision,
        TILE_POLL_REPAINT_INTERVAL, TileFallbackMode, adjusted_brush_size,
        append_interpolated_points, clamp_quick_depth, clip_pos2_polygon, clip_pos2_polyline,
        color_from_srgb, draft_is_committable, fast_stroke_fallback_shapes,
        format_overlay_coordinate, incremental_paint_operations, interpolation_step_count,
        parse_lateral_coordinate, point_append_decision, prepare_draft_for_commit,
        primary_pointer_positions, repaint_interval_for_work, set_straight_draft_endpoint,
        should_paint_fill_fallback, should_paint_live_draft, simplify_pos2_render_points,
        smooth_pos2_draft, smooth_pos2_saved_fallback, space_pan_requested,
        straight_line_requested, tile_display_rects, tile_fallback_mode, tile_generation_is_paused,
        tile_keys_for_view, tile_rebuild_deferred_since, z_drag_zoom_factor, z_zoom_requested,
        zoom_tile_requests_deferred_since,
    };
    use crate::coords::{CameraAddress, CanvasPoint};
    use crate::model::EditKind;
    use crate::model::{Color, EditOperation};
    use crate::settings::AppSettings;
    use crate::tile_cache::{TILE_BLEED, TILE_SIZE, tile_lod_for_resolution, tile_resolution};
    use eframe::egui::{Color32, Event, Modifiers, PointerButton, Pos2, Rect, Shape};
    use num_bigint::BigInt;
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

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

        tracker.update(1.0 / 20.0, false);
        assert_eq!(tracker.metrics(), None);

        tracker.update(1.0 / 60.0, true);
        let (fps, frame_time) = tracker.metrics().expect("active measurement");
        assert!((fps - 60.0).abs() < 0.01);
        assert!((frame_time - 1000.0 / 60.0).abs() < 0.01);

        tracker.update(1.0 / 30.0, true);
        let expected = 1.0 / 60.0 + (1.0 / 30.0 - 1.0 / 60.0) * FRAME_TIME_EMA_ALPHA;
        assert!(
            (tracker.metrics().expect("averaged measurement").1 - expected * 1000.0).abs() < 0.01
        );

        tracker.update(1.0 / 20.0, false);
        tracker.update(1.0 / 40.0, true);
        let (fps, _) = tracker.metrics().expect("reset measurement");
        assert!((fps - 40.0).abs() < 0.01);
    }

    #[test]
    fn frame_rate_tracker_rejects_invalid_frame_times() {
        let mut tracker = FrameRateTracker::default();

        tracker.update(f32::NAN, true);
        tracker.update(0.0, true);
        tracker.update(1.0, true);

        assert_eq!(tracker.metrics(), None);
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

        let (press, positions) = primary_pointer_positions(&events, false);

        assert_eq!(press, Some(Pos2::new(1.0, 1.0)));
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

        let (press, positions) = primary_pointer_positions(&events, false);

        assert_eq!(press, None);
        assert_eq!(positions, vec![Pos2::new(6.0, 6.0), Pos2::new(7.0, 7.0)]);
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
    fn fill_interpolation_is_bounded_by_the_draft_point_limit() {
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
            1
        );
        assert_eq!(
            interpolation_step_count(EditKind::Fill, max_fill_draft_points, 10_000.0, &settings),
            0
        );
        assert!(
            interpolation_step_count(EditKind::Paint, max_fill_draft_points, 10_000.0, &settings)
                > 1
        );
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
    fn rgb_color_edit_produces_opaque_drawing_color() {
        assert_eq!(
            color_from_srgb([250, 240, 40]),
            Color::rgba(250, 240, 40, 255)
        );
    }

    #[test]
    fn fast_stroke_fallback_keeps_raw_points_and_uses_three_shapes() {
        let points: Vec<_> = (0..100)
            .map(|index| Pos2::new(index as f32, (index % 7) as f32))
            .collect();
        let expected_points = points.clone();

        let shapes = fast_stroke_fallback_shapes(points, 8.0, Color32::BLACK);

        assert_eq!(shapes.len(), 3);
        let Shape::Path(path) = &shapes[0] else {
            panic!("first fallback shape must be one polyline");
        };
        assert_eq!(path.points, expected_points);
        assert!(matches!(shapes[1], Shape::Circle(_)));
        assert!(matches!(shapes[2], Shape::Circle(_)));
    }

    #[test]
    fn saved_fallback_geometry_is_simplified_and_clipped() {
        let dense: Vec<_> = (0..1000)
            .map(|index| Pos2::new(index as f32 * 0.1, 5.0))
            .collect();
        let simplified = simplify_pos2_render_points(&dense, false);
        assert_eq!(simplified.first(), dense.first());
        assert_eq!(simplified.last(), dense.last());
        assert!(simplified.len() < 10, "points={}", simplified.len());

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
}
