use crate::model::{Color, EditOperation};
use crate::tile_cache::{TileCache, TileKey};
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, unbounded};
use image::RgbaImage;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_QUEUED_TILE_JOBS: usize = 256;

pub struct IncrementalTileUpdate {
    pub base_revision: u64,
    pub operations: Arc<Vec<EditOperation>>,
}

pub struct TileJob {
    pub key: TileKey,
    pub revision: u64,
    pub snapshot_sequence: i64,
    pub generation: u64,
    pub operations: Arc<Vec<EditOperation>>,
    pub incremental_update: Option<IncrementalTileUpdate>,
    pub background: Color,
}

pub struct TileResult {
    pub key: TileKey,
    pub revision: u64,
    pub snapshot_sequence: i64,
    pub generation: u64,
    pub result: Result<RgbaImage, String>,
}

pub struct TileScheduler {
    requests: Sender<TileJob>,
    results: Receiver<TileResult>,
    latest_generation: Arc<AtomicU64>,
}

impl TileScheduler {
    pub fn new(cache: TileCache, worker_count: usize) -> Self {
        let (request_sender, request_receiver) = bounded::<TileJob>(MAX_QUEUED_TILE_JOBS);
        let (result_sender, result_receiver) = unbounded::<TileResult>();
        let latest_generation = Arc::new(AtomicU64::new(0));
        let worker_count = worker_count.clamp(1, 4);
        for worker_index in 0..worker_count {
            std::thread::Builder::new()
                .name(format!("tile-renderer-{worker_index}"))
                .spawn({
                    let cache = cache.clone();
                    let request_receiver = request_receiver.clone();
                    let result_sender = result_sender.clone();
                    let latest_generation = Arc::clone(&latest_generation);
                    move || worker(cache, request_receiver, result_sender, latest_generation)
                })
                .expect("failed to create tile renderer thread");
        }
        Self {
            requests: request_sender,
            results: result_receiver,
            latest_generation,
        }
    }

    pub fn set_generation(&self, generation: u64) {
        self.latest_generation.store(generation, Ordering::Relaxed);
    }

    pub fn request(&self, job: TileJob) -> Result<(), String> {
        self.requests.try_send(job).map_err(|error| match error {
            TrySendError::Full(_) => "tile queue is full".to_owned(),
            TrySendError::Disconnected(_) => "tile renderer is stopped".to_owned(),
        })
    }

    pub fn try_result(&self) -> Option<TileResult> {
        self.results.try_recv().ok()
    }
}

fn worker(
    cache: TileCache,
    requests: Receiver<TileJob>,
    results: Sender<TileResult>,
    latest_generation: Arc<AtomicU64>,
) {
    while let Ok(job) = requests.recv() {
        if job.generation != latest_generation.load(Ordering::Relaxed) {
            continue;
        }
        let result = render_job(&cache, &job).map_err(|error| format!("{error:#}"));
        if job.generation != latest_generation.load(Ordering::Relaxed) {
            continue;
        }
        if results
            .send(TileResult {
                key: job.key,
                revision: job.revision,
                snapshot_sequence: job.snapshot_sequence,
                generation: job.generation,
                result,
            })
            .is_err()
        {
            break;
        }
    }
}

fn render_job(cache: &TileCache, job: &TileJob) -> anyhow::Result<RgbaImage> {
    if let Some(image) = cache.load(&job.key, job.revision)? {
        return Ok(image);
    }
    if let Some(update) = &job.incremental_update
        && let Some(image) = cache.rebuild_incremental(
            &job.key,
            update.base_revision,
            job.revision,
            &update.operations,
            job.background,
        )?
    {
        return Ok(image);
    }
    cache.rebuild(&job.key, job.revision, &job.operations, job.background)
}

#[cfg(test)]
mod tests {
    use super::{IncrementalTileUpdate, TileJob, TileScheduler};
    use crate::model::Color;
    use crate::tile_cache::{TileCache, TileKey, tile_lod_for_resolution};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn stale_generation_is_skipped_before_cache_write() {
        let temporary = tempfile::tempdir().expect("temp dir");
        let cache = TileCache::new(temporary.path()).expect("tile cache");
        let stale_key = TileKey {
            depth: 0,
            x: 0.into(),
            y: 0.into(),
            lod: tile_lod_for_resolution(64),
        };
        let current_key = TileKey {
            depth: 0,
            x: 1.into(),
            y: 0.into(),
            lod: tile_lod_for_resolution(64),
        };
        let scheduler = TileScheduler::new(cache.clone(), 1);
        scheduler.set_generation(1);
        scheduler
            .request(TileJob {
                key: stale_key.clone(),
                revision: 1,
                snapshot_sequence: 10,
                generation: 0,
                operations: Arc::new(Vec::new()),
                incremental_update: None,
                background: Color::WHITE,
            })
            .expect("queue stale job");
        scheduler
            .request(TileJob {
                key: current_key.clone(),
                revision: 1,
                snapshot_sequence: 20,
                generation: 1,
                operations: Arc::new(Vec::new()),
                incremental_update: Some(IncrementalTileUpdate {
                    base_revision: 99,
                    operations: Arc::new(Vec::new()),
                }),
                background: Color::WHITE,
            })
            .expect("queue current job");

        let deadline = Instant::now() + Duration::from_secs(2);
        let result = loop {
            if let Some(result) = scheduler.try_result() {
                break result;
            }
            assert!(Instant::now() < deadline, "current tile job timed out");
            std::thread::sleep(Duration::from_millis(5));
        };

        assert_eq!(result.key, current_key);
        assert_eq!(result.snapshot_sequence, 20);
        assert!(result.result.is_ok());
        assert!(!cache.path_for(&stale_key, 1).exists());
        assert!(cache.path_for(&current_key, 1).exists());
    }
}
