use crate::coords::{CameraAddress, CanvasPoint};
use crate::document::CanvasDocument;
use crate::model::{Bookmark, Color, EditKind, EditOperation, ToolKind};
use crate::raster::operation_width;
use crate::tile_cache::{MAX_TILE_LOD, TILE_BLEED, TILE_SIZE, TileCache, TileKey, lod_scale};
use crate::tile_scheduler::{TileJob, TileScheduler};
use anyhow::{Context, Result};
use eframe::egui::{self, Color32, Painter, PointerButton, Pos2, Rect, Sense, Stroke};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

const BACKGROUND: Color = Color::WHITE;
const DRAFT_SAVE_INTERVAL: Duration = Duration::from_millis(100);
const BRUSH_POINT_SPACING_PX: f32 = 0.75;
const FILL_POINT_SPACING_PX: f32 = 1.0;
const MAX_DRAW_SEGMENT_PX: f32 = 96.0;
const ZOOM_DRAG_SENSITIVITY: f64 = 0.01;
const MIN_ZOOM_DRAG_FACTOR: f64 = 0.5;
const MAX_ZOOM_DRAG_FACTOR: f64 = 2.0;
const MAX_FILL_FALLBACK_DEPTH_DELTA: i64 = 6;
const MAX_FILL_FALLBACK_POINTS: usize = 4096;

