use crate::coords::CameraAddress;
use crate::model::{Bookmark, DEFAULT_LAYER_ID, EditOperation, Layer};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 3;
const MAX_UNDO_OPERATIONS: i64 = 10_000;
const MAX_LAYER_NAME_CHARS: usize = 128;
const HISTORY_KIND_OPERATIONS: &str = "operations";
const HISTORY_KIND_LAYERS: &str = "layers";

#[derive(Debug)]
pub enum HistoryChange {
    Operations(Vec<EditOperation>),
    Layers { operations_changed: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LayerHistoryPayload {
    before: Vec<Layer>,
    after: Vec<Layer>,
    affects_render: bool,
    #[serde(default)]
    operation_ids: Vec<Uuid>,
    #[serde(default)]
    operations_active_after: bool,
    #[serde(default)]
    operation_activation_changes: Vec<OperationActivationChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OperationActivationChange {
    operation_id: Uuid,
    active_after: bool,
}

pub struct CanvasStore {
    root: PathBuf,
    connection: Connection,
}

impl CanvasStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = normalize_document_path(root.as_ref());
        fs::create_dir_all(root.join("cache/tiles"))
            .with_context(|| format!("failed to create {}", root.display()))?;
        fs::create_dir_all(root.join("assets"))?;
        write_manifest_if_missing(&root)?;

        let connection = Connection::open(root.join("canvas.sqlite3"))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;

        let mut store = Self { root, connection };
        store.create_pre_migration_backup_if_needed()?;
        store.initialize_schema()?;
        update_manifest_schema_version(&store.root)?;
        store.recover_drafts()?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn load_active_operations(&self) -> Result<Vec<EditOperation>> {
        Ok(resolve_effective_operations(self.load_active_commands()?))
    }

    fn load_active_commands(&self) -> Result<Vec<EditOperation>> {
        let mut statement = self.connection.prepare(
            "SELECT operation_id, transaction_id, layer_id, payload
             FROM operations WHERE undone = 0 ORDER BY sequence ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (operation_id, transaction_id, layer_id, payload) = row?;
            decode_stored_operation(&operation_id, &transaction_id, &layer_id, &payload)
        })
        .collect()
    }

    pub fn load_operations_by_ids(
        &self,
        operation_ids: &HashSet<Uuid>,
    ) -> Result<Vec<EditOperation>> {
        if operation_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", operation_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT operation_id, transaction_id, layer_id, payload
             FROM operations
             WHERE undone = 0 AND operation_id IN ({placeholders})
             ORDER BY sequence ASC"
        );
        let parameters = operation_ids
            .iter()
            .map(|id| id.as_bytes().to_vec())
            .collect::<Vec<_>>();
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(parameters), |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (operation_id, transaction_id, layer_id, payload) = row?;
            decode_stored_operation(&operation_id, &transaction_id, &layer_id, &payload)
        })
        .filter(|operation| {
            operation
                .as_ref()
                .is_ok_and(|operation| !operation.is_metadata_command())
        })
        .collect()
    }

    pub fn active_max_sequence(&self) -> Result<i64> {
        Ok(self.connection.query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM operations WHERE undone = 0",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn load_layers(&self) -> Result<Vec<Layer>> {
        let mut statement = self.connection.prepare(
            "SELECT id, name, sort_order, visible, locked
             FROM layers WHERE active = 1 ORDER BY sort_order ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })?;
        rows.map(|row| {
            let (id, name, sort_order, visible, locked) = row?;
            Ok(Layer {
                id: Uuid::from_slice(&id)?,
                name,
                sort_order,
                visible,
                locked,
            })
        })
        .collect()
    }

    pub fn create_layer(&mut self, name: &str) -> Result<Layer> {
        let name = normalized_layer_name(name)?;
        let before = self.load_layers()?;
        let max_sort_order = before.last().map_or(-1, |layer| layer.sort_order);
        let sort_order = max_sort_order
            .checked_add(1)
            .context("layer sort order overflow")?;
        let layer = Layer {
            id: Uuid::new_v4(),
            name: name.to_owned(),
            sort_order,
            visible: true,
            locked: false,
        };
        let mut after = before.clone();
        after.push(layer.clone());
        self.commit_layer_history(before, after, false)?;
        Ok(layer)
    }

    pub fn rename_layer(&mut self, id: Uuid, name: &str) -> Result<bool> {
        let name = normalized_layer_name(name)?;
        let before = self.load_layers()?;
        let mut after = before.clone();
        let Some(layer) = after.iter_mut().find(|layer| layer.id == id) else {
            return Ok(false);
        };
        if layer.name == name {
            return Ok(false);
        }
        layer.name = name.to_owned();
        self.commit_layer_history(before, after, false)?;
        Ok(true)
    }

    pub fn move_layer(&mut self, id: Uuid, direction: i32) -> Result<Option<u64>> {
        if direction == 0 {
            return Ok(None);
        }
        let layers = self.load_layers()?;
        let Some(current_index) = layers.iter().position(|layer| layer.id == id) else {
            return Ok(None);
        };
        let target_index = current_index as i64 + i64::from(direction.signum());
        if target_index < 0 || target_index >= layers.len() as i64 {
            return Ok(None);
        }
        let mut after = layers.clone();
        let target_index = target_index as usize;
        let current_order = after[current_index].sort_order;
        after[current_index].sort_order = after[target_index].sort_order;
        after[target_index].sort_order = current_order;
        after.sort_by_key(|layer| layer.sort_order);
        Ok(Some(self.commit_layer_history(layers, after, true)?))
    }

    pub fn set_layer_visibility(&mut self, id: Uuid, visible: bool) -> Result<Option<u64>> {
        self.update_layer_flag(id, visible, true)
    }

    pub fn set_layer_locked(&mut self, id: Uuid, locked: bool) -> Result<Option<u64>> {
        self.update_layer_flag(id, locked, false)
    }

    pub fn duplicate_layer(
        &mut self,
        id: Uuid,
        source_operations: &[EditOperation],
    ) -> Result<Option<(Layer, Vec<EditOperation>, u64)>> {
        let before = self.load_layers()?;
        let Some(source_index) = before.iter().position(|layer| layer.id == id) else {
            return Ok(None);
        };
        if source_operations
            .iter()
            .any(|operation| operation.layer_id != id || operation.is_tombstone())
        {
            bail!("duplicate layer source contains an invalid operation");
        }

        let transaction_id = Uuid::new_v4();
        let layer = Layer {
            id: Uuid::new_v4(),
            name: unique_duplicate_layer_name(&before[source_index].name, &before),
            sort_order: 0,
            visible: before[source_index].visible,
            locked: before[source_index].locked,
        };
        let mut after = before.clone();
        after.insert(source_index + 1, layer.clone());
        normalize_layer_sort_orders(&mut after)?;

        let mut operations = source_operations
            .iter()
            .map(|source| {
                let mut duplicate = source.clone();
                duplicate.id = Uuid::new_v4();
                duplicate.sequence = 0;
                duplicate.paint_order = Some(source.effective_paint_order());
                duplicate.transaction_id = transaction_id;
                duplicate.layer_id = layer.id;
                duplicate.affects_before_sequence = None;
                duplicate
            })
            .collect::<Vec<_>>();
        let payload = encode_layer_history(&LayerHistoryPayload {
            before,
            after: after.clone(),
            affects_render: !operations.is_empty(),
            operation_ids: operations.iter().map(|operation| operation.id).collect(),
            operations_active_after: true,
            operation_activation_changes: Vec::new(),
        })?;
        let transaction = self.connection.transaction()?;
        clear_redo_branch(&transaction)?;
        apply_layer_state(&transaction, &after)?;
        insert_operation_rows(&transaction, &mut operations)?;
        let history_sequence = next_history_sequence(&transaction)?;
        transaction.execute(
            "INSERT INTO history_entries(
                history_sequence, transaction_id, kind, payload, undone
             ) VALUES (?1, ?2, ?3, ?4, 0)",
            params![
                history_sequence,
                transaction_id.as_bytes(),
                HISTORY_KIND_LAYERS,
                payload
            ],
        )?;
        let revision = revision_after_change(&transaction, !operations.is_empty())?;
        transaction.commit()?;
        Ok(Some((layer, operations, revision)))
    }

    pub fn delete_layer(&mut self, id: Uuid) -> Result<Option<u64>> {
        let before = self.load_layers()?;
        if before.len() <= 1 {
            bail!("cannot delete the last layer");
        }
        let Some(index) = before.iter().position(|layer| layer.id == id) else {
            return Ok(None);
        };
        let operation_ids = {
            let mut statement = self.connection.prepare(
                "SELECT operation_id
                 FROM operations
                 WHERE layer_id = ?1 AND undone = 0
                 ORDER BY sequence",
            )?;
            let rows =
                statement.query_map(params![id.as_bytes()], |row| row.get::<_, Vec<u8>>(0))?;
            rows.map(|row| Ok(Uuid::from_slice(&row?)?))
                .collect::<Result<Vec<_>>>()?
        };
        let mut after = before.clone();
        after.remove(index);
        normalize_layer_sort_orders(&mut after)?;
        let payload = encode_layer_history(&LayerHistoryPayload {
            before,
            after: after.clone(),
            affects_render: !operation_ids.is_empty(),
            operation_ids: operation_ids.clone(),
            operations_active_after: false,
            operation_activation_changes: Vec::new(),
        })?;
        let transaction_id = Uuid::new_v4();
        let transaction = self.connection.transaction()?;
        clear_redo_branch(&transaction)?;
        apply_layer_state(&transaction, &after)?;
        set_operations_active(&transaction, &operation_ids, false)?;
        let history_sequence = next_history_sequence(&transaction)?;
        transaction.execute(
            "INSERT INTO history_entries(
                history_sequence, transaction_id, kind, payload, undone
             ) VALUES (?1, ?2, ?3, ?4, 0)",
            params![
                history_sequence,
                transaction_id.as_bytes(),
                HISTORY_KIND_LAYERS,
                payload
            ],
        )?;
        let revision = revision_after_change(&transaction, !operation_ids.is_empty())?;
        transaction.commit()?;
        Ok(Some(revision))
    }

    pub fn merge_layer_down(
        &mut self,
        source_layer_id: Uuid,
        source_operations: &[EditOperation],
    ) -> Result<Option<(Layer, Vec<EditOperation>, u64)>> {
        let before = self.load_layers()?;
        let Some(source_index) = before.iter().position(|layer| layer.id == source_layer_id) else {
            return Ok(None);
        };
        let Some(destination_index) = source_index.checked_sub(1) else {
            return Ok(None);
        };
        let source_layer = &before[source_index];
        let destination = before[destination_index].clone();
        if !source_layer.visible || source_layer.locked {
            bail!("source layer is hidden or locked");
        }
        if !destination.visible || destination.locked {
            bail!("destination layer is hidden or locked");
        }
        if source_operations.iter().any(|operation| {
            operation.layer_id != source_layer_id || operation.is_metadata_command()
        }) {
            bail!("merge source contains an invalid operation");
        }

        let source_command_ids = {
            let mut statement = self.connection.prepare(
                "SELECT operation_id
                 FROM operations
                 WHERE layer_id = ?1 AND undone = 0
                 ORDER BY sequence",
            )?;
            let rows = statement.query_map(params![source_layer_id.as_bytes()], |row| {
                row.get::<_, Vec<u8>>(0)
            })?;
            rows.map(|row| Ok(Uuid::from_slice(&row?)?))
                .collect::<Result<Vec<_>>>()?
        };
        let transaction_id = Uuid::new_v4();
        let mut sources = source_operations.to_vec();
        sort_operations_by_paint_order(&mut sources);
        let mut replacements = sources
            .into_iter()
            .map(|mut replacement| {
                replacement.id = Uuid::new_v4();
                replacement.sequence = 0;
                replacement.paint_order = None;
                replacement.transaction_id = transaction_id;
                replacement.layer_id = destination.id;
                replacement.affects_before_sequence = None;
                replacement
            })
            .collect::<Vec<_>>();
        let mut after = before.clone();
        after.remove(source_index);
        normalize_layer_sort_orders(&mut after)?;
        let mut activation_changes = source_command_ids
            .iter()
            .map(|operation_id| OperationActivationChange {
                operation_id: *operation_id,
                active_after: false,
            })
            .collect::<Vec<_>>();
        activation_changes.extend(
            replacements
                .iter()
                .map(|operation| OperationActivationChange {
                    operation_id: operation.id,
                    active_after: true,
                }),
        );
        let payload = encode_layer_history(&LayerHistoryPayload {
            before,
            after: after.clone(),
            affects_render: !source_operations.is_empty(),
            operation_ids: Vec::new(),
            operations_active_after: false,
            operation_activation_changes: activation_changes,
        })?;

        let transaction = self.connection.transaction()?;
        clear_redo_branch(&transaction)?;
        apply_layer_state(&transaction, &after)?;
        set_operations_active(&transaction, &source_command_ids, false)?;
        insert_operation_rows(&transaction, &mut replacements)?;
        let history_sequence = next_history_sequence(&transaction)?;
        transaction.execute(
            "INSERT INTO history_entries(
                history_sequence, transaction_id, kind, payload, undone
             ) VALUES (?1, ?2, ?3, ?4, 0)",
            params![
                history_sequence,
                transaction_id.as_bytes(),
                HISTORY_KIND_LAYERS,
                payload
            ],
        )?;
        let revision = revision_after_change(&transaction, !source_operations.is_empty())?;
        transaction.commit()?;
        Ok(Some((destination, replacements, revision)))
    }

    fn update_layer_flag(
        &mut self,
        id: Uuid,
        value: bool,
        visibility: bool,
    ) -> Result<Option<u64>> {
        let before = self.load_layers()?;
        let mut after = before.clone();
        let Some(layer) = after.iter_mut().find(|layer| layer.id == id) else {
            return Ok(None);
        };
        let target = if visibility {
            &mut layer.visible
        } else {
            &mut layer.locked
        };
        if *target == value {
            return Ok(None);
        }
        *target = value;
        Ok(Some(self.commit_layer_history(before, after, visibility)?))
    }

    fn commit_layer_history(
        &mut self,
        before: Vec<Layer>,
        after: Vec<Layer>,
        affects_render: bool,
    ) -> Result<u64> {
        if before == after {
            return self.content_revision();
        }
        let payload = encode_layer_history(&LayerHistoryPayload {
            before,
            after: after.clone(),
            affects_render,
            operation_ids: Vec::new(),
            operations_active_after: false,
            operation_activation_changes: Vec::new(),
        })?;
        let transaction_id = Uuid::new_v4();
        let transaction = self.connection.transaction()?;
        clear_redo_branch(&transaction)?;
        apply_layer_state(&transaction, &after)?;
        let history_sequence = next_history_sequence(&transaction)?;
        transaction.execute(
            "INSERT INTO history_entries(
                history_sequence, transaction_id, kind, payload, undone
             ) VALUES (?1, ?2, ?3, ?4, 0)",
            params![
                history_sequence,
                transaction_id.as_bytes(),
                HISTORY_KIND_LAYERS,
                payload
            ],
        )?;
        let revision = revision_after_change(&transaction, affects_render)?;
        transaction.commit()?;
        Ok(revision)
    }

    pub fn commit(&mut self, operation: &mut EditOperation) -> Result<u64> {
        self.commit_group(std::slice::from_mut(operation))
    }

    pub fn commit_group(&mut self, operations: &mut [EditOperation]) -> Result<u64> {
        if operations.is_empty() {
            bail!("cannot commit an empty operation group");
        }
        for operation in operations.iter_mut() {
            operation.normalize_metadata();
        }
        let transaction_id = operations[0].transaction_id;
        if operations
            .iter()
            .any(|operation| operation.transaction_id != transaction_id)
        {
            bail!("operation group must share one transaction ID");
        }

        let transaction = self.connection.transaction()?;
        clear_redo_branch(&transaction)?;
        insert_operation_rows(&transaction, operations)?;
        let history_sequence = next_history_sequence(&transaction)?;
        transaction.execute(
            "INSERT INTO history_entries(
                history_sequence, transaction_id, kind, payload, undone
             ) VALUES (?1, ?2, ?3, NULL, 0)
             ON CONFLICT(transaction_id) DO NOTHING",
            params![
                history_sequence,
                transaction_id.as_bytes(),
                HISTORY_KIND_OPERATIONS
            ],
        )?;
        transaction.execute(
            "DELETE FROM operations WHERE sequence < (SELECT MAX(sequence) - ?1 FROM operations) AND undone = 1",
            params![MAX_UNDO_OPERATIONS],
        )?;
        let revision = revision_after_change(&transaction, true)?;
        transaction.commit()?;
        Ok(revision)
    }

    pub fn save_draft(&self, operation: &EditOperation) -> Result<()> {
        let payload = encode_operation(operation)?;
        self.connection.execute(
            "INSERT INTO drafts(operation_id, payload, updated_at) VALUES (?1, ?2, unixepoch())
             ON CONFLICT(operation_id) DO UPDATE SET payload=excluded.payload, updated_at=excluded.updated_at",
            params![operation.id.as_bytes(), payload],
        )?;
        Ok(())
    }

    pub fn discard_draft(&self, operation: &EditOperation) -> Result<()> {
        self.connection.execute(
            "DELETE FROM drafts WHERE operation_id = ?1",
            params![operation.id.as_bytes()],
        )?;
        Ok(())
    }

    pub fn undo(&mut self) -> Result<Option<(HistoryChange, u64)>> {
        let transaction = self.connection.transaction()?;
        let entry: Option<(Vec<u8>, String, Option<Vec<u8>>)> = transaction
            .query_row(
                "SELECT transaction_id, kind, payload
                 FROM history_entries
                 WHERE undone = 0
                 ORDER BY history_sequence DESC
                 LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((transaction_id, kind, payload)) = entry else {
            return Ok(None);
        };
        let (change, affects_render) = match kind.as_str() {
            HISTORY_KIND_OPERATIONS => {
                let operations = load_transaction_operations(&transaction, &transaction_id, false)?;
                transaction.execute(
                    "UPDATE operations SET undone = 1
                     WHERE transaction_id = ?1 AND undone = 0",
                    params![transaction_id],
                )?;
                (HistoryChange::Operations(operations), true)
            }
            HISTORY_KIND_LAYERS => {
                let payload = payload.context("layer history entry has no payload")?;
                let payload = decode_layer_history(&payload)?;
                set_operations_active(
                    &transaction,
                    &payload.operation_ids,
                    !payload.operations_active_after,
                )?;
                apply_operation_activation_changes(
                    &transaction,
                    &payload.operation_activation_changes,
                    false,
                )?;
                apply_layer_state(&transaction, &payload.before)?;
                (
                    HistoryChange::Layers {
                        operations_changed: !payload.operation_ids.is_empty()
                            || !payload.operation_activation_changes.is_empty(),
                    },
                    payload.affects_render,
                )
            }
            _ => bail!("unsupported history entry kind {kind}"),
        };
        transaction.execute(
            "UPDATE history_entries SET undone = 1 WHERE transaction_id = ?1",
            params![transaction_id],
        )?;
        let revision = revision_after_change(&transaction, affects_render)?;
        transaction.commit()?;
        Ok(Some((change, revision)))
    }

    pub fn redo(&mut self) -> Result<Option<(HistoryChange, u64)>> {
        let transaction = self.connection.transaction()?;
        let entry: Option<(Vec<u8>, String, Option<Vec<u8>>)> = transaction
            .query_row(
                "SELECT transaction_id, kind, payload
                 FROM history_entries
                 WHERE undone = 1
                 ORDER BY history_sequence ASC
                 LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((transaction_id, kind, payload)) = entry else {
            return Ok(None);
        };
        let (change, affects_render) = match kind.as_str() {
            HISTORY_KIND_OPERATIONS => {
                let operations = load_transaction_operations(&transaction, &transaction_id, true)?;
                transaction.execute(
                    "UPDATE operations SET undone = 0
                     WHERE transaction_id = ?1 AND undone = 1",
                    params![transaction_id],
                )?;
                (HistoryChange::Operations(operations), true)
            }
            HISTORY_KIND_LAYERS => {
                let payload = payload.context("layer history entry has no payload")?;
                let payload = decode_layer_history(&payload)?;
                set_operations_active(
                    &transaction,
                    &payload.operation_ids,
                    payload.operations_active_after,
                )?;
                apply_operation_activation_changes(
                    &transaction,
                    &payload.operation_activation_changes,
                    true,
                )?;
                apply_layer_state(&transaction, &payload.after)?;
                (
                    HistoryChange::Layers {
                        operations_changed: !payload.operation_ids.is_empty()
                            || !payload.operation_activation_changes.is_empty(),
                    },
                    payload.affects_render,
                )
            }
            _ => bail!("unsupported history entry kind {kind}"),
        };
        transaction.execute(
            "UPDATE history_entries SET undone = 0 WHERE transaction_id = ?1",
            params![transaction_id],
        )?;
        let revision = revision_after_change(&transaction, affects_render)?;
        transaction.commit()?;
        Ok(Some((change, revision)))
    }

    pub fn integrity_check(&self) -> Result<()> {
        let result: String = self
            .connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if result != "ok" {
            bail!("SQLite integrity check failed: {result}");
        }
        Ok(())
    }

    pub fn save_bookmark(&self, name: &str, camera: &CameraAddress) -> Result<Bookmark> {
        let bookmark = Bookmark {
            id: Uuid::new_v4(),
            name: name.to_owned(),
            camera: camera.clone(),
        };
        self.connection.execute(
            "INSERT INTO bookmarks(id, name, camera_json) VALUES (?1, ?2, ?3)",
            params![
                bookmark.id.as_bytes(),
                bookmark.name,
                serde_json::to_string(&bookmark.camera)?
            ],
        )?;
        Ok(bookmark)
    }

    pub fn load_bookmarks(&self) -> Result<Vec<Bookmark>> {
        let mut statement = self
            .connection
            .prepare("SELECT id, name, camera_json FROM bookmarks ORDER BY rowid ASC")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (id, name, camera_json) = row?;
            Ok(Bookmark {
                id: Uuid::from_slice(&id)?,
                name,
                camera: serde_json::from_str(&camera_json)?,
            })
        })
        .collect()
    }

    pub fn rename_bookmark(&self, id: Uuid, name: &str) -> Result<bool> {
        let changed = self.connection.execute(
            "UPDATE bookmarks SET name = ?1 WHERE id = ?2",
            params![name, id.as_bytes()],
        )?;
        Ok(changed > 0)
    }

    pub fn delete_bookmark(&self, id: Uuid) -> Result<bool> {
        let changed = self.connection.execute(
            "DELETE FROM bookmarks WHERE id = ?1",
            params![id.as_bytes()],
        )?;
        Ok(changed > 0)
    }

    pub fn checkpoint(&self) -> Result<()> {
        self.connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        self.integrity_check()?;
        self.create_verified_backup()
    }

    pub fn content_revision(&self) -> Result<u64> {
        let revision: i64 = self.connection.query_row(
            "SELECT CAST(value AS INTEGER) FROM metadata WHERE key='content_revision'",
            [],
            |row| row.get(0),
        )?;
        Ok(revision.try_into()?)
    }

    fn initialize_schema(&mut self) -> Result<()> {
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS layers (
                id BLOB PRIMARY KEY,
                name TEXT NOT NULL,
                sort_order INTEGER NOT NULL UNIQUE,
                visible INTEGER NOT NULL DEFAULT 1 CHECK (visible IN (0, 1)),
                locked INTEGER NOT NULL DEFAULT 0 CHECK (locked IN (0, 1)),
                active INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1))
             );
             CREATE TABLE IF NOT EXISTS operations (
                sequence INTEGER PRIMARY KEY,
                operation_id BLOB NOT NULL UNIQUE,
                transaction_id BLOB NOT NULL,
                layer_id BLOB NOT NULL REFERENCES layers(id),
                payload BLOB NOT NULL,
                undone INTEGER NOT NULL DEFAULT 0 CHECK (undone IN (0, 1))
             );
             CREATE TABLE IF NOT EXISTS drafts (
                operation_id BLOB PRIMARY KEY,
                payload BLOB NOT NULL,
                updated_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS bookmarks (
                id BLOB PRIMARY KEY,
                name TEXT NOT NULL,
                camera_json TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS tile_revisions (
                depth INTEGER NOT NULL,
                tile_x TEXT NOT NULL,
                tile_y TEXT NOT NULL,
                revision INTEGER NOT NULL,
                PRIMARY KEY(depth, tile_x, tile_y)
             );
             CREATE TABLE IF NOT EXISTS history_entries (
                history_sequence INTEGER PRIMARY KEY,
                transaction_id BLOB NOT NULL UNIQUE,
                kind TEXT NOT NULL CHECK (kind IN ('operations', 'layers')),
                payload BLOB,
                undone INTEGER NOT NULL DEFAULT 0 CHECK (undone IN (0, 1))
             );",
        )?;
        self.connection.execute(
            "INSERT INTO metadata(key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO NOTHING",
            params![SCHEMA_VERSION.to_string()],
        )?;
        self.connection.execute(
            "INSERT INTO metadata(key, value) VALUES ('content_revision', '0')
             ON CONFLICT(key) DO NOTHING",
            [],
        )?;
        self.ensure_default_layer()?;
        let mut version: i64 = self.connection.query_row(
            "SELECT CAST(value AS INTEGER) FROM metadata WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )?;
        if version == 1 {
            self.migrate_schema_v1_to_v2()?;
            version = 2;
        }
        if version == 2 {
            self.migrate_schema_v2_to_v3()?;
            version = 3;
        }
        if version != SCHEMA_VERSION {
            bail!("unsupported canvas schema {version}");
        }
        self.connection.execute(
            "CREATE INDEX IF NOT EXISTS operations_transaction
             ON operations(transaction_id, sequence)",
            [],
        )?;
        self.connection.execute(
            "CREATE INDEX IF NOT EXISTS operations_history
             ON operations(undone, sequence)",
            [],
        )?;
        self.connection.execute(
            "CREATE INDEX IF NOT EXISTS unified_history
             ON history_entries(undone, history_sequence)",
            [],
        )?;
        Ok(())
    }

    fn create_pre_migration_backup_if_needed(&mut self) -> Result<()> {
        let has_metadata: bool = self.connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_master
                WHERE type = 'table' AND name = 'metadata'
             )",
            [],
            |row| row.get(0),
        )?;
        if !has_metadata {
            return Ok(());
        }
        let version: Option<i64> = self
            .connection
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM metadata WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if version.is_none_or(|version| version >= SCHEMA_VERSION) {
            return Ok(());
        }

        self.connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        self.integrity_check()?;
        let backup_root = self.root.join("backups/migrations");
        fs::create_dir_all(&backup_root)?;
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        let backup_path = backup_root.join(format!("pre-schema-v3-{timestamp}.sqlite3"));
        fs::copy(self.root.join("canvas.sqlite3"), &backup_path)?;
        let backup = Connection::open(&backup_path)?;
        let result: String = backup.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if result != "ok" {
            drop(backup);
            let _ = fs::remove_file(&backup_path);
            bail!("pre-migration backup integrity check failed: {result}");
        }
        Ok(())
    }

    fn ensure_default_layer(&self) -> Result<()> {
        let layer = Layer::default_layer();
        self.connection.execute(
            "INSERT INTO layers(id, name, sort_order, visible, locked)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO NOTHING",
            params![
                layer.id.as_bytes(),
                layer.name,
                layer.sort_order,
                layer.visible,
                layer.locked
            ],
        )?;
        Ok(())
    }

    fn migrate_schema_v1_to_v2(&mut self) -> Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute("ALTER TABLE operations RENAME TO operations_v1", [])?;
        transaction.execute_batch(
            "CREATE TABLE operations (
                sequence INTEGER PRIMARY KEY,
                operation_id BLOB NOT NULL UNIQUE,
                transaction_id BLOB NOT NULL,
                layer_id BLOB NOT NULL REFERENCES layers(id),
                payload BLOB NOT NULL,
                undone INTEGER NOT NULL DEFAULT 0 CHECK (undone IN (0, 1))
             );",
        )?;
        transaction.execute(
            "INSERT INTO operations(
                sequence, operation_id, transaction_id, layer_id, payload, undone
             )
             SELECT sequence, operation_id, operation_id, ?1, payload, undone
             FROM operations_v1",
            params![DEFAULT_LAYER_ID.as_bytes()],
        )?;
        transaction.execute("DROP TABLE operations_v1", [])?;
        transaction.execute(
            "UPDATE metadata SET value = ?1 WHERE key = 'schema_version'",
            params!["2"],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn migrate_schema_v2_to_v3(&mut self) -> Result<()> {
        let transaction = self.connection.transaction()?;
        let has_active_column: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM pragma_table_info('layers') WHERE name = 'active'
             )",
            [],
            |row| row.get(0),
        )?;
        if !has_active_column {
            transaction.execute(
                "ALTER TABLE layers
                 ADD COLUMN active INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1))",
                [],
            )?;
        }
        transaction.execute_batch(
            "CREATE TABLE IF NOT EXISTS history_entries (
                history_sequence INTEGER PRIMARY KEY,
                transaction_id BLOB NOT NULL UNIQUE,
                kind TEXT NOT NULL CHECK (kind IN ('operations', 'layers')),
                payload BLOB,
                undone INTEGER NOT NULL DEFAULT 0 CHECK (undone IN (0, 1))
             );
             INSERT INTO history_entries(
                history_sequence, transaction_id, kind, payload, undone
             )
             SELECT MAX(sequence), transaction_id, 'operations', NULL, MIN(undone)
             FROM operations
             GROUP BY transaction_id;
             UPDATE metadata SET value = '3' WHERE key = 'schema_version';",
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn recover_drafts(&mut self) -> Result<()> {
        let payloads: Vec<Vec<u8>> = {
            let mut statement = self
                .connection
                .prepare("SELECT payload FROM drafts ORDER BY updated_at ASC")?;
            let rows = statement.query_map([], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for payload in payloads {
            let mut operation = decode_operation(&payload)?;
            if operation.points.len() >= 2 {
                self.commit(&mut operation)?;
            } else {
                self.discard_draft(&operation)?;
            }
        }
        Ok(())
    }

    fn create_verified_backup(&self) -> Result<()> {
        let backup_root = self.root.join("backups");
        fs::create_dir_all(&backup_root)?;
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        let temporary = backup_root.join(format!("checkpoint-{timestamp}.sqlite3.tmp"));
        let final_path = backup_root.join(format!("checkpoint-{timestamp}.sqlite3"));
        fs::copy(self.root.join("canvas.sqlite3"), &temporary)?;
        let backup = Connection::open(&temporary)?;
        let result: String = backup.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if result != "ok" {
            let _ = fs::remove_file(&temporary);
            bail!("backup integrity check failed: {result}");
        }
        drop(backup);
        fs::rename(&temporary, &final_path)?;

        let mut backups: Vec<_> = fs::read_dir(&backup_root)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "sqlite3")
            })
            .collect();
        backups.sort();
        let remove_count = backups.len().saturating_sub(2);
        for obsolete in backups.into_iter().take(remove_count) {
            fs::remove_file(obsolete)?;
        }
        Ok(())
    }
}

