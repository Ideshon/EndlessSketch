use crate::model::Color;
use crate::model::EditOperation;
use crate::raster::render_tile;
use anyhow::{Context, Result};
use image::{ImageFormat, RgbaImage};
use num_bigint::BigInt;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

pub const TILE_SIZE: u32 = 512;
pub const TILE_BLEED: u32 = 2;
pub const MAX_TILE_LOD: u8 = 2;
const MAX_CACHE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TileKey {
    pub depth: i64,
    pub x: BigInt,
    pub y: BigInt,
    pub lod: u8,
}

pub fn lod_scale(lod: u8) -> u32 {
    1 << lod.min(MAX_TILE_LOD)
}

#[derive(Clone)]
pub struct TileCache {
    root: PathBuf,
    stores: Arc<AtomicU64>,
}

impl TileCache {
    pub fn new(document_root: impl AsRef<Path>) -> Result<Self> {
        let root = document_root.as_ref().join("cache/tiles");
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            stores: Arc::new(AtomicU64::new(0)),
        })
    }

    pub fn path_for(&self, key: &TileKey, revision: u64) -> PathBuf {
        self.root
            .join(format!("d{}", key.depth))
            .join(format!("l{}", key.lod))
            .join(sanitize_bigint(&key.x))
            .join(format!("{}_r{}.png", sanitize_bigint(&key.y), revision))
    }

    pub fn load(&self, key: &TileKey, revision: u64) -> Result<Option<RgbaImage>> {
        let path = self.path_for(key, revision);
        if !path.exists() {
            return Ok(None);
        }
        match image::open(&path) {
            Ok(image) => Ok(Some(image.into_rgba8())),
            Err(error) => {
                log::warn!("discarding corrupt tile {}: {error}", path.display());
                let _ = fs::remove_file(path);
                Ok(None)
            }
        }
    }

    pub fn store_atomic(&self, key: &TileKey, revision: u64, image: &RgbaImage) -> Result<PathBuf> {
        let path = self.path_for(key, revision);
        let parent = path.parent().context("tile path has no parent")?;
        fs::create_dir_all(parent)?;
        let temporary = path.with_extension("png.tmp");
        image.save_with_format(&temporary, ImageFormat::Png)?;
        if path.exists() {
            fs::remove_file(&path)?;
        }
        fs::rename(&temporary, &path)?;
        self.remove_stale_revisions(key, &path)?;
        if self.stores.fetch_add(1, Ordering::Relaxed) % 64 == 63 {
            self.enforce_budget(MAX_CACHE_BYTES)?;
        }
        Ok(path)
    }

    pub fn rebuild(
        &self,
        key: &TileKey,
        revision: u64,
        operations: &[EditOperation],
        background: Color,
    ) -> Result<RgbaImage> {
        let image = render_tile(operations, key, background);
        self.store_atomic(key, revision, &image)?;
        Ok(image)
    }

    pub fn blank(background: Color) -> RgbaImage {
        RgbaImage::from_pixel(
            TILE_SIZE + TILE_BLEED * 2,
            TILE_SIZE + TILE_BLEED * 2,
            image::Rgba([background.r, background.g, background.b, background.a]),
        )
    }

    fn remove_stale_revisions(&self, key: &TileKey, current: &Path) -> Result<()> {
        let Some(parent) = current.parent() else {
            return Ok(());
        };
        let prefix = format!("{}_r", sanitize_bigint(&key.y));
        for entry in fs::read_dir(parent)? {
            let path = entry?.path();
            if path == current {
                continue;
            }
            let is_stale_revision = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".png"));
            if is_stale_revision {
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    fn enforce_budget(&self, budget: u64) -> Result<()> {
        let mut files = Vec::new();
        collect_png_files(&self.root, &mut files)?;
        let mut total: u64 = files.iter().map(|file| file.size).sum();
        if total <= budget {
            return Ok(());
        }
        files.sort_by_key(|file| file.modified);
        let target = budget.saturating_mul(9) / 10;
        for file in files {
            if total <= target {
                break;
            }
            if fs::remove_file(&file.path).is_ok() {
                total = total.saturating_sub(file.size);
            }
        }
        Ok(())
    }
}

struct CacheFile {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
}

fn collect_png_files(directory: &Path, output: &mut Vec<CacheFile>) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            collect_png_files(&path, output)?;
        } else if path.extension().is_some_and(|extension| extension == "png") {
            output.push(CacheFile {
                path,
                size: metadata.len(),
                modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            });
        }
    }
    Ok(())
}

fn sanitize_bigint(value: &BigInt) -> String {
    value.to_string().replace('-', "n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn corrupt_png_is_rebuildable_cache_miss() -> Result<()> {
        let temporary = TempDir::new()?;
        let cache = TileCache::new(temporary.path())?;
        let key = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: 0,
        };
        let image = TileCache::blank(Color::WHITE);
        let path = cache.store_atomic(&key, 1, &image)?;
        fs::write(&path, b"corrupt")?;
        assert!(cache.load(&key, 1)?.is_none());
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn newer_revision_replaces_old_tile_revision() -> Result<()> {
        let temporary = TempDir::new()?;
        let cache = TileCache::new(temporary.path())?;
        let key = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: 0,
        };
        let image = TileCache::blank(Color::WHITE);
        let old = cache.store_atomic(&key, 1, &image)?;
        let new = cache.store_atomic(&key, 2, &image)?;
        assert!(!old.exists());
        assert!(new.exists());
        Ok(())
    }

    #[test]
    fn cache_budget_removes_oldest_png_files() -> Result<()> {
        let temporary = TempDir::new()?;
        let cache = TileCache::new(temporary.path())?;
        let first = cache.root.join("first.png");
        let second = cache.root.join("second.png");
        fs::write(&first, vec![0; 100])?;
        std::thread::sleep(std::time::Duration::from_millis(2));
        fs::write(&second, vec![0; 100])?;
        cache.enforce_budget(150)?;
        assert!(!first.exists());
        assert!(second.exists());
        Ok(())
    }
}
