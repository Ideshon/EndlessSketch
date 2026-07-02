use crate::coords::{CameraAddress, CanvasPoint, ScreenAffine};
use crate::document::CanvasDocument;
use crate::help::HelpLibrary;
use crate::local_paths::{default_canvas_path, settings_path};
use crate::model::{Bookmark, Color, DEFAULT_LAYER_ID, EditKind, EditOperation, Layer, ToolKind};
use crate::mouse_history::{MouseHistory, NativeWindow, native_window};
use crate::projection_cache::{ProjectedGeometryCache, VisibleOperationCache};
use crate::raster::operation_width;
use crate::selection_geometry::{Point2 as SelectionPoint, Rect2 as SelectionRect, SelectionShape};
use crate::settings::{
    AppSettings, EdgeQuality, MAX_BRUSH_INPUT_SPACING_PX, MAX_CACHE_SIZE_MIB,
    MAX_FILL_INPUT_SPACING_PX, MAX_PREVIEW_FPS, MAX_TILE_PREFETCH_RADIUS,
    MIN_BRUSH_INPUT_SPACING_PX, MIN_CACHE_SIZE_MIB, MIN_FILL_INPUT_SPACING_PX, MIN_PREVIEW_FPS,
    OverlayProfile, PerformanceProfile, PngCompression, SmoothingLevel, TileRebuildPolicy,
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
const MIN_LASSO_PREVIEW_SPACING_PX: f32 = 2.0;
const CLICK_SELECTION_DRAG_THRESHOLD_PX: f32 = 4.0;
const CLICK_SELECTION_TOLERANCE_PX: f64 = 6.0;
const SELECTION_CYCLE_POSITION_TOLERANCE_PX: f32 = 6.0;
const SELECTION_HANDLE_SIZE_PX: f32 = 10.0;
const SELECTION_HANDLE_HIT_RADIUS_PX: f32 = 9.0;
const MIN_SELECTION_SCALE: f32 = 0.05;
const MAX_SELECTION_SCALE: f32 = 64.0;

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
enum RectangleSelectionMode {
    Inside,
    Crossing,
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
    Clipboard(ClipboardCommand),
    Delete,
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
    new_bookmark_name: String,
    show_file: bool,
    file_window_tab: FileWindowTab,
    help_library: HelpLibrary,
    help_language_id: String,
    show_navigation: bool,
    show_bookmarks: bool,
    show_settings: bool,
    show_layers: bool,
    pending_layer_delete: Option<Uuid>,
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
    rectangle_selection_mode: RectangleSelectionMode,
    hovered_operation_id: Option<Uuid>,
    selection_hover_position: Option<Pos2>,
    selection_cycle_position: Option<Pos2>,
    selection_cycle_candidates: Vec<Uuid>,
    selection_cycle_index: usize,
    eraser_lasso_points: Vec<Pos2>,
    eraser_lasso_active: bool,
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
            new_bookmark_name,
            show_file: false,
            file_window_tab: FileWindowTab::File,
            help_library,
            help_language_id,
            show_navigation: false,
            show_bookmarks: false,
            show_settings: false,
            show_layers: false,
            pending_layer_delete: None,
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
            rectangle_selection_mode: RectangleSelectionMode::Inside,
            hovered_operation_id: None,
            selection_hover_position: None,
            selection_cycle_position: None,
            selection_cycle_candidates: Vec::new(),
            selection_cycle_index: 0,
            eraser_lasso_points: Vec::new(),
            eraser_lasso_active: false,
        })
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let mut selection_action = None;
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.tool, ToolKind::Brush, "Brush (B)");
            ui.selectable_value(&mut self.tool, ToolKind::Eraser, "Eraser (E)");
            ui.selectable_value(&mut self.tool, ToolKind::LassoFill, "Fill (L)");
            ui.selectable_value(&mut self.tool, ToolKind::Eyedropper, "Picker (I)");
            ui.selectable_value(&mut self.tool, ToolKind::Selection, "Select (S)");
            ui.selectable_value(&mut self.tool, ToolKind::EraserLasso, "Erase lasso (X)");
            if self.tool == ToolKind::Selection {
                ui.separator();
                let mode = match self.rectangle_selection_mode {
                    RectangleSelectionMode::Inside => "Inside",
                    RectangleSelectionMode::Crossing => "Crossing",
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
            SelectionMenuAction::Clipboard(command) => self.run_clipboard_command(command),
            SelectionMenuAction::Delete => self.delete_selected(),
        }
    }

    fn drawing_tool_menu_content(&mut self, ui: &mut egui::Ui) {
        ui.label("Color");
        ui.set_min_width(280.0);
        let mut color = Color32::from_rgb(self.color.r, self.color.g, self.color.b);
        if egui::color_picker::color_picker_color32(
            ui,
            &mut color,
            egui::color_picker::Alpha::Opaque,
        ) {
            self.color = color_from_srgb([color.r(), color.g(), color.b()]);
        }
        ui.add(
            egui::Slider::new(&mut self.brush_size, MIN_BRUSH_SIZE..=MAX_BRUSH_SIZE).text("Size"),
        );
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
                    self.selected_operation_ids.len()
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

    fn canvas(&mut self, ui: &mut egui::Ui, context: &egui::Context, window: Option<NativeWindow>) {
        let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        painter.rect_filled(response.rect, 0.0, to_color32(BACKGROUND));
        let frame_time = ui.input(|input| input.unstable_dt);
        let pointer_pressed = context.input(|input| input.pointer.any_pressed());
        surrender_canvas_keyboard_focus(context, response.hovered(), pointer_pressed);

        self.handle_shortcuts(context);
        self.handle_navigation(ui, &response);
        self.handle_drawing(ui, &response, window);
        let pointer_down = ui.input(|input| input.pointer.any_down());
        let transient_preview_active =
            self.selection_drag_active || self.selection_move_active || self.eraser_lasso_active;
        let input_active = self.draft.is_some() || (pointer_down && !transient_preview_active);
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
        self.paint_transient_overlays(&painter, response.rect);
        self.paint_overlay(&painter, response.rect);
        self.canvas_context_menu(&response);
    }

    fn handle_shortcuts(&mut self, context: &egui::Context) {
        if !context.text_edit_focused() && context.input(|input| input.key_pressed(egui::Key::F1)) {
            self.file_window_tab = FileWindowTab::Help;
            self.show_file = true;
        }
        let native_clipboard_command = self.clipboard_shortcut_keys.poll();
        if !context.text_edit_focused()
            && let Some(command) = context
                .input(|input| clipboard_command_from_events(&input.events, input.modifiers))
                .or(native_clipboard_command)
        {
            self.run_clipboard_command(command);
        }
        context.input(|input| {
            let tool_shortcut_allowed = tool_shortcut_allowed(input.modifiers);
            if tool_shortcut_allowed && input.key_pressed(egui::Key::B) {
                self.tool = ToolKind::Brush;
            } else if tool_shortcut_allowed && input.key_pressed(egui::Key::E) {
                self.tool = ToolKind::Eraser;
            } else if tool_shortcut_allowed && input.key_pressed(egui::Key::L) {
                self.tool = ToolKind::LassoFill;
            } else if tool_shortcut_allowed && input.key_pressed(egui::Key::I) {
                self.tool = ToolKind::Eyedropper;
            } else if tool_shortcut_allowed && input.key_pressed(egui::Key::S) {
                self.tool = ToolKind::Selection;
            } else if tool_shortcut_allowed && input.key_pressed(egui::Key::X) {
                self.tool = ToolKind::EraserLasso;
            }
        });
        if context.input(|input| input.key_pressed(egui::Key::Escape)) {
            if self.selection_scale_gesture.take().is_some() {
                self.status_message = "Scale cancelled".to_owned();
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

    fn handle_navigation(&mut self, ui: &egui::Ui, response: &egui::Response) {
        if response.hovered() {
            if self.tool == ToolKind::Selection
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

            let scroll = ui.input(|input| input.smooth_scroll_delta.y);
            if scroll.abs() > f32::EPSILON
                && let Some(position) = response.hover_pos()
            {
                if self.tool == ToolKind::Selection && ui.input(|input| input.modifiers.alt) {
                    return;
                }
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
                self.hovered_operation_id = None;
                self.selection_hover_position = None;
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
                self.hovered_operation_id = None;
                self.selection_hover_position = None;
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
        let modifiers = ui.input(|input| SelectionModifiers {
            shift: input.modifiers.shift,
            ctrl: input.modifiers.ctrl,
            alt: input.modifiers.alt,
        });
        let shift_down = modifiers.shift;

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
        if matches!(self.tool, ToolKind::Selection | ToolKind::EraserLasso) {
            self.mouse_history.reset();
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

        if primary_pressed && response.hovered() {
            let press_position = event_press_position.or(position);
            if self.tool == ToolKind::Eyedropper {
                self.pick_color_at(press_position, response.rect);
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
                let mut draft = EditOperation::draft(
                    kind,
                    self.camera.depth,
                    self.camera.zoom,
                    vec![point],
                    color,
                    self.brush_size,
                );
                draft.layer_id = self.active_layer_id;
                self.draft = Some(draft);
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
                if !primary_down
                    && !self.selection_drag_active
                    && !self.selection_move_active
                    && self.selection_scale_gesture.is_none()
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
                    let scale_handle = (!modifiers.shift
                        && !modifiers.ctrl
                        && !modifiers.alt
                        && !self.selected_operation_ids.is_empty())
                    .then(|| self.selection_screen_bounds(response.rect))
                    .flatten()
                    .and_then(|bounds| {
                        selection_scale_handle_at(bounds, position).map(|handle| (handle, bounds))
                    });
                    if let Some((handle, bounds)) = scale_handle {
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
                        self.status_message = "Active layer is hidden or locked".to_owned();
                        return;
                    }
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

    fn complete_rectangle_selection(&mut self, canvas_rect: Rect, modifiers: SelectionModifiers) {
        let (Some(start), Some(end)) = (self.selection_drag_start, self.selection_drag_current)
        else {
            return;
        };
        let selection_rect =
            SelectionRect::from_points(selection_point(start), selection_point(end));
        let selection = SelectionShape::Rectangle(selection_rect);
        let candidates = self.visible_selectable_operation_ids(canvas_rect);
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
        self.apply_selection_ids(matches, modifiers);
        self.reset_selection_cycle();
        self.status_message = format!("Selected {} objects", self.selected_operation_ids.len());
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
            if modifiers.ctrl {
                if !self.selected_operation_ids.remove(&selected) {
                    self.selected_operation_ids.insert(selected);
                }
            } else if modifiers.shift {
                self.selected_operation_ids.insert(selected);
            } else {
                self.selected_operation_ids.clear();
                self.selected_operation_ids.insert(selected);
            }
        } else if !modifiers.shift && !modifiers.ctrl {
            self.selected_operation_ids.clear();
        }
        self.status_message = format!("Selected {} objects", self.selected_operation_ids.len());
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
        self.selected_operation_ids.clear();
        self.selected_operation_ids.insert(selected);
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
                self.invalidate_tile_rendering();
                self.status_message = if direction > 0 {
                    "Selection moved forward".to_owned()
                } else {
                    "Selection moved backward".to_owned()
                };
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

    fn visible_selectable_operation_ids(&self, canvas_rect: Rect) -> HashSet<Uuid> {
        let eligible_layers = selectable_layer_ids(self.document.layers(), self.active_layer_id);
        let lod = tile_lod_for_resolution(self.settings.tile_resolution_px);
        let visible_tiles = self.visible_tiles(canvas_rect, lod);
        self.document
            .operation_ids_for_tiles_in_layers(&visible_tiles, &eligible_layers)
            .into_iter()
            .collect()
    }

    fn selection_candidates_at(
        &self,
        position: Pos2,
        canvas_rect: Rect,
    ) -> Vec<SelectionCandidate> {
        let candidate_ids = self.visible_selectable_operation_ids(canvas_rect);
        let click = selection_point(position);
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
        if modifiers.ctrl {
            for id in matches {
                if !self.selected_operation_ids.remove(&id) {
                    self.selected_operation_ids.insert(id);
                }
            }
        } else if modifiers.shift {
            self.selected_operation_ids.extend(matches);
        } else {
            self.selected_operation_ids = matches;
        }
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
            for position in operation
                .points
                .iter()
                .filter_map(|point| self.point_to_position(point, canvas_rect))
            {
                min.x = min.x.min(position.x - radius);
                min.y = min.y.min(position.y - radius);
                max.x = max.x.max(position.x + radius);
                max.y = max.y.max(position.y + radius);
                found = true;
            }
        }
        found.then(|| Rect::from_min_max(min, max))
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
        let selected = self.selected_operation_ids.clone();
        match self.document.transform_operations(
            &selected,
            &self.camera,
            canvas_rect.width() as f64,
            canvas_rect.height() as f64,
            transform,
            scale,
            self.active_layer_id,
        ) {
            Ok(replacement_ids) if !replacement_ids.is_empty() => {
                self.selected_operation_ids = replacement_ids.into_iter().collect();
                self.invalidate_tile_rendering();
                self.status_message = format!(
                    "Scaled {} objects to {:.1}%",
                    self.selected_operation_ids.len(),
                    scale * 100.0
                );
            }
            Ok(_) => self.status_message = "Nothing scaled".to_owned(),
            Err(error) => self.status_message = format!("Scale failed: {error:#}"),
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
        match self.document.move_operations(
            &selected,
            self.camera.depth,
            self.camera.zoom,
            delta.x as f64,
            delta.y as f64,
            self.active_layer_id,
        ) {
            Ok(replacement_ids) if !replacement_ids.is_empty() => {
                self.selected_operation_ids = replacement_ids.into_iter().collect();
                if let Some(start) = self.selection_drag_start.as_mut() {
                    *start += delta;
                }
                if let Some(current) = self.selection_drag_current.as_mut() {
                    *current += delta;
                }
                self.invalidate_tile_rendering();
                self.status_message =
                    format!("Moved {} objects", self.selected_operation_ids.len());
            }
            Ok(_) => self.status_message = "Nothing moved".to_owned(),
            Err(error) => self.status_message = format!("Move failed: {error:#}"),
        }
    }

    fn commit_eraser_lasso(&mut self, canvas_rect: Rect) {
        if !self.document.layer_is_editable(self.active_layer_id) {
            self.status_message = "Active layer is hidden or locked".to_owned();
            return;
        }
        let Some(operation) = eraser_lasso_operation(
            &self.eraser_lasso_points,
            &self.camera,
            canvas_rect,
            self.active_layer_id,
        ) else {
            self.status_message = "Eraser Lasso needs a non-degenerate area".to_owned();
            return;
        };
        let layer_id = operation.layer_id;
        match self.document.commit(operation) {
            Ok(()) => {
                self.begin_new_document_revision_for_layer(layer_id);
                self.status_message = "Area erased".to_owned();
            }
            Err(error) => self.status_message = format!("Eraser Lasso failed: {error:#}"),
        }
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
        let layer_id = draft.layer_id;
        match self.document.commit(draft) {
            Ok(()) => {
                self.begin_new_document_revision_for_layer(layer_id);
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
        if operation.kind.is_area() {
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

    fn paint_transient_overlays(&self, painter: &Painter, rect: Rect) {
        let selection_color = Color32::from_rgba_unmultiplied(20, 120, 220, 180);
        let selection_fill = Color32::from_rgba_unmultiplied(20, 120, 220, 28);
        let rectangle_color = match self.rectangle_selection_mode {
            RectangleSelectionMode::Inside => selection_color,
            RectangleSelectionMode::Crossing => Color32::from_rgba_unmultiplied(220, 130, 20, 190),
        };
        let move_delta = self.selection_move_delta();
        let scale_gesture = self.selection_scale_gesture;

        if let (Some(start), Some(end)) = (self.selection_drag_start, self.selection_drag_current) {
            let selection_rect =
                Rect::from_two_pos(start + move_delta, end + move_delta).intersect(rect);
            painter.rect_filled(selection_rect, 0.0, selection_fill);
            paint_rect_outline(painter, selection_rect, Stroke::new(1.5, rectangle_color));
        }

        for operation in self
            .document
            .operations()
            .iter()
            .filter(|operation| self.selected_operation_ids.contains(&operation.id))
        {
            let points: Vec<_> = operation
                .points
                .iter()
                .filter_map(|point| self.point_to_position(point, rect))
                .map(|point| {
                    scale_gesture.map_or_else(
                        || point + move_delta,
                        |gesture| gesture.transform_position(point),
                    )
                })
                .collect();
            let width = operation_width(operation, self.camera.depth, self.camera.zoom)
                * scale_gesture.map_or(1.0, SelectionScaleGesture::scale);
            paint_operation_highlight(painter, operation.kind, &points, width, selection_color);
        }

        let selection_bounds = (self.tool == ToolKind::Selection)
            .then(|| {
                scale_gesture
                    .map(SelectionScaleGesture::transformed_bounds)
                    .or_else(|| {
                        self.selection_screen_bounds(rect).map(|bounds| {
                            Rect::from_min_max(bounds.min + move_delta, bounds.max + move_delta)
                        })
                    })
            })
            .flatten();
        if let Some(bounds) = selection_bounds {
            paint_rect_outline(painter, bounds, Stroke::new(1.5, selection_color));
            for handle in SelectionScaleHandle::ALL {
                let handle_rect = Rect::from_center_size(
                    handle.position(bounds),
                    egui::vec2(SELECTION_HANDLE_SIZE_PX, SELECTION_HANDLE_SIZE_PX),
                );
                painter.rect_filled(handle_rect, 0.0, Color32::WHITE);
                paint_rect_outline(painter, handle_rect, Stroke::new(1.5, selection_color));
            }
        }

        if let Some(operation) = self.hovered_operation_id.and_then(|id| {
            self.document
                .operations()
                .iter()
                .find(|operation| operation.id == id)
        }) && !self.selected_operation_ids.contains(&operation.id)
        {
            let points: Vec<_> = operation
                .points
                .iter()
                .filter_map(|point| self.point_to_position(point, rect))
                .collect();
            let width = operation_width(operation, self.camera.depth, self.camera.zoom);
            paint_operation_highlight(
                painter,
                operation.kind,
                &points,
                width,
                Color32::from_rgba_unmultiplied(30, 175, 95, 175),
            );
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
        self.selected_operation_ids.clear();
        self.selection_drag_start = None;
        self.selection_drag_current = None;
        self.selection_drag_active = false;
        self.selection_move_start = None;
        self.selection_move_current = None;
        self.selection_move_active = false;
        self.selection_scale_gesture = None;
        self.hovered_operation_id = None;
        self.selection_hover_position = None;
        self.reset_selection_cycle();
        self.eraser_lasso_points.clear();
        self.eraser_lasso_active = false;
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
        let tile_state = if self.settings.pause_tile_generation {
            "tiles paused"
        } else if self.automatic_tile_generation_pause {
            "tiles paused: drawing"
        } else if self.tile_rebuild_is_deferred() {
            "rebuild waiting"
        } else {
            ""
        };
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
            tile_state,
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
        self.clear_transient_tools();
        match self.document.undo() {
            Ok(true) => {
                self.sync_active_layer_after_history();
                self.invalidate_tile_rendering();
                self.status_message = "Undone".to_owned();
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
        let mut operations = self
            .document
            .operations()
            .iter()
            .filter(|operation| self.selected_operation_ids.contains(&operation.id))
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
        let count = operations.len();
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
        let count = operations.len();
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
                    self.selected_operation_ids.len(),
                    if in_place { " in place" } else { "" }
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
        self.hovered_operation_id = None;
        self.selection_hover_position = None;
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
        self.file_window(&context);
        self.navigation_window(&context);
        self.bookmark_window(&context);
        self.settings_window(&context);
        self.layers_window(&context);
        self.layer_delete_confirmation(&context);
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

fn redo_shortcuts() -> [egui::KeyboardShortcut; 2] {
    [
        egui::KeyboardShortcut::new(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::Z),
        egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::Y),
    ]
}

fn tool_shortcut_allowed(modifiers: egui::Modifiers) -> bool {
    !modifiers.ctrl && !modifiers.command && !modifiers.alt && !modifiers.mac_cmd
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

fn layer_operation_counts(operations: &[EditOperation]) -> HashMap<Uuid, usize> {
    let mut counts = HashMap::new();
    for operation in operations {
        *counts.entry(operation.layer_id).or_insert(0) += 1;
    }
    counts
}

fn selectable_layer_ids(layers: &[Layer], active_layer_id: Uuid) -> HashSet<Uuid> {
    layers
        .iter()
        .filter(|layer| layer.id == active_layer_id && layer.visible && !layer.locked)
        .map(|layer| layer.id)
        .collect()
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
        EditKind::Fill | EditKind::EraseArea => selection.intersects_polygon(points),
    }
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
    let radius = if kind.is_area() {
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

    let radius = if kind.is_area() {
        0.0
    } else {
        width.max(0.0) * 0.5
    };
    let centerline_distance = if kind.is_area() {
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
    if points
        .last()
        .is_none_or(|previous| previous.distance(point) >= minimum_spacing)
    {
        points.push(point);
    }
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

fn paint_operation_highlight(
    painter: &Painter,
    kind: EditKind,
    points: &[Pos2],
    width: f32,
    color: Color32,
) {
    if kind.is_area() {
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
        CanvasContextMenuKind, ClipboardCommand, ClipboardShortcutKeys, FRAME_TIME_EMA_ALPHA,
        FrameRateTracker, MAX_BRUSH_SIZE, MAX_FALLBACK_SMOOTHING_INPUT_POINTS, MIN_BRUSH_SIZE,
        PointAppendDecision, SelectionScaleGesture, SelectionScaleHandle,
        TILE_POLL_REPAINT_INTERVAL, TileFallbackMode, adjusted_brush_size,
        append_interpolated_points, append_preview_point, canvas_context_menu_kind,
        canvas_overlay_text, clamp_quick_depth, clip_pos2_polygon, clip_pos2_polyline,
        clipboard_command_from_events, color_from_srgb, draft_is_committable,
        eraser_lasso_operation, fast_stroke_fallback_shapes, format_overlay_coordinate,
        incremental_paint_operations, interpolation_step_count, layer_operation_counts,
        operation_click_score, operation_contained_by_rectangle, operation_intersects_selection,
        paint_order_wheel_steps, parse_lateral_coordinate, point_append_decision,
        prepare_draft_for_commit, primary_pointer_positions, redo_shortcuts,
        repaint_interval_for_work, selectable_layer_ids, selection_scale_factor,
        selection_scale_handle_at, selection_wheel_steps, set_straight_draft_endpoint,
        should_paint_fill_fallback, should_paint_live_draft, simplify_pos2_render_points,
        smooth_pos2_draft, smooth_pos2_saved_fallback, space_pan_requested,
        straight_line_requested, surrender_canvas_keyboard_focus, tile_display_rects,
        tile_fallback_mode, tile_generation_is_paused, tile_keys_for_view,
        tile_rebuild_deferred_since, tool_shortcut_allowed, wrapped_cycle_index,
        z_drag_zoom_factor, z_zoom_requested, zoom_tile_requests_deferred_since,
    };
    use crate::coords::{CameraAddress, CanvasPoint};
    use crate::model::{Color, DEFAULT_LAYER_ID, EditKind, EditOperation, Layer, ToolKind};
    use crate::selection_geometry::{Point2, Rect2, SelectionShape};
    use crate::settings::{AppSettings, OverlayProfile};
    use crate::tile_cache::{TILE_BLEED, TILE_SIZE, tile_lod_for_resolution, tile_resolution};
    use eframe::egui::{
        Color32, Event, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect, Shape, TouchPhase,
    };
    use num_bigint::BigInt;
    use std::collections::HashSet;
    use std::time::{Duration, Instant};
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
    fn lasso_preview_skips_points_below_minimum_spacing() {
        let mut points = vec![Pos2::new(0.0, 0.0)];

        append_preview_point(&mut points, Pos2::new(1.0, 0.0), 2.0);
        append_preview_point(&mut points, Pos2::new(2.0, 0.0), 2.0);

        assert_eq!(points, vec![Pos2::new(0.0, 0.0), Pos2::new(2.0, 0.0)]);
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
    fn selection_layer_filter_includes_only_the_visible_unlocked_active_layer() {
        let mut default_layer = Layer::default_layer();
        let other_layer = Layer {
            id: Uuid::new_v4(),
            name: "Other".to_owned(),
            sort_order: 1,
            visible: true,
            locked: false,
        };
        let layers = [default_layer.clone(), other_layer.clone()];

        assert_eq!(
            selectable_layer_ids(&layers, DEFAULT_LAYER_ID),
            HashSet::from([DEFAULT_LAYER_ID])
        );
        assert_eq!(
            selectable_layer_ids(&layers, other_layer.id),
            HashSet::from([other_layer.id])
        );

        default_layer.locked = true;
        assert!(selectable_layer_ids(&[default_layer], DEFAULT_LAYER_ID).is_empty());
        let mut hidden = other_layer;
        hidden.visible = false;
        assert!(selectable_layer_ids(&[hidden.clone()], hidden.id).is_empty());
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
