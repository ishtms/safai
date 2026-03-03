//! streaming filesystem walker.
//!
//! 1. real scan, real events. run_scan walks roots via jwalk (rayon-parallel)
//!    and streams per-entry events + throttled progress snapshots to the UI
//!    over tauri's event bus
//! 2. pause/resume/cancel within ms. single AtomicU8 encodes controller
//!    state. workers check it inside process_read_dir so cancel drains the
//!    in-flight per-dir batches instead of running to completion
//! 3. hermetic testability. all IPC goes through an `Emit` trait, tests drop
//!    in a Vec-backed recorder. walker exercised against real temp trees,
//!    fast (<50ms/test), covers state machine + throttling + classifier
//!
//! events:
//!
//! | channel           | payload                     | volume per scan              |
//! | ----------------- | --------------------------- | ---------------------------- |
//! | `scan://event`    | [`ScanEvent`] per-file      | O(samples + hits)            |
//! | `scan://progress` | [`ScanProgress`] snapshot   | <=1 per [`PROGRESS_THROTTLE`]|
//! | `scan://done`     | [`ScanProgress`] final      | exactly once per scan        |
//!
//! frontend filters by handleId so concurrent scans stay on their own lanes

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::classify::{classify, should_sample_scan, Verdict};

// ---------- state machine ----------

/// encoded as AtomicU8 on the controller so workers can check with a single
/// acquire load per directory
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScanState {
    Running,
    Paused,
    /// user hit cancel, walker drains in-flight batches and exits
    Cancelled,
    /// walker ran to completion of its own accord
    Done,
}

impl ScanState {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => ScanState::Running,
            1 => ScanState::Paused,
            2 => ScanState::Cancelled,
            3 => ScanState::Done,
            _ => ScanState::Running,
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            ScanState::Running => 0,
            ScanState::Paused => 1,
            ScanState::Cancelled => 2,
            ScanState::Done => 3,
        }
    }
}

// ---------- controller ----------

/// shared state between the tauri command surface and worker threads.
/// cheap to clone, it's an Arc around atomics + a progress mutex
pub struct ScanController {
    state: AtomicU8,
    files_scanned: AtomicU64,
    bytes_scanned: AtomicU64,
    // files that produced a Verdict, "found so far" counter
    flagged_bytes: AtomicU64,
    flagged_items: AtomicU64,
    /// start of the *current* running segment. Some while Running, None
    /// otherwise. paired with `accum_active_ms` so `elapsed_ms` advances
    /// only during active walking, UI's Elapsed and ETA both freeze on pause
    run_start: Mutex<Option<Instant>>,
    /// active walking time completed in previous running segments. never
    /// reset, only set_state writes on Running -> ! transition
    accum_active_ms: AtomicU64,
    last_progress_emit: Mutex<Instant>,
    last_path_seen: Mutex<Option<String>>,
}

impl ScanController {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            state: AtomicU8::new(ScanState::Running.to_u8()),
            files_scanned: AtomicU64::new(0),
            bytes_scanned: AtomicU64::new(0),
            flagged_bytes: AtomicU64::new(0),
            flagged_items: AtomicU64::new(0),
            run_start: Mutex::new(Some(now)),
            accum_active_ms: AtomicU64::new(0),
            // `now - PROGRESS_THROTTLE` would be nicer but Instant has no
            // guaranteed "zero". first check on hot path still emits
            // immediately since Instant::now() has moved on
            last_progress_emit: Mutex::new(now - PROGRESS_THROTTLE),
            last_path_seen: Mutex::new(None),
        }
    }

    pub fn state(&self) -> ScanState {
        ScanState::from_u8(self.state.load(Ordering::Acquire))
    }

    /// idempotent transition, returns previous state. terminal states
    /// (Done, Cancelled) are sticky, later transitions are no-ops. keeps
    /// a late `resume_scan` from restarting a walker that already exited.
    ///
    /// side-effect: manages active-elapsed timer. leaving Running folds the
    /// current segment into accum_active_ms and clears run_start, entering
    /// Running sets run_start = now
    pub fn set_state(&self, next: ScanState) -> ScanState {
        let prev_raw = self.state.load(Ordering::Acquire);
        let prev = ScanState::from_u8(prev_raw);
        if matches!(prev, ScanState::Done | ScanState::Cancelled) {
            return prev;
        }

        let was_running = matches!(prev, ScanState::Running);
        let will_run = matches!(next, ScanState::Running);
        if was_running != will_run {
            let now = Instant::now();
            if let Ok(mut slot) = self.run_start.lock() {
                if was_running {
                    if let Some(start) = slot.take() {
                        let seg = now.saturating_duration_since(start).as_millis() as u64;
                        self.accum_active_ms.fetch_add(seg, Ordering::Relaxed);
                    }
                } else {
                    *slot = Some(now);
                }
            }
        }

        self.state.store(next.to_u8(), Ordering::Release);
        prev
    }

    /// counts only time spent Running
    pub fn active_elapsed_ms(&self) -> u64 {
        let accum = self.accum_active_ms.load(Ordering::Relaxed);
        let extra = self
            .run_start
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| s.elapsed().as_millis() as u64))
            .unwrap_or(0);
        accum + extra
    }

    pub fn snapshot(&self) -> ScanProgress {
        ScanProgress {
            files_scanned: self.files_scanned.load(Ordering::Relaxed),
            bytes_scanned: self.bytes_scanned.load(Ordering::Relaxed),
            flagged_bytes: self.flagged_bytes.load(Ordering::Relaxed),
            flagged_items: self.flagged_items.load(Ordering::Relaxed),
            elapsed_ms: self.active_elapsed_ms(),
            state: self.state(),
            current_path: self.last_path_seen.lock().ok().and_then(|g| g.clone()),
        }
    }
}

