//! streaming dupe finder.
//!
//! sync [`super::scan_duplicates`] is fine for tests + small roots. on a
//! home dir with hundreds of gigs the full-hash pass dominates wall-time,
//! UI should show partial results instead of spinning for 30s.
//!
//! this variant:
//!
//! * pipeline runs to completion but emits progress events along the way.
//!   walking progress carries counters, phase progress carries pruning
//!   counts, and the terminal done carries confirmed groups.
//! * [`DupesRegistry`] tracks live handles for cancel.
//! * cancel checked inside hash passes via AtomicBool, takes effect on
//!   next file not at completion.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;

use super::pipeline::{FindError, Phase};
use super::DuplicateReport;

// ---------- controller ----------

/// shared state between Tauri command surface + walker thread
pub struct DupesController {
    cancelled: Arc<AtomicBool>,
    started: Instant,
    files_scanned: AtomicU64,
}

impl DupesController {
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

    /// used by tests + future progress callbacks. walker-side cancel is
    /// polled on the AtomicBool directly via [`Self::cancel_token`]
    #[allow(dead_code)]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// handle the pipeline checks during hash passes
    pub fn cancel_token(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }
}

impl Default for DupesController {
    fn default() -> Self {
        Self::new()
    }
}

// ---------- emitter ----------

/// emit sink. tests use a Vec recorder, Tauri adapter bridges to
/// AppHandle::emit. walker emits counter progress, then emit_done exactly
/// once with final groups.
pub trait DupesEmit: Send + Sync {
    fn emit_progress(&self, handle_id: &str, resp: &DuplicateReport);
    fn emit_done(&self, handle_id: &str, resp: &DuplicateReport);
}

/// phase label on the wire. UI renders concrete copy for walking,
/// size-grouping, and full-content hashing.
#[derive(Debug, Clone, Copy, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ScanPhase {
    Walking,
    SizeGrouped,
    HeadHashed,
    Done,
}

impl ScanPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            ScanPhase::Walking => "walking",
            ScanPhase::SizeGrouped => "size-grouped",
            ScanPhase::HeadHashed => "head-hashed",
            ScanPhase::Done => "done",
        }
    }

    fn from_pipeline(p: Phase) -> Self {
        match p {
            Phase::Walking | Phase::WalkDone => ScanPhase::Walking,
            Phase::SizeGrouped => ScanPhase::SizeGrouped,
            Phase::HeadHashed => ScanPhase::HeadHashed,
            Phase::Done => ScanPhase::Done,
        }
    }
}

/// returned by start_duplicates. id correlates streamed events with the run
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DupesHandle {
    pub id: String,
    pub root: String,
}