fn normalize_document_path(path: &Path) -> PathBuf {
    if path
        .extension()
        .is_some_and(|extension| extension == "esketch")
    {
        path.to_path_buf()
    } else {
        path.with_extension("esketch")
    }
}

fn write_manifest_if_missing(root: &Path) -> Result<()> {
    let path = root.join("manifest.json");
    if path.exists() {
        return Ok(());
    }
    let temporary = root.join("manifest.json.tmp");
    fs::write(
        &temporary,
        b"{\n  \"format\": \"EndlessSketch\",\n  \"schema_version\": 3,\n  \"tile_cache_is_rebuildable\": true\n}\n",
    )?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn update_manifest_schema_version(root: &Path) -> Result<()> {
    let path = root.join("manifest.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    if manifest["schema_version"] == SCHEMA_VERSION {
        return Ok(());
    }
    manifest["schema_version"] = SCHEMA_VERSION.into();
    let temporary = root.join("manifest.json.tmp");
    let mut json = serde_json::to_vec_pretty(&manifest)?;
    json.push(b'\n');
    fs::write(&temporary, json)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn encode_operation(operation: &EditOperation) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(operation)?;
    Ok(zstd::stream::encode_all(Cursor::new(json), 3)?)
}

fn decode_operation(payload: &[u8]) -> Result<EditOperation> {
    let json = zstd::stream::decode_all(Cursor::new(payload))?;
    let mut operation: EditOperation = serde_json::from_slice(&json)?;
    operation.normalize_metadata();
    Ok(operation)
}

fn encode_layer_history(payload: &LayerHistoryPayload) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(payload)?;
    Ok(zstd::stream::encode_all(Cursor::new(json), 3)?)
}