fn tile_lod_for_zoom(zoom: f64) -> Option<u8> {
    if !zoom.is_finite() || zoom <= 0.0 {
        return None;
    }
    let lod = zoom.log2().floor().max(0.0) as u8;
    Some(lod.min(MAX_TILE_LOD))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PointAppendDecision {
    Append,
    Skip,
    Interpolate,
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
    tile_textures: HashMap<(TileKey, u64), egui::TextureHandle>,
    pending_tiles: HashSet<(TileKey, u64)>,
    visible_tile_generation: u64,
    visible_tile_set: HashSet<TileKey>,
    bookmarks: Vec<Bookmark>,
    bookmark_edits: HashMap<Uuid, String>,
    show_bookmarks: bool,
}

impl EndlessSketchApp {
    pub fn new(_context: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Result<Self> {
        let path = path.unwrap_or_else(default_canvas_path);
        let document = CanvasDocument::open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        let tile_scheduler = TileScheduler::new(TileCache::new(document.root())?);
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
            status_message: "Ready".to_owned(),
            tile_scheduler,
            tile_textures: HashMap::new(),
            pending_tiles: HashSet::new(),
            visible_tile_generation: 0,
            visible_tile_set: HashSet::new(),
            bookmarks,
            bookmark_edits,
            show_bookmarks: false,
        })
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tool, ToolKind::Brush, "Brush (B)");
            ui.selectable_value(&mut self.tool, ToolKind::Eraser, "Eraser (E)");
            ui.selectable_value(&mut self.tool, ToolKind::LassoFill, "Fill (L)");
            ui.selectable_value(&mut self.tool, ToolKind::Eyedropper, "Picker (I)");
            ui.separator();
            ui.add(egui::Slider::new(&mut self.brush_size, 1.0..=100.0).text("Size"));
            let mut color = Color32::from_rgba_unmultiplied(
                self.color.r,
                self.color.g,
                self.color.b,
                self.color.a,
            );
            if ui.color_edit_button_srgba(&mut color).changed() {
                self.color = Color::rgba(color.r(), color.g(), color.b(), color.a());
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
        });
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

    fn canvas(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        painter.rect_filled(response.rect, 0.0, to_color32(BACKGROUND));

        self.handle_shortcuts(context);
        self.handle_navigation(ui, &response);
        self.handle_drawing(ui, &response);
        let tiles_ready = self.paint_cached_tiles(context, &painter, response.rect);
        if !tiles_ready {
            self.paint_operations(&painter, response.rect);
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

    fn handle_drawing(&mut self, ui: &egui::Ui, response: &egui::Response) {
        if is_space_pan_active(ui) || is_z_zoom_active(ui) {
            return;
        }

        let pointer = ui.input(|input| input.pointer.clone());
        let position = pointer.interact_pos();
        let primary_pressed = pointer.button_pressed(PointerButton::Primary);
        let primary_down = pointer.button_down(PointerButton::Primary);
        let primary_released = pointer.button_released(PointerButton::Primary);

        if primary_pressed && response.hovered() {
            if self.tool == ToolKind::Eyedropper {
                self.pick_color_at(position, response.rect);
                return;
            }
            if let Some(position) = position {
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
                self.last_draft_save = Instant::now();
            }
        }

        if primary_down
            && response.hovered()
            && let (Some(position), Some(draft)) = (position, self.draft.as_ref())
        {
            let point = screen_to_canvas_for_camera(&self.camera, position, response.rect);
            let append_decision = draft
                .points
                .last()
                .map_or(PointAppendDecision::Append, |last| {
                    self.camera
                        .canvas_to_screen(
                            last,
                            response.rect.width() as f64,
                            response.rect.height() as f64,
                        )
                        .map_or(PointAppendDecision::Append, |(x, y)| {
                            let previous = Pos2::new(
                                response.rect.left() + x as f32,
                                response.rect.top() + y as f32,
                            );
                            point_append_decision(draft.kind, previous.distance(position))
                        })
                });
            match append_decision {
                PointAppendDecision::Append => {
                    if let Some(draft) = self.draft.as_mut() {
                        draft.points.push(point);
                    }
                }
                PointAppendDecision::Skip => {}
                PointAppendDecision::Interpolate => {
                    if let Some(draft) = self.draft.as_mut() {
                        append_interpolated_points(draft, &self.camera, position, response.rect);
                    }
                }
            }
            if self.last_draft_save.elapsed() >= DRAFT_SAVE_INTERVAL {
                if let Some(draft) = &self.draft
                    && let Err(error) = self.document.save_draft(draft)
                {
                    self.status_message = format!("Autosave failed: {error:#}");
                }
                self.last_draft_save = Instant::now();
            }
        }

        if primary_released {
            self.finish_draft();
        }
        self.last_pointer_position = position;
    }

    fn finish_draft(&mut self) {
        let Some(mut draft) = self.draft.take() else {
            return;
        };
        if draft.kind == EditKind::Fill && draft.points.len() >= 3 {
            draft.points.push(draft.points[0].clone());
        }
        if draft.points.len() < 2 {
            let _ = self.document.discard_draft(&draft);
            return;
        }
        match self.document.commit(draft) {
            Ok(()) => self.status_message = "Saved".to_owned(),
            Err(error) => self.status_message = format!("Save failed: {error:#}"),
        }
    }

    fn paint_operations(&self, painter: &Painter, rect: Rect) {
        for operation in self.document.operations() {
            self.paint_operation(painter, rect, operation, false);
        }
    }

    fn paint_draft(&self, painter: &Painter, rect: Rect) {
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
        if points.len() < 2 {
            return;
        }

        let width = operation_width(operation, self.camera.depth, self.camera.zoom);
        let color = to_color32(operation.visible_color(BACKGROUND));
        if operation.kind == EditKind::Fill {
            if is_draft {
                painter.add(egui::Shape::line(points, Stroke::new(1.5, color)));
            } else if should_paint_fill_fallback(operation, self.camera.depth, points.len()) {
                paint_fill_scanlines(painter, rect, &points, color);
            }
        } else {
            for &point in &points {
                painter.circle_filled(point, width * 0.5, color);
            }
            for segment in points.windows(2) {
                painter.line_segment([segment[0], segment[1]], Stroke::new(width, color));
            }
        }
    }

    fn paint_overlay(&self, painter: &Painter, rect: Rect) {
        let text = format!(
            "depth {}   zoom {:.3}×   {} ops   {}",
            self.camera.depth,
            self.camera.zoom,
            self.document.operations().len(),
            self.status_message
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
    ) -> bool {
        self.collect_tile_results(context);
        let revision = self.document.revision();
        self.tile_textures
            .retain(|(_, cached_revision), _| *cached_revision == revision);
        self.pending_tiles
            .retain(|(_, cached_revision)| *cached_revision == revision);

        let Some(lod) = tile_lod_for_zoom(self.camera.zoom) else {
            return false;
        };
        let visible = self.visible_tiles(rect, lod);
        let visible_set: HashSet<_> = visible.iter().cloned().collect();
        self.tile_textures.retain(|(key, cached_revision), _| {
            *cached_revision == revision && visible_set.contains(key)
        });
        if visible_set != self.visible_tile_set {
            self.visible_tile_generation = self.visible_tile_generation.saturating_add(1);
            self.visible_tile_set = visible_set;
            self.tile_scheduler
                .set_generation(self.visible_tile_generation);
            self.pending_tiles.retain(|(key, cached_revision)| {
                *cached_revision == revision && self.visible_tile_set.contains(key)
            });
        }

        let missing: Vec<TileKey> = visible
            .iter()
            .filter(|key| {
                !self.tile_textures.contains_key(&((*key).clone(), revision))
                    && !self.pending_tiles.contains(&((*key).clone(), revision))
            })
            .cloned()
            .collect();
        if !missing.is_empty() {
            for key in missing {
                let operations = Arc::new(self.document.operations_for_tile(&key));
                if self
                    .tile_scheduler
                    .request(TileJob {
                        key: key.clone(),
                        revision,
                        generation: self.visible_tile_generation,
                        operations: Arc::clone(&operations),
                        background: BACKGROUND,
                    })
                    .is_ok()
                {
                    self.pending_tiles.insert((key, revision));
                }
            }
        }

        let mut all_tiles_ready = true;
        for key in visible {
            let Some(texture) = self.tile_textures.get(&(key.clone(), revision)) else {
                all_tiles_ready = false;
                continue;
            };
            let top_left = CanvasPoint::new(key.depth, key.x, key.y, 0.0, 0.0);
            let Some(position) = self.point_to_position(&top_left, rect) else {
                continue;
            };
            let source_scale = lod_scale(key.lod) as f32;
            let world_bleed = TILE_BLEED as f32 / source_scale;
            let bleed = world_bleed * self.camera.zoom as f32;
            let size = (TILE_SIZE as f32 + world_bleed * 2.0) * self.camera.zoom as f32;
            let tile_rect =
                Rect::from_min_size(position - egui::vec2(bleed, bleed), egui::vec2(size, size));
            painter.image(
                texture.id(),
                tile_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        all_tiles_ready
    }

    fn texture_name(&self, key: &TileKey, revision: u64) -> String {
        format!(
            "tile-d{}-l{}-x{}-y{}-r{}",
            key.depth, key.lod, key.x, key.y, revision
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
                    self.tile_textures
                        .insert((result.key, result.revision), texture);
                }
                Err(error) => self.status_message = format!("Tile rebuild failed: {error}"),
            }
        }
    }

    fn visible_tiles(&self, rect: Rect, lod: u8) -> Vec<TileKey> {
        let half_width =
            rect.width() as f64 / (2.0 * crate::coords::TILE_PIXELS * self.camera.zoom);
        let half_height =
            rect.height() as f64 / (2.0 * crate::coords::TILE_PIXELS * self.camera.zoom);
        let min_x = (self.camera.local_x - half_width).floor() as i64 - 1;
        let max_x = (self.camera.local_x + half_width).ceil() as i64 + 1;
        let min_y = (self.camera.local_y - half_height).floor() as i64 - 1;
        let max_y = (self.camera.local_y + half_height).ceil() as i64 + 1;
        let mut keys = Vec::new();
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                keys.push(TileKey {
                    depth: self.camera.depth,
                    x: &self.camera.tile_x + x,
                    y: &self.camera.tile_y + y,
                    lod,
                });
            }
        }
        keys
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
            let color = operation.visible_color(BACKGROUND);
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
            Ok(true) => self.status_message = "Undone".to_owned(),
            Ok(false) => self.status_message = "Nothing to undo".to_owned(),
            Err(error) => self.status_message = format!("Undo failed: {error:#}"),
        }
    }

    fn run_redo(&mut self) {
        match self.document.redo() {
            Ok(true) => self.status_message = "Redone".to_owned(),
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
                match TileCache::new(self.document.root()) {
                    Ok(cache) => self.tile_scheduler = TileScheduler::new(cache),
                    Err(error) => {
                        self.status_message = format!("Tile cache failed: {error:#}");
                        return;
                    }
                }
                self.tile_textures.clear();
                self.pending_tiles.clear();
                self.visible_tile_generation = self.visible_tile_generation.saturating_add(1);
                self.visible_tile_set.clear();
                self.tile_scheduler
                    .set_generation(self.visible_tile_generation);
                self.bookmarks = self.document.bookmarks().unwrap_or_default();
                self.bookmark_edits = bookmark_edit_names(&self.bookmarks);
                self.camera = CameraAddress::default();
                self.status_message = "Canvas opened".to_owned();
            }
            Err(error) => self.status_message = format!("Open failed: {error:#}"),
        }
    }
}

