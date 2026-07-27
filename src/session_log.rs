use crate::settings::SessionLoggingLevel;
use anyhow::{Context, Result};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

const SESSION_LOG_KEEP_COUNT: usize = 20;
const NAVIGATION_LOG_INTERVAL: Duration = Duration::from_millis(250);
const PHASE_AGGREGATE_INTERVAL: Duration = Duration::from_secs(1);

pub const SLOW_FRAME_MS: f64 = 100.0;
pub const LOW_FPS_EMA: f32 = 15.0;
pub const SLOW_PHASE_MS: f64 = 50.0;

#[derive(Debug, Default)]
struct PhaseAggregate {
    count: u64,
    sum_ms: BTreeMap<String, f64>,
    max_ms: BTreeMap<String, f64>,
}

impl PhaseAggregate {
    fn push(&mut self, phases: &BTreeMap<String, f64>) {
        self.count = self.count.saturating_add(1);
        for (name, ms) in phases {
            *self.sum_ms.entry(name.clone()).or_insert(0.0) += *ms;
            self.max_ms
                .entry(name.clone())
                .and_modify(|current| *current = current.max(*ms))
                .or_insert(*ms);
        }
    }

    fn snapshot(&self) -> Value {
        let mut phases = Map::new();
        for (name, sum) in &self.sum_ms {
            let average = if self.count == 0 {
                0.0
            } else {
                *sum / self.count as f64
            };
            phases.insert(
                name.clone(),
                json!({
                    "avg_ms": average,
                    "max_ms": self.max_ms.get(name).copied().unwrap_or(0.0),
                }),
            );
        }
        json!({
            "frame_count": self.count,
            "phases": phases,
        })
    }

    fn clear(&mut self) {
        self.count = 0;
        self.sum_ms.clear();
        self.max_ms.clear();
    }
}

#[derive(Debug)]
pub struct SessionLogger {
    level: SessionLoggingLevel,
    launch_time: Instant,
    log_dir: PathBuf,
    path: Option<PathBuf>,
    writer: Option<BufWriter<File>>,
    next_marker_id: u64,
    last_navigation_emit: Option<Instant>,
    pending_navigation: Option<(String, Value, Instant)>,
    last_phase_emit: Instant,
    phase_aggregate: PhaseAggregate,
    last_fps_drop_emit: Option<Instant>,
}

impl SessionLogger {
    pub fn new(log_dir: PathBuf, level: SessionLoggingLevel) -> Self {
        let launch_time = Instant::now();
        let mut logger = Self {
            level: SessionLoggingLevel::Off,
            launch_time,
            log_dir,
            path: None,
            writer: None,
            next_marker_id: 1,
            last_navigation_emit: None,
            pending_navigation: None,
            last_phase_emit: launch_time,
            phase_aggregate: PhaseAggregate::default(),
            last_fps_drop_emit: None,
        };
        if let Err(error) = logger.set_level(level) {
            log::warn!("Session logging disabled: {error:#}");
        }
        logger
    }