fn decode_layer_history(payload: &[u8]) -> Result<LayerHistoryPayload> {
    let json = zstd::stream::decode_all(Cursor::new(payload))?;
    Ok(serde_json::from_slice(&json)?)
}

fn insert_operation_rows(
    transaction: &Transaction<'_>,
    operations: &mut [EditOperation],
) -> Result<()> {
    let first_sequence: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(sequence), 0) + 1 FROM operations",
        [],
        |row| row.get(0),
    )?;
    for (offset, operation) in operations.iter_mut().enumerate() {
        operation.normalize_metadata();
        let offset = i64::try_from(offset)?;
        operation.sequence = first_sequence
            .checked_add(offset)
            .context("operation sequence overflow")?;
        operation.affects_before_sequence = operation.destructive.then_some(operation.sequence - 1);
        let payload = encode_operation(operation)?;
        transaction.execute(
            "INSERT INTO operations(
                sequence, operation_id, transaction_id, layer_id, payload, undone
             ) VALUES (?1, ?2, ?3, ?4, ?5, 0)",
            params![
                operation.sequence,
                operation.id.as_bytes(),
                operation.transaction_id.as_bytes(),
                operation.layer_id.as_bytes(),
                payload
            ],
        )?;
        transaction.execute(
            "DELETE FROM drafts WHERE operation_id = ?1",
            params![operation.id.as_bytes()],
        )?;
    }
    Ok(())
}