fn should_paint_fill_fallback(
    operation: &EditOperation,
    camera_depth: i64,
    screen_point_count: usize,
) -> bool {
    screen_point_count <= MAX_FILL_FALLBACK_POINTS
        && camera_depth.saturating_sub(operation.native_depth).abs()
            <= MAX_FILL_FALLBACK_DEPTH_DELTA
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

impl eframe::App for EndlessSketchApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = ui.ctx().clone();
        egui::Panel::top("toolbar").show_inside(ui, |ui| self.toolbar(ui));
        self.bookmark_window(&context);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| self.canvas(ui, &context));
        context.request_repaint_after(Duration::from_millis(16));
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

fn append_interpolated_points(
    draft: &mut EditOperation,
    camera: &CameraAddress,
    position: Pos2,
    rect: Rect,
) {
    let Some(last) = draft.points.last() else {
        draft
            .points
            .push(screen_to_canvas_for_camera(camera, position, rect));
        return;
    };
    let Some((last_x, last_y)) =
        camera.canvas_to_screen(last, rect.width() as f64, rect.height() as f64)
    else {
        draft
            .points
            .push(screen_to_canvas_for_camera(camera, position, rect));
        return;
    };
    let previous = Pos2::new(rect.left() + last_x as f32, rect.top() + last_y as f32);
    let distance = previous.distance(position);
    if !distance.is_finite() || distance <= MAX_DRAW_SEGMENT_PX {
        draft
            .points
            .push(screen_to_canvas_for_camera(camera, position, rect));
        return;
    }

    let step_count = (distance / (MAX_DRAW_SEGMENT_PX * 0.5)).ceil().max(1.0) as usize;
    for step in 1..=step_count {
        let t = step as f32 / step_count as f32;
        let interpolated = previous.lerp(position, t);
        draft
            .points
            .push(screen_to_canvas_for_camera(camera, interpolated, rect));
    }
}

