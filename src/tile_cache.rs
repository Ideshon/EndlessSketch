use crate::model::Color;
use crate::model::EditOperation;
use crate::raster::{
    RasterOptions, render_operations_onto_tile_with_options, render_tile_with_options,
};
use anyhow::{Context, Result};
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder, RgbaImage};
use num_bigint::BigInt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

pub const TILE_SIZE: u32 = 512;
pub const TILE_BLEED: u32 = 2;
pub const TILE_RESOLUTIONS: [u32; 6] = [64, 128, 256, 512, 1024, 2048];
pub const DEFAULT_TILE_RESOLUTION: u32 = 512;
pub const MAX_TILE_LOD: u8 = TILE_RESOLUTIONS.len() as u8 - 1;
const TILE_RENDERER_CACHE_VERSION: u32 = 8;
const DEFAULT_MAX_CACHE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum PngCompression {
    #[default]
    Fast,
    Balanced,
    Small,
}

impl PngCompression {
    fn encoder_type(self) -> CompressionType {
        match self {
            Self::Fast => CompressionType::Fast,
            Self::Balanced => CompressionType::Default,
            Self::Small => CompressionType::Best,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileCacheOptions {
    pub render_options: RasterOptions,
    pub max_cache_bytes: u64,
    pub png_compression: PngCompression,
}

impl Default for TileCacheOptions {
    fn default() -> Self {
        Self {
            render_options: RasterOptions::default(),
            max_cache_bytes: DEFAULT_MAX_CACHE_BYTES,
            png_compression: PngCompression::Fast,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TileKey {
    pub depth: i64,
    pub x: BigInt,
    pub y: BigInt,
    pub lod: u8,
}

pub fn tile_resolution(lod: u8) -> u32 {
    TILE_RESOLUTIONS[usize::from(lod.min(MAX_TILE_LOD))]
}

pub fn tile_lod_for_resolution(resolution: u32) -> u8 {
    TILE_RESOLUTIONS
        .iter()
        .position(|candidate| *candidate == resolution)
        .map_or_else(
            || {
                TILE_RESOLUTIONS
                    .iter()
                    .position(|candidate| *candidate == DEFAULT_TILE_RESOLUTION)
                    .expect("default tile resolution must be supported") as u8
            },
            |index| index as u8,
        )
}

pub fn nearest_tile_resolution(resolution: u32) -> u32 {
    TILE_RESOLUTIONS
        .iter()
        .copied()
        .min_by_key(|candidate| candidate.abs_diff(resolution))
        .expect("tile resolutions must not be empty")
}

#[derive(Clone)]
pub struct TileCache {
    root: PathBuf,
    stores: Arc<AtomicU64>,
    render_options: RasterOptions,
    max_cache_bytes: u64,
    png_compression: PngCompression,
}

impl TileCache {
    pub fn new(document_root: impl AsRef<Path>) -> Result<Self> {
        Self::with_render_options(document_root, RasterOptions::default())
    }

    pub fn with_render_options(
        document_root: impl AsRef<Path>,
        render_options: RasterOptions,
    ) -> Result<Self> {
        Self::with_options(
            document_root,
            TileCacheOptions {
                render_options,
                ..TileCacheOptions::default()
            },
        )
    }

    pub fn with_options(
        document_root: impl AsRef<Path>,
        options: TileCacheOptions,
    ) -> Result<Self> {
        let root = document_root.as_ref().join("cache/tiles");
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            stores: Arc::new(AtomicU64::new(0)),
            render_options: options.render_options,
            max_cache_bytes: options.max_cache_bytes,
            png_compression: options.png_compression,
        })
    }

    pub fn path_for(&self, key: &TileKey, revision: u64) -> PathBuf {
        self.root
            .join(format!("d{}", key.depth))
            .join(format!("p{}", tile_resolution(key.lod)))
            .join(self.render_options.cache_tag())
            .join(sanitize_bigint(&key.x))
            .join(format!(
                "{}_rv{}_r{}.png",
                sanitize_bigint(&key.y),
                TILE_RENDERER_CACHE_VERSION,
                revision
            ))
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
        let file = File::create(&temporary)?;
        let writer = BufWriter::new(file);
        PngEncoder::new_with_quality(
            writer,
            self.png_compression.encoder_type(),
            FilterType::Adaptive,
        )
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgba8,
        )?;
        if path.exists() {
            fs::remove_file(&path)?;
        }
        fs::rename(&temporary, &path)?;
        self.remove_stale_revisions(key, &path)?;
        if self.stores.fetch_add(1, Ordering::Relaxed) % 64 == 63 {
            self.enforce_budget(self.max_cache_bytes)?;
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
        let image = render_tile_with_options(operations, key, background, self.render_options);
        self.store_atomic(key, revision, &image)?;
        Ok(image)
    }

    pub fn rebuild_incremental(
        &self,
        key: &TileKey,
        base_revision: u64,
        revision: u64,
        operations: &[EditOperation],
        background: Color,
    ) -> Result<Option<RgbaImage>> {
        let Some(mut image) = self.load(key, base_revision)? else {
            return Ok(None);
        };
        let expected_size = tile_resolution(key.lod) + TILE_BLEED * 2;
        if image.width() != expected_size || image.height() != expected_size {
            return Ok(None);
        }
        render_operations_onto_tile_with_options(
            &mut image,
            operations,
            key,
            background,
            self.render_options,
        );
        self.store_atomic(key, revision, &image)?;
        Ok(Some(image))
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
    use crate::raster::{RasterOptions, render_tile};
    use image::ImageFormat;
    use tempfile::TempDir;

    #[test]
    fn tile_resolution_indices_round_trip_all_supported_sizes() {
        for (index, resolution) in TILE_RESOLUTIONS.into_iter().enumerate() {
            let lod = tile_lod_for_resolution(resolution);
            assert_eq!(lod, index as u8);
            assert_eq!(tile_resolution(lod), resolution);
        }
        assert_eq!(nearest_tile_resolution(300), 256);
        assert_eq!(
            tile_lod_for_resolution(300),
            tile_lod_for_resolution(DEFAULT_TILE_RESOLUTION)
        );
    }

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
    fn renderer_version_invalidates_legacy_tile_paths() -> Result<()> {
        let temporary = TempDir::new()?;
        let cache = TileCache::new(temporary.path())?;
        let key = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: 0,
        };
        let current = cache.path_for(&key, 7);
        let legacy = current.parent().expect("tile path parent").join("0_r7.png");
        fs::create_dir_all(legacy.parent().expect("legacy tile parent"))?;
        TileCache::blank(Color::WHITE).save_with_format(&legacy, ImageFormat::Png)?;

        assert!(legacy.exists());
        assert!(
            current
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("_rv8_r7.png"))
        );
        assert!(current.to_string_lossy().contains("p64"));
        assert!(cache.load(&key, 7)?.is_none());
        Ok(())
    }

    #[test]
    fn render_quality_uses_separate_cache_namespaces() -> Result<()> {
        let temporary = TempDir::new()?;
        let key = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: 0,
        };
        let performance =
            TileCache::with_render_options(temporary.path(), RasterOptions::new(0, 0))?;
        let balanced = TileCache::with_render_options(temporary.path(), RasterOptions::new(1, 2))?;
        let quality = TileCache::with_render_options(temporary.path(), RasterOptions::new(2, 3))?;

        assert_ne!(performance.path_for(&key, 1), balanced.path_for(&key, 1));
        assert_ne!(balanced.path_for(&key, 1), quality.path_for(&key, 1));
        assert!(
            performance
                .path_for(&key, 1)
                .to_string_lossy()
                .contains("e0_s0")
        );
        assert!(
            quality
                .path_for(&key, 1)
                .to_string_lossy()
                .contains("e2_s3")
        );
        Ok(())
    }

    #[test]
    fn configured_cache_budget_and_png_modes_are_applied() -> Result<()> {
        assert_eq!(PngCompression::Fast.encoder_type(), CompressionType::Fast);
        assert_eq!(
            PngCompression::Balanced.encoder_type(),
            CompressionType::Default
        );
        assert_eq!(PngCompression::Small.encoder_type(), CompressionType::Best);

        let temporary = TempDir::new()?;
        let key = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: 0,
        };
        let image = RgbaImage::from_fn(66, 66, |x, y| {
            image::Rgba([(x % 255) as u8, (y % 255) as u8, ((x + y) % 255) as u8, 255])
        });

        for (index, compression) in [
            PngCompression::Fast,
            PngCompression::Balanced,
            PngCompression::Small,
        ]
        .into_iter()
        .enumerate()
        {
            let cache = TileCache::with_options(
                temporary.path().join(index.to_string()),
                TileCacheOptions {
                    max_cache_bytes: 512 * 1024 * 1024,
                    png_compression: compression,
                    ..TileCacheOptions::default()
                },
            )?;
            assert_eq!(cache.max_cache_bytes, 512 * 1024 * 1024);
            assert_eq!(cache.png_compression, compression);
            cache.store_atomic(&key, 1, &image)?;
            assert_eq!(cache.load(&key, 1)?, Some(image.clone()));
        }
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

    #[test]
    fn incremental_rebuild_requires_a_valid_matching_base_image() -> Result<()> {
        let temporary = TempDir::new()?;
        let cache = TileCache::new(temporary.path())?;
        let key = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: tile_lod_for_resolution(64),
        };

        assert!(
            cache
                .rebuild_incremental(&key, 1, 2, &[], Color::WHITE)?
                .is_none()
        );

        let wrong_size = RgbaImage::new(10, 10);
        cache.store_atomic(&key, 1, &wrong_size)?;
        assert!(
            cache
                .rebuild_incremental(&key, 1, 2, &[], Color::WHITE)?
                .is_none()
        );

        let valid = render_tile(&[], &key, Color::WHITE);
        cache.store_atomic(&key, 1, &valid)?;
        let rebuilt = cache
            .rebuild_incremental(&key, 1, 2, &[], Color::WHITE)?
            .expect("valid incremental base");
        assert_eq!(rebuilt, valid);
        assert!(cache.path_for(&key, 2).exists());
        Ok(())
    }
}