impl Default for ScanController {
    fn default() -> Self {
        Self::new()
    }
}

// ---------- event types ----------

/// min interval between progress snapshots. short enough that stats + ETA
/// feel live, long enough that a "every file is 10 bytes" tree doesn't
/// flood IPC
pub const PROGRESS_THROTTLE: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScanEventKind {
    /// sampled path, "we just peeked here"
    Scan,
    /// surprise-sized, "this is big, worth your attention"
    Found,
    /// known regenerable cache, "safe to sweep"
    Safe,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanEvent {
    pub handle_id: String,
    pub kind: ScanEventKind,
    pub path: String,
    pub bytes: u64,
    pub elapsed_ms: u64,
}

/// frontend drives the radial sweep + stats grid off this. `currentPath` is
/// the most-recent path touched by any worker, imprecise under rayon but
/// good enough as a marquee
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgress {
    pub files_scanned: u64,
    pub bytes_scanned: u64,
    pub flagged_bytes: u64,
    pub flagged_items: u64,
    pub elapsed_ms: u64,
    pub state: ScanState,
    pub current_path: Option<String>,
}

/// tauri's AppHandle impls via a thin adapter in commands.rs, tests use Vec.
/// methods take &self and must be Sync, walker hands the emitter to rayon workers
pub trait Emit: Send + Sync {
    fn emit_event(&self, ev: &ScanEvent);
    fn emit_progress(&self, p: &ScanProgress);
    /// called exactly once per scan after walker exits
    fn emit_done(&self, p: &ScanProgress);
}

// ---------- walker ----------

/// blocks until scan finishes or is cancelled. callers spawn on std::thread
/// from the tauri command surface, do NOT call from a tokio task, jwalk uses
/// rayon which assumes OS threads
pub fn run_scan<E: Emit>(
    handle_id: String,
    roots: Vec<PathBuf>,
    ctrl: Arc<ScanController>,
    emit: E,
) {
    let emit = Arc::new(emit);
    for root in roots {
        if matches!(ctrl.state(), ScanState::Cancelled) {
            break;
        }
        walk_one(&handle_id, &root, ctrl.clone(), emit.clone());
    }
    // terminal transition. set_state sticky on Cancelled -> Done no-op, so a
    // cancelled scan reports final state as Cancelled
    ctrl.set_state(ScanState::Done);
    let snap = ctrl.snapshot();
    emit.emit_done(&snap);
}

fn walk_one<E: Emit>(
    handle_id: &str,
    root: &Path,
    ctrl: Arc<ScanController>,
    emit: Arc<E>,
) {
    let walker = jwalk::WalkDir::new(root)
        .skip_hidden(false)
        .follow_links(false)
        .process_read_dir({
            let ctrl = ctrl.clone();
            move |_depth, _path, _read_dir_state, children| {
                // each worker blocks on the per-dir batch while paused.
                // short sleeps keep responsiveness without pinning CPU
                loop {
                    match ctrl.state() {
                        ScanState::Cancelled | ScanState::Done => {
                            // drop remaining children, cheapest way to
                            // short-circuit a jwalk frontier
                            children.clear();
                            return;
                        }
                        ScanState::Paused => {
                            std::thread::sleep(Duration::from_millis(50));
                            continue;
                        }
                        ScanState::Running => break,
                    }
                }
            }
        });

    for entry_res in walker {
        match ctrl.state() {
            ScanState::Cancelled | ScanState::Done => return,
            // process_read_dir already blocked on Paused, but a Pause issued
            // *between* dir batches lands here
            ScanState::Paused => {
                while matches!(ctrl.state(), ScanState::Paused) {
                    std::thread::sleep(Duration::from_millis(50));
                }
                if matches!(ctrl.state(), ScanState::Cancelled | ScanState::Done) {
                    return;
                }
            }
            ScanState::Running => {}
        }

        let Ok(entry) = entry_res else { continue };
        handle_entry(handle_id, &entry, &ctrl, emit.as_ref());
    }
}

fn handle_entry<E: Emit>(
    handle_id: &str,
    entry: &jwalk::DirEntry<((), ())>,
    ctrl: &ScanController,
    emit: &E,
) {
    let path = entry.path();
    let is_file = entry.file_type().is_file();
    let size = if is_file {
        entry.metadata().map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };

    if is_file {
        let nth = ctrl.files_scanned.fetch_add(1, Ordering::Relaxed) + 1;
        ctrl.bytes_scanned.fetch_add(size, Ordering::Relaxed);

        // keeps the live log moving even when nothing crosses Found/Safe thresholds
        if should_sample_scan(nth) {
            emit.emit_event(&ScanEvent {
                handle_id: handle_id.to_string(),
                kind: ScanEventKind::Scan,
                path: path.to_string_lossy().into_owned(),
                bytes: size,
                elapsed_ms: ctrl.active_elapsed_ms(),
            });
            if let Ok(mut g) = ctrl.last_path_seen.lock() {
                *g = Some(path.to_string_lossy().into_owned());
            }
        }
    }

    if let Some(verdict) = classify(&path, size, is_file) {
        ctrl.flagged_items.fetch_add(1, Ordering::Relaxed);
        ctrl.flagged_bytes.fetch_add(size, Ordering::Relaxed);
        emit.emit_event(&ScanEvent {
            handle_id: handle_id.to_string(),
            kind: match verdict {
                Verdict::Found => ScanEventKind::Found,
                Verdict::Safe => ScanEventKind::Safe,
            },
            path: path.to_string_lossy().into_owned(),
            bytes: size,
            elapsed_ms: ctrl.active_elapsed_ms(),
        });
    }

    // throttled progress, ~one per PROGRESS_THROTTLE across all workers.
    // try_lock + early-return keeps hot workers from serializing
    if let Ok(mut last) = ctrl.last_progress_emit.try_lock() {
        let now = Instant::now();
        if now.duration_since(*last) >= PROGRESS_THROTTLE {
            *last = now;
            drop(last);
            let snap = ctrl.snapshot();
            emit.emit_progress(&snap);
        }
    }
}

// ---------- registry (id -> controller) ----------

/// wrapped so commands.rs can `Manage` it in tauri
#[derive(Default)]
pub struct ScanRegistry {
    inner: Mutex<std::collections::HashMap<String, Arc<ScanController>>>,
}

impl ScanRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, id: String, ctrl: Arc<ScanController>) {
        if let Ok(mut g) = self.inner.lock() {
            g.insert(id, ctrl);
        }
    }

    pub fn get(&self, id: &str) -> Option<Arc<ScanController>> {
        self.inner.lock().ok().and_then(|g| g.get(id).cloned())
    }

    pub fn remove(&self, id: &str) -> Option<Arc<ScanController>> {
        self.inner.lock().ok().and_then(|mut g| g.remove(id))
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }
}