fn point_append_decision(kind: EditKind, distance: f32) -> PointAppendDecision {
    if distance < point_spacing_px(kind) {
        return PointAppendDecision::Skip;
    }
    if distance > MAX_DRAW_SEGMENT_PX {
        if matches!(kind, EditKind::Paint | EditKind::Erase) {
            PointAppendDecision::Interpolate
        } else {
            PointAppendDecision::Skip
        }
    } else {
        PointAppendDecision::Append
    }
}

fn point_spacing_px(kind: EditKind) -> f32 {
    if kind == EditKind::Fill {
        FILL_POINT_SPACING_PX
    } else {
        BRUSH_POINT_SPACING_PX
    }
}

fn to_color32(color: Color) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r, color.g, color.b, color.a)
}

fn default_canvas_path() -> PathBuf {
    directories::ProjectDirs::from("app", "EndlessSketch", "EndlessSketch")
        .map(|directories| directories.data_local_dir().join("default.esketch"))
        .unwrap_or_else(|| Path::new("default.esketch").to_path_buf())
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
        PointAppendDecision, point_append_decision, should_paint_fill_fallback,
        space_pan_requested, tile_lod_for_zoom, z_drag_zoom_factor, z_zoom_requested,
    };
    use crate::coords::CanvasPoint;
    use crate::model::EditKind;
    use crate::model::{Color, EditOperation};
    use num_bigint::BigInt;

    #[test]
    fn cached_tiles_are_softly_upscaled_between_lod_steps() {
        assert_eq!(tile_lod_for_zoom(1.0), Some(0));
        assert_eq!(tile_lod_for_zoom(1.01), Some(0));
        assert_eq!(tile_lod_for_zoom(2.0), Some(1));
        assert_eq!(tile_lod_for_zoom(2.01), Some(1));
        assert_eq!(tile_lod_for_zoom(4.0), Some(2));
        assert_eq!(tile_lod_for_zoom(4.01), Some(2));
        assert_eq!(tile_lod_for_zoom(7.99), Some(2));
    }

    #[test]
    fn drawing_rejects_tiny_fill_jitter_and_large_pointer_jumps() {
        assert_eq!(
            point_append_decision(EditKind::Paint, 1.0),
            PointAppendDecision::Append
        );
        assert_eq!(
            point_append_decision(EditKind::Paint, 128.0),
            PointAppendDecision::Interpolate
        );
        assert_eq!(
            point_append_decision(EditKind::Fill, 1.0),
            PointAppendDecision::Append
        );
        assert_eq!(
            point_append_decision(EditKind::Fill, 0.5),
            PointAppendDecision::Skip
        );
        assert_eq!(
            point_append_decision(EditKind::Fill, 128.0),
            PointAppendDecision::Skip
        );
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

        assert!(should_paint_fill_fallback(&fill, 6, fill.points.len()));
        assert!(!should_paint_fill_fallback(&fill, 7, fill.points.len()));
    }
}
