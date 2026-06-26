use crate::coords::CameraAddress;
use crate::model::{Bookmark, EditOperation};
use crate::spatial::OperationIndex;
use crate::storage::CanvasStore;
use crate::tile_cache::TileKey;
use anyhow::Result;
use std::path::Path;
use uuid::Uuid;

pub struct CanvasDocument {
    store: CanvasStore,
    operations: Vec<EditOperation>,
    revision: u64,
    index: OperationIndex,
}

impl CanvasDocument {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let store = CanvasStore::open(path)?;
        let operations = store.load_active_operations()?;
        let index = OperationIndex::build(&operations);
        let revision = store.content_revision()?;
        Ok(Self {
            store,
            operations,
            revision,
            index,
        })
    }

    pub fn operations(&self) -> &[EditOperation] {
        &self.operations
    }

    pub fn root(&self) -> &Path {
        self.store.root()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn operations_for_tile(&self, key: &TileKey) -> Vec<EditOperation> {
        self.index
            .query(key)
            .into_iter()
            .filter_map(|index| self.operations.get(index).cloned())
            .collect()
    }

    pub fn save_draft(&self, operation: &EditOperation) -> Result<()> {
        self.store.save_draft(operation)
    }

    pub fn discard_draft(&self, operation: &EditOperation) -> Result<()> {
        self.store.discard_draft(operation)
    }

    pub fn commit(&mut self, mut operation: EditOperation) -> Result<()> {
        self.revision = self.store.commit(&mut operation)?;
        self.operations.push(operation);
        let index = self.operations.len() - 1;
        self.index.insert(index, &self.operations[index]);
        Ok(())
    }

    pub fn undo(&mut self) -> Result<bool> {
        let Some((_, revision)) = self.store.undo()? else {
            return Ok(false);
        };
        self.operations = self.store.load_active_operations()?;
        self.index = OperationIndex::build(&self.operations);
        self.revision = revision;
        Ok(true)
    }

    pub fn redo(&mut self) -> Result<bool> {
        let Some((_, revision)) = self.store.redo()? else {
            return Ok(false);
        };
        self.operations = self.store.load_active_operations()?;
        self.index = OperationIndex::build(&self.operations);
        self.revision = revision;
        Ok(true)
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
