use crate::coords::CameraAddress;
use crate::model::{Bookmark, EditOperation, Layer, PaintOrderUpdate};
use crate::spatial::OperationIndex;
use crate::storage::{CanvasStore, HistoryChange, sort_operations_by_paint_order};
use crate::tile_cache::TileKey;
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use uuid::Uuid;

pub struct CanvasDocument {
    store: CanvasStore,
    operations: Vec<EditOperation>,
    layers: Vec<Layer>,
    revision: u64,
    max_sequence: i64,
    index: OperationIndex,
}

impl CanvasDocument {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let store = CanvasStore::open(path)?;
        let operations = store.load_active_operations()?;
        let layers = store.load_layers()?;
        let index = OperationIndex::build(&operations);
        let revision = store.content_revision()?;
        let max_sequence = store.active_max_sequence()?;
        Ok(Self {
            store,
            operations,
            layers,
            revision,
            max_sequence,
            index,
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

    pub fn root(&self) -> &Path {
        self.store.root()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn max_sequence(&self) -> i64 {
        self.max_sequence
    }

    pub fn operations_for_tile(&self, key: &TileKey) -> Vec<EditOperation> {
        let visible_layers = self.visible_layer_ids();
        let mut operations: Vec<_> = self
            .index
            .query(key)
            .into_iter()
            .filter_map(|index| self.operations.get(index).cloned())
            .filter(|operation| visible_layers.contains(&operation.layer_id))
            .collect();
        self.sort_operations_for_render(&mut operations);
        operations
    }

    pub fn operation_indices_for_tiles(&self, keys: &[TileKey]) -> Vec<usize> {
        let mut indices = self.index.query_many(keys);
        let visible_layers = self.visible_layer_ids();
        indices.retain(|index| {
            self.operations
                .get(*index)
                .is_some_and(|operation| visible_layers.contains(&operation.layer_id))
        });
        let layer_orders = self.layer_orders();
        indices.sort_by_key(|index| {
            self.operations
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

    pub fn operation_ids_for_tiles(&self, keys: &[TileKey]) -> Vec<Uuid> {
        self.index.query_many_ids(keys)
    }

    pub fn operation_ids_for_tiles_in_layers(
        &self,
        keys: &[TileKey],
        eligible_layers: &HashSet<Uuid>,
    ) -> Vec<Uuid> {
        self.index.query_many_ids_in_layers(keys, eligible_layers)
    }

    pub fn save_draft(&self, operation: &EditOperation) -> Result<()> {
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
        self.index = OperationIndex::build(&self.operations);
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
        self.operations.push(operation);
        let index = self.operations.len() - 1;
        self.index.insert(index, &self.operations[index]);
        Ok(())
    }

    pub fn delete_operations(
        &mut self,
        target_ids: &HashSet<Uuid>,
        layer_id: Uuid,
    ) -> Result<usize> {
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
        self.index = OperationIndex::build(&self.operations);
        Ok(target_ids.len())
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
        if delta_x == 0.0 && delta_y == 0.0 {
            return Ok(Vec::new());
        }
        let targets: Vec<_> = self
            .operations
            .iter()
            .filter(|operation| target_ids.contains(&operation.id))
            .cloned()
            .collect();
        if targets.is_empty() {
            return Ok(Vec::new());
        }

        let transaction_id = Uuid::new_v4();
        let mut tombstone = EditOperation::tombstone(
            targets.iter().map(|operation| operation.id).collect(),
            tombstone_layer_id,
        );
        tombstone.transaction_id = transaction_id;
        let mut replacements = Vec::with_capacity(targets.len());
        for source in &targets {
            let mut replacement = source.clone();
            replacement.id = Uuid::new_v4();
            replacement.sequence = 0;
            replacement.paint_order = Some(source.effective_paint_order());
            replacement.transaction_id = transaction_id;
            replacement.affects_before_sequence = None;
            replacement.points = source
                .points
                .iter()
                .map(|point| {
                    point
                        .translated_by_screen_delta(camera_depth, camera_zoom, delta_x, delta_y)
                        .context("movement is not representable at the operation depth")
                })
                .collect::<Result<_>>()?;
            replacements.push(replacement);
        }

        let mut commands = Vec::with_capacity(replacements.len() + 1);
        commands.push(tombstone);
        commands.extend(replacements);
        self.revision = self.store.commit_group(&mut commands)?;
        self.max_sequence = commands
            .last()
            .map_or(self.max_sequence, |operation| operation.sequence);

        let removed_ids: HashSet<_> = targets.iter().map(|operation| operation.id).collect();
        self.operations
            .retain(|operation| !removed_ids.contains(&operation.id));
        let replacement_ids: Vec<_> = commands
            .iter()
            .filter(|operation| !operation.is_tombstone())
            .map(|operation| operation.id)
            .collect();
        self.operations.extend(
            commands
                .into_iter()
                .filter(|operation| !operation.is_tombstone()),
        );
        sort_operations_by_paint_order(&mut self.operations);
        self.index = OperationIndex::build(&self.operations);
        Ok(replacement_ids)
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
            replacement.layer_id = target_layer_id;
            replacement.affects_before_sequence = None;
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
        self.index = OperationIndex::build(&self.operations);
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

        let mut ordered_ids = self
            .operations
            .iter()
            .filter(|operation| operation.layer_id == layer_id)
            .map(|operation| operation.id)
            .collect::<Vec<_>>();
        let original_ids = ordered_ids.clone();
        if direction > 0 {
            for index in (0..ordered_ids.len().saturating_sub(1)).rev() {
                if target_ids.contains(&ordered_ids[index])
                    && !target_ids.contains(&ordered_ids[index + 1])
                {
                    ordered_ids.swap(index, index + 1);
                }
            }
        } else {
            for index in 1..ordered_ids.len() {
                if target_ids.contains(&ordered_ids[index])
                    && !target_ids.contains(&ordered_ids[index - 1])
                {
                    ordered_ids.swap(index - 1, index);
                }
            }
        }
        if ordered_ids == original_ids {
            return Ok(false);
        }

        let updates = ordered_ids
            .iter()
            .enumerate()
            .map(|(index, operation_id)| PaintOrderUpdate {
                operation_id: *operation_id,
                paint_order: i64::try_from(index).unwrap_or(i64::MAX),
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
        self.index = OperationIndex::build(&self.operations);
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
        sort_operations_by_paint_order(&mut sources);
        let transaction_id = Uuid::new_v4();
        let mut pasted = Vec::with_capacity(sources.len());
        for source in sources {
            let mut operation = source;
            operation.id = Uuid::new_v4();
            operation.sequence = 0;
            operation.paint_order = None;
            operation.transaction_id = transaction_id;
            operation.layer_id = target_layer_id;
            operation.affects_before_sequence = None;
            operation.points = operation
                .points
                .iter()
                .map(|point| {
                    point
                        .translated_by_screen_delta(camera_depth, camera_zoom, delta_x, delta_y)
                        .context("paste offset is not representable at the operation depth")
                })
                .collect::<Result<_>>()?;
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
        self.index = OperationIndex::build(&self.operations);
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
            self.index = OperationIndex::build(&self.operations);
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
            for operation in operations {
                self.operations.push(operation);
                let index = self.operations.len() - 1;
                self.index.insert(index, &self.operations[index]);
            }
        } else {
            self.operations
                .retain(|operation| !tombstone_targets.contains(&operation.id));
            self.operations.extend(
                operations
                    .into_iter()
                    .filter(|operation| !operation.is_tombstone()),
            );
            sort_operations_by_paint_order(&mut self.operations);
            self.index = OperationIndex::build(&self.operations);
        }
        Ok(())
    }

    fn remove_operations(&mut self, operation_ids: &HashSet<Uuid>) {
        let first_removed = self
            .operations
            .iter()
            .position(|operation| operation_ids.contains(&operation.id));
        let removed_are_tail = first_removed.is_some_and(|first_removed| {
            self.operations[first_removed..]
                .iter()
                .all(|operation| operation_ids.contains(&operation.id))
        });
        if let Some(first_removed) = first_removed.filter(|_| removed_are_tail) {
            let removed_indices: HashSet<_> = (first_removed..self.operations.len()).collect();
            self.operations.truncate(first_removed);
            self.index.remove_indices(&removed_indices);
        } else {
            self.operations
                .retain(|operation| !operation_ids.contains(&operation.id));
            self.index = OperationIndex::build(&self.operations);
        }
    }

    fn reload_active_operations(&mut self) -> Result<()> {
        self.operations = self.store.load_active_operations()?;
        self.index = OperationIndex::build(&self.operations);
        self.max_sequence = self.store.active_max_sequence()?;
        Ok(())
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

    fn sort_operations_for_render(&self, operations: &mut [EditOperation]) {
        let layer_orders = self.layer_orders();
        operations.sort_by_key(|operation| {
            (
                layer_orders
                    .get(&operation.layer_id)
                    .copied()
                    .unwrap_or(i64::MIN),
                operation.effective_paint_order(),
                operation.sequence,
            )
        });
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

#[cfg(test)]
mod tests {
    use super::CanvasDocument;
    use crate::coords::CanvasPoint;
    use crate::model::{Color, EditKind, EditOperation, Layer};
    use crate::tile_cache::TileKey;
    use num_bigint::BigInt;
    use std::collections::HashSet;
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
                .operation_indices_for_tiles(std::slice::from_ref(&key))
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