    pub fn level(&self) -> SessionLoggingLevel {
        self.level
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn set_level(&mut self, level: SessionLoggingLevel) -> Result<()> {
        if level == self.level {
            return Ok(());
        }
        self.flush_navigation();
        self.flush()?;
        self.level = level;
        if level == SessionLoggingLevel::Off {
            self.writer = None;
            self.path = None;
            return Ok(());
        }

        self.open_writer()
    }

    pub fn log_error_event(&mut self, message: &str, fields: Value) {
        self.flush_navigation();
        if self.writer.is_none()
            && let Err(error) = self.open_writer()
        {
            log::warn!("Session error logging disabled: {error:#}");
            return;
        }
        self.write_event_unchecked(
            "error",
            with_field(fields, "error", json!({ "message": message })),
        );
    }

    fn open_writer(&mut self) -> Result<()> {
        fs::create_dir_all(&self.log_dir).with_context(|| {
            format!("failed to create log directory {}", self.log_dir.display())
        })?;
        let path = unique_session_log_path(&self.log_dir);
        let file = File::create(&path)
            .with_context(|| format!("failed to create session log {}", path.display()))?;
        self.writer = Some(BufWriter::new(file));
        self.path = Some(path);
        retain_newest_session_logs(&self.log_dir, SESSION_LOG_KEEP_COUNT)?;
        Ok(())
    }

    pub fn log_event(&mut self, event: &str, fields: Value) {
        self.flush_due_navigation(Instant::now());
        self.write_event(event, fields);
    }

    pub fn log_marker(&mut self, fields: Value) -> u64 {
        self.flush_navigation();
        let marker_id = self.next_marker_id;
        self.next_marker_id = self.next_marker_id.saturating_add(1);
        let mut fields = object_or_empty(fields);
        fields.insert("marker_id".to_owned(), json!(marker_id));
        self.write_event("marker", Value::Object(fields));
        marker_id
    }

    pub fn log_navigation(&mut self, label: &str, fields: Value) {
        if self.level == SessionLoggingLevel::Off {
            return;
        }
        let now = Instant::now();
        if self
            .last_navigation_emit
            .is_none_or(|last| now.duration_since(last) >= NAVIGATION_LOG_INTERVAL)
        {
            self.pending_navigation = None;
            self.last_navigation_emit = Some(now);
            self.write_event(
                "navigation",
                with_field(fields, "navigation", json!({ "kind": label })),
            );
        } else {
            self.pending_navigation = Some((label.to_owned(), fields, now));
        }
    }

    pub fn flush_due_navigation(&mut self, now: Instant) {
        let Some((_, _, pending_at)) = self.pending_navigation.as_ref() else {
            return;
        };
        if now.duration_since(*pending_at) >= NAVIGATION_LOG_INTERVAL {
            self.flush_navigation();
        }
    }

    pub fn flush_navigation(&mut self) {
        let Some((label, fields, _)) = self.pending_navigation.take() else {
            return;
        };
        self.last_navigation_emit = Some(Instant::now());
        self.write_event(
            "navigation",
            with_field(
                fields,
                "navigation",
                json!({ "kind": label, "coalesced": true }),
            ),
        );
    }

    pub fn record_frame_phases(
        &mut self,
        interaction_active: bool,
        phases: &BTreeMap<String, f64>,
        fields: &Value,
    ) {
        if self.level == SessionLoggingLevel::Off {
            return;
        }
        let now = Instant::now();
        for (name, ms) in phases {
            let threshold = if name == "total_frame" {
                SLOW_FRAME_MS
            } else {
                SLOW_PHASE_MS
            };
            if *ms >= threshold {
                self.write_event(
                    "slow_phase",
                    with_field(
                        fields.clone(),
                        "perf",
                        json!({
                            "phase": name,
                            "phase_ms": ms,
                            "phases": phases,
                        }),
                    ),
                );
            }
        }

        if !interaction_active {
            self.phase_aggregate.clear();
            self.last_phase_emit = now;
            return;
        }
        self.phase_aggregate.push(phases);
        if now.duration_since(self.last_phase_emit) >= PHASE_AGGREGATE_INTERVAL {
            let payload = with_field(fields.clone(), "perf", self.phase_aggregate.snapshot());
            self.write_event("phase_aggregate", payload);
            self.phase_aggregate.clear();
            self.last_phase_emit = now;
        }
    }

    pub fn log_fps_drop(&mut self, frame_ms: f32, fps_ema: Option<f32>, fields: Value) {
        if self.level == SessionLoggingLevel::Off {
            return;
        }
        let now = Instant::now();
        if self
            .last_fps_drop_emit
            .is_some_and(|last| now.duration_since(last) < Duration::from_secs(1))
        {
            return;
        }
        self.last_fps_drop_emit = Some(now);
        self.write_event(
            "fps_drop",
            with_field(
                fields,
                "perf",
                json!({
                    "frame_ms": frame_ms,
                    "fps_ema": fps_ema,
                }),
            ),
        );
    }

    pub fn flush(&mut self) -> Result<()> {
        if let Some(writer) = self.writer.as_mut() {
            writer.flush()?;
        }
        Ok(())
    }

    fn write_event(&mut self, event: &str, fields: Value) {
        if self.level == SessionLoggingLevel::Off {
            return;
        }
        self.write_event_unchecked(event, fields);
    }

    fn write_event_unchecked(&mut self, event: &str, fields: Value) {
        let Some(writer) = self.writer.as_mut() else {
            return;
        };
        let mut root = object_or_empty(fields);
        root.insert(
            "t_ms".to_owned(),
            json!(self.launch_time.elapsed().as_millis()),
        );
        root.insert("wall_time".to_owned(), json!(local_wall_time()));
        root.insert("event".to_owned(), json!(event));
        if serde_json::to_writer(&mut *writer, &Value::Object(root)).is_ok() {
            let _ = writer.write_all(b"\n");
        }
    }
}

pub fn retain_newest_session_logs(log_dir: &Path, keep: usize) -> Result<()> {
    let mut entries = Vec::new();
    if !log_dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(log_dir)
        .with_context(|| format!("failed to read log directory {}", log_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !is_session_log_path(&path) {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        entries.push((modified, path));
    }
    entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    for (_, path) in entries.into_iter().skip(keep) {
        let _ = fs::remove_file(path);
    }
    Ok(())
}

fn unique_session_log_path(log_dir: &Path) -> PathBuf {
    let stem = format!("session-{}", compact_local_wall_time());
    let mut path = log_dir.join(format!("{stem}.jsonl"));
    for suffix in 1..100 {
        if !path.exists() {
            return path;
        }
        path = log_dir.join(format!("{stem}-{suffix}.jsonl"));
    }
    path
}

fn is_session_log_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("session-") && name.ends_with(".jsonl"))
}

fn object_or_empty(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn with_field(value: Value, key: &str, field: Value) -> Value {
    let mut map = object_or_empty(value);
    let merged = match (map.remove(key), field) {
        (Some(Value::Object(mut existing)), Value::Object(new)) => {
            existing.extend(new);
            Value::Object(existing)
        }
        (_, field) => field,
    };
    map.insert(key.to_owned(), merged);
    Value::Object(map)
}

#[cfg(windows)]
fn local_wall_time() -> String {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;

    let mut time = SYSTEMTIME::default();
    // SAFETY: GetLocalTime writes to the provided SYSTEMTIME pointer and has no aliasing requirements.
    unsafe {
        GetLocalTime(&mut time);
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}",
        time.wYear,
        time.wMonth,
        time.wDay,
        time.wHour,
        time.wMinute,
        time.wSecond,
        time.wMilliseconds
    )
}

#[cfg(not(windows))]
fn local_wall_time() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("unix_ms:{millis}")
}