/// drive one streaming dedup walk to completion. blocks, spawn in a
/// dedicated thread.
///
/// emits walking snapshots with live counters, phase snapshots after each
/// pruning pass, then a terminal done with the grouped result.
pub fn run_dupes_stream<E: DupesEmit>(
    handle_id: String,
    root: PathBuf,
    min_bytes: u64,
    ctrl: Arc<DupesController>,
    emit: E,
) {
    let root_echo = root.to_string_lossy().into_owned();
    let started = ctrl.started;

    let empty_done = |phase: ScanPhase| DuplicateReport {
        root: root_echo.clone(),
        groups: Vec::new(),
        total_files_scanned: 0,
        total_groups: 0,
        wasted_bytes: 0,
        duration_ms: started.elapsed().as_millis() as u64,
        phase,
        candidates_remaining: 0,
    };

    // preflight: if root vanished between the Tauri command and here,
    // emit a done with empty results
    if std::fs::symlink_metadata(&root).is_err() {
        emit.emit_done(&handle_id, &empty_done(ScanPhase::Done));
        return;
    }

    let cancel = ctrl.cancel_token();

    // shared cells for phase callback. rayon hash passes run on workers
    // so we need Sync storage
    let files_scanned_atomic = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let remaining_atomic = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let phase_atomic = Arc::new(std::sync::atomic::AtomicU8::new(phase_to_u8(
        ScanPhase::Walking,
    )));

    // phase callback: store snapshot + emit progress with empty groups.
    // UI keeps previously-seen groups and re-reads phase + counters.
    // don't blast a big payload until done.
    let emit_ref = &emit;
    let handle_ref = &handle_id;
    let root_ref = root_echo.clone();
    let files_atomic_cb = Arc::clone(&files_scanned_atomic);
    let remaining_atomic_cb = Arc::clone(&remaining_atomic);
    let phase_atomic_cb = Arc::clone(&phase_atomic);
    let on_phase = move |p, files_scanned: u64, candidates_remaining: u64| {
        let phase = ScanPhase::from_pipeline(p);
        phase_atomic_cb.store(phase_to_u8(phase), Ordering::Release);
        match phase {
            ScanPhase::Walking => {
                files_atomic_cb.store(files_scanned, Ordering::Release);
                remaining_atomic_cb.store(candidates_remaining, Ordering::Release);
            }
            ScanPhase::SizeGrouped | ScanPhase::HeadHashed => {
                files_atomic_cb.store(files_scanned, Ordering::Release);
                remaining_atomic_cb.store(candidates_remaining, Ordering::Release);
            }
            ScanPhase::Done => {
                files_atomic_cb.store(files_scanned, Ordering::Release);
                remaining_atomic_cb.store(candidates_remaining, Ordering::Release);
            }
        }
        // skip the terminal Done snapshot here, outer function assembles
        // with final groups + calls emit_done for a coherent switchover
        if phase == ScanPhase::Done {
            return;
        }
        let resp = DuplicateReport {
            root: root_ref.clone(),
            groups: Vec::new(),
            total_files_scanned: files_atomic_cb.load(Ordering::Acquire),
            total_groups: 0,
            wasted_bytes: 0,
            duration_ms: started.elapsed().as_millis() as u64,
            phase,
            candidates_remaining: remaining_atomic_cb.load(Ordering::Acquire),
        };
        emit_ref.emit_progress(handle_ref, &resp);
    };

    let result = super::pipeline::find_duplicates_with_progress(
        &root,
        min_bytes,
        Some(cancel),
        Some(&on_phase),
    );
    let (groups, files_scanned) = match result {
        Ok(r) => r,
        Err(FindError::NotFound(_)) | Err(FindError::Io(_)) => (Vec::new(), 0),
    };
    ctrl.files_scanned.store(files_scanned, Ordering::Relaxed);

    let wasted: u64 = groups.iter().map(|g| g.wasted_bytes).sum();
    let total_groups = groups.len() as u64;
    let resp = DuplicateReport {
        root: root_echo,
        total_groups,
        wasted_bytes: wasted,
        groups,
        total_files_scanned: files_scanned,
        duration_ms: started.elapsed().as_millis() as u64,
        phase: ScanPhase::Done,
        candidates_remaining: total_groups,
    };
    emit.emit_done(&handle_id, &resp);
}

fn phase_to_u8(p: ScanPhase) -> u8 {
    match p {
        ScanPhase::Walking => 0,
        ScanPhase::SizeGrouped => 1,
        ScanPhase::HeadHashed => 2,
        ScanPhase::Done => 3,
    }
}

// ---------- registry ----------

#[derive(Default)]
pub struct DupesRegistry {
    inner: Mutex<std::collections::HashMap<String, DupesEntry>>,
}

#[derive(Clone)]
struct DupesEntry {
    ctrl: Arc<DupesController>,
    handle: DupesHandle,
    done: Option<DuplicateReport>,
}

#[derive(Debug, Clone)]
pub enum DupesInsert {
    Inserted,
    Active(DupesHandle),
}

impl DupesRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_or_active(&self, handle: DupesHandle, ctrl: Arc<DupesController>) -> DupesInsert {
        let mut g = self.inner.lock().expect("dupes registry poisoned");
        if let Some(active) = g.values().find(|entry| entry.done.is_none()) {
            return DupesInsert::Active(active.handle.clone());
        }
        g.insert(
            handle.id.clone(),
            DupesEntry {
                ctrl,
                handle,
                done: None,
            },
        );
        DupesInsert::Inserted
    }

    pub fn active_handle(&self) -> Option<DupesHandle> {
        self.inner.lock().ok().and_then(|g| {
            g.values()
                .find(|entry| entry.done.is_none())
                .map(|e| e.handle.clone())
        })
    }

    pub fn get(&self, id: &str) -> Option<Arc<DupesController>> {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.get(id).map(|e| e.ctrl.clone()))
    }

    pub fn remove(&self, id: &str) -> Option<Arc<DupesController>> {
        self.inner
            .lock()
            .ok()
            .and_then(|mut g| g.remove(id).map(|e| e.ctrl))
    }

    pub fn finish(&self, id: &str, report: DuplicateReport) {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(entry) = g.get_mut(id) {
                entry.done = Some(report);
            }
        }
    }

    pub fn terminal_snapshot(&self, id: &str) -> Option<DuplicateReport> {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.get(id).and_then(|e| e.done.clone()))
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }
}