fn set_operations_active(
    transaction: &Transaction<'_>,
    operation_ids: &[Uuid],
    active: bool,
) -> Result<()> {
    let mut statement =
        transaction.prepare("UPDATE operations SET undone = ?1 WHERE operation_id = ?2")?;
    for operation_id in operation_ids {
        statement.execute(params![!active, operation_id.as_bytes()])?;
    }
    Ok(())
}

fn apply_operation_activation_changes(
    transaction: &Transaction<'_>,
    changes: &[OperationActivationChange],
    use_after_state: bool,
) -> Result<()> {
    let mut statement =
        transaction.prepare("UPDATE operations SET undone = ?1 WHERE operation_id = ?2")?;
    for change in changes {
        let active = if use_after_state {
            change.active_after
        } else {
            !change.active_after
        };
        statement.execute(params![!active, change.operation_id.as_bytes()])?;
    }
    Ok(())
}

fn next_history_sequence(transaction: &Transaction<'_>) -> Result<i64> {
    Ok(transaction.query_row(
        "SELECT COALESCE(MAX(history_sequence), 0) + 1 FROM history_entries",
        [],
        |row| row.get(0),
    )?)
}

fn clear_redo_branch(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute(
        "DELETE FROM operations
         WHERE transaction_id IN (
            SELECT transaction_id
            FROM history_entries
            WHERE undone = 1
         )",
        [],
    )?;
    transaction.execute("DELETE FROM history_entries WHERE undone = 1", [])?;
    Ok(())
}