fn compact_local_wall_time() -> String {
    let digits: String = local_wall_time()
        .chars()
        .filter(|ch| ch.is_ascii_digit())
        .take(14)
        .collect();
    if digits.len() >= 14 {
        format!("{}-{}", &digits[..8], &digits[8..14])
    } else {
        digits
    }
}

#[cfg(test)]
mod tests {
    use super::{SessionLogger, is_session_log_path, retain_newest_session_logs};
    use crate::settings::SessionLoggingLevel;
    use serde_json::{Value, json};
    use std::collections::BTreeMap;
    use std::fs;
    use std::time::Duration;

    fn read_session_events(logger: &SessionLogger) -> Vec<Value> {
        let path = logger.path().expect("log path").to_owned();
        let text = fs::read_to_string(path).expect("read log");
        text.lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("event"))
            .collect()
    }

    #[test]
    fn json_event_serialization_is_valid() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let mut logger =
            SessionLogger::new(temp_dir.path().join("logs"), SessionLoggingLevel::Basic);
        logger.log_event(
            "launch",
            json!({ "context": { "status_message": "Ready" } }),
        );
        logger.flush().expect("flush");
        let events = read_session_events(&logger);
        let event = events.first().expect("one event");
        assert_eq!(event["event"], "launch");
        assert!(event["t_ms"].is_number());
        assert!(event["wall_time"].is_string());
    }

    #[test]
    fn retention_keeps_newest_session_logs() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let log_dir = temp_dir.path();
        for index in 0..25 {
            fs::write(
                log_dir.join(format!("session-20260710-{:06}.jsonl", index)),
                b"{}\n",
            )
            .expect("write log");
            std::thread::sleep(Duration::from_millis(2));
        }

        retain_newest_session_logs(log_dir, 20).expect("retain logs");

        let remaining = fs::read_dir(log_dir)
            .expect("read dir")
            .filter_map(Result::ok)
            .filter(|entry| is_session_log_path(&entry.path()))
            .count();
        assert_eq!(remaining, 20);
    }

    #[test]
    fn marker_ids_increment() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let mut logger =
            SessionLogger::new(temp_dir.path().join("logs"), SessionLoggingLevel::Basic);

        assert_eq!(logger.log_marker(json!({})), 1);
        assert_eq!(logger.log_marker(json!({})), 2);
    }

    #[test]
    fn navigation_coalescing_suppresses_spam_and_preserves_latest() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let mut logger =
            SessionLogger::new(temp_dir.path().join("logs"), SessionLoggingLevel::Basic);
        logger.log_navigation("zoom", json!({ "camera": { "zoom": 1.0 } }));
        logger.log_navigation("zoom", json!({ "camera": { "zoom": 2.0 } }));
        logger.log_navigation("zoom", json!({ "camera": { "zoom": 3.0 } }));
        logger.flush_navigation();
        logger.flush().expect("flush");

        let events = read_session_events(&logger);

        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["camera"]["zoom"], 1.0);
        assert_eq!(events[1]["camera"]["zoom"], 3.0);
        assert_eq!(events[1]["navigation"]["coalesced"], true);
    }

    #[test]
    fn fps_drop_preserves_existing_perf_counters() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let mut logger =
            SessionLogger::new(temp_dir.path().join("logs"), SessionLoggingLevel::Basic);

        logger.log_fps_drop(
            120.0,
            Some(8.0),
            json!({
                "perf": {
                    "fallback_stroke_shape_count": 42,
                    "fallback_fill_shape_count": 24,
                    "fallback_segmented_strokes": 3,
                    "fallback_fast_strokes": 4,
                    "fallback_visible_cache_hits": 1,
                    "fallback_visible_cache_misses": 0,
                    "fallback_project_ms": 5.0,
                    "fallback_visible_operations_ms": 1.5,
                    "fallback_projection_loop_ms": 3.5,
                    "fallback_derive_ms": 6.0,
                    "fallback_clip_ms": 7.0,
                    "fallback_shape_paint_ms": 8.0
                }
            }),
        );
        logger.flush().expect("flush");

        let events = read_session_events(&logger);
        let perf = &events.first().expect("one event")["perf"];
        assert_eq!(perf["fallback_stroke_shape_count"], 42);
        assert_eq!(perf["fallback_fill_shape_count"], 24);
        assert_eq!(perf["fallback_segmented_strokes"], 3);
        assert_eq!(perf["fallback_fast_strokes"], 4);
        assert_eq!(perf["fallback_visible_cache_hits"], 1);
        assert_eq!(perf["fallback_visible_cache_misses"], 0);
        assert_eq!(perf["fallback_project_ms"], 5.0);
        assert_eq!(perf["fallback_visible_operations_ms"], 1.5);
        assert_eq!(perf["fallback_projection_loop_ms"], 3.5);
        assert_eq!(perf["fallback_derive_ms"], 6.0);
        assert_eq!(perf["fallback_clip_ms"], 7.0);
        assert_eq!(perf["fallback_shape_paint_ms"], 8.0);
        assert_eq!(perf["frame_ms"], 120.0);
        assert_eq!(perf["fps_ema"], 8.0);
    }

    #[test]
    fn slow_phase_preserves_existing_perf_counters() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let mut logger =
            SessionLogger::new(temp_dir.path().join("logs"), SessionLoggingLevel::Basic);
        let mut phases = BTreeMap::new();
        phases.insert("fallback_paint".to_owned(), 75.0);
        phases.insert("tile_total".to_owned(), 0.25);

        logger.record_frame_phases(
            true,
            &phases,
            &json!({
                "perf": {
                    "fallback_stroke_shape_count": 99,
                    "fallback_fill_shape_count": 88,
                    "fallback_segmented_strokes": 11,
                    "fallback_fast_strokes": 12,
                    "fallback_visible_cache_hits": 0,
                    "fallback_visible_cache_misses": 1,
                    "fallback_project_ms": 13.0,
                    "fallback_visible_operations_ms": 4.0,
                    "fallback_projection_loop_ms": 9.0,
                    "fallback_derive_ms": 14.0,
                    "fallback_clip_ms": 15.0,
                    "fallback_shape_paint_ms": 16.0
                }
            }),
        );
        logger.flush().expect("flush");

        let events = read_session_events(&logger);
        let perf = &events.first().expect("one event")["perf"];
        assert_eq!(perf["fallback_stroke_shape_count"], 99);
        assert_eq!(perf["fallback_fill_shape_count"], 88);
        assert_eq!(perf["fallback_segmented_strokes"], 11);
        assert_eq!(perf["fallback_fast_strokes"], 12);
        assert_eq!(perf["fallback_visible_cache_hits"], 0);
        assert_eq!(perf["fallback_visible_cache_misses"], 1);
        assert_eq!(perf["fallback_project_ms"], 13.0);
        assert_eq!(perf["fallback_visible_operations_ms"], 4.0);
        assert_eq!(perf["fallback_projection_loop_ms"], 9.0);
        assert_eq!(perf["fallback_derive_ms"], 14.0);
        assert_eq!(perf["fallback_clip_ms"], 15.0);
        assert_eq!(perf["fallback_shape_paint_ms"], 16.0);
        assert_eq!(perf["phase"], "fallback_paint");
        assert_eq!(perf["phase_ms"], 75.0);
        assert_eq!(perf["phases"]["fallback_paint"], 75.0);
        assert_eq!(perf["phases"]["tile_total"], 0.25);
    }

    #[test]
    fn off_level_still_writes_explicit_error_events() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let mut logger = SessionLogger::new(temp_dir.path().join("logs"), SessionLoggingLevel::Off);
        logger.log_event("launch", json!({}));
        assert!(logger.path().is_none());

        logger.log_error_event(
            "checkpoint failed",
            json!({ "context": { "tool": "Brush" } }),
        );
        logger.flush().expect("flush");

        let path = logger.path().expect("error log path").to_owned();
        let text = fs::read_to_string(path).expect("read log");
        let event: serde_json::Value =
            serde_json::from_str(text.lines().next().expect("one line")).expect("json event");
        assert_eq!(event["event"], "error");
        assert_eq!(event["error"]["message"], "checkpoint failed");
    }
}