/// short non-crypto handle id. matches `dupe-<pid>-<t>-<n>` shape used by
/// the scanner + treemap for wire consistency.
pub fn next_dupes_handle_id() -> String {
    use std::sync::atomic::AtomicU64;
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("dupe-{pid:x}-{now:x}-{n:x}")
}

// ---------- tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::path::Path;

    fn write_bytes(root: &Path, rel: &str, content: &[u8]) {
        let full = root.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(&full).unwrap();
        f.write_all(content).unwrap();
    }

    #[derive(Default)]
    struct Recorder {
        progress: Mutex<Vec<DuplicateReport>>,
        done: Mutex<Option<DuplicateReport>>,
    }

    struct ArcEmit(Arc<Recorder>);
    impl DupesEmit for ArcEmit {
        fn emit_progress(&self, _id: &str, resp: &DuplicateReport) {
            self.0.progress.lock().unwrap().push(resp.clone());
        }
        fn emit_done(&self, _id: &str, resp: &DuplicateReport) {
            *self.0.done.lock().unwrap() = Some(resp.clone());
        }
    }

    #[test]
    fn done_fires_with_final_groups() {
        let tmp = tempfile::tempdir().unwrap();
        let data = vec![9u8; 8 * 1024];
        write_bytes(tmp.path(), "a.bin", &data);
        write_bytes(tmp.path(), "b.bin", &data);

        let ctrl = Arc::new(DupesController::new());
        let rec = Arc::new(Recorder::default());
        run_dupes_stream(
            "h1".into(),
            tmp.path().to_path_buf(),
            0,
            ctrl,
            ArcEmit(rec.clone()),
        );

        let done = rec.done.lock().unwrap();
        let r = done.as_ref().unwrap();
        assert_eq!(r.total_groups, 1);
        assert_eq!(r.wasted_bytes, data.len() as u64);
    }

    #[test]
    fn cancel_before_walk_yields_empty_done() {
        let tmp = tempfile::tempdir().unwrap();
        let data = vec![1u8; 8 * 1024];
        for i in 0..50 {
            write_bytes(tmp.path(), &format!("a{i}.bin"), &data);
            write_bytes(tmp.path(), &format!("b{i}.bin"), &data);
        }

        let ctrl = Arc::new(DupesController::new());
        ctrl.cancel();

        let rec = Arc::new(Recorder::default());
        run_dupes_stream(
            "h-cancel".into(),
            tmp.path().to_path_buf(),
            0,
            ctrl,
            ArcEmit(rec.clone()),
        );

        let done = rec.done.lock().unwrap();
        let r = done.as_ref().unwrap();
        assert_eq!(r.total_groups, 0, "cancelled run, no groups");
    }

    #[test]
    fn missing_root_still_emits_done() {
        let ctrl = Arc::new(DupesController::new());
        let rec = Arc::new(Recorder::default());
        run_dupes_stream(
            "h-nope".into(),
            PathBuf::from("/definitely/not/a/path/safai-dupes-xyz"),
            0,
            ctrl,
            ArcEmit(rec.clone()),
        );
        let done = rec.done.lock().unwrap();
        assert!(done.is_some());
        let r = done.as_ref().unwrap();
        assert_eq!(r.total_groups, 0);
    }

    #[test]
    fn registry_round_trip() {
        let reg = DupesRegistry::new();
        let id = next_dupes_handle_id();
        let ctrl = Arc::new(DupesController::new());
        let handle = DupesHandle {
            id: id.clone(),
            root: "/tmp".into(),
        };
        assert!(matches!(
            reg.insert_or_active(handle, ctrl.clone()),
            DupesInsert::Inserted
        ));
        assert_eq!(reg.len(), 1);
        reg.get(&id).unwrap().cancel();
        assert!(ctrl.is_cancelled());
        assert!(reg.remove(&id).is_some());
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn registry_reuses_active_handle() {
        let reg = DupesRegistry::new();
        let first = DupesHandle {
            id: "dupes-a".into(),
            root: "/one".into(),
        };
        let second = DupesHandle {
            id: "dupes-b".into(),
            root: "/two".into(),
        };
        assert!(matches!(
            reg.insert_or_active(first.clone(), Arc::new(DupesController::new())),
            DupesInsert::Inserted
        ));
        match reg.insert_or_active(second, Arc::new(DupesController::new())) {
            DupesInsert::Active(active) => assert_eq!(active.id, first.id),
            DupesInsert::Inserted => panic!("expected active handle reuse"),
        }
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn terminal_snapshot_does_not_block_next_handle() {
        let reg = DupesRegistry::new();
        let first = DupesHandle {
            id: "dupes-a".into(),
            root: "/one".into(),
        };
        assert!(matches!(
            reg.insert_or_active(first.clone(), Arc::new(DupesController::new())),
            DupesInsert::Inserted
        ));
        let report = DuplicateReport {
            root: first.root.clone(),
            groups: Vec::new(),
            total_files_scanned: 1,
            total_groups: 0,
            wasted_bytes: 0,
            duration_ms: 1,
            phase: ScanPhase::Done,
            candidates_remaining: 0,
        };

        reg.finish(&first.id, report.clone());

        assert_eq!(
            reg.terminal_snapshot(&first.id)
                .unwrap()
                .total_files_scanned,
            report.total_files_scanned,
        );
        assert!(matches!(
            reg.insert_or_active(
                DupesHandle {
                    id: "dupes-b".into(),
                    root: "/two".into(),
                },
                Arc::new(DupesController::new()),
            ),
            DupesInsert::Inserted
        ));
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn handle_ids_unique_under_rapid_allocation() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        for _ in 0..5_000 {
            assert!(set.insert(next_dupes_handle_id()));
        }
    }

    #[test]
    fn progress_events_cover_every_phase_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let data = vec![3u8; 8 * 1024];
        // need at least one full-hash round for all three phases to
        // fire, a pair of identical files does it
        write_bytes(tmp.path(), "a.bin", &data);
        write_bytes(tmp.path(), "b.bin", &data);

        let ctrl = Arc::new(DupesController::new());
        let rec = Arc::new(Recorder::default());
        run_dupes_stream(
            "h-phase".into(),
            tmp.path().to_path_buf(),
            0,
            ctrl,
            ArcEmit(rec.clone()),
        );

        let progress = rec.progress.lock().unwrap();
        let phases: Vec<ScanPhase> = progress.iter().map(|r| r.phase).collect();
        // Walking can emit more than once, then phase transitions follow.
        let first_walking = phases
            .iter()
            .position(|p| *p == ScanPhase::Walking)
            .expect("missing walking progress");
        let size_grouped = phases
            .iter()
            .position(|p| *p == ScanPhase::SizeGrouped)
            .expect("missing size-grouped progress");
        let head_hashed = phases
            .iter()
            .position(|p| *p == ScanPhase::HeadHashed)
            .expect("missing head-hashed progress");
        assert!(first_walking < size_grouped);
        assert!(size_grouped < head_hashed);

        // candidates should narrow or stay the same after walking finishes.
        let pruning: Vec<&DuplicateReport> = progress
            .iter()
            .filter(|r| r.phase != ScanPhase::Walking)
            .collect();
        for pair in pruning.windows(2) {
            assert!(
                pair[1].candidates_remaining <= pair[0].candidates_remaining,
                "candidate count should shrink across phases",
            );
        }
        // every progress event carries total_files_scanned
        for r in progress.iter() {
            assert!(r.total_files_scanned >= 1);
        }
        // done sets phase=Done + has groups
        let done = rec.done.lock().unwrap();
        let d = done.as_ref().unwrap();
        assert_eq!(d.phase, ScanPhase::Done);
        assert_eq!(d.total_groups, 1);
    }

    #[test]
    fn empty_directory_emits_empty_report() {
        let tmp = tempfile::tempdir().unwrap();
        let ctrl = Arc::new(DupesController::new());
        let rec = Arc::new(Recorder::default());
        run_dupes_stream(
            "h-empty".into(),
            tmp.path().to_path_buf(),
            0,
            ctrl,
            ArcEmit(rec.clone()),
        );
        let done = rec.done.lock().unwrap();
        let r = done.as_ref().unwrap();
        assert_eq!(r.total_files_scanned, 0);
        assert_eq!(r.total_groups, 0);
    }
}