fn normalize_layer_sort_orders(layers: &mut [Layer]) -> Result<()> {
    for (index, layer) in layers.iter_mut().enumerate() {
        layer.sort_order = i64::try_from(index)?;
    }
    Ok(())
}

fn unique_duplicate_layer_name(source_name: &str, layers: &[Layer]) -> String {
    let existing: HashSet<_> = layers.iter().map(|layer| layer.name.as_str()).collect();
    for copy_number in 1.. {
        let suffix = if copy_number == 1 {
            " copy".to_owned()
        } else {
            format!(" copy {copy_number}")
        };
        let base_limit = MAX_LAYER_NAME_CHARS.saturating_sub(suffix.chars().count());
        let base = source_name.chars().take(base_limit).collect::<String>();
        let candidate = format!("{base}{suffix}");
        if !existing.contains(candidate.as_str()) {
            return candidate;
        }
    }
    unreachable!("copy number iterator is unbounded")
}

fn revision_after_change(transaction: &Transaction<'_>, changed: bool) -> Result<u64> {
    if changed {
        transaction.execute(
            "UPDATE metadata
             SET value = CAST(value AS INTEGER) + 1
             WHERE key = 'content_revision'",
            [],
        )?;
    }
    let revision: i64 = transaction.query_row(
        "SELECT CAST(value AS INTEGER)
         FROM metadata
         WHERE key = 'content_revision'",
        [],
        |row| row.get(0),
    )?;
    Ok(revision.try_into()?)
}

