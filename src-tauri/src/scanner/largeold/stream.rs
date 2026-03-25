//! streaming wrapper for large & old.
//!
//! mirrors dupes / treemap stream: [`LargeOldController`] holds cancel
//! flag + started instant, [`LargeOldRegistry`] hands out handles, and
//! [`run_large_old_stream`] runs the pipeline on a dedicated thread and
//! forwards progress/done via [`LargeOldEmit`].
//!
//! cadence:
//! * one Walking event per [`pipeline::PROGRESS_EVERY`] files so the
//!   "walked N files" counter ticks
//! * one terminal Done with the final sorted rows
//!
//! no partial sorted results, pipeline sorts once at the end. rendering
//! a point per walk tick would thrash the scatter plot.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;

use super::pipeline::{self, FindError, Phase};
use super::LargeOldReport;

pub struct LargeOldController {
    cancelled: Arc<AtomicBool>,
    started: Instant,
    files_scanned: AtomicU64,
}

impl LargeOldController {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            started: Instant::now(),
            files_scanned: AtomicU64::new(0),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    #[allow(dead_code)]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub fn cancel_token(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }
}

impl Default for LargeOldController {
    fn default() -> Self {
        Self::new()
    }
}

/// emit sink. tests use a Vec recorder, Tauri adapter bridges to
/// AppHandle::emit.
pub trait LargeOldEmit: Send + Sync {
    fn emit_progress(&self, handle_id: &str, resp: &LargeOldReport);
    fn emit_done(&self, handle_id: &str, resp: &LargeOldReport);
}

/// phase label on the wire. matches the frontend's ScanPhase union.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ScanPhase {
    Walking,
    Done,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LargeOldHandle {
    pub id: String,
    pub root: String,
}

/// drive one streaming scan to completion. blocks, spawn in a dedicated
/// OS thread from the Tauri command.
pub fn run_large_old_stream<E: LargeOldEmit>(
    handle_id: String,
    root: PathBuf,
    min_bytes: u64,
    min_days_idle: u64,
    max_results: usize,
    ctrl: Arc<LargeOldController>,
    emit: E,
) {
    let root_echo = root.to_string_lossy().into_owned();
    let started = ctrl.started;

    let make_report = |phase: ScanPhase,
                       files: Vec<pipeline::FileSummary>,
                       matched: u64,
                       bytes: u64,
                       scanned: u64|
     -> LargeOldReport {
        LargeOldReport {
            root: root_echo.clone(),
            files,
            total_matched: matched,
            total_bytes: bytes,
            total_files_scanned: scanned,
            duration_ms: started.elapsed().as_millis() as u64,
            phase,
            min_bytes,
            min_days_idle,
        }
    };

    // preflight. if root vanished between command and here, emit a done
    // with zeroed totals so UI resets instead of waiting forever.
    if std::fs::symlink_metadata(&root).is_err() {
        emit.emit_done(&handle_id, &make_report(ScanPhase::Done, Vec::new(), 0, 0, 0));
        return;
    }

    let now_secs = pipeline::now_unix_secs();
    let cancel = ctrl.cancel_token();

    let handle_ref = handle_id.clone();
    let root_for_cb = root_echo.clone();
    let emit_ref = &emit;
    let files_scanned_atomic = Arc::new(AtomicU64::new(0));
    let files_cb = Arc::clone(&files_scanned_atomic);
    let on_phase = move |p: Phase, count: u64| {
        match p {
            Phase::Walking => {
                files_cb.store(count, Ordering::Release);
                let resp = LargeOldReport {
                    root: root_for_cb.clone(),
                    files: Vec::new(),
                    total_matched: 0,
                    total_bytes: 0,
                    total_files_scanned: count,
                    duration_ms: started.elapsed().as_millis() as u64,
                    phase: ScanPhase::Walking,
                    min_bytes,
                    min_days_idle,
                };
                emit_ref.emit_progress(&handle_ref, &resp);
            }
            Phase::Done => {
                // outer fn emits the terminal done with actual rows,
                // nothing to do here
            }
        }
    };

    let result = pipeline::find_large_old(
        &root,
        min_bytes,
        min_days_idle,
        max_results,
        now_secs,
        Some(cancel),
        Some(&on_phase),
    );

    let (rows, matched, bytes, scanned) = match result {
        Ok(r) => r,
        Err(FindError::NotFound(_)) | Err(FindError::Io(_)) => (Vec::new(), 0, 0, 0),
    };
    ctrl.files_scanned.store(scanned, Ordering::Relaxed);
    emit.emit_done(
        &handle_id,
        &make_report(ScanPhase::Done, rows, matched, bytes, scanned),
    );
}

/// process-wide registry. same shape as dupes / treemap.
#[derive(Default)]
pub struct LargeOldRegistry {
    inner: Mutex<std::collections::HashMap<String, Arc<LargeOldController>>>,
}

impl LargeOldRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, id: String, ctrl: Arc<LargeOldController>) {
        if let Ok(mut g) = self.inner.lock() {
            g.insert(id, ctrl);
        }
    }

    pub fn get(&self, id: &str) -> Option<Arc<LargeOldController>> {
        self.inner.lock().ok().and_then(|g| g.get(id).cloned())
    }

    pub fn remove(&self, id: &str) -> Option<Arc<LargeOldController>> {
        self.inner.lock().ok().and_then(|mut g| g.remove(id))
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }
}

