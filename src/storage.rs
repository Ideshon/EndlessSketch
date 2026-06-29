use crate::coords::CameraAddress;
use crate::model::{Bookmark, EditOperation};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 1;
const MAX_UNDO_OPERATIONS: i64 = 10_000;

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
        store.initialize_schema()?;
        store.recover_drafts()?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn load_active_operations(&self) -> Result<Vec<EditOperation>> {
        let mut statement = self
            .connection
            .prepare("SELECT payload FROM operations WHERE undone = 0 ORDER BY sequence ASC")?;
        let rows = statement.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
        rows.map(|payload| decode_operation(&payload?)).collect()
    }

    pub fn commit(&mut self, operation: &mut EditOperation) -> Result<u64> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM operations WHERE undone = 1", [])?;
        let next_sequence: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM operations",
            [],
            |row| row.get(0),
        )?;
        operation.sequence = next_sequence;
        operation.affects_before_sequence = operation.destructive.then_some(next_sequence - 1);
        let payload = encode_operation(operation)?;
        transaction.execute(
            "INSERT INTO operations(sequence, operation_id, payload, undone) VALUES (?1, ?2, ?3, 0)",
            params![next_sequence, operation.id.as_bytes(), payload],
        )?;
        transaction.execute(
            "DELETE FROM drafts WHERE operation_id = ?1",
            params![operation.id.as_bytes()],
        )?;
        transaction.execute(
            "DELETE FROM operations WHERE sequence < (SELECT MAX(sequence) - ?1 FROM operations) AND undone = 1",
            params![MAX_UNDO_OPERATIONS],
        )?;
        transaction.execute(
            "UPDATE metadata SET value = CAST(value AS INTEGER) + 1 WHERE key='content_revision'",
            [],
        )?;
        let revision: i64 = transaction.query_row(
            "SELECT CAST(value AS INTEGER) FROM metadata WHERE key='content_revision'",
            [],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(revision.try_into()?)
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

    pub fn undo(&mut self) -> Result<Option<(EditOperation, u64)>> {
        let transaction = self.connection.transaction()?;
        let payload: Option<Vec<u8>> = transaction
            .query_row(
                "SELECT payload FROM operations WHERE undone = 0 ORDER BY sequence DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let Some(payload) = payload else {
            return Ok(None);
        };
        let operation = decode_operation(&payload)?;
        transaction.execute(
            "UPDATE operations SET undone = 1 WHERE sequence = ?1",
            params![operation.sequence],
        )?;
        transaction.execute(
            "UPDATE metadata SET value = CAST(value AS INTEGER) + 1 WHERE key='content_revision'",
            [],
        )?;
        let revision: i64 = transaction.query_row(
            "SELECT CAST(value AS INTEGER) FROM metadata WHERE key='content_revision'",
            [],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(Some((operation, revision.try_into()?)))
    }

    pub fn redo(&mut self) -> Result<Option<(EditOperation, u64)>> {
        let transaction = self.connection.transaction()?;
        let payload: Option<Vec<u8>> = transaction
            .query_row(
                "SELECT payload FROM operations WHERE undone = 1 ORDER BY sequence ASC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let Some(payload) = payload else {
            return Ok(None);
        };
        let operation = decode_operation(&payload)?;
        transaction.execute(
            "UPDATE operations SET undone = 0 WHERE sequence = ?1",
            params![operation.sequence],
        )?;
        transaction.execute(
            "UPDATE metadata SET value = CAST(value AS INTEGER) + 1 WHERE key='content_revision'",
            [],
        )?;
        let revision: i64 = transaction.query_row(
            "SELECT CAST(value AS INTEGER) FROM metadata WHERE key='content_revision'",
            [],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(Some((operation, revision.try_into()?)))
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
             CREATE TABLE IF NOT EXISTS operations (
                sequence INTEGER PRIMARY KEY,
                operation_id BLOB NOT NULL UNIQUE,
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
        let version: i64 = self.connection.query_row(
            "SELECT CAST(value AS INTEGER) FROM metadata WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )?;
        if version != SCHEMA_VERSION {
            bail!("unsupported canvas schema {version}");
        }
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
        b"{\n  \"format\": \"EndlessSketch\",\n  \"schema_version\": 1,\n  \"tile_cache_is_rebuildable\": true\n}\n",
    )?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn encode_operation(operation: &EditOperation) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(operation)?;
    Ok(zstd::stream::encode_all(Cursor::new(json), 3)?)
}

fn decode_operation(payload: &[u8]) -> Result<EditOperation> {
    let json = zstd::stream::decode_all(Cursor::new(payload))?;
    Ok(serde_json::from_slice(&json)?)
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