/// short, non-crypto. monotonic + pid so two concurrent scans started in the
/// same ms still collision-free
pub fn next_handle_id() -> String {
    use std::sync::atomic::AtomicU64;
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("scan-{pid:x}-{now:x}-{n:x}")
}

// ---------- tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn make_tree(root: &Path, files: &[(&str, usize)]) {
        for (rel, size) in files {
            let full = root.join(rel);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            let mut f = fs::File::create(&full).unwrap();
            if *size > 0 {
                let chunk = vec![0u8; (*size).min(64 * 1024)];
                let mut remaining = *size;
                while remaining > 0 {
                    let n = remaining.min(chunk.len());
                    f.write_all(&chunk[..n]).unwrap();
                    remaining -= n;
                }
            }
        }
    }

    #[test]
    fn elapsed_freezes_during_pause() {
        let c = ScanController::new();
        std::thread::sleep(Duration::from_millis(40));
        let before_pause = c.active_elapsed_ms();
        assert!(before_pause >= 30, "expected some elapsed, got {before_pause}");

        c.set_state(ScanState::Paused);
        let at_pause = c.active_elapsed_ms();
        std::thread::sleep(Duration::from_millis(60));
        let after_pause = c.active_elapsed_ms();

        // after pausing, elapsed must not have moved (modulo ~5ms rounding
        // from the transition itself, we just bound it)
        assert!(
            after_pause < at_pause + 10,
            "elapsed advanced during pause: {at_pause} -> {after_pause}",
        );

        c.set_state(ScanState::Running);
        std::thread::sleep(Duration::from_millis(30));
        let after_resume = c.active_elapsed_ms();
        assert!(
            after_resume >= after_pause + 25,
            "elapsed did not resume: {after_pause} -> {after_resume}",
        );

        // total active ~= first 40ms + 30ms, never counts the 60ms paused window
        assert!(after_resume < before_pause + 100);
    }

    #[test]
    fn elapsed_is_frozen_after_cancel() {
        let c = ScanController::new();
        std::thread::sleep(Duration::from_millis(30));
        c.set_state(ScanState::Cancelled);
        let at_cancel = c.active_elapsed_ms();
        std::thread::sleep(Duration::from_millis(50));
        let later = c.active_elapsed_ms();
        assert!(
            later < at_cancel + 10,
            "elapsed advanced after cancel: {at_cancel} -> {later}",
        );
    }

    #[test]
    fn state_transitions_are_sticky_on_terminal() {
        let c = ScanController::new();
        assert_eq!(c.state(), ScanState::Running);
        c.set_state(ScanState::Paused);
        assert_eq!(c.state(), ScanState::Paused);
        c.set_state(ScanState::Running);
        assert_eq!(c.state(), ScanState::Running);
        c.set_state(ScanState::Done);
        // post-terminal, ignored
        c.set_state(ScanState::Running);
        assert_eq!(c.state(), ScanState::Done);

        let c2 = ScanController::new();
        c2.set_state(ScanState::Cancelled);
        c2.set_state(ScanState::Running);
        assert_eq!(c2.state(), ScanState::Cancelled);
    }

    #[test]
    fn scan_counts_every_file_and_emits_done_once() {
        let tmp = tempfile::tempdir().unwrap();
        let files: Vec<(&str, usize)> = vec![
            ("a.txt", 100),
            ("b/c.txt", 200),
            ("b/d/e.txt", 50),
            ("b/d/f.txt", 0),
        ];
        make_tree(tmp.path(), &files);

        let ctrl = Arc::new(ScanController::new());
        let rec = Arc::new(ArcRecorder::default());
        run_scan(
            "h1".into(),
            vec![tmp.path().to_path_buf()],
            ctrl.clone(),
            ArcEmit(rec),
        );

        assert_eq!(ctrl.state(), ScanState::Done);
        let snap = ctrl.snapshot();
        assert_eq!(snap.files_scanned, files.len() as u64);
        assert_eq!(snap.bytes_scanned, files.iter().map(|(_, n)| *n as u64).sum::<u64>());
    }

    #[test]
    fn cancel_stops_walk_quickly() {
        // broad cheap tree so the walker has lots of entries to burn through,
        // a cancel between state checks should exit well before completion
        let tmp = tempfile::tempdir().unwrap();
        let mut files: Vec<(String, usize)> = Vec::new();
        for i in 0..2_000 {
            files.push((format!("dir{}/file{}.txt", i % 50, i), 8));
        }
        let pairs: Vec<(&str, usize)> = files.iter().map(|(s, n)| (s.as_str(), *n)).collect();
        make_tree(tmp.path(), &pairs);

        let ctrl = Arc::new(ScanController::new());

        // cancel immediately, walker should exit before emitting a done snapshot
        // with full counts
        ctrl.set_state(ScanState::Cancelled);

        let rec = Arc::new(ArcRecorder::default());
        run_scan(
            "h-cancel".into(),
            vec![tmp.path().to_path_buf()],
            ctrl.clone(),
            ArcEmit(rec),
        );

        let snap = ctrl.snapshot();
        // final emission state is Done via run_scan, but set_state(Done) is
        // a no-op post-Cancelled so snapshot should still read Cancelled
        assert_eq!(snap.state, ScanState::Cancelled);
        assert!(
            snap.files_scanned < 2_000,
            "cancel should have short-circuited the walk, got {}",
            snap.files_scanned,
        );
    }

    #[test]
    fn large_file_emits_found_event() {
        let tmp = tempfile::tempdir().unwrap();
        // real 100 MiB write hits disk hard, use sparse via set_len so this
        // stays fast on any fs
        let big = tmp.path().join("big.bin");
        let f = fs::File::create(&big).unwrap();
        f.set_len(super::super::classify::FOUND_MIN_BYTES + 1024).unwrap();
        make_tree(tmp.path(), &[("small1.txt", 10), ("small2.txt", 20)]);

        let ctrl = Arc::new(ScanController::new());
        let rec = Arc::new(ArcRecorder::default());
        run_scan(
            "h-big".into(),
            vec![tmp.path().to_path_buf()],
            ctrl,
            ArcEmit(rec.clone()),
        );

        let events = rec.events.lock().unwrap();
        let found: Vec<&ScanEvent> = events
            .iter()
            .filter(|e| matches!(e.kind, ScanEventKind::Found))
            .collect();
        assert_eq!(found.len(), 1, "exactly one big file -> one Found event");
        assert!(found[0].path.ends_with("big.bin"));
        assert!(found[0].bytes >= super::super::classify::FOUND_MIN_BYTES);
    }

    // arc-cloneable recorder so we can inspect results after the walker
    // consumed the emitter by value
    #[derive(Default)]
    struct ArcRecorder {
        events: Mutex<Vec<ScanEvent>>,
        progress: Mutex<Vec<ScanProgress>>,
        done: Mutex<Option<ScanProgress>>,
    }
    struct ArcEmit(Arc<ArcRecorder>);
    impl Emit for ArcEmit {
        fn emit_event(&self, ev: &ScanEvent) {
            self.0.events.lock().unwrap().push(ev.clone());
        }
        fn emit_progress(&self, p: &ScanProgress) {
            self.0.progress.lock().unwrap().push(p.clone());
        }
        fn emit_done(&self, p: &ScanProgress) {
            *self.0.done.lock().unwrap() = Some(p.clone());
        }
    }

    #[test]
    fn safe_path_emits_safe_event_not_found() {
        // nested path that matches one of SAFE_MARKERS
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join("Library/Caches/com.example");
        fs::create_dir_all(&cache_dir).unwrap();
        let cached = cache_dir.join("blob.bin");
        let f = fs::File::create(&cached).unwrap();
        // big, to prove Safe wins over Found
        f.set_len(super::super::classify::FOUND_MIN_BYTES + 1).unwrap();

        let ctrl = Arc::new(ScanController::new());
        let rec = Arc::new(ArcRecorder::default());
        run_scan(
            "h-safe".into(),
            vec![tmp.path().to_path_buf()],
            ctrl,
            ArcEmit(rec.clone()),
        );

        let events = rec.events.lock().unwrap();
        let safe = events.iter().filter(|e| matches!(e.kind, ScanEventKind::Safe)).count();
        let found = events.iter().filter(|e| matches!(e.kind, ScanEventKind::Found)).count();
        assert_eq!(safe, 1);
        assert_eq!(found, 0);
    }

    #[test]
    fn done_event_fired_exactly_once() {
        let tmp = tempfile::tempdir().unwrap();
        make_tree(tmp.path(), &[("a.txt", 4)]);
        let ctrl = Arc::new(ScanController::new());
        let rec = Arc::new(ArcRecorder::default());
        run_scan(
            "h-done".into(),
            vec![tmp.path().to_path_buf()],
            ctrl.clone(),
            ArcEmit(rec.clone()),
        );
        assert!(rec.done.lock().unwrap().is_some());
        assert_eq!(ctrl.state(), ScanState::Done);
    }

    #[test]
    fn progress_throttle_caps_emission_rate() {
        // tree w/ many files, verify progress events are at most one per
        // PROGRESS_THROTTLE interval during the walk
        let tmp = tempfile::tempdir().unwrap();
        let mut pairs = Vec::new();
        for i in 0..500 {
            pairs.push((format!("f{i}.txt"), 8));
        }
        let refs: Vec<(&str, usize)> = pairs.iter().map(|(s, n)| (s.as_str(), *n)).collect();
        make_tree(tmp.path(), &refs);

        let ctrl = Arc::new(ScanController::new());
        let rec = Arc::new(ArcRecorder::default());
        let started = Instant::now();
        run_scan(
            "h-prog".into(),
            vec![tmp.path().to_path_buf()],
            ctrl,
            ArcEmit(rec.clone()),
        );
        let elapsed = started.elapsed();

        let emitted = rec.progress.lock().unwrap().len();
        // upper bound: snapshot per throttle window + slack for parallel
        // workers racing try_lock. 4x to stay non-flaky
        let max_expected = (elapsed.as_millis() / PROGRESS_THROTTLE.as_millis()).max(1) as usize * 4 + 4;
        assert!(
            emitted <= max_expected,
            "throttle violated: {emitted} progress events in {:?} (cap {max_expected})",
            elapsed,
        );
    }

    #[test]
    fn registry_round_trip() {
        let reg = ScanRegistry::new();
        let id = next_handle_id();
        let ctrl = Arc::new(ScanController::new());
        reg.insert(id.clone(), ctrl.clone());
        assert_eq!(reg.len(), 1);
        let got = reg.get(&id).expect("present");
        got.set_state(ScanState::Cancelled);
        assert_eq!(ctrl.state(), ScanState::Cancelled);
        assert!(reg.remove(&id).is_some());
        assert_eq!(reg.len(), 0);
        assert!(reg.get(&id).is_none());
    }

    #[test]
    fn handle_ids_are_unique_under_rapid_allocation() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        for _ in 0..10_000 {
            let id = next_handle_id();
            assert!(set.insert(id.clone()), "duplicate id: {id}");
        }
    }

    #[test]
    fn empty_directory_still_emits_done() {
        let tmp = tempfile::tempdir().unwrap();
        let ctrl = Arc::new(ScanController::new());
        let rec = Arc::new(ArcRecorder::default());
        run_scan(
            "h-empty".into(),
            vec![tmp.path().to_path_buf()],
            ctrl.clone(),
            ArcEmit(rec.clone()),
        );
        assert!(rec.done.lock().unwrap().is_some());
        assert_eq!(ctrl.snapshot().files_scanned, 0);
        assert_eq!(ctrl.state(), ScanState::Done);
    }

    #[test]
    fn nonexistent_root_does_not_panic() {
        let ctrl = Arc::new(ScanController::new());
        let rec = Arc::new(ArcRecorder::default());
        run_scan(
            "h-bad".into(),
            vec![PathBuf::from("/definitely/does/not/exist/safai-test-root-xyz")],
            ctrl.clone(),
            ArcEmit(rec.clone()),
        );
        // walker emits done + no files. no panic = pass
        assert!(rec.done.lock().unwrap().is_some());
        assert_eq!(ctrl.snapshot().files_scanned, 0);
    }

    #[test]
    fn serialization_uses_camelcase_and_kebab_enum() {
        let ev = ScanEvent {
            handle_id: "h".into(),
            kind: ScanEventKind::Safe,
            path: "/tmp".into(),
            bytes: 42,
            elapsed_ms: 1,
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert!(v.get("handleId").is_some());
        assert!(v.get("elapsedMs").is_some());
        assert_eq!(v["kind"], "safe");

        let p = ScanProgress {
            files_scanned: 1,
            bytes_scanned: 2,
            flagged_bytes: 3,
            flagged_items: 4,
            elapsed_ms: 5,
            state: ScanState::Running,
            current_path: Some("/x".into()),
        };
        let v = serde_json::to_value(&p).unwrap();
        assert!(v.get("filesScanned").is_some());
        assert!(v.get("bytesScanned").is_some());
        assert!(v.get("flaggedBytes").is_some());
        assert!(v.get("flaggedItems").is_some());
        assert!(v.get("elapsedMs").is_some());
        assert!(v.get("currentPath").is_some());
        assert_eq!(v["state"], "running");
    }
}