pub fn next_large_old_handle_id() -> String {
    use std::sync::atomic::AtomicU64;
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("lo-{pid:x}-{now:x}-{n:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use std::time::SystemTime;

    fn write_aged(root: &std::path::Path, rel: &str, content: &[u8], secs_ago: u64) {
        let full = root.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = File::create(&full).unwrap();
        f.write_all(content).unwrap();
        drop(f);
        let h = File::options().write(true).open(&full).unwrap();
        h.set_modified(SystemTime::now() - std::time::Duration::from_secs(secs_ago))
            .unwrap();
    }

    #[derive(Default)]
    struct Recorder {
        progress: Mutex<Vec<LargeOldReport>>,
        done: Mutex<Option<LargeOldReport>>,
    }

    struct ArcEmit(Arc<Recorder>);
    impl LargeOldEmit for ArcEmit {
        fn emit_progress(&self, _id: &str, resp: &LargeOldReport) {
            self.0.progress.lock().unwrap().push(resp.clone());
        }
        fn emit_done(&self, _id: &str, resp: &LargeOldReport) {
            *self.0.done.lock().unwrap() = Some(resp.clone());
        }
    }

    #[test]
    fn done_fires_with_final_rows() {
        let tmp = tempfile::tempdir().unwrap();
        write_aged(tmp.path(), "a.bin", &vec![1u8; 4096], 365 * 86400);
        let ctrl = Arc::new(LargeOldController::new());
        let rec = Arc::new(Recorder::default());
        run_large_old_stream(
            "h1".into(),
            tmp.path().to_path_buf(),
            1024,
            30,
            1000,
            ctrl,
            ArcEmit(rec.clone()),
        );
        let done = rec.done.lock().unwrap();
        let r = done.as_ref().unwrap();
        assert_eq!(r.phase, ScanPhase::Done);
        assert_eq!(r.files.len(), 1);
        assert_eq!(r.total_matched, 1);
    }

    #[test]
    fn cancel_before_walk_yields_empty_done() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..10 {
            write_aged(tmp.path(), &format!("a{i}.bin"), &vec![0u8; 4096], 365 * 86400);
        }
        let ctrl = Arc::new(LargeOldController::new());
        ctrl.cancel();
        let rec = Arc::new(Recorder::default());
        run_large_old_stream(
            "h-cancel".into(),
            tmp.path().to_path_buf(),
            1024,
            30,
            1000,
            ctrl,
            ArcEmit(rec.clone()),
        );
        let done = rec.done.lock().unwrap();
        let r = done.as_ref().unwrap();
        assert_eq!(r.files.len(), 0);
        assert_eq!(r.total_matched, 0);
    }

    #[test]
    fn missing_root_still_emits_done() {
        let ctrl = Arc::new(LargeOldController::new());
        let rec = Arc::new(Recorder::default());
        run_large_old_stream(
            "h-nope".into(),
            PathBuf::from("/definitely/not/a/real/path-safai-lo"),
            1024,
            30,
            1000,
            ctrl,
            ArcEmit(rec.clone()),
        );
        let done = rec.done.lock().unwrap();
        assert!(done.is_some());
        assert_eq!(done.as_ref().unwrap().files.len(), 0);
    }

    #[test]
    fn registry_round_trip() {
        let reg = LargeOldRegistry::new();
        let id = next_large_old_handle_id();
        let ctrl = Arc::new(LargeOldController::new());
        reg.insert(id.clone(), ctrl.clone());
        assert_eq!(reg.len(), 1);
        reg.get(&id).unwrap().cancel();
        assert!(ctrl.is_cancelled());
        assert!(reg.remove(&id).is_some());
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn handle_ids_unique_under_rapid_load() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        for _ in 0..5_000 {
            assert!(set.insert(next_large_old_handle_id()));
        }
    }

    #[test]
    fn progress_tick_during_big_walk() {
        let tmp = tempfile::tempdir().unwrap();
        // write enough files to trigger at least one Walking tick
        let n = super::pipeline::PROGRESS_EVERY + 5;
        for i in 0..n {
            let full = tmp.path().join(format!("f{i:06}.bin"));
            let mut f = File::create(&full).unwrap();
            f.write_all(&[0u8]).unwrap();
        }
        let ctrl = Arc::new(LargeOldController::new());
        let rec = Arc::new(Recorder::default());
        run_large_old_stream(
            "h-tick".into(),
            tmp.path().to_path_buf(),
            u64::MAX, // filter everything, we only want ticks
            0,
            100,
            ctrl,
            ArcEmit(rec.clone()),
        );
        let progress = rec.progress.lock().unwrap();
        assert!(
            !progress.is_empty(),
            "expected at least one Walking progress snapshot",
        );
        // every progress snapshot carries Walking
        assert!(progress.iter().all(|p| p.phase == ScanPhase::Walking));
        // done still fires
        assert!(rec.done.lock().unwrap().is_some());
    }
}