fn apply_layer_state(transaction: &Transaction<'_>, layers: &[Layer]) -> Result<()> {
    let existing: Vec<(Vec<u8>, i64)> = {
        let mut statement = transaction.prepare("SELECT id, sort_order FROM layers")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut used_orders: HashSet<i64> = existing
        .iter()
        .map(|(_, sort_order)| *sort_order)
        .chain(layers.iter().map(|layer| layer.sort_order))
        .collect();
    let mut temporary_order = i64::MIN;
    for (id, _) in existing {
        while used_orders.contains(&temporary_order) {
            temporary_order = temporary_order
                .checked_add(1)
                .context("temporary layer sort order overflow")?;
        }
        transaction.execute(
            "UPDATE layers SET sort_order = ?1, active = 0 WHERE id = ?2",
            params![temporary_order, id],
        )?;
        used_orders.insert(temporary_order);
        temporary_order = temporary_order
            .checked_add(1)
            .context("temporary layer sort order overflow")?;
    }
    for layer in layers {
        transaction.execute(
            "INSERT INTO layers(id, name, sort_order, visible, locked, active)
             VALUES (?1, ?2, ?3, ?4, ?5, 1)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                sort_order = excluded.sort_order,
                visible = excluded.visible,
                locked = excluded.locked,
                active = 1",
            params![
                layer.id.as_bytes(),
                layer.name,
                layer.sort_order,
                layer.visible,
                layer.locked
            ],
        )?;
    }
    Ok(())
}

fn resolve_effective_operations(commands: Vec<EditOperation>) -> Vec<EditOperation> {
    let hidden_ids: HashSet<_> = commands
        .iter()
        .filter(|operation| operation.is_tombstone())
        .flat_map(|operation| operation.tombstone_targets.iter().copied())
        .collect();
    let paint_order_overrides: HashMap<_, _> = commands
        .iter()
        .filter(|operation| operation.is_paint_order_command())
        .flat_map(|operation| {
            operation
                .paint_order_updates
                .iter()
                .map(|update| (update.operation_id, update.paint_order))
        })
        .collect();
    let mut operations: Vec<_> = commands
        .into_iter()
        .filter(|operation| !operation.is_metadata_command() && !hidden_ids.contains(&operation.id))
        .map(|mut operation| {
            if let Some(paint_order) = paint_order_overrides.get(&operation.id) {
                operation.paint_order = Some(*paint_order);
            }
            operation
        })
        .collect();
    sort_operations_by_paint_order(&mut operations);
    operations
}

fn normalized_layer_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        bail!("layer name cannot be empty");
    }
    if name.chars().count() > MAX_LAYER_NAME_CHARS {
        bail!("layer name exceeds {MAX_LAYER_NAME_CHARS} characters");
    }
    Ok(name)
}

pub(crate) fn sort_operations_by_paint_order(operations: &mut [EditOperation]) {
    operations.sort_by_key(|operation| (operation.effective_paint_order(), operation.sequence));
}

fn decode_stored_operation(
    operation_id: &[u8],
    transaction_id: &[u8],
    layer_id: &[u8],
    payload: &[u8],
) -> Result<EditOperation> {
    let mut operation = decode_operation(payload)?;
    operation.id = Uuid::from_slice(operation_id)?;
    operation.transaction_id = Uuid::from_slice(transaction_id)?;
    operation.layer_id = Uuid::from_slice(layer_id)?;
    Ok(operation)
}

