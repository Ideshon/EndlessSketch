use crate::coords::{CameraAddress, CanvasPoint, DEPTH_RATIO, ScreenAffine, TILE_PIXELS};
use crate::model::{
    Bookmark, Color, EditKind, EditOperation, Layer, PaintOrderUpdate, remap_fill_group_ids,
    set_operation_layer_recursive,
};
use crate::settings::StorageCommitMode;
use crate::spatial::{OperationIndex, operation_bounds};
use crate::storage::{
    CanvasStore, HistoryChange, OperationCommitStats, sort_operations_by_paint_order,
};
use crate::tile_cache::TileKey;
use anyhow::{Context, Result, bail};
use num_bigint::BigInt;
use num_traits::{Euclid, FromPrimitive, ToPrimitive};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct DocumentOperationCommitStats {
    pub target_count: usize,
    pub replacement_count: usize,
    pub target_collect_ms: f64,
    pub sort_targets_ms: f64,
    pub replacement_build_ms: f64,
    pub command_build_ms: f64,
    pub storage_commit_ms: f64,
    pub memory_prepare_ms: f64,
    pub memory_replace_ms: f64,
    pub memory_replace_fallback: bool,
    pub memory_replace_fallback_reason: Option<&'static str>,
    pub memory_replace: MemoryReplaceStats,
    pub total_ms: f64,
    pub storage: OperationCommitStats,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct MemoryReplaceStats {
    pub replacement_count: usize,
    pub compact_replacement_count: usize,
    pub metadata_replacement_count: usize,
    pub missing_operation_count: usize,
    pub missing_render_count: usize,
    pub position_map_ms: f64,
    pub position_lookup_ms: f64,
    pub bounds_ms: f64,
    pub remove_operation_index_ms: f64,
    pub remove_render_index_ms: f64,
    pub assign_insert_ms: f64,
    pub total_ms: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisibleDepthMode {
    Empty,
    Near,
    Distant,
}

enum RenderReplacementPlan {
    Single(usize),
    CompactSources(Vec<usize>),
}

pub struct CanvasDocument {
    store: CanvasStore,
    operations: Vec<EditOperation>,
    render_operations: Vec<Arc<EditOperation>>,
    layers: Vec<Layer>,
    revision: u64,
    max_sequence: i64,
    index: OperationIndex,
    render_index: OperationIndex,
    last_operation_commit_stats: Option<DocumentOperationCommitStats>,
}

impl CanvasDocument {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let store = CanvasStore::open(path)?;
        let operations = store.load_active_operations()?;
        let layers = store.load_layers()?;
        let index = OperationIndex::build(&operations);
        let render_operations = build_render_operations(&operations);
        let render_index = OperationIndex::build(&render_operations);
        let render_operations = render_operations.into_iter().map(Arc::new).collect();
        let revision = store.content_revision()?;
        let max_sequence = store.active_max_sequence()?;
        Ok(Self {
            store,
            operations,
            render_operations,
            layers,
            revision,
            max_sequence,
            index,
            render_index,
            last_operation_commit_stats: None,
        })
    }

    pub fn operations(&self) -> &[EditOperation] {
        &self.operations
    }

    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    pub fn is_top_layer(&self, id: Uuid) -> bool {
        self.layers.last().is_some_and(|layer| layer.id == id)
    }

    pub fn layer_sort_order(&self, id: Uuid) -> i64 {
        self.layers
            .iter()
            .find(|layer| layer.id == id)
            .map_or(i64::MIN, |layer| layer.sort_order)
    }

    pub fn layer_is_editable(&self, id: Uuid) -> bool {
        self.layers
            .iter()
            .find(|layer| layer.id == id)
            .is_some_and(|layer| layer.visible && !layer.locked)
    }

    pub fn expanded_object_group_ids(
        &self,
        target_ids: &HashSet<Uuid>,
        layer_id: Uuid,
    ) -> HashSet<Uuid> {
        let group_ids = self
            .operations
            .iter()
            .filter(|operation| {
                operation.layer_id == layer_id && target_ids.contains(&operation.id)
            })
            .map(EditOperation::object_group_id)
            .collect::<HashSet<_>>();
        let mut expanded = target_ids.clone();
        expanded.extend(
            self.operations
                .iter()
                .filter(|operation| {
                    operation.layer_id == layer_id
                        && group_ids.contains(&operation.object_group_id())
                })
                .map(|operation| operation.id),
        );
        expanded
    }

    pub fn root(&self) -> &Path {
        self.store.root()
    }

    pub fn set_storage_commit_mode(&self, mode: StorageCommitMode) -> Result<()> {
        self.store.set_commit_mode(mode)
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn max_sequence(&self) -> i64 {
        self.max_sequence
    }

    pub fn last_operation_commit_stats(&self) -> Option<DocumentOperationCommitStats> {
        self.last_operation_commit_stats
    }

    pub fn operations_for_tile(&self, key: &TileKey) -> Vec<EditOperation> {
        self.render_operations_for_tiles(std::slice::from_ref(key))
    }

    pub fn render_operations_for_tiles(&self, keys: &[TileKey]) -> Vec<EditOperation> {
        self.render_operation_indices_for_tiles(keys)
            .into_iter()
            .filter_map(|index| self.render_operations.get(index))
            .map(|operation| operation.as_ref().clone())
            .collect()
    }

    pub fn render_operation_indices_for_tiles(&self, keys: &[TileKey]) -> Vec<usize> {
        let mut indices = self.render_index.query_many(keys);
        let visible_layers = self.visible_layer_ids();
        indices.retain(|index| {
            self.render_operations
                .get(*index)
                .is_some_and(|operation| visible_layers.contains(&operation.layer_id))
        });
        let layer_orders = self.layer_orders();
        indices.sort_by_key(|index| {
            self.render_operations
                .get(*index)
                .map_or((i64::MIN, i64::MIN, i64::MIN), |operation| {
                    (
                        layer_orders
                            .get(&operation.layer_id)
                            .copied()
                            .unwrap_or(i64::MIN),
                        operation.effective_paint_order(),
                        operation.sequence,
                    )
                })
        });
        indices
    }

    pub fn render_operations_by_indices(&self, indices: &[usize]) -> Vec<Arc<EditOperation>> {
        indices
            .iter()
            .filter_map(|index| self.render_operations.get(*index).cloned())
            .collect()
    }

    pub fn editable_operation_ids_for_tiles(
        &self,
        keys: &[TileKey],
        active_layer_id: Uuid,
        camera_depth: i64,
        depth_radius: i64,
    ) -> Vec<Uuid> {
        if !self.layer_is_editable(active_layer_id) {
            return Vec::new();
        }
        let depth_radius = depth_radius.max(0);
        let min_depth = camera_depth.saturating_sub(depth_radius);
        let max_depth = camera_depth.saturating_add(depth_radius);
        self.index
            .query_many(keys)
            .into_iter()
            .filter_map(|index| self.operations.get(index))
            .filter(|operation| operation.layer_id == active_layer_id)
            .filter(|operation| {
                operation_depth_span(operation)
                    .is_some_and(|(min, max)| min >= min_depth && max <= max_depth)
            })
            .map(|operation| operation.id)
            .collect()
    }

    pub fn visible_depth_mode_for_tiles(
        &self,
        keys: &[TileKey],
        active_layer_id: Uuid,
        camera_depth: i64,
        depth_radius: i64,
    ) -> VisibleDepthMode {
        let depth_radius = depth_radius.max(0);
        let min_depth = camera_depth.saturating_sub(depth_radius);
        let max_depth = camera_depth.saturating_add(depth_radius);
        if self.layer_is_editable(active_layer_id)
            && self
                .index
                .query_many(keys)
                .into_iter()
                .filter_map(|index| self.operations.get(index))
                .any(|operation| {
                    operation.layer_id == active_layer_id
                        && operation_depth_span(operation)
                            .is_some_and(|(min, max)| min >= min_depth && max <= max_depth)
                })
        {
            return VisibleDepthMode::Near;
        }

        let visible_layers = self.visible_layer_ids();
        let mut has_visible_content = false;
        for index in self.render_index.query_many(keys) {
            let Some(operation) = self.render_operations.get(index) else {
                continue;
            };
            if !visible_layers.contains(&operation.layer_id) {
                continue;
            }
            has_visible_content = true;
        }
        if has_visible_content {
            VisibleDepthMode::Distant
        } else {
            VisibleDepthMode::Empty
        }
    }

    pub fn save_draft(&self, operation: &EditOperation) -> Result<()> {
        if operation.kind == EditKind::Erase {
            return Ok(());
        }
        self.store.save_draft(operation)
    }

    pub fn create_layer(&mut self, name: &str) -> Result<Layer> {
        let layer = self.store.create_layer(name)?;
        self.layers = self.store.load_layers()?;
        self.revision = self.store.content_revision()?;
        Ok(layer)
    }

    pub fn rename_layer(&mut self, id: Uuid, name: &str) -> Result<bool> {
        if !self.store.rename_layer(id, name)? {
            return Ok(false);
        }
        self.layers = self.store.load_layers()?;
        self.revision = self.store.content_revision()?;
        Ok(true)
    }

    pub fn move_layer(&mut self, id: Uuid, direction: i32) -> Result<bool> {
        let Some(revision) = self.store.move_layer(id, direction)? else {
            return Ok(false);
        };
        self.layers = self.store.load_layers()?;
        self.revision = revision;
        Ok(true)
    }

    pub fn set_layer_visibility(&mut self, id: Uuid, visible: bool) -> Result<bool> {
        let Some(revision) = self.store.set_layer_visibility(id, visible)? else {
            return Ok(false);
        };
        self.layers = self.store.load_layers()?;
        self.revision = revision;
        Ok(true)
    }

    pub fn set_layer_locked(&mut self, id: Uuid, locked: bool) -> Result<bool> {
        let Some(revision) = self.store.set_layer_locked(id, locked)? else {
            return Ok(false);
        };
        self.layers = self.store.load_layers()?;
        self.revision = revision;
        Ok(true)
    }

    pub fn duplicate_layer(&mut self, id: Uuid) -> Result<Option<(Layer, usize)>> {
        let source_operations = self
            .operations
            .iter()
            .filter(|operation| operation.layer_id == id)
            .cloned()
            .collect::<Vec<_>>();
        let Some((layer, operations, revision)) =
            self.store.duplicate_layer(id, &source_operations)?
        else {
            return Ok(None);
        };
        let operation_count = operations.len();
        self.layers = self.store.load_layers()?;
        self.operations.extend(operations);
        sort_operations_by_paint_order(&mut self.operations);
        self.rebuild_indexes();
        self.max_sequence = self.store.active_max_sequence()?;
        self.revision = revision;
        Ok(Some((layer, operation_count)))
    }

    pub fn delete_layer(&mut self, id: Uuid) -> Result<bool> {
        let Some(revision) = self.store.delete_layer(id)? else {
            return Ok(false);
        };
        self.layers = self.store.load_layers()?;
        self.reload_active_operations()?;
        self.revision = revision;
        Ok(true)
    }

    pub fn merge_layer_down(&mut self, id: Uuid) -> Result<Option<(Layer, usize)>> {
        let source_operations = self
            .operations
            .iter()
            .filter(|operation| operation.layer_id == id)
            .cloned()
            .collect::<Vec<_>>();
        let Some((destination, replacements, revision)) =
            self.store.merge_layer_down(id, &source_operations)?
        else {
            return Ok(None);
        };
        let merged_count = replacements.len();
        self.layers = self.store.load_layers()?;
        self.reload_active_operations()?;
        self.revision = revision;
        Ok(Some((destination, merged_count)))
    }

    pub fn discard_draft(&self, operation: &EditOperation) -> Result<()> {
        self.store.discard_draft(operation)
    }

    pub fn commit(&mut self, mut operation: EditOperation) -> Result<()> {
        self.revision = self.store.commit(&mut operation)?;
        self.max_sequence = operation.sequence;
        self.append_committed_operation(operation);
        Ok(())
    }

    pub fn commit_group(&mut self, mut operations: Vec<EditOperation>) -> Result<()> {
        if operations.is_empty() {
            return Ok(());
        }
        self.revision = self.store.commit_group(&mut operations)?;
        self.max_sequence = operations
            .last()
            .map_or(self.max_sequence, |operation| operation.sequence);
        self.operations.extend(operations);
        sort_operations_by_paint_order(&mut self.operations);
        self.rebuild_indexes();
        Ok(())
    }

    pub fn delete_operations(
        &mut self,
        target_ids: &HashSet<Uuid>,
        layer_id: Uuid,
    ) -> Result<usize> {
        let target_ids = self.expanded_object_group_ids(target_ids, layer_id);
        let targets: Vec<_> = self
            .operations
            .iter()
            .filter(|operation| target_ids.contains(&operation.id))
            .map(|operation| operation.id)
            .collect();
        if targets.is_empty() {
            return Ok(0);
        }

        let mut tombstone = EditOperation::tombstone(targets.clone(), layer_id);
        self.revision = self.store.commit(&mut tombstone)?;
        self.max_sequence = tombstone.sequence;
        let target_ids: HashSet<_> = targets.into_iter().collect();
        self.operations
            .retain(|operation| !target_ids.contains(&operation.id));
        self.rebuild_indexes();
        Ok(target_ids.len())
    }

    pub fn commit_paint_subtraction(
        &mut self,
        changes: Vec<(Uuid, Vec<Vec<CanvasPoint>>)>,
        layer_id: Uuid,
    ) -> Result<Option<Vec<Uuid>>> {
        self.commit_vector_subtraction(changes, layer_id, EditKind::Paint)
    }

    pub fn commit_fill_subtraction(
        &mut self,
        changes: Vec<(Uuid, Vec<Vec<CanvasPoint>>)>,
        layer_id: Uuid,
    ) -> Result<Option<Vec<Uuid>>> {
        self.commit_vector_subtraction(changes, layer_id, EditKind::Fill)
    }

    pub fn commit_eraser_subtraction(
        &mut self,
        changes: Vec<(Uuid, Vec<EditOperation>)>,
        layer_id: Uuid,
    ) -> Result<Option<Vec<Uuid>>> {
        if changes.is_empty() {
            return Ok(None);
        }
        let mut seen = HashSet::with_capacity(changes.len());
        let mut targets = Vec::with_capacity(changes.len());
        for (source_id, replacements) in changes {
            if !seen.insert(source_id) {
                bail!("eraser subtraction contains a duplicate target");
            }
            let source = self
                .operations
                .iter()
                .find(|operation| operation.id == source_id)
                .context("eraser subtraction target is unavailable")?;
            if source.layer_id != layer_id
                || !matches!(
                    source.kind,
                    EditKind::Paint | EditKind::Fill | EditKind::CompactBlock
                )
                || replacements.iter().any(|replacement| {
                    replacement.layer_id != layer_id
                        || replacement.kind != source.kind
                        || replacement.effective_paint_order() != source.effective_paint_order()
                        || (!source.is_compact_block()
                            && (replacement.color != source.color
                                || replacement.native_depth != source.native_depth
                                || replacement.native_zoom != source.native_zoom))
                        || !subtraction_replacement_geometry_is_valid(replacement, Some(source))
                })
                || (source.is_compact_block() && replacements.len() > 1)
            {
                bail!("eraser subtraction contains an invalid replacement");
            }
            targets.push((source.clone(), replacements));
        }
        self.commit_vector_replacements(targets, layer_id)
    }

    fn commit_vector_subtraction(
        &mut self,
        changes: Vec<(Uuid, Vec<Vec<CanvasPoint>>)>,
        layer_id: Uuid,
        source_kind: EditKind,
    ) -> Result<Option<Vec<Uuid>>> {
        if changes.is_empty() {
            return Ok(None);
        }
        if !self.layer_is_editable(layer_id) {
            bail!("active layer is hidden or locked");
        }

        let mut seen = HashSet::with_capacity(changes.len());
        let mut targets = Vec::with_capacity(changes.len());
        for (source_id, runs) in changes {
            if !seen.insert(source_id) {
                bail!("vector subtraction contains a duplicate target");
            }
            let source = self
                .operations
                .iter()
                .find(|operation| operation.id == source_id)
                .context("vector subtraction target is unavailable")?;
            if source.layer_id != layer_id || source.kind != source_kind {
                bail!("vector subtraction target has the wrong kind or layer");
            }
            if runs.iter().any(|run| {
                run.is_empty()
                    || run
                        .iter()
                        .any(|point| !point.local_x.is_finite() || !point.local_y.is_finite())
                    || (source_kind == EditKind::Fill
                        && (run.len() < 4 || run.first() != run.last()))
            }) {
                bail!("vector subtraction produced invalid geometry");
            }
            if runs.len() == 1 && runs[0] == source.points {
                continue;
            }
            targets.push((source.clone(), runs));
        }
        let targets = targets
            .into_iter()
            .map(|(source, runs)| {
                let replacements = runs
                    .into_iter()
                    .map(|points| {
                        let mut replacement = source.clone();
                        replacement.points = points;
                        if replacement.kind == EditKind::Fill {
                            replacement.smooth_area = false;
                        }
                        replacement
                    })
                    .collect();
                (source, replacements)
            })
            .collect();
        self.commit_vector_replacements(targets, layer_id)
    }

    fn commit_vector_replacements(
        &mut self,
        mut targets: Vec<(EditOperation, Vec<EditOperation>)>,
        layer_id: Uuid,
    ) -> Result<Option<Vec<Uuid>>> {
        if targets.is_empty() {
            return Ok(None);
        }
        if !self.layer_is_editable(layer_id) {
            bail!("active layer is hidden or locked");
        }
        targets.sort_by_key(|(source, _)| (source.effective_paint_order(), source.sequence));

        let transaction_id = Uuid::new_v4();
        let mut tombstone = EditOperation::tombstone(
            targets.iter().map(|(source, _)| source.id).collect(),
            layer_id,
        );
        tombstone.transaction_id = transaction_id;
        let mut commands = vec![tombstone];
        let mut fill_group_ids = HashMap::new();
        for (source, replacements) in &targets {
            let fill_group_id = (source.kind == EditKind::Fill).then(|| {
                *fill_group_ids
                    .entry(source.object_group_id())
                    .or_insert_with(Uuid::new_v4)
            });
            for replacement in replacements {
                let mut replacement = replacement.clone();
                replacement.id = Uuid::new_v4();
                replacement.sequence = 0;
                replacement.paint_order = Some(source.effective_paint_order());
                replacement.transaction_id = transaction_id;
                replacement.affects_before_sequence = None;
                replacement.fill_group_id = fill_group_id;
                if replacement.is_compact_block() {
                    refresh_compact_source_geometry_ids(&mut replacement);
                }
                commands.push(replacement);
            }
        }

        self.revision = self.store.commit_group(&mut commands)?;
        self.max_sequence = commands
            .last()
            .map_or(self.max_sequence, |operation| operation.sequence);
        let removed_ids: HashSet<_> = targets.iter().map(|(source, _)| source.id).collect();
        self.operations
            .retain(|operation| !removed_ids.contains(&operation.id));
        let replacement_ids = commands
            .iter()
            .skip(1)
            .map(|operation| operation.id)
            .collect::<Vec<_>>();
        self.operations.extend(commands.into_iter().skip(1));
        sort_operations_by_paint_order(&mut self.operations);
        self.rebuild_indexes();
        Ok(Some(replacement_ids))
    }

    pub fn move_operations(
        &mut self,
        target_ids: &HashSet<Uuid>,
        camera_depth: i64,
        camera_zoom: f64,
        delta_x: f64,
        delta_y: f64,
        tombstone_layer_id: Uuid,
    ) -> Result<Vec<Uuid>> {
        self.last_operation_commit_stats = None;
        let total_start = Instant::now();
        if delta_x == 0.0 && delta_y == 0.0 {
            return Ok(Vec::new());
        }
        let target_ids = self.expanded_object_group_ids(target_ids, tombstone_layer_id);
        let target_collect_start = Instant::now();
        let targets: Vec<_> = self
            .operations
            .iter()
            .filter(|operation| target_ids.contains(&operation.id))
            .cloned()
            .collect();
        let target_collect_ms = target_collect_start.elapsed().as_secs_f64() * 1_000.0;
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        let transaction_id = Uuid::new_v4();
        let mut tombstone = EditOperation::tombstone(
            targets.iter().map(|operation| operation.id).collect(),
            tombstone_layer_id,
        );
        tombstone.transaction_id = transaction_id;
        let replacement_build_start = Instant::now();
        let mut replacements = Vec::with_capacity(targets.len());
        for source in &targets {
            let mut replacement = source.clone();
            replacement.id = Uuid::new_v4();
            replacement.sequence = 0;
            replacement.paint_order = Some(source.effective_paint_order());
            replacement.transaction_id = transaction_id;
            replacement.affects_before_sequence = None;
            refresh_compact_source_geometry_ids(&mut replacement);
            translate_operation_by_screen_delta(
                &mut replacement,
                camera_depth,
                camera_zoom,
                delta_x,
                delta_y,
            )?;
            replacements.push(replacement);
        }
        let replacement_build_ms = replacement_build_start.elapsed().as_secs_f64() * 1_000.0;

        let command_build_start = Instant::now();
        let mut commands = Vec::with_capacity(replacements.len() + 1);
        commands.push(tombstone);
        commands.extend(replacements);
        let command_build_ms = command_build_start.elapsed().as_secs_f64() * 1_000.0;
        let storage_commit_start = Instant::now();
        let (revision, storage_stats) = self.store.commit_group_with_stats(&mut commands)?;
        let storage_commit_ms = storage_commit_start.elapsed().as_secs_f64() * 1_000.0;
        self.revision = revision;
        self.max_sequence = commands
            .last()
            .map_or(self.max_sequence, |operation| operation.sequence);

        let memory_prepare_start = Instant::now();
        let replacement_ids: Vec<_> = commands
            .iter()
            .filter(|operation| !operation.is_tombstone())
            .map(|operation| operation.id)
            .collect();
        let replacements = commands
            .iter()
            .filter(|operation| !operation.is_tombstone())
            .cloned()
            .collect::<Vec<_>>();
        let indexed_replacements = targets
            .iter()
            .map(|operation| operation.id)
            .zip(replacements.clone())
            .collect::<Vec<_>>();
        let memory_prepare_ms = memory_prepare_start.elapsed().as_secs_f64() * 1_000.0;
        let memory_replace_start = Instant::now();
        let (replace_stats, replace_fallback_reason) =
            self.replace_operations_in_place(indexed_replacements);
        if replace_fallback_reason.is_some() {
            let removed_ids: HashSet<_> = targets.iter().map(|operation| operation.id).collect();
            self.operations
                .retain(|operation| !removed_ids.contains(&operation.id));
            self.operations.extend(replacements);
            sort_operations_by_paint_order(&mut self.operations);
            self.rebuild_indexes();
        }
        let memory_replace_ms = memory_replace_start.elapsed().as_secs_f64() * 1_000.0;
        let mut memory_replace = replace_stats;
        if memory_replace.total_ms == 0.0 {
            memory_replace.total_ms = memory_replace_ms;
        }
        self.last_operation_commit_stats = Some(DocumentOperationCommitStats {
            target_count: targets.len(),
            replacement_count: replacement_ids.len(),
            target_collect_ms,
            sort_targets_ms: 0.0,
            replacement_build_ms,
            command_build_ms,
            storage_commit_ms,
            memory_prepare_ms,
            memory_replace_ms,
            memory_replace_fallback: replace_fallback_reason.is_some(),
            memory_replace_fallback_reason: replace_fallback_reason,
            memory_replace,
            total_ms: total_start.elapsed().as_secs_f64() * 1_000.0,
            storage: storage_stats,
        });
        Ok(replacement_ids)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn transform_operations(
        &mut self,
        target_ids: &HashSet<Uuid>,
        camera: &CameraAddress,
        viewport_width: f64,
        viewport_height: f64,
        transform: ScreenAffine,
        width_scale: f32,
        layer_id: Uuid,
    ) -> Result<Vec<Uuid>> {
        self.last_operation_commit_stats = None;
        let total_start = Instant::now();
        if target_ids.is_empty() || !width_scale.is_finite() || width_scale <= 0.0 {
            return Ok(Vec::new());
        }
        if !self.layer_is_editable(layer_id) {
            bail!("active layer is hidden or locked");
        }
        let target_ids = self.expanded_object_group_ids(target_ids, layer_id);
        let target_collect_start = Instant::now();
        let mut targets: Vec<_> = self
            .operations
            .iter()
            .filter(|operation| {
                target_ids.contains(&operation.id) && operation.layer_id == layer_id
            })
            .cloned()
            .collect();
        let target_collect_ms = target_collect_start.elapsed().as_secs_f64() * 1_000.0;
        if targets.len() != target_ids.len() {
            bail!("selection contains unavailable operations");
        }
        let sort_targets_start = Instant::now();
        sort_operations_by_paint_order(&mut targets);
        let sort_targets_ms = sort_targets_start.elapsed().as_secs_f64() * 1_000.0;

        let transaction_id = Uuid::new_v4();
        let mut tombstone = EditOperation::tombstone(
            targets.iter().map(|operation| operation.id).collect(),
            layer_id,
        );
        tombstone.transaction_id = transaction_id;
        let replacement_build_start = Instant::now();
        let mut replacements = Vec::with_capacity(targets.len());
        for source in &targets {
            let mut replacement = source.clone();
            replacement.id = Uuid::new_v4();
            replacement.sequence = 0;
            replacement.paint_order = Some(source.effective_paint_order());
            replacement.transaction_id = transaction_id;
            replacement.affects_before_sequence = None;
            refresh_compact_source_geometry_ids(&mut replacement);
            transform_operation_points(
                &mut replacement,
                camera,
                viewport_width,
                viewport_height,
                transform,
                width_scale,
            )?;
            replacements.push(replacement);
        }
        let replacement_build_ms = replacement_build_start.elapsed().as_secs_f64() * 1_000.0;

        let command_build_start = Instant::now();
        let mut commands = Vec::with_capacity(replacements.len() + 1);
        commands.push(tombstone);
        commands.extend(replacements);
        let command_build_ms = command_build_start.elapsed().as_secs_f64() * 1_000.0;
        let storage_commit_start = Instant::now();
        let (revision, storage_stats) = self.store.commit_group_with_stats(&mut commands)?;
        let storage_commit_ms = storage_commit_start.elapsed().as_secs_f64() * 1_000.0;
        self.revision = revision;
        self.max_sequence = commands
            .last()
            .map_or(self.max_sequence, |operation| operation.sequence);

        let memory_prepare_start = Instant::now();
        let replacement_ids = commands
            .iter()
            .filter(|operation| !operation.is_tombstone())
            .map(|operation| operation.id)
            .collect::<Vec<_>>();
        let replacements = commands
            .iter()
            .filter(|operation| !operation.is_tombstone())
            .cloned()
            .collect::<Vec<_>>();
        let indexed_replacements = targets
            .iter()
            .map(|operation| operation.id)
            .zip(replacements.clone())
            .collect::<Vec<_>>();
        let memory_prepare_ms = memory_prepare_start.elapsed().as_secs_f64() * 1_000.0;
        let memory_replace_start = Instant::now();
        let (replace_stats, replace_fallback_reason) =
            self.replace_operations_in_place(indexed_replacements);
        if replace_fallback_reason.is_some() {
            let removed_ids: HashSet<_> = targets.iter().map(|operation| operation.id).collect();
            self.operations
                .retain(|operation| !removed_ids.contains(&operation.id));
            self.operations.extend(replacements);
            sort_operations_by_paint_order(&mut self.operations);
            self.rebuild_indexes();
        }
        let memory_replace_ms = memory_replace_start.elapsed().as_secs_f64() * 1_000.0;
        let mut memory_replace = replace_stats;
        if memory_replace.total_ms == 0.0 {
            memory_replace.total_ms = memory_replace_ms;
        }
        self.last_operation_commit_stats = Some(DocumentOperationCommitStats {
            target_count: targets.len(),
            replacement_count: replacement_ids.len(),
            target_collect_ms,
            sort_targets_ms,
            replacement_build_ms,
            command_build_ms,
            storage_commit_ms,
            memory_prepare_ms,
            memory_replace_ms,
            memory_replace_fallback: replace_fallback_reason.is_some(),
            memory_replace_fallback_reason: replace_fallback_reason,
            memory_replace,
            total_ms: total_start.elapsed().as_secs_f64() * 1_000.0,
            storage: storage_stats,
        });
        Ok(replacement_ids)
    }

    pub fn recolor_operations(
        &mut self,
        target_ids: &HashSet<Uuid>,
        layer_id: Uuid,
        color: Color,
    ) -> Result<Vec<(Uuid, Uuid)>> {
        if target_ids.is_empty() {
            return Ok(Vec::new());
        }
        if !self.layer_is_editable(layer_id) {
            bail!("active layer is hidden or locked");
        }
        let target_ids = self.expanded_object_group_ids(target_ids, layer_id);
        let mut selected: Vec<_> = self
            .operations
            .iter()
            .filter(|operation| {
                target_ids.contains(&operation.id) && operation.layer_id == layer_id
            })
            .cloned()
            .collect();
        if selected.len() != target_ids.len() {
            bail!("selection contains unavailable operations");
        }
        selected.retain(|operation| matches!(operation.kind, EditKind::Paint | EditKind::Fill));
        if selected.is_empty() {
            return Ok(Vec::new());
        }
        sort_operations_by_paint_order(&mut selected);

        let opaque_color = Color::rgba(color.r, color.g, color.b, u8::MAX);
        let transaction_id = Uuid::new_v4();
        let mut tombstone = EditOperation::tombstone(
            selected.iter().map(|operation| operation.id).collect(),
            layer_id,
        );
        tombstone.transaction_id = transaction_id;
        let mut replacements = Vec::with_capacity(selected.len());
        let mut id_map = Vec::with_capacity(selected.len());
        for source in &selected {
            let replacement_id = Uuid::new_v4();
            let mut replacement = source.clone();
            replacement.id = replacement_id;
            replacement.sequence = 0;
            replacement.paint_order = Some(source.effective_paint_order());
            replacement.transaction_id = transaction_id;
            replacement.affects_before_sequence = None;
            replacement.color = opaque_color;
            replacement.destructive = true;
            id_map.push((source.id, replacement_id));
            replacements.push(replacement);
        }

        let mut commands = Vec::with_capacity(replacements.len() + 1);
        commands.push(tombstone);
        commands.extend(replacements);
        self.revision = self.store.commit_group(&mut commands)?;
        self.max_sequence = commands
            .last()
            .map_or(self.max_sequence, |operation| operation.sequence);

        let removed_ids: HashSet<_> = selected.iter().map(|operation| operation.id).collect();
        self.operations
            .retain(|operation| !removed_ids.contains(&operation.id));
        self.operations.extend(
            commands
                .into_iter()
                .filter(|operation| !operation.is_tombstone()),
        );
        sort_operations_by_paint_order(&mut self.operations);
        self.rebuild_indexes();
        Ok(id_map)
    }

    pub fn compact_operations(
        &mut self,
        target_ids: &HashSet<Uuid>,
        layer_id: Uuid,
    ) -> Result<Option<Uuid>> {
        if target_ids.is_empty() {
            return Ok(None);
        }
        if !self.layer_is_editable(layer_id) {
            bail!("active layer is hidden or locked");
        }
        let target_ids = self.expanded_object_group_ids(target_ids, layer_id);
        let mut selected = self
            .operations
            .iter()
            .filter(|operation| {
                target_ids.contains(&operation.id) && operation.layer_id == layer_id
            })
            .cloned()
            .collect::<Vec<_>>();
        if selected.len() != target_ids.len() {
            bail!("selection contains unavailable operations");
        }
        if selected.iter().any(EditOperation::is_metadata_command) {
            bail!("selection contains non-compactable operations");
        }
        sort_operations_by_paint_order(&mut selected);
        let compact_sources = compact_snapshot_sources(&selected);
        let depth = compact_anchor_depth(&compact_sources)?;

        let transaction_id = Uuid::new_v4();
        let mut tombstone = EditOperation::tombstone(
            selected.iter().map(|operation| operation.id).collect(),
            layer_id,
        );
        tombstone.transaction_id = transaction_id;
        let mut block = EditOperation::compact_block(
            layer_id,
            compact_block_bounds(&compact_sources, depth)?,
            compact_sources,
        );
        block.transaction_id = transaction_id;
        block.paint_order = tombstone
            .tombstone_targets
            .iter()
            .filter_map(|id| {
                self.operations
                    .iter()
                    .find(|operation| operation.id == *id)
                    .map(EditOperation::effective_paint_order)
            })
            .min();
        let block_id = block.id;

        let mut commands = vec![tombstone, block];
        self.revision = self.store.commit_group(&mut commands)?;
        self.max_sequence = commands
            .last()
            .map_or(self.max_sequence, |operation| operation.sequence);

        let removed_ids = target_ids.clone();
        self.operations
            .retain(|operation| !removed_ids.contains(&operation.id));
        self.operations.extend(
            commands
                .into_iter()
                .filter(|operation| !operation.is_tombstone()),
        );
        sort_operations_by_paint_order(&mut self.operations);
        self.rebuild_indexes();
        Ok(Some(block_id))
    }

    pub fn compact_older_operations(&mut self, keep_latest: usize) -> Result<usize> {
        let mut operations = self
            .operations
            .iter()
            .filter(|operation| !operation.is_metadata_command())
            .cloned()
            .collect::<Vec<_>>();
        if operations.len() <= keep_latest {
            return Ok(0);
        }
        operations.sort_by_key(|operation| (operation.sequence, operation.effective_paint_order()));
        let keep_ids = operations
            .iter()
            .rev()
            .take(keep_latest)
            .map(|operation| operation.id)
            .collect::<HashSet<_>>();
        operations.sort_by_key(|operation| {
            (
                self.layer_sort_order(operation.layer_id),
                operation.effective_paint_order(),
                operation.sequence,
            )
        });
        let mut groups = Vec::<(Uuid, Vec<Uuid>)>::new();
        let mut current_key = None::<(Uuid, i64)>;
        let mut current_ids = Vec::<Uuid>::new();
        for operation in operations {
            let next_key = (!keep_ids.contains(&operation.id)
                && self.layer_is_editable(operation.layer_id))
            .then(|| (operation.layer_id, operation_compaction_depth(&operation)));
            if next_key != current_key {
                if current_ids.len() >= 2 {
                    let layer_id = current_key
                        .map(|(layer_id, _)| layer_id)
                        .expect("compact run key");
                    groups.push((layer_id, std::mem::take(&mut current_ids)));
                } else {
                    current_ids.clear();
                }
                current_key = next_key;
            }
            if next_key.is_some() {
                current_ids.push(operation.id);
            }
        }
        if current_ids.len() >= 2 {
            let layer_id = current_key
                .map(|(layer_id, _)| layer_id)
                .expect("compact run key");
            groups.push((layer_id, current_ids));
        }

        let mut compacted = 0usize;
        for (layer_id, ids) in groups {
            if self
                .compact_operations(&ids.into_iter().collect(), layer_id)?
                .is_some()
            {
                compacted += 1;
            }
        }
        Ok(compacted)
    }

    pub fn move_operations_to_layer(
        &mut self,
        target_ids: &HashSet<Uuid>,
        source_layer_id: Uuid,
        target_layer_id: Uuid,
    ) -> Result<Vec<Uuid>> {
        if target_ids.is_empty() || source_layer_id == target_layer_id {
            return Ok(Vec::new());
        }
        if !self.layer_is_editable(source_layer_id) {
            anyhow::bail!("source layer is hidden or locked");
        }
        if !self.layer_is_editable(target_layer_id) {
            anyhow::bail!("target layer is hidden or locked");
        }
        let target_ids = self.expanded_object_group_ids(target_ids, source_layer_id);

        let mut targets = self
            .operations
            .iter()
            .filter(|operation| {
                target_ids.contains(&operation.id) && operation.layer_id == source_layer_id
            })
            .cloned()
            .collect::<Vec<_>>();
        if targets.len() != target_ids.len() {
            anyhow::bail!("selection contains unavailable operations");
        }
        sort_operations_by_paint_order(&mut targets);

        let transaction_id = Uuid::new_v4();
        let mut tombstone = EditOperation::tombstone(
            targets.iter().map(|operation| operation.id).collect(),
            source_layer_id,
        );
        tombstone.transaction_id = transaction_id;
        let mut replacements = Vec::with_capacity(targets.len());
        for source in &targets {
            let mut replacement = source.clone();
            replacement.id = Uuid::new_v4();
            replacement.sequence = 0;
            replacement.paint_order = None;
            replacement.transaction_id = transaction_id;
            replacement.affects_before_sequence = None;
            set_operation_layer_recursive(&mut replacement, target_layer_id);
            replacements.push(replacement);
        }

        let mut commands = Vec::with_capacity(replacements.len() + 1);
        commands.push(tombstone);
        commands.extend(replacements);
        self.revision = self.store.commit_group(&mut commands)?;
        self.max_sequence = commands
            .last()
            .map_or(self.max_sequence, |operation| operation.sequence);

        let removed_ids = targets
            .iter()
            .map(|operation| operation.id)
            .collect::<HashSet<_>>();
        self.operations
            .retain(|operation| !removed_ids.contains(&operation.id));
        let replacement_ids = commands
            .iter()
            .filter(|operation| !operation.is_tombstone())
            .map(|operation| operation.id)
            .collect::<Vec<_>>();
        self.operations.extend(
            commands
                .into_iter()
                .filter(|operation| !operation.is_tombstone()),
        );
        sort_operations_by_paint_order(&mut self.operations);
        self.rebuild_indexes();
        Ok(replacement_ids)
    }

    pub fn reorder_operations(
        &mut self,
        target_ids: &HashSet<Uuid>,
        layer_id: Uuid,
        direction: isize,
    ) -> Result<bool> {
        if target_ids.is_empty() || direction == 0 {
            return Ok(false);
        }
        if !self.layer_is_editable(layer_id) {
            anyhow::bail!("active layer is hidden or locked");
        }
        let target_ids = self.expanded_object_group_ids(target_ids, layer_id);

        let mut ordered_objects = Vec::<(Uuid, Vec<Uuid>)>::new();
        let mut object_positions = HashMap::<Uuid, usize>::new();
        for operation in self
            .operations
            .iter()
            .filter(|operation| operation.layer_id == layer_id)
        {
            let object_id = operation.object_group_id();
            if let Some(index) = object_positions.get(&object_id).copied() {
                ordered_objects[index].1.push(operation.id);
            } else {
                object_positions.insert(object_id, ordered_objects.len());
                ordered_objects.push((object_id, vec![operation.id]));
            }
        }
        let original_object_ids = ordered_objects
            .iter()
            .map(|(object_id, _)| *object_id)
            .collect::<Vec<_>>();
        let is_selected = |object: &(Uuid, Vec<Uuid>)| {
            object
                .1
                .iter()
                .any(|operation_id| target_ids.contains(operation_id))
        };
        if direction > 0 {
            for index in (0..ordered_objects.len().saturating_sub(1)).rev() {
                if is_selected(&ordered_objects[index]) && !is_selected(&ordered_objects[index + 1])
                {
                    ordered_objects.swap(index, index + 1);
                }
            }
        } else {
            for index in 1..ordered_objects.len() {
                if is_selected(&ordered_objects[index]) && !is_selected(&ordered_objects[index - 1])
                {
                    ordered_objects.swap(index - 1, index);
                }
            }
        }
        if ordered_objects
            .iter()
            .map(|(object_id, _)| *object_id)
            .eq(original_object_ids)
        {
            return Ok(false);
        }

        let updates = ordered_objects
            .iter()
            .enumerate()
            .flat_map(|(index, (_, operation_ids))| {
                operation_ids
                    .iter()
                    .map(move |operation_id| PaintOrderUpdate {
                        operation_id: *operation_id,
                        paint_order: i64::try_from(index).unwrap_or(i64::MAX),
                    })
            })
            .collect::<Vec<_>>();
        let mut command = EditOperation::paint_order(updates.clone(), layer_id);
        self.revision = self.store.commit(&mut command)?;
        self.max_sequence = command.sequence;

        let paint_orders = updates
            .into_iter()
            .map(|update| (update.operation_id, update.paint_order))
            .collect::<HashMap<_, _>>();
        for operation in &mut self.operations {
            if let Some(paint_order) = paint_orders.get(&operation.id) {
                operation.paint_order = Some(*paint_order);
            }
        }
        sort_operations_by_paint_order(&mut self.operations);
        self.rebuild_indexes();
        Ok(true)
    }

    pub fn paste_operations(
        &mut self,
        source_operations: &[EditOperation],
        target_layer_id: Uuid,
        camera_depth: i64,
        camera_zoom: f64,
        delta_x: f64,
        delta_y: f64,
    ) -> Result<Vec<Uuid>> {
        if source_operations.is_empty() {
            return Ok(Vec::new());
        }
        if !self.layer_is_editable(target_layer_id) {
            anyhow::bail!("target layer is hidden or locked");
        }
        if source_operations.iter().any(EditOperation::is_tombstone) {
            anyhow::bail!("cannot paste history tombstone operations");
        }
        let mut sources = source_operations.to_vec();
        remap_fill_group_ids(&mut sources);
        sort_operations_by_paint_order(&mut sources);
        let transaction_id = Uuid::new_v4();
        let mut next_paint_order = self
            .operations
            .iter()
            .map(EditOperation::effective_paint_order)
            .max()
            .unwrap_or(-1)
            .saturating_add(1);
        let mut object_orders = HashMap::<Uuid, i64>::new();
        let mut pasted = Vec::with_capacity(sources.len());
        for source in sources {
            let mut operation = source;
            let object_id = operation.object_group_id();
            prepare_pasted_operation(&mut operation, transaction_id, target_layer_id);
            operation.paint_order = Some(*object_orders.entry(object_id).or_insert_with(|| {
                let paint_order = next_paint_order;
                next_paint_order = next_paint_order.saturating_add(1);
                paint_order
            }));
            translate_operation_by_screen_delta(
                &mut operation,
                camera_depth,
                camera_zoom,
                delta_x,
                delta_y,
            )
            .context("paste offset is not representable at the operation depth")?;
            pasted.push(operation);
        }

        self.revision = self.store.commit_group(&mut pasted)?;
        self.max_sequence = pasted
            .last()
            .map_or(self.max_sequence, |operation| operation.sequence);
        let pasted_ids = pasted
            .iter()
            .map(|operation| operation.id)
            .collect::<Vec<_>>();
        self.operations.extend(pasted);
        sort_operations_by_paint_order(&mut self.operations);
        self.rebuild_indexes();
        Ok(pasted_ids)
    }

    pub fn undo(&mut self) -> Result<bool> {
        let Some((change, revision)) = self.store.undo()? else {
            return Ok(false);
        };
        match change {
            HistoryChange::Operations(operations) => self.apply_operation_undo(operations)?,
            HistoryChange::Layers { operations_changed } => {
                if operations_changed {
                    self.reload_active_operations()?;
                }
                self.layers = self.store.load_layers()?;
            }
        }
        self.max_sequence = self.store.active_max_sequence()?;
        self.revision = revision;
        Ok(true)
    }

    fn apply_operation_undo(&mut self, operations: Vec<EditOperation>) -> Result<()> {
        if operations.iter().any(EditOperation::is_paint_order_command) {
            return self.reload_active_operations();
        }
        let tombstone_targets: HashSet<_> = operations
            .iter()
            .filter(|operation| operation.is_tombstone())
            .flat_map(|operation| operation.tombstone_targets.iter().copied())
            .collect();
        let operation_ids: HashSet<_> = operations
            .iter()
            .filter(|operation| !operation.is_tombstone())
            .map(|operation| operation.id)
            .collect();
        if tombstone_targets.is_empty() {
            self.remove_operations(&operation_ids);
        } else {
            self.operations
                .retain(|operation| !operation_ids.contains(&operation.id));
            let restored = self.store.load_operations_by_ids(&tombstone_targets)?;
            let existing_ids: HashSet<_> = self
                .operations
                .iter()
                .map(|operation| operation.id)
                .collect();
            self.operations.extend(
                restored
                    .into_iter()
                    .filter(|operation| !existing_ids.contains(&operation.id)),
            );
            sort_operations_by_paint_order(&mut self.operations);
            self.rebuild_indexes();
        }
        Ok(())
    }

    pub fn redo(&mut self) -> Result<bool> {
        let Some((change, revision)) = self.store.redo()? else {
            return Ok(false);
        };
        match change {
            HistoryChange::Operations(operations) => self.apply_operation_redo(operations)?,
            HistoryChange::Layers { operations_changed } => {
                if operations_changed {
                    self.reload_active_operations()?;
                }
                self.layers = self.store.load_layers()?;
            }
        }
        self.max_sequence = self.store.active_max_sequence()?;
        self.revision = revision;
        Ok(true)
    }

    fn apply_operation_redo(&mut self, operations: Vec<EditOperation>) -> Result<()> {
        if operations.iter().any(EditOperation::is_paint_order_command) {
            return self.reload_active_operations();
        }
        let tombstone_targets: HashSet<_> = operations
            .iter()
            .filter(|operation| operation.is_tombstone())
            .flat_map(|operation| operation.tombstone_targets.iter().copied())
            .collect();
        if tombstone_targets.is_empty() {
            self.operations.extend(operations);
            sort_operations_by_paint_order(&mut self.operations);
            self.rebuild_indexes();
        } else {
            self.operations
                .retain(|operation| !tombstone_targets.contains(&operation.id));
            self.operations.extend(
                operations
                    .into_iter()
                    .filter(|operation| !operation.is_tombstone()),
            );
            sort_operations_by_paint_order(&mut self.operations);
            self.rebuild_indexes();
        }
        Ok(())
    }

    fn remove_operations(&mut self, operation_ids: &HashSet<Uuid>) {
        self.operations
            .retain(|operation| !operation_ids.contains(&operation.id));
        self.rebuild_indexes();
    }

    fn reload_active_operations(&mut self) -> Result<()> {
        self.operations = self.store.load_active_operations()?;
        self.rebuild_indexes();
        self.max_sequence = self.store.active_max_sequence()?;
        Ok(())
    }

    fn append_committed_operation(&mut self, operation: EditOperation) {
        if operation_can_append_to_render_index(
            &operation,
            self.render_operations.last().map(Arc::as_ref),
        ) {
            let operation_index = self.operations.len();
            let render_operation_index = self.render_operations.len();
            self.index.insert(operation_index, &operation);
            self.render_index.insert(render_operation_index, &operation);
            self.render_operations.push(Arc::new(operation.clone()));
            self.operations.push(operation);
        } else {
            self.operations.push(operation);
            self.rebuild_indexes();
        }
    }

    fn replace_operations_in_place(
        &mut self,
        replacements: Vec<(Uuid, EditOperation)>,
    ) -> (MemoryReplaceStats, Option<&'static str>) {
        let total_start = Instant::now();
        let mut stats = MemoryReplaceStats {
            replacement_count: replacements.len(),
            ..MemoryReplaceStats::default()
        };
        if replacements.is_empty() {
            return (stats, Some("empty_replacements"));
        }
        stats.compact_replacement_count = replacements
            .iter()
            .filter(|(_, operation)| operation.is_compact_block())
            .count();
        stats.metadata_replacement_count = replacements
            .iter()
            .filter(|(_, operation)| operation.is_metadata_command())
            .count();
        if stats.metadata_replacement_count > 0 {
            return (stats, Some("metadata_replacement"));
        }
        let position_map_start = Instant::now();
        let operation_position_by_id = self
            .operations
            .iter()
            .enumerate()
            .map(|(index, operation)| (operation.id, index))
            .collect::<HashMap<_, _>>();
        let render_position_by_id = self
            .render_operations
            .iter()
            .enumerate()
            .map(|(index, operation)| (operation.id, index))
            .collect::<HashMap<_, _>>();
        stats.position_map_ms = position_map_start.elapsed().as_secs_f64() * 1_000.0;
        let mut operation_positions = Vec::with_capacity(replacements.len());
        let mut render_plans = Vec::with_capacity(replacements.len());
        let position_lookup_start = Instant::now();
        for (old_id, replacement) in &replacements {
            if let Some(operation_position) = operation_position_by_id.get(old_id).copied() {
                operation_positions.push(operation_position);
                if replacement.is_compact_block() {
                    let old_operation = &self.operations[operation_position];
                    if !old_operation.is_compact_block() {
                        stats.total_ms = total_start.elapsed().as_secs_f64() * 1_000.0;
                        return (stats, Some("compact_replacement_for_noncompact_source"));
                    }
                    let mut old_render_sources = Vec::new();
                    collect_render_operations(old_operation, &mut old_render_sources);
                    let mut source_positions = Vec::with_capacity(old_render_sources.len());
                    for source in old_render_sources {
                        if let Some(render_position) =
                            render_position_by_id.get(&source.id).copied()
                        {
                            source_positions.push(render_position);
                        } else {
                            stats.missing_render_count += 1;
                        }
                    }
                    render_plans.push(RenderReplacementPlan::CompactSources(source_positions));
                } else if let Some(render_position) = render_position_by_id.get(old_id).copied() {
                    render_plans.push(RenderReplacementPlan::Single(render_position));
                } else {
                    stats.missing_render_count += 1;
                    render_plans.push(RenderReplacementPlan::CompactSources(Vec::new()));
                }
            } else {
                stats.missing_operation_count += 1;
                if !render_position_by_id.contains_key(old_id) {
                    stats.missing_render_count += 1;
                }
            }
        }
        stats.position_lookup_ms = position_lookup_start.elapsed().as_secs_f64() * 1_000.0;
        if stats.missing_operation_count > 0 && stats.missing_render_count > 0 {
            stats.total_ms = total_start.elapsed().as_secs_f64() * 1_000.0;
            return (stats, Some("missing_operation_and_render_positions"));
        }
        if stats.missing_operation_count > 0 {
            stats.total_ms = total_start.elapsed().as_secs_f64() * 1_000.0;
            return (stats, Some("missing_operation_positions"));
        }
        if stats.missing_render_count > 0 {
            stats.total_ms = total_start.elapsed().as_secs_f64() * 1_000.0;
            return (stats, Some("missing_render_positions"));
        }

        let operation_indices = operation_positions.iter().copied().collect::<HashSet<_>>();
        let render_indices = render_plans
            .iter()
            .flat_map(|plan| match plan {
                RenderReplacementPlan::Single(index) => std::slice::from_ref(index),
                RenderReplacementPlan::CompactSources(indices) => indices.as_slice(),
            })
            .copied()
            .collect::<HashSet<_>>();
        let bounds_start = Instant::now();
        let replacement_bounds = replacements
            .iter()
            .map(|(_, operation)| operation_bounds(operation))
            .collect::<Vec<_>>();
        stats.bounds_ms = bounds_start.elapsed().as_secs_f64() * 1_000.0;
        let remove_operation_start = Instant::now();
        self.index.remove_indices(&operation_indices);
        stats.remove_operation_index_ms = remove_operation_start.elapsed().as_secs_f64() * 1_000.0;
        let remove_render_start = Instant::now();
        self.render_index.remove_indices(&render_indices);
        stats.remove_render_index_ms = remove_render_start.elapsed().as_secs_f64() * 1_000.0;
        let assign_insert_start = Instant::now();
        for ((((_, replacement), operation_position), render_plan), bounds) in replacements
            .into_iter()
            .zip(operation_positions)
            .zip(render_plans)
            .zip(replacement_bounds)
        {
            self.operations[operation_position] = replacement.clone();
            self.index
                .insert_with_bounds(operation_position, bounds.as_ref());
            match render_plan {
                RenderReplacementPlan::Single(render_position) => {
                    self.render_operations[render_position] = Arc::new(replacement.clone());
                    self.render_index
                        .insert_with_bounds(render_position, bounds.as_ref());
                }
                RenderReplacementPlan::CompactSources(source_positions) => {
                    let mut render_sources = Vec::new();
                    collect_render_operations(&replacement, &mut render_sources);
                    sort_operations_by_paint_order(&mut render_sources);
                    let mut render_sources = render_sources.into_iter();
                    for render_position in source_positions {
                        let Some(source) = render_sources.next() else {
                            break;
                        };
                        self.render_operations[render_position] = Arc::new(source.clone());
                        let source_bounds = operation_bounds(&source);
                        self.render_index
                            .insert_with_bounds(render_position, source_bounds.as_ref());
                    }
                    for source in render_sources {
                        let render_position = self.render_operations.len();
                        let source_bounds = operation_bounds(&source);
                        self.render_index
                            .insert_with_bounds(render_position, source_bounds.as_ref());
                        self.render_operations.push(Arc::new(source));
                    }
                }
            }
        }
        stats.assign_insert_ms = assign_insert_start.elapsed().as_secs_f64() * 1_000.0;
        stats.total_ms = total_start.elapsed().as_secs_f64() * 1_000.0;
        (stats, None)
    }

    fn rebuild_indexes(&mut self) {
        self.index = OperationIndex::build(&self.operations);
        let render_operations = build_render_operations(&self.operations);
        self.render_index = OperationIndex::build(&render_operations);
        self.render_operations = render_operations.into_iter().map(Arc::new).collect();
    }

    fn layer_orders(&self) -> HashMap<Uuid, i64> {
        self.layers
            .iter()
            .map(|layer| (layer.id, layer.sort_order))
            .collect()
    }

    fn visible_layer_ids(&self) -> HashSet<Uuid> {
        self.layers
            .iter()
            .filter(|layer| layer.visible)
            .map(|layer| layer.id)
            .collect()
    }

    pub fn checkpoint(&self) -> Result<()> {
        self.store.checkpoint()
    }

    pub fn bookmarks(&self) -> Result<Vec<Bookmark>> {
        self.store.load_bookmarks()
    }

    pub fn add_bookmark(&self, name: &str, camera: &CameraAddress) -> Result<Bookmark> {
        self.store.save_bookmark(name, camera)
    }

    pub fn rename_bookmark(&self, id: Uuid, name: &str) -> Result<bool> {
        self.store.rename_bookmark(id, name)
    }

    pub fn delete_bookmark(&self, id: Uuid) -> Result<bool> {
        self.store.delete_bookmark(id)
    }
}

fn compact_anchor_depth(operations: &[EditOperation]) -> Result<i64> {
    if operations
        .iter()
        .any(|operation| operation.points.is_empty())
    {
        bail!("selection has no compactable geometry");
    }
    let Some(first_depth) = operations
        .iter()
        .find_map(|operation| operation.points.first().map(|point| point.depth))
    else {
        bail!("selection has no compactable geometry");
    };
    Ok(first_depth)
}

fn operation_compaction_depth(operation: &EditOperation) -> i64 {
    if operation.is_compact_block() {
        operation.native_depth
    } else {
        operation
            .points
            .first()
            .map_or(operation.native_depth, |point| point.depth)
    }
}

fn operation_depth_span(operation: &EditOperation) -> Option<(i64, i64)> {
    if operation.is_compact_block() {
        return operation
            .compact_sources
            .iter()
            .filter_map(operation_depth_span)
            .fold(None, |span, (min, max)| {
                Some(span.map_or((min, max), |(old_min, old_max): (i64, i64)| {
                    (old_min.min(min), old_max.max(max))
                }))
            });
    }
    let mut min = operation.native_depth;
    let mut max = operation.native_depth;
    for point in &operation.points {
        min = min.min(point.depth);
        max = max.max(point.depth);
    }
    Some((min, max))
}

fn compact_snapshot_sources(operations: &[EditOperation]) -> Vec<EditOperation> {
    let mut sources = Vec::new();
    for operation in operations {
        collect_compact_snapshot_sources(operation, &mut sources);
    }
    sources
}

fn build_render_operations(operations: &[EditOperation]) -> Vec<EditOperation> {
    let mut render_operations = Vec::new();
    for operation in operations {
        collect_render_operations(operation, &mut render_operations);
    }
    sort_operations_by_paint_order(&mut render_operations);
    render_operations
}

fn operation_can_append_to_render_index(
    operation: &EditOperation,
    last_render_operation: Option<&EditOperation>,
) -> bool {
    !operation.is_metadata_command()
        && !operation.is_compact_block()
        && last_render_operation.is_none_or(|last| {
            operation_render_sort_key(last) <= operation_render_sort_key(operation)
        })
}

fn operation_render_sort_key(operation: &EditOperation) -> (i64, i64) {
    (operation.effective_paint_order(), operation.sequence)
}

fn collect_render_operations(operation: &EditOperation, operations: &mut Vec<EditOperation>) {
    if operation.is_compact_block() {
        for source in &operation.compact_sources {
            collect_render_operations(source, operations);
        }
    } else {
        operations.push(operation.clone());
    }
}

fn collect_compact_snapshot_sources(operation: &EditOperation, sources: &mut Vec<EditOperation>) {
    if operation.is_compact_block() {
        for source in &operation.compact_sources {
            collect_compact_snapshot_sources(source, sources);
        }
    } else {
        sources.push(operation.clone());
    }
}

fn subtraction_replacement_geometry_is_valid(
    operation: &EditOperation,
    source: Option<&EditOperation>,
) -> bool {
    let points_are_finite = operation
        .points
        .iter()
        .all(|point| point.local_x.is_finite() && point.local_y.is_finite());
    match operation.kind {
        EditKind::Paint => operation.points.len() >= 2 && points_are_finite,
        EditKind::Fill => {
            (operation.points.len() >= 4
                && operation.points.first() == operation.points.last()
                && points_are_finite)
                || source.is_some_and(|source| {
                    source.id == operation.id
                        && source.kind == EditKind::Fill
                        && source.points == operation.points
                })
        }
        EditKind::CompactBlock => {
            !operation.points.is_empty()
                && points_are_finite
                && !operation.compact_sources.is_empty()
                && operation.compact_sources.iter().all(|replacement| {
                    let source = source.and_then(|source| {
                        source
                            .compact_sources
                            .iter()
                            .find(|source| source.id == replacement.id)
                    });
                    subtraction_replacement_geometry_is_valid(replacement, source)
                })
        }
        EditKind::Erase => !operation.points.is_empty() && points_are_finite,
        EditKind::EraseArea => operation.points.len() >= 3 && points_are_finite,
    }
}

fn compact_block_bounds(operations: &[EditOperation], depth: i64) -> Result<Vec<CanvasPoint>> {
    let mut bounds = ContinuousBounds::new(depth);
    for operation in operations {
        accumulate_operation_bounds(operation, &mut bounds)?;
    }
    let Some((min_x, max_x, min_y, max_y)) = bounds.into_axes() else {
        bail!("selection has no compactable geometry");
    };
    Ok(vec![
        CanvasPoint::new(
            depth,
            min_x.tile.clone(),
            min_y.tile.clone(),
            min_x.local,
            min_y.local,
        ),
        CanvasPoint::new(
            depth,
            max_x.tile.clone(),
            min_y.tile,
            max_x.local,
            min_y.local,
        ),
        CanvasPoint::new(
            depth,
            max_x.tile.clone(),
            max_y.tile.clone(),
            max_x.local,
            max_y.local,
        ),
        CanvasPoint::new(depth, min_x.tile, max_y.tile, min_x.local, max_y.local),
    ])
}

#[derive(Debug, Clone)]
struct AxisPoint {
    tile: BigInt,
    local: f64,
}

#[derive(Default)]
struct ContinuousBounds {
    depth: i64,
    min_x: Option<AxisPoint>,
    max_x: Option<AxisPoint>,
    min_y: Option<AxisPoint>,
    max_y: Option<AxisPoint>,
}

impl ContinuousBounds {
    fn new(depth: i64) -> Self {
        Self {
            depth,
            ..Self::default()
        }
    }

    fn include(&mut self, point: &CanvasPoint, radius_tiles: f64) -> Result<()> {
        let left = axis_to_depth(
            point.depth,
            shifted_axis(&point.tile_x, point.local_x, -radius_tiles)?,
            self.depth,
        )?;
        let right = axis_to_depth(
            point.depth,
            shifted_axis(&point.tile_x, point.local_x, radius_tiles)?,
            self.depth,
        )?;
        let top = axis_to_depth(
            point.depth,
            shifted_axis(&point.tile_y, point.local_y, -radius_tiles)?,
            self.depth,
        )?;
        let bottom = axis_to_depth(
            point.depth,
            shifted_axis(&point.tile_y, point.local_y, radius_tiles)?,
            self.depth,
        )?;
        include_min(&mut self.min_x, left.clone());
        include_max(&mut self.max_x, right);
        include_min(&mut self.min_y, top.clone());
        include_max(&mut self.max_y, bottom);
        Ok(())
    }

    fn into_axes(self) -> Option<(AxisPoint, AxisPoint, AxisPoint, AxisPoint)> {
        Some((self.min_x?, self.max_x?, self.min_y?, self.max_y?))
    }
}

fn accumulate_operation_bounds(
    operation: &EditOperation,
    bounds: &mut ContinuousBounds,
) -> Result<()> {
    if operation.is_compact_block() {
        for source in &operation.compact_sources {
            accumulate_operation_bounds(source, bounds)?;
        }
        return Ok(());
    }
    let radius_tiles = operation_radius_tiles(operation)?;
    for point in &operation.points {
        bounds.include(point, radius_tiles)?;
    }
    Ok(())
}

fn operation_radius_tiles(operation: &EditOperation) -> Result<f64> {
    if operation.kind.is_area() {
        return Ok(0.0);
    }
    let native_zoom = operation.native_zoom.max(1.0e-12);
    let radius = f64::from(operation.width_px.max(0.0)) / (2.0 * TILE_PIXELS * native_zoom);
    if radius.is_finite() {
        Ok(radius)
    } else {
        bail!("operation width is not finite")
    }
}

fn shifted_axis(tile: &BigInt, local: f64, delta: f64) -> Result<AxisPoint> {
    let shifted = local + delta;
    if !shifted.is_finite() {
        bail!("compact bounds are not finite");
    }
    let whole = shifted.floor();
    if whole < i64::MIN as f64 || whole > i64::MAX as f64 {
        bail!("compact bounds exceed supported local offset");
    }
    Ok(AxisPoint {
        tile: tile + BigInt::from(whole as i64),
        local: shifted - whole,
    })
}

fn axis_to_depth(source_depth: i64, axis: AxisPoint, target_depth: i64) -> Result<AxisPoint> {
    let delta = target_depth.saturating_sub(source_depth);
    if delta == 0 {
        return Ok(axis);
    }
    let levels = delta.unsigned_abs();
    let factor = depth_factor_bigint(levels)?;
    let scale = depth_factor_f64(levels)?;
    if delta > 0 {
        let scaled_local = axis.local * scale;
        if !scaled_local.is_finite() {
            bail!("compact bounds depth conversion is not finite");
        }
        let whole = scaled_local.floor();
        let whole = BigInt::from_f64(whole)
            .context("compact bounds depth conversion exceeds supported precision")?;
        Ok(AxisPoint {
            tile: axis.tile * factor + whole,
            local: scaled_local - scaled_local.floor(),
        })
    } else {
        let quotient = axis.tile.div_euclid(&factor);
        let remainder = axis
            .tile
            .rem_euclid(&factor)
            .to_f64()
            .context("compact bounds depth conversion exceeds supported precision")?;
        Ok(AxisPoint {
            tile: quotient,
            local: (remainder + axis.local) / scale,
        })
    }
}

fn depth_factor_bigint(levels: u64) -> Result<BigInt> {
    let exponent = u32::try_from(levels).context("compact bounds depth delta is too large")?;
    Ok(BigInt::from(DEPTH_RATIO).pow(exponent))
}

fn depth_factor_f64(levels: u64) -> Result<f64> {
    let exponent = i32::try_from(levels).context("compact bounds depth delta is too large")?;
    let factor = (DEPTH_RATIO as f64).powi(exponent);
    if factor.is_finite() {
        Ok(factor)
    } else {
        bail!("compact bounds depth delta is too large")
    }
}

fn include_min(target: &mut Option<AxisPoint>, candidate: AxisPoint) {
    if target
        .as_ref()
        .is_none_or(|current| axis_less(&candidate, current))
    {
        *target = Some(candidate);
    }
}

fn include_max(target: &mut Option<AxisPoint>, candidate: AxisPoint) {
    if target
        .as_ref()
        .is_none_or(|current| axis_less(current, &candidate))
    {
        *target = Some(candidate);
    }
}

fn axis_less(left: &AxisPoint, right: &AxisPoint) -> bool {
    left.tile < right.tile || (left.tile == right.tile && left.local < right.local)
}

fn translate_operation_by_screen_delta(
    operation: &mut EditOperation,
    camera_depth: i64,
    camera_zoom: f64,
    delta_x: f64,
    delta_y: f64,
) -> Result<()> {
    if operation.is_compact_block() {
        for source in &mut operation.compact_sources {
            translate_operation_by_screen_delta(
                source,
                camera_depth,
                camera_zoom,
                delta_x,
                delta_y,
            )?;
        }
        recompute_compact_block_bounds(operation)?;
        return Ok(());
    }
    operation.points = operation
        .points
        .iter()
        .map(|point| {
            point
                .translated_by_screen_delta(camera_depth, camera_zoom, delta_x, delta_y)
                .context("movement is not representable at the operation depth")
        })
        .collect::<Result<_>>()?;
    Ok(())
}

fn prepare_pasted_operation(
    operation: &mut EditOperation,
    transaction_id: Uuid,
    target_layer_id: Uuid,
) {
    refresh_compact_source_identity(operation);
    operation.id = Uuid::new_v4();
    operation.sequence = 0;
    operation.paint_order = None;
    operation.transaction_id = transaction_id;
    operation.affects_before_sequence = None;
    set_operation_layer_recursive(operation, target_layer_id);
}

fn refresh_compact_source_identity(operation: &mut EditOperation) {
    for source in &mut operation.compact_sources {
        source.id = Uuid::new_v4();
        source.transaction_id = Uuid::new_v4();
        source.sequence = 0;
        source.affects_before_sequence = None;
        refresh_compact_source_identity(source);
    }
}

fn refresh_compact_source_geometry_ids(operation: &mut EditOperation) {
    for source in &mut operation.compact_sources {
        source.id = Uuid::new_v4();
        refresh_compact_source_geometry_ids(source);
    }
}

fn transform_operation_points(
    operation: &mut EditOperation,
    camera: &CameraAddress,
    viewport_width: f64,
    viewport_height: f64,
    transform: ScreenAffine,
    width_scale: f32,
) -> Result<()> {
    if operation.is_compact_block() {
        for source in &mut operation.compact_sources {
            transform_operation_points(
                source,
                camera,
                viewport_width,
                viewport_height,
                transform,
                width_scale,
            )?;
        }
        recompute_compact_block_bounds(operation)?;
        return Ok(());
    }
    let scaled_width = operation.width_px * width_scale;
    if !scaled_width.is_finite() {
        bail!("scaled operation width is not finite");
    }
    operation.width_px = scaled_width;
    operation.points = operation
        .points
        .iter()
        .map(|point| {
            transform
                .transform_canvas_point(point, camera, viewport_width, viewport_height)
                .context("transformed point is not representable at its operation depth")
        })
        .collect::<Result<_>>()?;
    Ok(())
}

pub(crate) fn recompute_compact_block_bounds(operation: &mut EditOperation) -> Result<()> {
    let depth = compact_anchor_depth(&operation.compact_sources)?;
    operation.points = compact_block_bounds(&operation.compact_sources, depth)?;
    operation.native_depth = depth;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{CanvasDocument, VisibleDepthMode};
    use crate::coords::{CameraAddress, CanvasPoint, ScreenAffine};
    use crate::model::{Color, EditKind, EditOperation, Layer};
    use crate::tile_cache::TileKey;
    use num_bigint::BigInt;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use tempfile::TempDir;
    use uuid::Uuid;

    fn operation() -> EditOperation {
        EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![
                CanvasPoint::new(0, 0.into(), 0.into(), 0.1, 0.2),
                CanvasPoint::new(0, 0.into(), 0.into(), 0.3, 0.4),
            ],
            Color::BLACK,
            5.0,
        )
    }

    fn fill_operation(offset: f64, color: Color) -> EditOperation {
        let depth = 17;
        let points = [(0.1, 0.1), (0.8, 0.1), (0.8, 0.8), (0.1, 0.8)]
            .into_iter()
            .map(|(x, y)| CanvasPoint::new(depth, 12.into(), (-9).into(), x + offset, y))
            .collect::<Vec<_>>();
        let mut closed = points.clone();
        closed.push(points[0].clone());
        EditOperation::draft(EditKind::Fill, depth, 3.25, closed, color, 0.0)
    }

    fn split_fill_points(points: &[CanvasPoint]) -> Vec<Vec<CanvasPoint>> {
        vec![
            vec![
                points[0].clone(),
                points[1].clone(),
                points[2].clone(),
                points[0].clone(),
            ],
            vec![
                points[0].clone(),
                points[2].clone(),
                points[3].clone(),
                points[0].clone(),
            ],
        ]
    }

    fn fill_group_fragments(group_id: Uuid) -> Vec<EditOperation> {
        [
            [(0.1, 0.1), (0.9, 0.1), (0.1, 0.9)],
            [(0.9, 0.1), (0.9, 0.9), (0.1, 0.9)],
        ]
        .into_iter()
        .map(|coordinates| {
            let mut points = coordinates
                .into_iter()
                .map(|(x, y)| CanvasPoint::new(0, 0.into(), 0.into(), x, y))
                .collect::<Vec<_>>();
            points.push(points[0].clone());
            let mut operation =
                EditOperation::draft(EditKind::Fill, 0, 1.0, points, Color::BLACK, 0.0);
            operation.paint_order = Some(0);
            operation.transaction_id = group_id;
            operation.fill_group_id = Some(group_id);
            operation
        })
        .collect()
    }

    #[test]
    fn partial_fill_group_selection_expands_through_edit_commands() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        let group_id = Uuid::new_v4();
        let fragments = fill_group_fragments(group_id);
        let first_id = fragments[0].id;
        document.commit_group(fragments)?;
        let mut later = operation();
        later.kind = EditKind::Fill;
        later.width_px = 0.0;
        let later_id = later.id;
        document.commit(later)?;

        let partial = HashSet::from([first_id]);
        assert_eq!(
            document
                .expanded_object_group_ids(&partial, crate::model::DEFAULT_LAYER_ID)
                .len(),
            2
        );
        let replacement_ids = document.transform_operations(
            &partial,
            &CameraAddress::default(),
            512.0,
            512.0,
            ScreenAffine::uniform_scale(256.0, 256.0, 0.9).unwrap(),
            1.0,
            crate::model::DEFAULT_LAYER_ID,
        )?;
        assert_eq!(replacement_ids.len(), 2);
        assert!(
            document
                .operations()
                .iter()
                .filter(|operation| { operation.fill_group_id == Some(group_id) })
                .count()
                == 2
        );
        drop(document);

        let mut document = CanvasDocument::open(&root)?;
        let group_member = document
            .operations()
            .iter()
            .find(|operation| operation.fill_group_id == Some(group_id))
            .expect("persisted fill group member")
            .id;
        let recolored = document.recolor_operations(
            &HashSet::from([group_member]),
            crate::model::DEFAULT_LAYER_ID,
            Color::rgba(200, 100, 50, 255),
        )?;
        assert_eq!(recolored.len(), 2);
        let recolored_member = recolored[0].1;
        assert!(document.reorder_operations(
            &HashSet::from([recolored_member]),
            crate::model::DEFAULT_LAYER_ID,
            1,
        )?);
        let grouped = document
            .operations()
            .iter()
            .filter(|operation| operation.fill_group_id == Some(group_id))
            .collect::<Vec<_>>();
        assert_eq!(grouped.len(), 2);
        assert_eq!(
            grouped[0].effective_paint_order(),
            grouped[1].effective_paint_order()
        );
        assert_eq!(document.operations().first().unwrap().id, later_id);

        assert_eq!(
            document.delete_operations(
                &HashSet::from([grouped[0].id]),
                crate::model::DEFAULT_LAYER_ID,
            )?,
            2
        );
        assert_eq!(document.operations().len(), 1);
        assert_eq!(document.operations()[0].id, later_id);
        Ok(())
    }

    #[test]
    fn pasted_fill_group_gets_one_fresh_identity_and_paint_order() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        let source_group_id = Uuid::new_v4();
        document.commit_group(fill_group_fragments(source_group_id))?;
        let sources = document.operations().to_vec();

        let pasted_ids = document.paste_operations(
            &sources,
            crate::model::DEFAULT_LAYER_ID,
            0,
            1.0,
            16.0,
            16.0,
        )?;
        assert_eq!(pasted_ids.len(), 2);
        let pasted = document
            .operations()
            .iter()
            .filter(|operation| pasted_ids.contains(&operation.id))
            .collect::<Vec<_>>();
        let pasted_group_ids = pasted
            .iter()
            .filter_map(|operation| operation.fill_group_id)
            .collect::<HashSet<_>>();
        assert_eq!(pasted_group_ids.len(), 1);
        assert!(!pasted_group_ids.contains(&source_group_id));
        assert_eq!(
            pasted[0].effective_paint_order(),
            pasted[1].effective_paint_order()
        );
        drop(document);

        let mut reopened = CanvasDocument::open(&root)?;
        let original_group_ids = reopened
            .operations()
            .iter()
            .filter_map(|operation| operation.fill_group_id)
            .collect::<HashSet<_>>();
        assert_eq!(original_group_ids.len(), 2);
        let (duplicate_layer, duplicate_count) = reopened
            .duplicate_layer(crate::model::DEFAULT_LAYER_ID)?
            .expect("duplicated grouped layer");
        assert_eq!(duplicate_count, 4);
        let duplicate_group_ids = reopened
            .operations()
            .iter()
            .filter(|operation| operation.layer_id == duplicate_layer.id)
            .filter_map(|operation| operation.fill_group_id)
            .collect::<HashSet<_>>();
        assert_eq!(duplicate_group_ids.len(), 2);
        assert!(original_group_ids.is_disjoint(&duplicate_group_ids));
        Ok(())
    }

    #[test]
    fn grouped_history_updates_the_in_memory_document_incrementally() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let first = operation();
        let first_id = first.id;
        document.commit(first)?;

        let transaction_id = Uuid::new_v4();
        let mut second = operation();
        second.transaction_id = transaction_id;
        let second_id = second.id;
        document.commit(second)?;
        let mut third = operation();
        third.transaction_id = transaction_id;
        let third_id = third.id;
        document.commit(third)?;
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![first_id, second_id, third_id]
        );

        assert!(document.undo()?);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![first_id]
        );
        assert!(document.redo()?);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![first_id, second_id, third_id]
        );
        Ok(())
    }

    #[test]
    fn append_commit_is_immediately_queryable_without_reopen() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let first = operation();
        let first_id = first.id;
        document.commit(first)?;
        let second = operation();
        let second_id = second.id;
        document.commit(second)?;

        let key = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: 0,
        };
        let render_ids = document
            .render_operations_for_tiles(std::slice::from_ref(&key))
            .into_iter()
            .map(|operation| operation.id)
            .collect::<Vec<_>>();

        assert_eq!(render_ids, vec![first_id, second_id]);
        Ok(())
    }

    #[test]
    fn editable_query_filters_viewport_layer_lock_and_complete_depth_span() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let mut ids_by_depth = HashMap::new();
        for depth in [-3, -2, 0, 2, 3] {
            let mut source = operation();
            source.native_depth = depth;
            source.points = vec![
                CanvasPoint::new(depth, 0.into(), 0.into(), 0.1, 0.2),
                CanvasPoint::new(depth, 0.into(), 0.into(), 0.3, 0.4),
            ];
            ids_by_depth.insert(depth, source.id);
            document.commit(source)?;
        }
        let other_layer = document.create_layer("Other")?;
        let mut other = operation();
        other.layer_id = other_layer.id;
        let other_id = other.id;
        document.commit(other)?;
        let key = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: 0,
        };

        let editable = document
            .editable_operation_ids_for_tiles(
                std::slice::from_ref(&key),
                crate::model::DEFAULT_LAYER_ID,
                0,
                2,
            )
            .into_iter()
            .collect::<HashSet<_>>();
        assert_eq!(
            editable,
            HashSet::from([ids_by_depth[&-2], ids_by_depth[&0], ids_by_depth[&2]])
        );
        assert!(!editable.contains(&other_id));

        let exact_negative_depth = document.editable_operation_ids_for_tiles(
            std::slice::from_ref(&key),
            crate::model::DEFAULT_LAYER_ID,
            -2,
            0,
        );
        assert_eq!(exact_negative_depth, vec![ids_by_depth[&-2]]);

        document.set_layer_locked(crate::model::DEFAULT_LAYER_ID, true)?;
        assert!(
            document
                .editable_operation_ids_for_tiles(
                    std::slice::from_ref(&key),
                    crate::model::DEFAULT_LAYER_ID,
                    0,
                    2,
                )
                .is_empty()
        );
        document.set_layer_locked(crate::model::DEFAULT_LAYER_ID, false)?;
        document.set_layer_visibility(crate::model::DEFAULT_LAYER_ID, false)?;
        assert!(
            document
                .editable_operation_ids_for_tiles(
                    std::slice::from_ref(&key),
                    crate::model::DEFAULT_LAYER_ID,
                    0,
                    2,
                )
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn visible_depth_mode_distinguishes_empty_near_distant_and_hidden() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let key = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: 0,
        };
        assert_eq!(
            document.visible_depth_mode_for_tiles(
                std::slice::from_ref(&key),
                crate::model::DEFAULT_LAYER_ID,
                0,
                2,
            ),
            VisibleDepthMode::Empty
        );

        let mut source = operation();
        source.native_depth = 3;
        source.points = vec![
            CanvasPoint::new(3, 0.into(), 0.into(), 0.1, 0.2),
            CanvasPoint::new(3, 0.into(), 0.into(), 0.3, 0.4),
        ];
        document.commit(source)?;
        assert_eq!(
            document.visible_depth_mode_for_tiles(
                std::slice::from_ref(&key),
                crate::model::DEFAULT_LAYER_ID,
                0,
                2,
            ),
            VisibleDepthMode::Distant
        );
        assert_eq!(
            document.visible_depth_mode_for_tiles(
                std::slice::from_ref(&key),
                crate::model::DEFAULT_LAYER_ID,
                0,
                3,
            ),
            VisibleDepthMode::Near
        );

        let other_layer = document.create_layer("Other")?;
        let mut other = operation();
        other.layer_id = other_layer.id;
        document.commit(other)?;
        assert_eq!(
            document.visible_depth_mode_for_tiles(
                std::slice::from_ref(&key),
                crate::model::DEFAULT_LAYER_ID,
                0,
                2,
            ),
            VisibleDepthMode::Distant
        );
        assert_eq!(
            document
                .visible_depth_mode_for_tiles(std::slice::from_ref(&key), other_layer.id, 0, 2,),
            VisibleDepthMode::Near
        );

        document.set_layer_visibility(other_layer.id, false)?;
        document.set_layer_locked(crate::model::DEFAULT_LAYER_ID, true)?;
        assert_eq!(
            document.visible_depth_mode_for_tiles(
                std::slice::from_ref(&key),
                crate::model::DEFAULT_LAYER_ID,
                0,
                3,
            ),
            VisibleDepthMode::Distant
        );
        document.set_layer_locked(crate::model::DEFAULT_LAYER_ID, false)?;
        document.set_layer_visibility(crate::model::DEFAULT_LAYER_ID, false)?;
        assert_eq!(
            document.visible_depth_mode_for_tiles(
                std::slice::from_ref(&key),
                crate::model::DEFAULT_LAYER_ID,
                0,
                3,
            ),
            VisibleDepthMode::Empty
        );
        Ok(())
    }

    #[test]
    fn editable_query_keeps_mixed_depth_compact_block_atomic() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let coarse = operation();
        let coarse_id = coarse.id;
        document.commit(coarse)?;
        let mut fine = operation();
        fine.native_depth = 3;
        fine.points = vec![
            CanvasPoint::new(3, 0.into(), 0.into(), 0.1, 0.2),
            CanvasPoint::new(3, 0.into(), 0.into(), 0.3, 0.4),
        ];
        let fine_id = fine.id;
        document.commit(fine)?;
        let block_id = document
            .compact_operations(
                &HashSet::from([coarse_id, fine_id]),
                crate::model::DEFAULT_LAYER_ID,
            )?
            .expect("compact block");
        let key = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: 0,
        };

        assert!(
            document
                .editable_operation_ids_for_tiles(
                    std::slice::from_ref(&key),
                    crate::model::DEFAULT_LAYER_ID,
                    0,
                    2,
                )
                .is_empty()
        );
        assert_eq!(
            document.visible_depth_mode_for_tiles(
                std::slice::from_ref(&key),
                crate::model::DEFAULT_LAYER_ID,
                0,
                2,
            ),
            VisibleDepthMode::Distant
        );
        assert_eq!(
            document.editable_operation_ids_for_tiles(
                std::slice::from_ref(&key),
                crate::model::DEFAULT_LAYER_ID,
                0,
                3,
            ),
            vec![block_id]
        );
        assert_eq!(
            document.visible_depth_mode_for_tiles(
                std::slice::from_ref(&key),
                crate::model::DEFAULT_LAYER_ID,
                0,
                3,
            ),
            VisibleDepthMode::Near
        );
        Ok(())
    }

    #[test]
    fn out_of_order_commit_falls_back_to_sorted_render_index() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let mut first = operation();
        first.paint_order = Some(10);
        let first_id = first.id;
        document.commit(first)?;
        let mut second = operation();
        second.paint_order = Some(0);
        let second_id = second.id;
        document.commit(second)?;

        let key = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: 0,
        };
        let render_ids = document
            .render_operations_for_tiles(std::slice::from_ref(&key))
            .into_iter()
            .map(|operation| operation.id)
            .collect::<Vec<_>>();

        assert_eq!(render_ids, vec![second_id, first_id]);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![first_id, second_id]
        );
        Ok(())
    }

    #[test]
    fn selected_reorder_preserves_identity_and_round_trips_history() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        let mut ids = Vec::new();
        for _ in 0..4 {
            let operation = operation();
            ids.push(operation.id);
            document.commit(operation)?;
        }
        let original_points = document
            .operations()
            .iter()
            .map(|operation| (operation.id, operation.points.clone()))
            .collect::<std::collections::HashMap<_, _>>();
        let selected = HashSet::from([ids[1], ids[2]]);

        assert!(document.reorder_operations(&selected, crate::model::DEFAULT_LAYER_ID, 1)?);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![ids[0], ids[3], ids[1], ids[2]]
        );
        assert!(document.operations().iter().all(|operation| {
            original_points
                .get(&operation.id)
                .is_some_and(|points| points == &operation.points)
        }));
        let revision_at_edge = document.revision();
        assert!(!document.reorder_operations(&selected, crate::model::DEFAULT_LAYER_ID, 1)?);
        assert_eq!(document.revision(), revision_at_edge);
        drop(document);

        let mut document = CanvasDocument::open(&root)?;
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![ids[0], ids[3], ids[1], ids[2]]
        );
        assert!(document.undo()?);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            ids
        );
        assert!(document.redo()?);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![ids[0], ids[3], ids[1], ids[2]]
        );
        assert!(document.reorder_operations(&selected, crate::model::DEFAULT_LAYER_ID, -1)?);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            ids
        );
        Ok(())
    }

    #[test]
    fn selected_delete_survives_reopen_and_round_trips_history() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        let first = operation();
        let first_id = first.id;
        document.commit(first)?;
        let second = operation();
        let second_id = second.id;
        document.commit(second)?;
        let third = operation();
        let third_id = third.id;
        document.commit(third)?;

        let targets = HashSet::from([first_id, third_id, Uuid::new_v4()]);
        assert_eq!(
            document.delete_operations(&targets, crate::model::DEFAULT_LAYER_ID)?,
            2
        );
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![second_id]
        );
        assert_eq!(document.max_sequence(), 4);
        drop(document);

        let mut document = CanvasDocument::open(&root)?;
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![second_id]
        );
        assert!(document.undo()?);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![first_id, second_id, third_id]
        );
        assert!(document.redo()?);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![second_id]
        );
        assert!(document.undo()?);
        document.commit(operation())?;
        assert!(!document.redo()?);
        assert_eq!(document.operations().len(), 4);
        Ok(())
    }

    #[test]
    fn erase_draft_is_not_recovered_as_a_legacy_operation() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let document = CanvasDocument::open(&root)?;
        let mut erase = operation();
        erase.kind = EditKind::Erase;
        document.save_draft(&erase)?;
        drop(document);

        let document = CanvasDocument::open(&root)?;
        assert!(document.operations().is_empty());
        Ok(())
    }

    #[test]
    fn paint_subtraction_split_is_one_persisted_history_transaction() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        let mut source = operation();
        source.points = vec![
            CanvasPoint::new(0, 0.into(), 0.into(), 0.1, 0.2),
            CanvasPoint::new(0, 0.into(), 0.into(), 0.3, 0.4),
            CanvasPoint::new(0, 0.into(), 0.into(), 0.6, 0.5),
            CanvasPoint::new(0, 0.into(), 0.into(), 0.8, 0.7),
        ];
        let source_id = source.id;
        let source_points = source.points.clone();
        document.commit(source)?;

        assert!(
            document
                .commit_paint_subtraction(
                    vec![(source_id, vec![source_points.clone()])],
                    crate::model::DEFAULT_LAYER_ID,
                )?
                .is_none()
        );
        assert_eq!(document.revision(), 1);
        assert_eq!(document.max_sequence(), 1);

        let runs = vec![source_points[..2].to_vec(), source_points[2..].to_vec()];
        let replacement_ids = document
            .commit_paint_subtraction(
                vec![(source_id, runs.clone())],
                crate::model::DEFAULT_LAYER_ID,
            )?
            .expect("split changes the source");

        assert_eq!(replacement_ids.len(), 2);
        assert_eq!(document.revision(), 2);
        assert_eq!(document.max_sequence(), 4);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.points.clone())
                .collect::<Vec<_>>(),
            runs
        );
        assert!(document.operations().iter().all(|operation| {
            replacement_ids.contains(&operation.id)
                && operation.effective_paint_order() == 1
                && operation.width_px == 5.0
                && operation.color == Color::BLACK
        }));
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.transaction_id)
                .collect::<HashSet<_>>()
                .len(),
            1
        );
        drop(document);

        let mut document = CanvasDocument::open(&root)?;
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            replacement_ids
        );
        assert!(document.undo()?);
        assert_eq!(document.operations().len(), 1);
        assert_eq!(document.operations()[0].id, source_id);
        assert_eq!(document.operations()[0].points, source_points);
        assert!(document.redo()?);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            replacement_ids
        );

        let full_delete = document
            .commit_paint_subtraction(
                replacement_ids
                    .iter()
                    .map(|operation_id| (*operation_id, Vec::new()))
                    .collect(),
                crate::model::DEFAULT_LAYER_ID,
            )?
            .expect("full subtraction still commits a tombstone");
        assert!(full_delete.is_empty());
        assert!(document.operations().is_empty());
        assert_eq!(document.revision(), 5);
        assert!(document.undo()?);
        assert_eq!(document.operations().len(), 2);
        Ok(())
    }

    #[test]
    fn fill_subtraction_persists_grouped_fragments_and_history() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        let mut source = fill_operation(0.0, Color::rgba(15, 45, 75, 205));
        source.paint_order = Some(44);
        let old_group_id = Uuid::new_v4();
        source.fill_group_id = Some(old_group_id);
        let source_id = source.id;
        let source_points = source.points.clone();
        document.commit(source.clone())?;

        assert!(
            document
                .commit_fill_subtraction(
                    vec![(source_id, vec![source_points.clone()])],
                    crate::model::DEFAULT_LAYER_ID,
                )?
                .is_none()
        );
        assert_eq!(document.revision(), 1);
        assert_eq!(document.max_sequence(), 1);

        let fragments = split_fill_points(&source_points);
        let replacement_ids = document
            .commit_fill_subtraction(
                vec![(source_id, fragments.clone())],
                crate::model::DEFAULT_LAYER_ID,
            )?
            .expect("split Fill changes the source");
        assert_eq!(replacement_ids.len(), 2);
        assert!(!replacement_ids.contains(&source_id));
        assert_eq!(document.revision(), 2);
        assert_eq!(document.max_sequence(), 4);
        let replacement_group_ids = document
            .operations()
            .iter()
            .filter_map(|operation| operation.fill_group_id)
            .collect::<HashSet<_>>();
        assert_eq!(replacement_group_ids.len(), 1);
        assert!(!replacement_group_ids.contains(&old_group_id));
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.points.clone())
                .collect::<Vec<_>>(),
            fragments
        );
        assert!(document.operations().iter().all(|operation| {
            replacement_ids.contains(&operation.id)
                && operation.kind == EditKind::Fill
                && operation.layer_id == source.layer_id
                && operation.color == source.color
                && operation.native_depth == source.native_depth
                && operation.native_zoom == source.native_zoom
                && operation.effective_paint_order() == source.effective_paint_order()
        }));
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.transaction_id)
                .collect::<HashSet<_>>()
                .len(),
            1
        );
        drop(document);

        let mut document = CanvasDocument::open(&root)?;
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            replacement_ids
        );
        assert_eq!(
            document
                .operations()
                .iter()
                .filter_map(|operation| operation.fill_group_id)
                .collect::<HashSet<_>>()
                .len(),
            1
        );
        assert!(document.undo()?);
        assert_eq!(document.operations().len(), 1);
        assert_eq!(document.operations()[0].id, source_id);
        assert_eq!(document.operations()[0].points, source_points);
        assert_eq!(document.operations()[0].fill_group_id, Some(old_group_id));
        assert!(document.redo()?);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            replacement_ids
        );

        let max_sequence_before_delete = document.max_sequence();
        let full_delete = document
            .commit_fill_subtraction(
                replacement_ids
                    .iter()
                    .map(|operation_id| (*operation_id, Vec::new()))
                    .collect(),
                crate::model::DEFAULT_LAYER_ID,
            )?
            .expect("full subtraction still commits a tombstone");
        assert!(full_delete.is_empty());
        assert!(document.operations().is_empty());
        assert_eq!(document.max_sequence(), max_sequence_before_delete + 1);
        assert!(document.undo()?);
        assert_eq!(document.operations().len(), 2);
        Ok(())
    }

    #[test]
    fn fill_subtraction_is_atomic_across_sources_and_clears_redo_branch() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        let first = fill_operation(0.0, Color::rgba(200, 20, 30, 255));
        let second = fill_operation(0.05, Color::rgba(30, 40, 210, 255));
        let first_id = first.id;
        let second_id = second.id;
        let first_fragments = split_fill_points(&first.points);
        let second_fragment = vec![split_fill_points(&second.points)[0].clone()];
        document.commit(first)?;
        document.commit(second)?;

        let replacement_ids = document
            .commit_fill_subtraction(
                vec![
                    (first_id, first_fragments.clone()),
                    (second_id, second_fragment),
                ],
                crate::model::DEFAULT_LAYER_ID,
            )?
            .expect("both sources change");
        assert_eq!(replacement_ids.len(), 3);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.transaction_id)
                .collect::<HashSet<_>>()
                .len(),
            1
        );
        assert_eq!(
            document
                .operations()
                .iter()
                .filter_map(|operation| operation.fill_group_id)
                .collect::<HashSet<_>>()
                .len(),
            2
        );
        assert!(document.undo()?);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<HashSet<_>>(),
            HashSet::from([first_id, second_id])
        );

        let branch_ids = document
            .commit_fill_subtraction(
                vec![(first_id, first_fragments[..1].to_vec())],
                crate::model::DEFAULT_LAYER_ID,
            )?
            .expect("branch replacement changes the source");
        assert_eq!(branch_ids.len(), 1);
        assert!(!document.redo()?);
        drop(document);

        let document = CanvasDocument::open(&root)?;
        assert_eq!(document.operations().len(), 2);
        assert!(
            document
                .operations()
                .iter()
                .any(|operation| operation.id == second_id)
        );
        assert!(
            document
                .operations()
                .iter()
                .any(|operation| branch_ids.contains(&operation.id))
        );
        Ok(())
    }

    #[test]
    fn fill_subtraction_rejects_invalid_batches_without_partial_commit() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        let first = fill_operation(0.0, Color::BLACK);
        let second = fill_operation(0.05, Color::WHITE);
        let first_id = first.id;
        let second_id = second.id;
        let valid_fragments = split_fill_points(&first.points);
        let invalid_fragment = second.points[..3].to_vec();
        document.commit(first)?;
        document.commit(second)?;
        let mut paint = operation();
        let paint_id = paint.id;
        paint.points[0].local_x = 0.45;
        document.commit(paint)?;

        let original_ids = document
            .operations()
            .iter()
            .map(|operation| operation.id)
            .collect::<Vec<_>>();
        let original_revision = document.revision();
        assert!(
            document
                .commit_fill_subtraction(
                    vec![
                        (first_id, valid_fragments.clone()),
                        (Uuid::new_v4(), Vec::new()),
                    ],
                    crate::model::DEFAULT_LAYER_ID,
                )
                .is_err()
        );
        assert!(
            document
                .commit_fill_subtraction(
                    vec![
                        (first_id, valid_fragments.clone()),
                        (first_id, valid_fragments.clone()),
                    ],
                    crate::model::DEFAULT_LAYER_ID,
                )
                .is_err()
        );
        assert!(
            document
                .commit_fill_subtraction(
                    vec![
                        (first_id, valid_fragments),
                        (second_id, vec![invalid_fragment])
                    ],
                    crate::model::DEFAULT_LAYER_ID,
                )
                .is_err()
        );
        assert!(
            document
                .commit_fill_subtraction(
                    vec![(paint_id, vec![document.operations()[2].points.clone()])],
                    crate::model::DEFAULT_LAYER_ID,
                )
                .is_err()
        );
        assert_eq!(document.revision(), original_revision);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            original_ids
        );

        let other_layer = document.create_layer("Other")?;
        let revision_after_layer = document.revision();
        assert!(
            document
                .commit_fill_subtraction(
                    vec![(first_id, vec![document.operations()[0].points.clone()])],
                    other_layer.id,
                )
                .is_err()
        );
        assert_eq!(document.revision(), revision_after_layer);

        assert!(document.set_layer_locked(crate::model::DEFAULT_LAYER_ID, true)?);
        let locked_revision = document.revision();
        assert!(
            document
                .commit_fill_subtraction(
                    vec![(first_id, Vec::new())],
                    crate::model::DEFAULT_LAYER_ID,
                )
                .is_err()
        );
        assert_eq!(document.revision(), locked_revision);
        assert!(document.set_layer_locked(crate::model::DEFAULT_LAYER_ID, false)?);
        assert!(document.set_layer_visibility(crate::model::DEFAULT_LAYER_ID, false)?);
        let hidden_revision = document.revision();
        assert!(
            document
                .commit_fill_subtraction(
                    vec![(first_id, Vec::new())],
                    crate::model::DEFAULT_LAYER_ID,
                )
                .is_err()
        );
        assert_eq!(document.revision(), hidden_revision);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            original_ids
        );
        Ok(())
    }

    #[test]
    fn compact_selected_survives_reopen_and_round_trips_history() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        let first = operation();
        let first_id = first.id;
        document.commit(first)?;
        let mut second = operation();
        second.points[0].local_x = 0.6;
        second.points[1].local_x = 0.8;
        let second_id = second.id;
        document.commit(second)?;

        let block_id = document
            .compact_operations(
                &HashSet::from([first_id, second_id]),
                crate::model::DEFAULT_LAYER_ID,
            )?
            .expect("compact block");
        assert_eq!(document.operations().len(), 1);
        let block = &document.operations()[0];
        assert_eq!(block.id, block_id);
        assert!(block.is_compact_block());
        assert_eq!(block.compact_sources.len(), 2);
        assert_eq!(block.points[0].tile_x, BigInt::from(0));
        assert!(block.points[0].local_x > 0.09);
        assert!(block.points[2].local_x < 0.81);

        assert!(document.undo()?);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<HashSet<_>>(),
            HashSet::from([first_id, second_id])
        );
        assert!(document.redo()?);
        assert_eq!(document.operations().len(), 1);
        assert!(document.operations()[0].is_compact_block());
        drop(document);

        let document = CanvasDocument::open(&root)?;
        assert_eq!(document.operations().len(), 1);
        assert_eq!(document.operations()[0].id, block_id);
        assert_eq!(document.operations()[0].compact_sources.len(), 2);
        Ok(())
    }

    #[test]
    fn compact_block_move_transforms_snapshot_and_bounds() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let first = operation();
        let first_id = first.id;
        document.commit(first)?;
        let second = operation();
        let second_id = second.id;
        document.commit(second)?;
        let block_id = document
            .compact_operations(
                &HashSet::from([first_id, second_id]),
                crate::model::DEFAULT_LAYER_ID,
            )?
            .expect("compact block");
        let original_source_ids = document.operations()[0]
            .compact_sources
            .iter()
            .map(|source| source.id)
            .collect::<HashSet<_>>();

        let replacement_ids = document.move_operations(
            &HashSet::from([block_id]),
            0,
            1.0,
            512.0,
            0.0,
            crate::model::DEFAULT_LAYER_ID,
        )?;

        assert_eq!(replacement_ids.len(), 1);
        let stats = document
            .last_operation_commit_stats()
            .expect("operation commit stats");
        assert!(!stats.memory_replace_fallback);
        assert_eq!(stats.memory_replace.compact_replacement_count, 1);
        assert_eq!(stats.memory_replace.missing_render_count, 0);
        let moved = &document.operations()[0];
        assert!(moved.is_compact_block());
        assert_eq!(moved.compact_sources.len(), 2);
        assert!(
            moved
                .compact_sources
                .iter()
                .all(|source| !original_source_ids.contains(&source.id))
        );
        assert!(
            moved
                .compact_sources
                .iter()
                .flat_map(|source| source.points.iter())
                .all(|point| point.tile_x == BigInt::from(1))
        );
        assert_eq!(moved.points[0].tile_x, BigInt::from(1));
        assert_eq!(moved.points[2].tile_x, BigInt::from(1));
        Ok(())
    }

    #[test]
    fn mixed_compact_and_plain_move_uses_fast_memory_replace() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let first = operation();
        let first_id = first.id;
        document.commit(first)?;
        let second = operation();
        let second_id = second.id;
        document.commit(second)?;
        let block_id = document
            .compact_operations(
                &HashSet::from([first_id, second_id]),
                crate::model::DEFAULT_LAYER_ID,
            )?
            .expect("compact block");
        let third = operation();
        let third_id = third.id;
        document.commit(third)?;

        let replacement_ids = document.move_operations(
            &HashSet::from([block_id, third_id]),
            0,
            1.0,
            512.0,
            0.0,
            crate::model::DEFAULT_LAYER_ID,
        )?;

        assert_eq!(replacement_ids.len(), 2);
        let stats = document
            .last_operation_commit_stats()
            .expect("operation commit stats");
        assert!(!stats.memory_replace_fallback);
        assert_eq!(stats.memory_replace.replacement_count, 2);
        assert_eq!(stats.memory_replace.compact_replacement_count, 1);
        assert_eq!(stats.memory_replace.missing_operation_count, 0);
        assert_eq!(stats.memory_replace.missing_render_count, 0);
        let key = TileKey {
            depth: 0,
            x: BigInt::from(1),
            y: BigInt::from(0),
            lod: 0,
        };
        assert!(
            document
                .render_operations_for_tiles(std::slice::from_ref(&key))
                .len()
                >= 3
        );
        Ok(())
    }

    #[test]
    fn compact_block_transform_scales_snapshot_width_and_bounds() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let first = operation();
        let first_id = first.id;
        document.commit(first)?;
        let second = operation();
        let second_id = second.id;
        document.commit(second)?;
        let block_id = document
            .compact_operations(
                &HashSet::from([first_id, second_id]),
                crate::model::DEFAULT_LAYER_ID,
            )?
            .expect("compact block");
        let transform = ScreenAffine::uniform_scale(0.0, 0.0, 2.0).expect("valid scale");

        let replacement_ids = document.transform_operations(
            &HashSet::from([block_id]),
            &CameraAddress::default(),
            512.0,
            512.0,
            transform,
            2.0,
            crate::model::DEFAULT_LAYER_ID,
        )?;

        assert_eq!(replacement_ids.len(), 1);
        let scaled = &document.operations()[0];
        assert!(scaled.is_compact_block());
        assert!(
            scaled
                .compact_sources
                .iter()
                .all(|source| (source.width_px - 10.0).abs() < f32::EPSILON)
        );
        assert!(scaled.points[2].local_x > 0.6);
        assert!(scaled.points[2].local_y > 0.8);
        Ok(())
    }

    #[test]
    fn compact_selected_flattens_existing_compact_blocks() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let first = operation();
        let first_id = first.id;
        document.commit(first)?;
        let second = operation();
        let second_id = second.id;
        document.commit(second)?;
        let block_id = document
            .compact_operations(
                &HashSet::from([first_id, second_id]),
                crate::model::DEFAULT_LAYER_ID,
            )?
            .expect("compact block");
        let third = operation();
        let third_id = third.id;
        document.commit(third)?;

        let merged_id = document
            .compact_operations(
                &HashSet::from([block_id, third_id]),
                crate::model::DEFAULT_LAYER_ID,
            )?
            .expect("merged compact block");

        assert_eq!(document.operations().len(), 1);
        let merged = &document.operations()[0];
        assert_eq!(merged.id, merged_id);
        assert!(merged.is_compact_block());
        assert_eq!(merged.compact_sources.len(), 3);
        assert!(
            merged
                .compact_sources
                .iter()
                .all(|source| !source.is_compact_block())
        );
        Ok(())
    }

    #[test]
    fn compact_selected_merges_two_existing_compact_blocks() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let first = operation();
        let first_id = first.id;
        document.commit(first)?;
        let second = operation();
        let second_id = second.id;
        document.commit(second)?;
        let first_block_id = document
            .compact_operations(
                &HashSet::from([first_id, second_id]),
                crate::model::DEFAULT_LAYER_ID,
            )?
            .expect("first compact block");
        let mut third = operation();
        third.points[0].tile_x = BigInt::from(5);
        third.points[1].tile_x = BigInt::from(5);
        let third_id = third.id;
        document.commit(third)?;
        let mut fourth = operation();
        fourth.points[0].tile_x = BigInt::from(6);
        fourth.points[1].tile_x = BigInt::from(6);
        let fourth_id = fourth.id;
        document.commit(fourth)?;
        let second_block_id = document
            .compact_operations(
                &HashSet::from([third_id, fourth_id]),
                crate::model::DEFAULT_LAYER_ID,
            )?
            .expect("second compact block");

        let merged_id = document
            .compact_operations(
                &HashSet::from([first_block_id, second_block_id]),
                crate::model::DEFAULT_LAYER_ID,
            )?
            .expect("merged compact block");

        assert_eq!(document.operations().len(), 1);
        let merged = &document.operations()[0];
        assert_eq!(merged.id, merged_id);
        assert!(merged.is_compact_block());
        assert_eq!(merged.compact_sources.len(), 4);
        assert!(
            merged
                .compact_sources
                .iter()
                .all(|source| !source.is_compact_block())
        );
        Ok(())
    }

    #[test]
    fn compact_block_paste_translates_snapshot_and_bounds() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let first = operation();
        let first_id = first.id;
        document.commit(first)?;
        let second = operation();
        let second_id = second.id;
        document.commit(second)?;
        let block_id = document
            .compact_operations(
                &HashSet::from([first_id, second_id]),
                crate::model::DEFAULT_LAYER_ID,
            )?
            .expect("compact block");
        let block = document
            .operations()
            .iter()
            .find(|operation| operation.id == block_id)
            .cloned()
            .expect("active block");

        let pasted_ids = document.paste_operations(
            &[block],
            crate::model::DEFAULT_LAYER_ID,
            0,
            1.0,
            512.0,
            0.0,
        )?;

        assert_eq!(pasted_ids.len(), 1);
        let pasted = document
            .operations()
            .iter()
            .find(|operation| operation.id == pasted_ids[0])
            .expect("pasted block");
        assert!(pasted.is_compact_block());
        assert_eq!(pasted.compact_sources.len(), 2);
        assert!(
            pasted
                .compact_sources
                .iter()
                .flat_map(|source| source.points.iter())
                .all(|point| point.tile_x == BigInt::from(1))
        );
        assert_eq!(pasted.points[0].tile_x, BigInt::from(1));
        assert_eq!(pasted.points[2].tile_x, BigInt::from(1));
        Ok(())
    }

    #[test]
    fn compact_block_move_to_layer_updates_snapshot_layers() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let first = operation();
        let first_id = first.id;
        document.commit(first)?;
        let second = operation();
        let second_id = second.id;
        document.commit(second)?;
        let block_id = document
            .compact_operations(
                &HashSet::from([first_id, second_id]),
                crate::model::DEFAULT_LAYER_ID,
            )?
            .expect("compact block");
        let original_source_ids = document.operations()[0]
            .compact_sources
            .iter()
            .map(|source| source.id)
            .collect::<HashSet<_>>();
        let target_layer = document.create_layer("Target")?;

        let replacement_ids = document.move_operations_to_layer(
            &HashSet::from([block_id]),
            crate::model::DEFAULT_LAYER_ID,
            target_layer.id,
        )?;

        assert_eq!(replacement_ids.len(), 1);
        let moved = document
            .operations()
            .iter()
            .find(|operation| operation.id == replacement_ids[0])
            .expect("moved block");
        assert!(moved.is_compact_block());
        assert_eq!(moved.layer_id, target_layer.id);
        assert_eq!(
            moved
                .compact_sources
                .iter()
                .map(|source| source.id)
                .collect::<HashSet<_>>(),
            original_source_ids
        );
        assert!(
            moved
                .compact_sources
                .iter()
                .all(|source| source.layer_id == target_layer.id)
        );
        Ok(())
    }

    #[test]
    fn compact_selected_allows_disjoint_non_contiguous_objects() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let first = operation();
        let first_id = first.id;
        document.commit(first)?;
        let mut middle = operation();
        middle.points[0].tile_x = BigInt::from(5);
        middle.points[1].tile_x = BigInt::from(5);
        let middle_id = middle.id;
        document.commit(middle)?;
        let mut third = operation();
        third.points[0].tile_x = BigInt::from(10);
        third.points[1].tile_x = BigInt::from(10);
        let third_id = third.id;
        document.commit(third)?;

        let block_id = document
            .compact_operations(
                &HashSet::from([first_id, third_id]),
                crate::model::DEFAULT_LAYER_ID,
            )?
            .expect("compact block");

        assert_eq!(document.operations().len(), 2);
        let block = document
            .operations()
            .iter()
            .find(|operation| operation.id == block_id)
            .expect("compact block remains active");
        assert!(block.is_compact_block());
        assert_eq!(block.compact_sources.len(), 2);
        assert_eq!(block.points[0].tile_x, BigInt::from(0));
        assert_eq!(block.points[2].tile_x, BigInt::from(10));
        assert!(
            document
                .operations()
                .iter()
                .any(|operation| operation.id == middle_id)
        );
        Ok(())
    }

    #[test]
    fn compact_selected_allows_mixed_geometry_depths() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let coarse = operation();
        let coarse_id = coarse.id;
        document.commit(coarse)?;
        let mut fine = operation();
        fine.native_depth = 1;
        fine.points = vec![
            CanvasPoint::new(1, BigInt::from(16), BigInt::from(0), 0.1, 0.2),
            CanvasPoint::new(1, BigInt::from(16), BigInt::from(0), 0.3, 0.4),
        ];
        let fine_id = fine.id;
        document.commit(fine)?;

        let block_id = document
            .compact_operations(
                &HashSet::from([coarse_id, fine_id]),
                crate::model::DEFAULT_LAYER_ID,
            )?
            .expect("compact block");

        assert_eq!(document.operations().len(), 1);
        let block = &document.operations()[0];
        assert_eq!(block.id, block_id);
        assert!(block.is_compact_block());
        assert_eq!(block.native_depth, 0);
        assert_eq!(block.compact_sources.len(), 2);
        assert!(block.compact_sources.iter().any(|source| {
            source
                .points
                .iter()
                .all(|point| point.depth == 1 && point.tile_x == BigInt::from(16))
        }));
        assert_eq!(block.points[0].depth, 0);
        assert_eq!(block.points[2].depth, 0);
        assert_eq!(block.points[2].tile_x, BigInt::from(2));
        Ok(())
    }

    #[test]
    fn compact_older_operations_keeps_latest_objects_editable() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let mut ids = Vec::new();
        for index in 0..5 {
            let mut source = operation();
            source.points[0].tile_x = BigInt::from(index);
            source.points[1].tile_x = BigInt::from(index);
            ids.push(source.id);
            document.commit(source)?;
        }

        let compacted = document.compact_older_operations(2)?;

        assert_eq!(compacted, 1);
        assert_eq!(document.operations().len(), 3);
        assert!(
            document
                .operations()
                .iter()
                .any(|operation| operation.id == ids[3])
        );
        assert!(
            document
                .operations()
                .iter()
                .any(|operation| operation.id == ids[4])
        );
        let block = document
            .operations()
            .iter()
            .find(|operation| operation.is_compact_block())
            .expect("older objects compacted");
        assert_eq!(block.compact_sources.len(), 3);
        assert!(
            block
                .compact_sources
                .iter()
                .all(|source| ids[..3].contains(&source.id))
        );
        Ok(())
    }

    #[test]
    fn compact_older_operations_splits_unrepresentable_depth_ranges() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        for depth in [0, 0, 1_000, 1_000, 2_000, 2_000, 3_000] {
            let mut source = operation();
            source.native_depth = depth;
            source.points = vec![
                CanvasPoint::new(depth, BigInt::from(0), BigInt::from(0), 0.1, 0.2),
                CanvasPoint::new(depth, BigInt::from(0), BigInt::from(0), 0.3, 0.4),
            ];
            document.commit(source)?;
        }

        let compacted = document.compact_older_operations(1)?;

        assert_eq!(compacted, 3);
        assert_eq!(document.operations().len(), 4);
        assert_eq!(
            document
                .operations()
                .iter()
                .filter(|operation| operation.is_compact_block())
                .count(),
            3
        );
        assert!(
            document
                .operations()
                .iter()
                .filter(|operation| operation.is_compact_block())
                .all(|operation| operation.compact_sources.len() == 2)
        );
        assert!(
            document
                .operations()
                .iter()
                .any(|operation| !operation.is_compact_block() && operation.native_depth == 3_000)
        );
        Ok(())
    }

    #[test]
    fn compact_older_operations_preserves_interleaved_depth_order() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let mut ids_by_depth = Vec::new();
        for depth in [0, 1, 0, 1, 0] {
            let mut source = operation();
            source.native_depth = depth;
            source.points = vec![
                CanvasPoint::new(depth, BigInt::from(0), BigInt::from(0), 0.1, 0.2),
                CanvasPoint::new(depth, BigInt::from(0), BigInt::from(0), 0.3, 0.4),
            ];
            ids_by_depth.push((source.id, depth));
            document.commit(source)?;
        }

        let compacted = document.compact_older_operations(1)?;

        assert_eq!(compacted, 0);
        assert_eq!(document.operations().len(), 5);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| (operation.id, operation.native_depth))
                .collect::<Vec<_>>(),
            ids_by_depth
        );
        Ok(())
    }

    #[test]
    fn render_operations_expand_compact_blocks_into_global_paint_order() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let bottom = operation();
        let bottom_id = bottom.id;
        document.commit(bottom)?;
        let middle = operation();
        let middle_id = middle.id;
        document.commit(middle)?;
        let top = operation();
        let top_id = top.id;
        document.commit(top)?;

        document.compact_operations(
            &HashSet::from([bottom_id, top_id]),
            crate::model::DEFAULT_LAYER_ID,
        )?;
        let key = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: 0,
        };

        let indices = document.render_operation_indices_for_tiles(std::slice::from_ref(&key));
        let first_lookup = document.render_operations_by_indices(&indices);
        let second_lookup = document.render_operations_by_indices(&indices);
        let render_ids = first_lookup
            .iter()
            .map(|operation| operation.id)
            .collect::<Vec<_>>();

        assert_eq!(render_ids, vec![bottom_id, middle_id, top_id]);
        assert!(
            first_lookup
                .iter()
                .zip(&second_lookup)
                .all(|(first, second)| Arc::ptr_eq(first, second))
        );
        assert_eq!(document.operations().len(), 2);
        assert!(
            document
                .operations()
                .iter()
                .any(|operation| operation.is_compact_block())
        );
        Ok(())
    }

    #[test]
    fn selected_move_is_one_persisted_history_transaction() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        let first = operation();
        let first_id = first.id;
        document.commit(first)?;
        let second = operation();
        let second_id = second.id;
        document.commit(second)?;

        let replacement_ids = document.move_operations(
            &HashSet::from([first_id, second_id]),
            0,
            1.0,
            512.0,
            -256.0,
            crate::model::DEFAULT_LAYER_ID,
        )?;
        assert_eq!(replacement_ids.len(), 2);
        assert_eq!(document.max_sequence(), 5);
        assert_eq!(document.revision(), 3);
        assert!(
            document
                .operations()
                .iter()
                .all(|operation| replacement_ids.contains(&operation.id))
        );
        assert!(document.operations().iter().all(|operation| {
            operation.points[0].tile_x == BigInt::from(1)
                && operation.points[0].tile_y == BigInt::from(-1)
                && (operation.points[0].local_y - 0.7).abs() < 1.0e-12
        }));
        let transaction_ids: HashSet<_> = document
            .operations()
            .iter()
            .map(|operation| operation.transaction_id)
            .collect();
        assert_eq!(transaction_ids.len(), 1);
        drop(document);

        let mut document = CanvasDocument::open(&root)?;
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<HashSet<_>>(),
            replacement_ids.iter().copied().collect()
        );
        assert!(document.undo()?);
        assert_eq!(document.revision(), 4);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![first_id, second_id]
        );
        assert!(document.redo()?);
        assert_eq!(document.revision(), 5);
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
    fn selected_scale_is_one_persisted_history_transaction() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        let source = operation();
        let source_id = source.id;
        let source_points = source.points.clone();
        document.commit(source)?;
        let camera = CameraAddress::default();
        let before: Vec<_> = source_points
            .iter()
            .map(|point| {
                camera
                    .canvas_to_screen(point, 512.0, 512.0)
                    .expect("source projects")
            })
            .collect();
        let transform = ScreenAffine::uniform_scale(256.0, 256.0, 2.0).expect("valid scale");

        let replacement_ids = document.transform_operations(
            &HashSet::from([source_id]),
            &camera,
            512.0,
            512.0,
            transform,
            2.0,
            crate::model::DEFAULT_LAYER_ID,
        )?;

        assert_eq!(replacement_ids.len(), 1);
        assert_eq!(document.max_sequence(), 3);
        assert_eq!(document.revision(), 2);
        let replacement = &document.operations()[0];
        assert_eq!(replacement.id, replacement_ids[0]);
        assert_eq!(replacement.width_px, 10.0);
        assert_eq!(replacement.effective_paint_order(), 1);
        for (point, before) in replacement.points.iter().zip(before) {
            let after = camera
                .canvas_to_screen(point, 512.0, 512.0)
                .expect("replacement projects");
            assert!((after.0 - (256.0 + (before.0 - 256.0) * 2.0)).abs() < 0.001);
            assert!((after.1 - (256.0 + (before.1 - 256.0) * 2.0)).abs() < 0.001);
        }
        drop(document);

        let mut document = CanvasDocument::open(&root)?;
        assert_eq!(document.operations()[0].id, replacement_ids[0]);
        assert_eq!(document.operations()[0].width_px, 10.0);
        assert!(document.undo()?);
        assert_eq!(document.operations()[0].id, source_id);
        assert_eq!(document.operations()[0].width_px, 5.0);
        assert!(document.redo()?);
        assert_eq!(document.operations()[0].id, replacement_ids[0]);
        assert_eq!(document.operations()[0].width_px, 10.0);
        Ok(())
    }

    #[test]
    fn selected_rotate_is_one_persisted_history_transaction() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        let source = operation();
        let source_id = source.id;
        let source_points = source.points.clone();
        document.commit(source)?;
        let camera = CameraAddress::default();
        let before: Vec<_> = source_points
            .iter()
            .map(|point| {
                camera
                    .canvas_to_screen(point, 512.0, 512.0)
                    .expect("source projects")
            })
            .collect();
        let transform = ScreenAffine::rotation(256.0, 256.0, std::f64::consts::FRAC_PI_2)
            .expect("valid rotate");

        let replacement_ids = document.transform_operations(
            &HashSet::from([source_id]),
            &camera,
            512.0,
            512.0,
            transform,
            1.0,
            crate::model::DEFAULT_LAYER_ID,
        )?;

        assert_eq!(replacement_ids.len(), 1);
        assert_eq!(document.max_sequence(), 3);
        assert_eq!(document.revision(), 2);
        let replacement = &document.operations()[0];
        assert_eq!(replacement.id, replacement_ids[0]);
        assert_eq!(replacement.width_px, 5.0);
        for (point, before) in replacement.points.iter().zip(before) {
            let after = camera
                .canvas_to_screen(point, 512.0, 512.0)
                .expect("replacement projects");
            assert!((after.0 - (256.0 - (before.1 - 256.0))).abs() < 0.001);
            assert!((after.1 - (256.0 + (before.0 - 256.0))).abs() < 0.001);
        }
        drop(document);

        let mut document = CanvasDocument::open(&root)?;
        assert_eq!(document.operations()[0].id, replacement_ids[0]);
        assert!(document.undo()?);
        assert_eq!(document.operations()[0].id, source_id);
        assert!(document.redo()?);
        assert_eq!(document.operations()[0].id, replacement_ids[0]);
        Ok(())
    }

    #[test]
    fn selected_flip_mirrors_screen_geometry_without_scaling_width() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let source = operation();
        let source_id = source.id;
        let source_points = source.points.clone();
        document.commit(source)?;
        let camera = CameraAddress::default();
        let before: Vec<_> = source_points
            .iter()
            .map(|point| {
                camera
                    .canvas_to_screen(point, 512.0, 512.0)
                    .expect("source projects")
            })
            .collect();

        let horizontal = ScreenAffine::flip_horizontal(256.0, 256.0).expect("valid flip");
        let replacement_ids = document.transform_operations(
            &HashSet::from([source_id]),
            &camera,
            512.0,
            512.0,
            horizontal,
            1.0,
            crate::model::DEFAULT_LAYER_ID,
        )?;
        assert_eq!(document.operations()[0].width_px, 5.0);
        for (point, before) in document.operations()[0].points.iter().zip(&before) {
            let after = camera
                .canvas_to_screen(point, 512.0, 512.0)
                .expect("replacement projects");
            assert!((after.0 - (512.0 - before.0)).abs() < 0.001);
            assert!((after.1 - before.1).abs() < 0.001);
        }

        assert!(document.undo()?);
        let vertical = ScreenAffine::flip_vertical(256.0, 256.0).expect("valid flip");
        document.transform_operations(
            &HashSet::from([source_id]),
            &camera,
            512.0,
            512.0,
            vertical,
            1.0,
            crate::model::DEFAULT_LAYER_ID,
        )?;
        assert!(
            !document
                .operations()
                .iter()
                .any(|operation| { replacement_ids.contains(&operation.id) })
        );
        assert_eq!(document.operations()[0].width_px, 5.0);
        for (point, before) in document.operations()[0].points.iter().zip(&before) {
            let after = camera
                .canvas_to_screen(point, 512.0, 512.0)
                .expect("replacement projects");
            assert!((after.0 - before.0).abs() < 0.001);
            assert!((after.1 - (512.0 - before.1)).abs() < 0.001);
        }
        Ok(())
    }

    #[test]
    fn selected_recolor_replaces_only_paint_and_fill() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        let old_color = Color::rgba(10, 20, 30, 128);
        let new_color = Color::rgba(200, 40, 90, 160);
        let mut selected = HashSet::new();
        let mut original_ids = Vec::new();
        for kind in [
            EditKind::Paint,
            EditKind::Fill,
            EditKind::Erase,
            EditKind::EraseArea,
        ] {
            let operation = EditOperation::draft(kind, 0, 1.0, operation().points, old_color, 7.0);
            selected.insert(operation.id);
            original_ids.push((kind, operation.id, operation.points.clone()));
            document.commit(operation)?;
        }
        let original_orders = document
            .operations()
            .iter()
            .map(|operation| (operation.id, operation.effective_paint_order()))
            .collect::<HashMap<_, _>>();

        let replacements =
            document.recolor_operations(&selected, crate::model::DEFAULT_LAYER_ID, new_color)?;

        assert_eq!(replacements.len(), 2);
        assert_eq!(document.revision(), 5);
        assert_eq!(document.max_sequence(), 7);
        let replacement_map = replacements.iter().copied().collect::<HashMap<_, _>>();
        for (kind, original_id, original_points) in &original_ids {
            let expected_id = replacement_map
                .get(original_id)
                .copied()
                .unwrap_or(*original_id);
            let operation = document
                .operations()
                .iter()
                .find(|operation| operation.id == expected_id)
                .expect("operation remains active");
            assert_eq!(operation.kind, *kind);
            assert_eq!(&operation.points, original_points);
            assert_eq!(operation.width_px, 7.0);
            assert_eq!(operation.layer_id, crate::model::DEFAULT_LAYER_ID);
            match kind {
                EditKind::Paint | EditKind::Fill => {
                    assert_ne!(operation.id, *original_id);
                    assert_eq!(operation.color, Color::rgba(200, 40, 90, 255));
                    assert_eq!(
                        operation.effective_paint_order(),
                        original_orders[original_id]
                    );
                    assert!(operation.destructive);
                }
                EditKind::Erase | EditKind::EraseArea => {
                    assert_eq!(operation.id, *original_id);
                    assert_eq!(operation.color, old_color);
                }
                EditKind::CompactBlock => unreachable!("test does not create compact blocks"),
            }
        }
        drop(document);

        let mut document = CanvasDocument::open(&root)?;
        assert!(replacements.iter().all(|(_, replacement_id)| {
            document
                .operations()
                .iter()
                .any(|operation| operation.id == *replacement_id)
        }));
        assert!(document.undo()?);
        assert!(original_ids.iter().all(|(_, original_id, _)| {
            document
                .operations()
                .iter()
                .any(|operation| operation.id == *original_id)
        }));
        assert!(document.redo()?);
        assert!(replacements.iter().all(|(_, replacement_id)| {
            document
                .operations()
                .iter()
                .any(|operation| operation.id == *replacement_id)
        }));
        Ok(())
    }

    #[test]
    fn selected_recolor_rejects_invalid_selection_without_partial_commit() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let source = operation();
        let source_id = source.id;
        document.commit(source)?;
        let other_layer = document.create_layer("Other")?;
        let mut other = operation();
        other.layer_id = other_layer.id;
        let other_id = other.id;
        document.commit(other)?;
        let color = Color::rgba(200, 40, 90, 255);

        let error = document
            .recolor_operations(
                &HashSet::from([source_id, other_id]),
                crate::model::DEFAULT_LAYER_ID,
                color,
            )
            .expect_err("mixed layer selection is rejected");
        assert!(error.to_string().contains("unavailable"));
        assert_eq!(document.revision(), 2);
        assert_eq!(document.operations()[0].id, source_id);

        let error = document
            .recolor_operations(
                &HashSet::from([source_id, Uuid::new_v4()]),
                crate::model::DEFAULT_LAYER_ID,
                color,
            )
            .expect_err("stale selection is rejected");
        assert!(error.to_string().contains("unavailable"));
        assert_eq!(document.revision(), 2);

        assert!(document.set_layer_locked(crate::model::DEFAULT_LAYER_ID, true)?);
        let error = document
            .recolor_operations(
                &HashSet::from([source_id]),
                crate::model::DEFAULT_LAYER_ID,
                color,
            )
            .expect_err("locked layer is rejected");
        assert!(error.to_string().contains("hidden or locked"));
        assert_eq!(document.revision(), 2);
        assert_eq!(document.operations()[0].id, source_id);
        Ok(())
    }

    #[test]
    fn selected_recolor_ignores_erase_only_selection_without_history() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let erase = EditOperation::draft(
            EditKind::Erase,
            0,
            1.0,
            operation().points,
            Color::WHITE,
            7.0,
        );
        let erase_id = erase.id;
        document.commit(erase)?;

        let replacements = document.recolor_operations(
            &HashSet::from([erase_id]),
            crate::model::DEFAULT_LAYER_ID,
            Color::rgba(200, 40, 90, 255),
        )?;

        assert!(replacements.is_empty());
        assert_eq!(document.revision(), 1);
        assert_eq!(document.operations()[0].id, erase_id);
        Ok(())
    }

    #[test]
    fn selected_scale_preserves_area_geometry_and_style() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let points = vec![
            CanvasPoint::new(0, 0.into(), 0.into(), 0.1, 0.1),
            CanvasPoint::new(0, 0.into(), 0.into(), 0.4, 0.1),
            CanvasPoint::new(0, 0.into(), 0.into(), 0.4, 0.4),
            CanvasPoint::new(0, 0.into(), 0.into(), 0.1, 0.1),
        ];
        let color = Color::rgba(10, 80, 160, 255);
        let mut source_ids = HashSet::new();
        for kind in [EditKind::Fill, EditKind::EraseArea] {
            let operation = EditOperation::draft(kind, 0, 1.0, points.clone(), color, 6.0);
            source_ids.insert(operation.id);
            document.commit(operation)?;
        }
        let transform = ScreenAffine::uniform_scale(256.0, 256.0, 1.5).expect("valid scale");

        let replacement_ids = document.transform_operations(
            &source_ids,
            &CameraAddress::default(),
            512.0,
            512.0,
            transform,
            1.5,
            crate::model::DEFAULT_LAYER_ID,
        )?;

        assert_eq!(replacement_ids.len(), 2);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.kind)
                .collect::<Vec<_>>(),
            vec![EditKind::Fill, EditKind::EraseArea]
        );
        for operation in document.operations() {
            assert_eq!(operation.layer_id, crate::model::DEFAULT_LAYER_ID);
            assert_eq!(operation.color, color);
            assert_eq!(operation.width_px, 9.0);
            assert!(operation.destructive);
            assert_eq!(operation.points.first(), operation.points.last());
        }
        Ok(())
    }

    #[test]
    fn selected_scale_rejects_locked_or_mixed_layer_selection() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let source = operation();
        let source_id = source.id;
        document.commit(source)?;
        let other_layer = document.create_layer("Other")?;
        let mut other = operation();
        other.layer_id = other_layer.id;
        let other_id = other.id;
        document.commit(other)?;
        let transform = ScreenAffine::uniform_scale(256.0, 256.0, 2.0).expect("valid scale");
        let revision_before = document.revision();
        let ids_before = document
            .operations()
            .iter()
            .map(|operation| operation.id)
            .collect::<Vec<_>>();

        let error = document
            .transform_operations(
                &HashSet::from([source_id, other_id]),
                &CameraAddress::default(),
                512.0,
                512.0,
                transform,
                2.0,
                crate::model::DEFAULT_LAYER_ID,
            )
            .expect_err("mixed-layer selection must be rejected");
        assert!(error.to_string().contains("unavailable operations"));
        assert_eq!(document.revision(), revision_before);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            ids_before
        );

        assert!(document.set_layer_locked(crate::model::DEFAULT_LAYER_ID, true)?);
        let revision_before = document.revision();
        let error = document
            .transform_operations(
                &HashSet::from([source_id]),
                &CameraAddress::default(),
                512.0,
                512.0,
                transform,
                2.0,
                crate::model::DEFAULT_LAYER_ID,
            )
            .expect_err("locked layer must be rejected");
        assert!(error.to_string().contains("hidden or locked"));
        assert_eq!(document.revision(), revision_before);
        Ok(())
    }

    #[test]
    fn selected_scale_failure_does_not_commit_partial_history() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let mut source = operation();
        source.native_depth = 1_000;
        source.points = vec![
            CanvasPoint::new(1_000, 0.into(), 0.into(), 0.1, 0.2),
            CanvasPoint::new(1_000, 0.into(), 0.into(), 0.3, 0.4),
        ];
        let source_id = source.id;
        document.commit(source)?;
        let revision_before = document.revision();
        let max_sequence_before = document.max_sequence();
        let transform = ScreenAffine::uniform_scale(256.0, 256.0, 2.0).expect("valid scale");

        document
            .transform_operations(
                &HashSet::from([source_id]),
                &CameraAddress::default(),
                512.0,
                512.0,
                transform,
                2.0,
                crate::model::DEFAULT_LAYER_ID,
            )
            .expect_err("unrepresentable transform must fail before commit");

        assert_eq!(document.revision(), revision_before);
        assert_eq!(document.max_sequence(), max_sequence_before);
        assert_eq!(document.operations().len(), 1);
        assert_eq!(document.operations()[0].id, source_id);
        Ok(())
    }

    #[test]
    fn selected_move_preserves_paint_order_through_history_and_reopen() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        let bottom = operation();
        let bottom_id = bottom.id;
        document.commit(bottom)?;
        let middle = operation();
        let middle_id = middle.id;
        document.commit(middle)?;
        let top = operation();
        let top_id = top.id;
        document.commit(top)?;

        let replacement_id = document.move_operations(
            &HashSet::from([middle_id]),
            0,
            1.0,
            16.0,
            0.0,
            crate::model::DEFAULT_LAYER_ID,
        )?[0];
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![bottom_id, replacement_id, top_id]
        );
        assert_eq!(document.operations()[1].effective_paint_order(), 2);
        assert!(document.operations()[1].sequence > document.operations()[2].sequence);
        drop(document);

        let mut document = CanvasDocument::open(&root)?;
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![bottom_id, replacement_id, top_id]
        );
        assert!(document.undo()?);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![bottom_id, middle_id, top_id]
        );
        assert!(document.redo()?);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![bottom_id, replacement_id, top_id]
        );
        Ok(())
    }

    #[test]
    fn erase_area_survives_reopen_and_round_trips_history() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        document.commit(operation())?;
        let erase = EditOperation::draft(
            EditKind::EraseArea,
            0,
            1.0,
            vec![
                CanvasPoint::new(0, 0.into(), 0.into(), 0.2, 0.2),
                CanvasPoint::new(0, 0.into(), 0.into(), 0.8, 0.2),
                CanvasPoint::new(0, 0.into(), 0.into(), 0.5, 0.8),
                CanvasPoint::new(0, 0.into(), 0.into(), 0.2, 0.2),
            ],
            Color::WHITE,
            0.0,
        );
        let erase_id = erase.id;
        document.commit(erase)?;
        drop(document);

        let mut document = CanvasDocument::open(&root)?;
        assert_eq!(document.operations().len(), 2);
        assert_eq!(document.operations()[1].id, erase_id);
        assert_eq!(document.operations()[1].kind, EditKind::EraseArea);
        assert!(document.undo()?);
        assert_eq!(document.operations().len(), 1);
        assert!(document.redo()?);
        assert_eq!(document.operations().len(), 2);
        assert_eq!(document.operations()[1].id, erase_id);
        assert_eq!(document.operations()[1].kind, EditKind::EraseArea);
        Ok(())
    }

    #[test]
    fn layer_create_rename_and_reorder_persist_and_control_render_order() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        let bottom = operation();
        let bottom_id = bottom.id;
        document.commit(bottom)?;
        let top_layer = document.create_layer("Ink")?;
        let mut top = operation();
        top.layer_id = top_layer.id;
        let top_id = top.id;
        document.commit(top)?;
        let key = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: 0,
        };

        assert_eq!(
            document
                .operations_for_tile(&key)
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![bottom_id, top_id]
        );
        assert!(document.rename_layer(top_layer.id, "Line art")?);
        let revision_before_reorder = document.revision();
        assert!(document.move_layer(top_layer.id, -1)?);
        assert_eq!(document.revision(), revision_before_reorder + 1);
        assert_eq!(
            document
                .operations_for_tile(&key)
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![top_id, bottom_id]
        );
        drop(document);

        let document = CanvasDocument::open(&root)?;
        assert_eq!(document.layers()[0].id, top_layer.id);
        assert_eq!(document.layers()[0].name, "Line art");
        assert_eq!(
            document
                .operations_for_tile(&key)
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![top_id, bottom_id]
        );
        Ok(())
    }

    #[test]
    fn hidden_and_locked_layers_are_not_rendered_or_editable() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        let layer = document.create_layer("Ink")?;
        let mut paint = operation();
        paint.layer_id = layer.id;
        document.commit(paint)?;
        let key = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: 0,
        };

        assert!(document.layer_is_editable(layer.id));
        assert_eq!(document.operations_for_tile(&key).len(), 1);
        assert!(document.set_layer_locked(layer.id, true)?);
        assert!(!document.layer_is_editable(layer.id));
        assert_eq!(document.operations_for_tile(&key).len(), 1);
        assert!(document.set_layer_locked(layer.id, false)?);
        assert!(document.set_layer_visibility(layer.id, false)?);
        assert!(!document.layer_is_editable(layer.id));
        assert!(document.operations_for_tile(&key).is_empty());
        assert!(
            document
                .render_operation_indices_for_tiles(std::slice::from_ref(&key))
                .is_empty()
        );

        assert!(document.undo()?);
        assert!(document.layers()[1].visible);
        assert_eq!(document.operations_for_tile(&key).len(), 1);
        Ok(())
    }

    #[test]
    fn duplicate_and_delete_layer_round_trip_through_reopen_and_history() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        let source_layer = document.create_layer("Ink")?;
        let mut source = operation();
        source.layer_id = source_layer.id;
        let source_id = source.id;
        document.commit(source)?;

        let (duplicate, copied_count) = document
            .duplicate_layer(source_layer.id)?
            .expect("source layer");
        assert_eq!(copied_count, 1);
        let copied = document
            .operations()
            .iter()
            .find(|operation| operation.layer_id == duplicate.id)
            .expect("copied operation");
        assert_ne!(copied.id, source_id);
        assert_eq!(
            document
                .layers()
                .iter()
                .map(|layer| layer.id)
                .collect::<Vec<_>>(),
            vec![
                crate::model::DEFAULT_LAYER_ID,
                source_layer.id,
                duplicate.id
            ]
        );

        assert!(document.delete_layer(duplicate.id)?);
        assert_eq!(document.layers().len(), 2);
        drop(document);

        let mut document = CanvasDocument::open(&root)?;
        assert_eq!(document.layers().len(), 2);
        assert!(document.undo()?);
        assert_eq!(document.layers().len(), 3);
        assert_eq!(
            document
                .operations()
                .iter()
                .filter(|operation| operation.layer_id == duplicate.id)
                .count(),
            1
        );
        assert!(document.undo()?);
        assert_eq!(document.layers().len(), 2);
        assert_eq!(document.operations().len(), 1);
        assert!(document.redo()?);
        assert_eq!(document.layers().len(), 3);
        assert_eq!(document.operations().len(), 2);
        Ok(())
    }

    #[test]
    fn merge_down_preserves_visual_order_and_round_trips_history() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        let destination_id = crate::model::DEFAULT_LAYER_ID;
        let mut destination_operation = operation();
        destination_operation.width_px = 2.0;
        let destination_operation_id = destination_operation.id;
        document.commit(destination_operation)?;
        let source_layer = document.create_layer("Source")?;
        let mut first = operation();
        first.layer_id = source_layer.id;
        first.width_px = 4.0;
        let first_id = first.id;
        let first_points = first.points.clone();
        document.commit(first)?;
        let mut second = operation();
        second.layer_id = source_layer.id;
        second.width_px = 8.0;
        let second_id = second.id;
        let second_points = second.points.clone();
        document.commit(second)?;

        let (destination, merged_count) = document
            .merge_layer_down(source_layer.id)?
            .expect("lower destination");
        assert_eq!(destination.id, destination_id);
        assert_eq!(merged_count, 2);
        assert_eq!(document.layers(), &[Layer::default_layer()]);
        let merged_ids = document
            .operations()
            .iter()
            .map(|operation| operation.id)
            .collect::<Vec<_>>();
        let replacement_ids = merged_ids[1..].to_vec();
        assert_eq!(merged_ids[0], destination_operation_id);
        assert!(!merged_ids.contains(&first_id));
        assert!(!merged_ids.contains(&second_id));
        assert_eq!(document.operations()[1].points, first_points);
        assert_eq!(document.operations()[1].width_px, 4.0);
        assert_eq!(document.operations()[2].points, second_points);
        assert_eq!(document.operations()[2].width_px, 8.0);
        drop(document);

        let mut document = CanvasDocument::open(&root)?;
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            merged_ids
        );
        assert!(document.undo()?);
        assert_eq!(
            document
                .layers()
                .iter()
                .map(|layer| layer.id)
                .collect::<Vec<_>>(),
            vec![destination_id, source_layer.id]
        );
        assert_eq!(
            document
                .operations()
                .iter()
                .filter(|operation| operation.layer_id == source_layer.id)
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![first_id, second_id]
        );
        assert!(document.redo()?);
        assert_eq!(document.layers(), &[Layer::default_layer()]);
        assert_eq!(
            document
                .operations()
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            merged_ids
        );
        assert!(document.undo()?);
        let mut branch_operation = operation();
        branch_operation.layer_id = source_layer.id;
        document.commit(branch_operation)?;
        assert!(!document.redo()?);
        drop(document);

        let document = CanvasDocument::open(&root)?;
        assert_eq!(document.layers().len(), 2);
        assert!(
            document
                .operations()
                .iter()
                .all(|operation| !replacement_ids.contains(&operation.id))
        );
        Ok(())
    }

    #[test]
    fn merge_down_keeps_compact_sources_visible_after_reopen_and_history() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        let destination_id = crate::model::DEFAULT_LAYER_ID;
        let source_layer = document.create_layer("Compacted source")?;
        let mut first = operation();
        first.layer_id = source_layer.id;
        let first_id = first.id;
        document.commit(first)?;
        let mut second = operation();
        second.layer_id = source_layer.id;
        second.points[0].local_y = 0.6;
        second.points[1].local_y = 0.8;
        let second_id = second.id;
        document.commit(second)?;
        document
            .compact_operations(&HashSet::from([first_id, second_id]), source_layer.id)?
            .expect("compact block");
        let key = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: 0,
        };
        let visible_source_ids = vec![first_id, second_id];
        assert_eq!(
            document
                .operations_for_tile(&key)
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            visible_source_ids
        );

        let (_, merged_count) = document
            .merge_layer_down(source_layer.id)?
            .expect("lower destination");
        assert_eq!(merged_count, 1);
        let merged = &document.operations()[0];
        assert!(merged.is_compact_block());
        assert_eq!(merged.layer_id, destination_id);
        assert!(
            merged
                .compact_sources
                .iter()
                .all(|source| source.layer_id == destination_id)
        );
        assert_eq!(
            document
                .operations_for_tile(&key)
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            visible_source_ids
        );
        drop(document);

        let mut document = CanvasDocument::open(&root)?;
        assert_eq!(
            document
                .operations_for_tile(&key)
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            visible_source_ids
        );
        assert!(document.undo()?);
        assert_eq!(document.layers().len(), 2);
        assert_eq!(
            document
                .operations_for_tile(&key)
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            visible_source_ids
        );
        assert!(document.redo()?);
        assert_eq!(document.layers(), &[Layer::default_layer()]);
        assert_eq!(
            document
                .operations_for_tile(&key)
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            visible_source_ids
        );
        Ok(())
    }

    #[test]
    fn merge_down_handles_empty_and_rejects_invalid_layer_states() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        assert!(
            document
                .merge_layer_down(crate::model::DEFAULT_LAYER_ID)?
                .is_none()
        );

        let locked_source = document.create_layer("Locked source")?;
        assert!(document.set_layer_locked(locked_source.id, true)?);
        assert!(document.merge_layer_down(locked_source.id).is_err());
        assert!(document.set_layer_locked(locked_source.id, false)?);
        assert!(document.set_layer_visibility(crate::model::DEFAULT_LAYER_ID, false)?);
        assert!(document.merge_layer_down(locked_source.id).is_err());
        assert!(document.set_layer_visibility(crate::model::DEFAULT_LAYER_ID, true)?);

        let (_, merged_count) = document
            .merge_layer_down(locked_source.id)?
            .expect("empty source can merge");
        assert_eq!(merged_count, 0);
        assert_eq!(document.layers(), &[Layer::default_layer()]);
        assert!(document.undo()?);
        assert_eq!(document.layers().len(), 2);
        Ok(())
    }

    #[test]
    fn paste_creates_one_ordered_cross_layer_history_transaction() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        let mut first = operation();
        first.width_px = 3.0;
        let first_id = first.id;
        document.commit(first.clone())?;
        let mut second = operation();
        second.width_px = 7.0;
        let second_id = second.id;
        document.commit(second)?;
        let first = document
            .operations()
            .iter()
            .find(|operation| operation.id == first_id)
            .expect("committed first operation")
            .clone();
        let second = document
            .operations()
            .iter()
            .find(|operation| operation.id == second_id)
            .expect("committed second operation")
            .clone();
        let target = document.create_layer("Paste target")?;

        let pasted_ids = document.paste_operations(
            &[second.clone(), first.clone()],
            target.id,
            0,
            1.0,
            16.0,
            16.0,
        )?;
        assert_eq!(pasted_ids.len(), 2);
        assert!(!pasted_ids.contains(&first_id));
        assert!(!pasted_ids.contains(&second_id));
        let pasted = document
            .operations()
            .iter()
            .filter(|operation| operation.layer_id == target.id)
            .collect::<Vec<_>>();
        assert_eq!(pasted.len(), 2);
        assert_eq!(
            pasted
                .iter()
                .map(|operation| operation.width_px)
                .collect::<Vec<_>>(),
            vec![3.0, 7.0]
        );
        assert_eq!(pasted[0].transaction_id, pasted[1].transaction_id);
        assert_eq!(
            pasted[0].points[0],
            first.points[0]
                .translated_by_screen_delta(0, 1.0, 16.0, 16.0)
                .expect("representable offset")
        );

        assert!(document.undo()?);
        assert_eq!(document.operations().len(), 2);
        assert!(document.redo()?);
        assert_eq!(document.operations().len(), 4);
        drop(document);

        let document = CanvasDocument::open(&root)?;
        assert_eq!(document.operations().len(), 4);
        assert_eq!(
            document
                .operations()
                .iter()
                .filter(|operation| operation.layer_id == target.id)
                .count(),
            2
        );
        Ok(())
    }

    #[test]
    fn paste_rejects_a_locked_destination_layer() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let source = operation();
        let target = document.create_layer("Locked")?;
        assert!(document.set_layer_locked(target.id, true)?);

        assert!(
            document
                .paste_operations(&[source], target.id, 0, 1.0, 0.0, 0.0)
                .is_err()
        );
        assert!(document.operations().is_empty());
        Ok(())
    }

    #[test]
    fn selected_cross_layer_move_round_trips_history_and_reopen() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut document = CanvasDocument::open(&root)?;
        let target_layer = document.create_layer("Target")?;
        let mut target_existing = operation();
        target_existing.layer_id = target_layer.id;
        let target_existing_id = target_existing.id;
        document.commit(target_existing)?;

        let mut first = operation();
        first.width_px = 3.0;
        let first_id = first.id;
        let first_points = first.points.clone();
        document.commit(first)?;
        let mut second = operation();
        second.width_px = 9.0;
        let second_id = second.id;
        let second_points = second.points.clone();
        document.commit(second)?;

        let replacement_ids = document.move_operations_to_layer(
            &HashSet::from([first_id, second_id]),
            crate::model::DEFAULT_LAYER_ID,
            target_layer.id,
        )?;
        assert_eq!(replacement_ids.len(), 2);
        let target_operations = document
            .operations()
            .iter()
            .filter(|operation| operation.layer_id == target_layer.id)
            .collect::<Vec<_>>();
        assert_eq!(
            target_operations
                .iter()
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![target_existing_id, replacement_ids[0], replacement_ids[1]]
        );
        assert_eq!(target_operations[1].points, first_points);
        assert_eq!(target_operations[1].width_px, 3.0);
        assert_eq!(target_operations[2].points, second_points);
        assert_eq!(target_operations[2].width_px, 9.0);
        assert!(
            document
                .operations()
                .iter()
                .all(|operation| operation.layer_id != crate::model::DEFAULT_LAYER_ID)
        );
        drop(document);

        let mut document = CanvasDocument::open(&root)?;
        assert_eq!(
            document
                .operations()
                .iter()
                .filter(|operation| operation.layer_id == target_layer.id)
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![target_existing_id, replacement_ids[0], replacement_ids[1]]
        );
        assert!(document.undo()?);
        assert_eq!(
            document
                .operations()
                .iter()
                .filter(|operation| operation.layer_id == crate::model::DEFAULT_LAYER_ID)
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![first_id, second_id]
        );
        assert_eq!(
            document
                .operations()
                .iter()
                .filter(|operation| operation.layer_id == target_layer.id)
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![target_existing_id]
        );
        assert!(document.redo()?);
        assert_eq!(
            document
                .operations()
                .iter()
                .filter(|operation| operation.layer_id == target_layer.id)
                .map(|operation| operation.id)
                .collect::<Vec<_>>(),
            vec![target_existing_id, replacement_ids[0], replacement_ids[1]]
        );
        Ok(())
    }

    #[test]
    fn selected_cross_layer_move_rejects_locked_or_stale_destinations() -> anyhow::Result<()> {
        let temporary = TempDir::new()?;
        let mut document = CanvasDocument::open(temporary.path().join("test.esketch"))?;
        let target_layer = document.create_layer("Target")?;
        let source = operation();
        let source_id = source.id;
        document.commit(source)?;
        assert!(document.set_layer_locked(target_layer.id, true)?);
        let revision = document.revision();

        assert!(
            document
                .move_operations_to_layer(
                    &HashSet::from([source_id]),
                    crate::model::DEFAULT_LAYER_ID,
                    target_layer.id,
                )
                .is_err()
        );
        assert_eq!(document.revision(), revision);
        assert_eq!(document.operations()[0].id, source_id);
        assert!(document.set_layer_locked(target_layer.id, false)?);
        assert!(
            document
                .move_operations_to_layer(
                    &HashSet::from([source_id, Uuid::new_v4()]),
                    crate::model::DEFAULT_LAYER_ID,
                    target_layer.id,
                )
                .is_err()
        );
        assert_eq!(document.operations()[0].id, source_id);
        Ok(())
    }
}