fn load_transaction_operations(
    transaction: &Transaction<'_>,
    transaction_id: &[u8],
    undone: bool,
) -> Result<Vec<EditOperation>> {
    let rows = {
        let mut statement = transaction.prepare(
            "SELECT operation_id, transaction_id, layer_id, payload
             FROM operations
             WHERE transaction_id = ?1 AND undone = ?2
             ORDER BY sequence ASC",
        )?;
        let rows = statement.query_map(params![transaction_id, undone], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    rows.into_iter()
        .map(|(operation_id, transaction_id, layer_id, payload)| {
            decode_stored_operation(&operation_id, &transaction_id, &layer_id, &payload)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::CanvasPoint;
    use crate::model::{Color, EditKind};
    use num_bigint::BigInt;
    use tempfile::TempDir;

    fn operation() -> EditOperation {
        EditOperation::draft(
            EditKind::Paint,
            0,
            1.0,
            vec![
                CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.1, 0.2),
                CanvasPoint::new(0, BigInt::from(0), BigInt::from(0), 0.3, 0.4),
            ],
            Color::BLACK,
            5.0,
        )
    }

    fn legacy_operation_payload(operation: &EditOperation) -> Result<Vec<u8>> {
        let mut value = serde_json::to_value(operation)?;
        let object = value
            .as_object_mut()
            .context("operation JSON must be an object")?;
        object.remove("transaction_id");
        object.remove("layer_id");
        let json = serde_json::to_vec(&value)?;
        Ok(zstd::stream::encode_all(Cursor::new(json), 3)?)
    }

    #[test]
    fn new_documents_have_a_default_layer() -> Result<()> {
        let temporary = TempDir::new()?;
        let store = CanvasStore::open(temporary.path().join("test.esketch"))?;

        assert_eq!(store.load_layers()?, vec![Layer::default_layer()]);
        Ok(())
    }

    #[test]
    fn schema_v1_migrates_operations_to_default_layer_transactions() -> Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("legacy.esketch");
        fs::create_dir_all(&root)?;
        fs::write(
            root.join("manifest.json"),
            b"{\"format\":\"EndlessSketch\",\"schema_version\":1,\"tile_cache_is_rebuildable\":true}",
        )?;
        let connection = Connection::open(root.join("canvas.sqlite3"))?;
        connection.execute_batch(
            "CREATE TABLE metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
             );
             INSERT INTO metadata(key, value) VALUES
                ('schema_version', '1'),
                ('content_revision', '1');
             CREATE TABLE operations (
                sequence INTEGER PRIMARY KEY,
                operation_id BLOB NOT NULL UNIQUE,
                payload BLOB NOT NULL,
                undone INTEGER NOT NULL DEFAULT 0 CHECK (undone IN (0, 1))
             );",
        )?;
        let mut legacy = operation();
        legacy.sequence = 1;
        let payload = legacy_operation_payload(&legacy)?;
        connection.execute(
            "INSERT INTO operations(sequence, operation_id, payload, undone)
             VALUES (1, ?1, ?2, 0)",
            params![legacy.id.as_bytes(), payload],
        )?;
        drop(connection);

        let store = CanvasStore::open(&root)?;
        let operations = store.load_active_operations()?;
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].id, legacy.id);
        assert_eq!(operations[0].transaction_id, legacy.id);
        assert_eq!(operations[0].layer_id, DEFAULT_LAYER_ID);
        assert_eq!(store.load_layers()?, vec![Layer::default_layer()]);
        let version: i64 = store.connection.query_row(
            "SELECT CAST(value AS INTEGER) FROM metadata WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, SCHEMA_VERSION);
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("manifest.json"))?)?;
        assert_eq!(manifest["schema_version"], SCHEMA_VERSION);
        let migration_backups: Vec<_> =
            fs::read_dir(root.join("backups/migrations"))?.collect::<Result<_, _>>()?;
        assert_eq!(migration_backups.len(), 1);
        let backup = Connection::open(migration_backups[0].path())?;
        let backup_version: i64 = backup.query_row(
            "SELECT CAST(value AS INTEGER) FROM metadata WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(backup_version, 1);
        store.integrity_check()?;
        Ok(())
    }

    #[test]
    fn schema_v2_migrates_to_unified_history() -> Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("legacy-v2.esketch");
        fs::create_dir_all(&root)?;
        fs::write(
            root.join("manifest.json"),
            b"{\"format\":\"EndlessSketch\",\"schema_version\":2,\"tile_cache_is_rebuildable\":true}",
        )?;
        let connection = Connection::open(root.join("canvas.sqlite3"))?;
        connection.execute_batch(
            "CREATE TABLE metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
             );
             INSERT INTO metadata(key, value) VALUES
                ('schema_version', '2'),
                ('content_revision', '1');
             CREATE TABLE layers (
                id BLOB PRIMARY KEY,
                name TEXT NOT NULL,
                sort_order INTEGER NOT NULL UNIQUE,
                visible INTEGER NOT NULL DEFAULT 1,
                locked INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE operations (
                sequence INTEGER PRIMARY KEY,
                operation_id BLOB NOT NULL UNIQUE,
                transaction_id BLOB NOT NULL,
                layer_id BLOB NOT NULL REFERENCES layers(id),
                payload BLOB NOT NULL,
                undone INTEGER NOT NULL DEFAULT 0
             );",
        )?;
        let layer = Layer::default_layer();
        connection.execute(
            "INSERT INTO layers(id, name, sort_order, visible, locked)
             VALUES (?1, ?2, ?3, 1, 0)",
            params![layer.id.as_bytes(), layer.name, layer.sort_order],
        )?;
        let mut legacy = operation();
        legacy.sequence = 1;
        let payload = encode_operation(&legacy)?;
        connection.execute(
            "INSERT INTO operations(
                sequence, operation_id, transaction_id, layer_id, payload, undone
             ) VALUES (1, ?1, ?2, ?3, ?4, 0)",
            params![
                legacy.id.as_bytes(),
                legacy.transaction_id.as_bytes(),
                DEFAULT_LAYER_ID.as_bytes(),
                payload
            ],
        )?;
        drop(connection);

        let mut store = CanvasStore::open(&root)?;
        assert_eq!(store.load_layers()?, vec![Layer::default_layer()]);
        let history_count: i64 =
            store
                .connection
                .query_row("SELECT COUNT(*) FROM history_entries", [], |row| row.get(0))?;
        assert_eq!(history_count, 1);
        let (change, _) = store.undo()?.expect("migrated operation history");
        let HistoryChange::Operations(operations) = change else {
            panic!("expected operation history");
        };
        assert_eq!(operations.len(), 1);
        let backups: Vec<_> =
            fs::read_dir(root.join("backups/migrations"))?.collect::<Result<_, _>>()?;
        assert_eq!(backups.len(), 1);
        Ok(())
    }

    #[test]
    fn undo_and_redo_apply_to_an_entire_transaction_group() -> Result<()> {
        let temporary = TempDir::new()?;
        let mut store = CanvasStore::open(temporary.path().join("test.esketch"))?;
        let transaction_id = Uuid::new_v4();
        for _ in 0..2 {
            let mut operation = operation();
            operation.transaction_id = transaction_id;
            store.commit(&mut operation)?;
        }
        assert_eq!(store.load_active_operations()?.len(), 2);

        let (HistoryChange::Operations(undone), _) = store.undo()?.expect("transaction to undo")
        else {
            panic!("expected operation history");
        };
        assert_eq!(undone.len(), 2);
        assert!(store.load_active_operations()?.is_empty());
        let (HistoryChange::Operations(redone), _) = store.redo()?.expect("transaction to redo")
        else {
            panic!("expected operation history");
        };
        assert_eq!(redone.len(), 2);
        assert_eq!(store.load_active_operations()?.len(), 2);
        Ok(())
    }

    #[test]
    fn interleaved_operation_and_layer_history_round_trips() -> Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut store = CanvasStore::open(&root)?;
        let mut paint = operation();
        store.commit(&mut paint)?;
        let layer = store.create_layer("Ink")?;
        assert!(store.rename_layer(layer.id, "Line art")?);
        assert!(store.set_layer_visibility(layer.id, false)?.is_some());

        assert!(matches!(
            store.undo()?.map(|entry| entry.0),
            Some(HistoryChange::Layers { .. })
        ));
        assert!(store.load_layers()?[1].visible);
        assert!(matches!(
            store.undo()?.map(|entry| entry.0),
            Some(HistoryChange::Layers { .. })
        ));
        assert_eq!(store.load_layers()?[1].name, "Ink");
        assert!(matches!(
            store.undo()?.map(|entry| entry.0),
            Some(HistoryChange::Layers { .. })
        ));
        assert_eq!(store.load_layers()?.len(), 1);
        assert!(matches!(
            store.undo()?.map(|entry| entry.0),
            Some(HistoryChange::Operations(_))
        ));
        assert!(store.load_active_operations()?.is_empty());

        assert!(matches!(
            store.redo()?.map(|entry| entry.0),
            Some(HistoryChange::Operations(_))
        ));
        assert!(matches!(
            store.redo()?.map(|entry| entry.0),
            Some(HistoryChange::Layers { .. })
        ));
        assert!(matches!(
            store.redo()?.map(|entry| entry.0),
            Some(HistoryChange::Layers { .. })
        ));
        assert!(matches!(
            store.redo()?.map(|entry| entry.0),
            Some(HistoryChange::Layers { .. })
        ));
        drop(store);

        let store = CanvasStore::open(&root)?;
        assert_eq!(store.load_active_operations()?.len(), 1);
        assert_eq!(store.load_layers()?[1].name, "Line art");
        assert!(!store.load_layers()?[1].visible);
        Ok(())
    }

    #[test]
    fn new_edit_replaces_the_unified_redo_branch() -> Result<()> {
        let temporary = TempDir::new()?;
        let mut store = CanvasStore::open(temporary.path().join("test.esketch"))?;
        let layer = store.create_layer("Ink")?;
        assert!(store.rename_layer(layer.id, "Line art")?);
        assert!(store.undo()?.is_some());

        let mut paint = operation();
        store.commit(&mut paint)?;

        assert!(store.redo()?.is_none());
        assert_eq!(store.load_layers()?[1].name, "Ink");
        assert_eq!(store.load_active_operations()?.len(), 1);
        Ok(())
    }

    #[test]
    fn duplicated_layer_operations_share_one_history_step_and_branch() -> Result<()> {
        let temporary = TempDir::new()?;
        let mut store = CanvasStore::open(temporary.path().join("test.esketch"))?;
        let source_layer = store.create_layer("Ink")?;
        let mut source = operation();
        source.layer_id = source_layer.id;
        store.commit(&mut source)?;

        let (duplicate, copied, _) = store
            .duplicate_layer(source_layer.id, std::slice::from_ref(&source))?
            .expect("source layer");
        assert_ne!(duplicate.id, source_layer.id);
        assert_eq!(duplicate.name, "Ink copy");
        assert_eq!(copied.len(), 1);
        assert_ne!(copied[0].id, source.id);
        assert_eq!(copied[0].layer_id, duplicate.id);
        assert_eq!(store.load_layers()?.len(), 3);
        assert_eq!(store.load_active_operations()?.len(), 2);

        let (HistoryChange::Layers { operations_changed }, _) =
            store.undo()?.expect("duplicate history")
        else {
            panic!("expected layer history");
        };
        assert!(operations_changed);
        assert_eq!(store.load_layers()?.len(), 2);
        assert_eq!(store.load_active_operations()?.len(), 1);
        let copied_id = copied[0].id;

        store.create_layer("New branch")?;
        assert!(store.redo()?.is_none());
        let copied_rows: i64 = store.connection.query_row(
            "SELECT COUNT(*) FROM operations WHERE operation_id = ?1",
            params![copied_id.as_bytes()],
            |row| row.get(0),
        )?;
        assert_eq!(copied_rows, 0);
        Ok(())
    }

    #[test]
    fn delete_layer_round_trips_and_rejects_the_last_layer() -> Result<()> {
        let temporary = TempDir::new()?;
        let mut store = CanvasStore::open(temporary.path().join("test.esketch"))?;
        assert!(store.delete_layer(DEFAULT_LAYER_ID).is_err());

        let layer = store.create_layer("Ink")?;
        let mut source = operation();
        source.layer_id = layer.id;
        store.commit(&mut source)?;
        assert!(store.delete_layer(layer.id)?.is_some());
        assert_eq!(store.load_layers()?, vec![Layer::default_layer()]);
        assert!(store.load_active_operations()?.is_empty());

        let (HistoryChange::Layers { operations_changed }, _) =
            store.undo()?.expect("delete history")
        else {
            panic!("expected layer history");
        };
        assert!(operations_changed);
        assert_eq!(store.load_layers()?.len(), 2);
        assert_eq!(store.load_active_operations()?.len(), 1);
        assert!(store.redo()?.is_some());
        assert_eq!(store.load_layers()?, vec![Layer::default_layer()]);
        assert!(store.load_active_operations()?.is_empty());
        Ok(())
    }

    #[test]
    fn persists_and_recovers_operations() -> Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        {
            let mut store = CanvasStore::open(&root)?;
            let mut operation = operation();
            store.commit(&mut operation)?;
            store.checkpoint()?;
        }
        let store = CanvasStore::open(&root)?;
        let operations = store.load_active_operations()?;
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].points.len(), 2);
        store.integrity_check()?;
        Ok(())
    }

    #[test]
    fn undo_redo_survives_reopen() -> Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut store = CanvasStore::open(&root)?;
        let mut operation = operation();
        store.commit(&mut operation)?;
        assert!(store.undo()?.is_some());
        drop(store);

        let mut store = CanvasStore::open(&root)?;
        assert!(store.load_active_operations()?.is_empty());
        assert!(store.redo()?.is_some());
        assert_eq!(store.load_active_operations()?.len(), 1);
        Ok(())
    }

    #[test]
    fn draft_is_recovered_after_unclean_shutdown() -> Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        {
            let store = CanvasStore::open(&root)?;
            store.save_draft(&operation())?;
        }
        let store = CanvasStore::open(&root)?;
        assert_eq!(store.load_active_operations()?.len(), 1);
        Ok(())
    }

    #[test]
    fn checkpoint_creates_a_verified_rotating_backup() -> Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut store = CanvasStore::open(&root)?;
        let mut operation = operation();
        store.commit(&mut operation)?;
        store.checkpoint()?;
        std::thread::sleep(std::time::Duration::from_millis(2));
        store.checkpoint()?;
        std::thread::sleep(std::time::Duration::from_millis(2));
        store.checkpoint()?;
        let backups: Vec<_> = fs::read_dir(root.join("backups"))?.collect::<Result<_, _>>()?;
        assert_eq!(backups.len(), 2);
        for backup in backups {
            let connection = Connection::open(backup.path())?;
            let result: String =
                connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
            assert_eq!(result, "ok");
        }
        Ok(())
    }

    #[test]
    fn bookmark_round_trip_preserves_bigint_camera() -> Result<()> {
        let temporary = TempDir::new()?;
        let store = CanvasStore::open(temporary.path().join("test.esketch"))?;
        let camera = CameraAddress {
            depth: 42,
            tile_x: BigInt::from(10u8).pow(100),
            tile_y: -BigInt::from(10u8).pow(100),
            ..CameraAddress::default()
        };
        store.save_bookmark("Deep", &camera)?;
        let bookmarks = store.load_bookmarks()?;
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].camera, camera);
        Ok(())
    }

    #[test]
    fn bookmark_can_be_renamed_and_deleted() -> Result<()> {
        let temporary = TempDir::new()?;
        let store = CanvasStore::open(temporary.path().join("test.esketch"))?;
        let bookmark = store.save_bookmark("Before", &CameraAddress::default())?;

        assert!(store.rename_bookmark(bookmark.id, "After")?);
        let bookmarks = store.load_bookmarks()?;
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].name, "After");

        assert!(store.delete_bookmark(bookmark.id)?);
        assert!(store.load_bookmarks()?.is_empty());
        assert!(!store.rename_bookmark(bookmark.id, "Missing")?);
        assert!(!store.delete_bookmark(bookmark.id)?);
        Ok(())
    }

    #[test]
    fn extreme_depth_operations_and_bookmarks_survive_reopen() -> Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        {
            let mut store = CanvasStore::open(&root)?;
            for depth in [-1000, -100, 100, 1000] {
                let mut operation = EditOperation::draft(
                    EditKind::Paint,
                    depth,
                    1.0,
                    vec![
                        CanvasPoint::new(
                            depth,
                            BigInt::from(10u8).pow(1000),
                            -BigInt::from(10u8).pow(1000),
                            0.1,
                            0.2,
                        ),
                        CanvasPoint::new(
                            depth,
                            BigInt::from(10u8).pow(1000),
                            -BigInt::from(10u8).pow(1000),
                            0.3,
                            0.4,
                        ),
                    ],
                    Color::BLACK,
                    5.0,
                );
                store.commit(&mut operation)?;
                store.save_bookmark(
                    &format!("Depth {depth}"),
                    &CameraAddress {
                        depth,
                        tile_x: BigInt::from(10u8).pow(1000),
                        tile_y: -BigInt::from(10u8).pow(1000),
                        ..CameraAddress::default()
                    },
                )?;
            }
            store.checkpoint()?;
        }

        let store = CanvasStore::open(&root)?;
        let operations = store.load_active_operations()?;
        let bookmarks = store.load_bookmarks()?;
        assert_eq!(
            operations
                .iter()
                .map(|operation| operation.native_depth)
                .collect::<Vec<_>>(),
            vec![-1000, -100, 100, 1000]
        );
        assert_eq!(
            bookmarks
                .iter()
                .map(|bookmark| bookmark.camera.depth)
                .collect::<Vec<_>>(),
            vec![-1000, -100, 100, 1000]
        );
        store.integrity_check()?;
        Ok(())
    }

    #[test]
    fn content_revision_is_monotonic_across_reopen_and_undo() -> Result<()> {
        let temporary = TempDir::new()?;
        let root = temporary.path().join("test.esketch");
        let mut store = CanvasStore::open(&root)?;
        assert_eq!(store.content_revision()?, 0);
        let mut first = operation();
        assert_eq!(store.commit(&mut first)?, 1);
        drop(store);

        let mut store = CanvasStore::open(&root)?;
        assert_eq!(store.content_revision()?, 1);
        let mut second = operation();
        assert_eq!(store.commit(&mut second)?, 2);
        let (_, undo_revision) = store.undo()?.expect("operation to undo");
        assert_eq!(undo_revision, 3);
        let (_, redo_revision) = store.redo()?.expect("operation to redo");
        assert_eq!(redo_revision, 4);
        Ok(())
    }
}
