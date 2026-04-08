//! streaming loop: tick -> [`sample::sample`] -> emit, repeat.
//!
//! loop lives on a dedicated OS thread spawned from the tauri command.
//! cadence via [`ActivityController::set_interval_ms`] so the UI can throttle
//! (e.g. pause to 5s when window is hidden).
//!
//! cancel uses a Condvar so a mid-sleep cancel wakes in ms, not at the next
//! tick boundary. without it a 5s interval would keep the thread alive for
//! up to 5s after the UI navigates away. resource-leak tests catch regressions
//!
//! events:
//! * `activity://snapshot` one per tick
//!
//! no `done` event, stream only ends when UI calls `cancel_activity`

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::sample::{self, SystemProbe};
use super::types::ActivitySnapshot;

/// anything tighter than 200ms is noise. sysinfo's CPU reading needs ~100ms
/// window anyway and tauri's event bus coalesces rapid fires
pub const MIN_INTERVAL_MS: u64 = 200;

/// 1s matches macOS Activity Monitor + Windows Task Manager "Normal" speed
pub const DEFAULT_INTERVAL_MS: u64 = 1_000;

/// anything >1min is indistinguishable from "paused". clamp so a bad UI value
/// can't leave the thread idle forever
pub const MAX_INTERVAL_MS: u64 = 60_000;

/// 10 matches the Memory screen's "top apps" card
pub const DEFAULT_TOP_N: usize = 10;

pub struct ActivityController {
    cancel_flag: AtomicBool,
    interval_ms: AtomicU64,
    started: Instant,
    /// lock/condvar for interruptible sleep. `()` payload is a sentinel, we
    /// only ever notify, never store state
    wait: Arc<(Mutex<()>, Condvar)>,
    tick: AtomicU64,
}

impl ActivityController {
    pub fn new() -> Self {
        Self {
            cancel_flag: AtomicBool::new(false),
            interval_ms: AtomicU64::new(DEFAULT_INTERVAL_MS),
            started: Instant::now(),
            wait: Arc::new((Mutex::new(()), Condvar::new())),
            tick: AtomicU64::new(0),
        }
    }

    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::Release);
        let (lock, cv) = &*self.wait;
        // take the lock just long enough to ensure any waiting thread is
        // parked on the condvar (past the is_cancelled check). guard dropped
        // immediately, notify_all is safe either way but pairing w/ the lock
        // keeps it deterministic
        let _g = lock.lock().unwrap();
        cv.notify_all();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel_flag.load(Ordering::Acquire)
    }

    pub fn set_interval_ms(&self, ms: u64) {
        let clamped = ms.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS);
        self.interval_ms.store(clamped, Ordering::Release);
        // bump condvar so a sleeping tick wakes and re-reads the interval.
        // otherwise 60s -> 1s change would wait up to 60s to apply
        let (lock, cv) = &*self.wait;
        let _g = lock.lock().unwrap();
        cv.notify_all();
    }

    pub fn interval_ms(&self) -> u64 {
        self.interval_ms.load(Ordering::Acquire)
    }

    #[allow(dead_code)]
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    #[allow(dead_code)]
    pub fn tick(&self) -> u64 {
        self.tick.load(Ordering::Acquire)
    }

    /// returns early on cancel or interval change
    pub fn sleep_cancellable(&self, dur: Duration) {
        let (lock, cv) = &*self.wait;
        let guard = lock.lock().unwrap();
        let _ = cv.wait_timeout(guard, dur);
    }
}

impl Default for ActivityController {
    fn default() -> Self {
        Self::new()
    }
}

/// emit sink for snapshots. tauri adapter in commands.rs, tests use a Vec recorder
pub trait ActivityEmit: Send + Sync {
    fn emit_snapshot(&self, handle_id: &str, snap: &ActivitySnapshot);
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityHandle {
    pub id: String,
    pub interval_ms: u64,
}

/// blocks, spawn in a dedicated OS thread. probe moved in because it carries
/// per-sample state (sysinfo's internal CPU delta cache)
pub fn run_activity_stream<P: SystemProbe, E: ActivityEmit>(
    handle_id: String,
    ctrl: Arc<ActivityController>,
    mut probe: P,
    top_n: usize,
    emit: E,
) {
    // sysinfo needs two CPU samples spaced apart for a useful percentage. do
    // a throwaway refresh + short pause so the first emit has real numbers
    // instead of all-zero
    probe.refresh();
    std::thread::sleep(Duration::from_millis(MIN_INTERVAL_MS.min(ctrl.interval_ms())));

    while !ctrl.is_cancelled() {
        let tick = ctrl.tick.fetch_add(1, Ordering::AcqRel);
        let snap = sample::sample(&mut probe, top_n, tick);
        emit.emit_snapshot(&handle_id, &snap);
        if ctrl.is_cancelled() {
            break;
        }
        ctrl.sleep_cancellable(Duration::from_millis(ctrl.interval_ms()));
    }
}

#[derive(Default)]
pub struct ActivityRegistry {
    inner: Mutex<HashMap<String, Arc<ActivityController>>>,
}

impl ActivityRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, id: String, ctrl: Arc<ActivityController>) {
        if let Ok(mut g) = self.inner.lock() {
            g.insert(id, ctrl);
        }
    }

    pub fn get(&self, id: &str) -> Option<Arc<ActivityController>> {
        self.inner.lock().ok().and_then(|g| g.get(id).cloned())
    }

    pub fn remove(&self, id: &str) -> Option<Arc<ActivityController>> {
        self.inner.lock().ok().and_then(|mut g| g.remove(id))
    }

    /// called from the process-exit hook so we don't leak sampler threads
    /// on window close
    #[allow(dead_code)]
    pub fn cancel_all(&self) {
        if let Ok(g) = self.inner.lock() {
            for ctrl in g.values() {
                ctrl.cancel();
            }
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }
}

pub fn next_activity_handle_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("act-{pid:x}-{now:x}-{n:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::activity::types::ProcessRow;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex as StdMutex;

    // ---- helpers ----

    struct TestProbe {
        procs: Vec<ProcessRow>,
        cpu: Vec<f32>,
        refreshes: Arc<AtomicU64>,
    }

    impl TestProbe {
        fn new() -> Self {
            Self {
                procs: vec![],
                cpu: vec![10.0],
                refreshes: Arc::new(AtomicU64::new(0)),
            }
        }
    }

    impl SystemProbe for TestProbe {
        fn refresh(&mut self) {
            self.refreshes.fetch_add(1, Ordering::Relaxed);
        }
        fn memory_total(&self) -> u64 {
            1000
        }
        fn memory_used(&self) -> u64 {
            500
        }
        fn memory_free(&self) -> u64 {
            500
        }
        fn memory_available(&self) -> u64 {
            500
        }
        fn swap_total(&self) -> u64 {
            0
        }
        fn swap_used(&self) -> u64 {
            0
        }
        fn cpu_per_core(&self) -> Vec<f32> {
            self.cpu.clone()
        }
        fn processes(&self) -> Vec<ProcessRow> {
            self.procs.clone()
        }
    }

    #[derive(Default)]
    struct Recorder {
        events: StdMutex<Vec<ActivitySnapshot>>,
        count: AtomicUsize,
    }

    struct ArcEmit(Arc<Recorder>);
    impl ActivityEmit for ArcEmit {
        fn emit_snapshot(&self, _id: &str, snap: &ActivitySnapshot) {
            self.0.count.fetch_add(1, Ordering::Relaxed);
            self.0.events.lock().unwrap().push(snap.clone());
        }
    }

    // ---- controller ----

    #[test]
    fn controller_interval_clamped_to_min() {
        let c = ActivityController::new();
        c.set_interval_ms(10);
        assert_eq!(c.interval_ms(), MIN_INTERVAL_MS);
    }

    #[test]
    fn controller_interval_clamped_to_max() {
        let c = ActivityController::new();
        c.set_interval_ms(10_000_000);
        assert_eq!(c.interval_ms(), MAX_INTERVAL_MS);
    }

    #[test]
    fn controller_interval_passthrough_inside_bounds() {
        let c = ActivityController::new();
        c.set_interval_ms(2_500);
        assert_eq!(c.interval_ms(), 2_500);
    }

    #[test]
    fn controller_defaults_are_sane() {
        let c = ActivityController::new();
        assert_eq!(c.interval_ms(), DEFAULT_INTERVAL_MS);
        assert!(!c.is_cancelled());
        assert_eq!(c.tick(), 0);
    }

    #[test]
    fn controller_cancel_is_idempotent() {
        let c = ActivityController::new();
        c.cancel();
        c.cancel();
        assert!(c.is_cancelled());
    }

    #[test]
    fn controller_sleep_wakes_on_cancel() {
        let c = Arc::new(ActivityController::new());
        let cc = c.clone();
        let start = Instant::now();
        // cancel after 50ms
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            cc.cancel();
        });
        c.sleep_cancellable(Duration::from_secs(5));
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(600),
            "sleep should have woken near-instantly on cancel, got {elapsed:?}",
        );
    }

    #[test]
    fn controller_sleep_wakes_on_interval_change() {
        let c = Arc::new(ActivityController::new());
        let cc = c.clone();
        let start = Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            cc.set_interval_ms(500);
        });
        c.sleep_cancellable(Duration::from_secs(10));
        // condvar wakes on any notify, including interval change
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(600),
            "expected sleep to wake on set_interval_ms, got {elapsed:?}",
        );
    }

    // ---- stream loop ----

    #[test]
    fn stream_emits_at_least_one_snapshot_then_cancels() {
        let probe = TestProbe::new();
        let rec = Arc::new(Recorder::default());
        let ctrl = Arc::new(ActivityController::new());
        ctrl.set_interval_ms(MIN_INTERVAL_MS);
        let cc = ctrl.clone();
        let handle = std::thread::spawn(move || {
            run_activity_stream("h1".into(), cc, probe, 3, ArcEmit(rec.clone()));
            rec
        });
        std::thread::sleep(Duration::from_millis(MIN_INTERVAL_MS * 2 + 300));
        ctrl.cancel();
        let rec = handle.join().unwrap();
        assert!(
            rec.count.load(Ordering::Relaxed) >= 1,
            "expected at least one snapshot",
        );
    }

    #[test]
    fn stream_cancel_before_start_exits_quickly() {
        let probe = TestProbe::new();
        let rec = Arc::new(Recorder::default());
        let ctrl = Arc::new(ActivityController::new());
        ctrl.set_interval_ms(MIN_INTERVAL_MS);
        ctrl.cancel();
        let cc = ctrl.clone();
        let start = Instant::now();
        let handle = std::thread::spawn(move || {
            run_activity_stream("h-cancel".into(), cc, probe, 3, ArcEmit(rec));
        });
        handle.join().unwrap();
        // probe() + warm-up + one check => <1s
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn stream_tick_is_monotonic() {
        let probe = TestProbe::new();
        let rec = Arc::new(Recorder::default());
        let ctrl = Arc::new(ActivityController::new());
        ctrl.set_interval_ms(MIN_INTERVAL_MS);
        let cc = ctrl.clone();
        let rec_c = rec.clone();
        let handle = std::thread::spawn(move || {
            run_activity_stream("h-tick".into(), cc, probe, 0, ArcEmit(rec_c));
        });
        // ~3 ticks after warm-up
        std::thread::sleep(Duration::from_millis(MIN_INTERVAL_MS * 4 + 300));
        ctrl.cancel();
        handle.join().unwrap();
        let events = rec.events.lock().unwrap();
        assert!(events.len() >= 2, "need multiple ticks, got {}", events.len());
        for w in events.windows(2) {
            assert!(
                w[1].tick > w[0].tick,
                "tick counter must be strictly increasing",
            );
        }
    }

    #[test]
    fn stream_respects_top_n_parameter() {
        let mut probe = TestProbe::new();
        probe.procs = (1..=5)
            .map(|i| ProcessRow {
                pid: i,
                parent_pid: None,
                name: format!("p{i}"),
                command: String::new(),
                user: None,
                cpu_percent: 0.0,
                memory_bytes: i as u64 * 10,
                start_time: 0,
                threads: None,
            })
            .collect();
        let rec = Arc::new(Recorder::default());
        let ctrl = Arc::new(ActivityController::new());
        ctrl.set_interval_ms(MIN_INTERVAL_MS);
        let cc = ctrl.clone();
        let rec_c = rec.clone();
        let handle = std::thread::spawn(move || {
            run_activity_stream("h-topn".into(), cc, probe, 2, ArcEmit(rec_c));
        });
        std::thread::sleep(Duration::from_millis(MIN_INTERVAL_MS * 2 + 300));
        ctrl.cancel();
        handle.join().unwrap();
        let events = rec.events.lock().unwrap();
        assert!(!events.is_empty());
        for snap in events.iter() {
            assert_eq!(snap.top_by_memory.len(), 2);
            assert_eq!(snap.processes.len(), 5);
        }
    }

    // ---- registry ----

    #[test]
    fn registry_round_trip() {
        let reg = ActivityRegistry::new();
        let id = next_activity_handle_id();
        let ctrl = Arc::new(ActivityController::new());
        reg.insert(id.clone(), ctrl.clone());
        assert_eq!(reg.len(), 1);
        reg.get(&id).unwrap().cancel();
        assert!(ctrl.is_cancelled());
        assert!(reg.remove(&id).is_some());
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn registry_cancel_all_covers_every_stream() {
        let reg = ActivityRegistry::new();
        let a = Arc::new(ActivityController::new());
        let b = Arc::new(ActivityController::new());
        reg.insert("a".into(), a.clone());
        reg.insert("b".into(), b.clone());
        reg.cancel_all();
        assert!(a.is_cancelled());
        assert!(b.is_cancelled());
    }

    #[test]
    fn registry_get_on_missing_is_none() {
        let reg = ActivityRegistry::new();
        assert!(reg.get("nope").is_none());
        assert!(reg.remove("nope").is_none());
    }

    #[test]
    fn handle_ids_unique_under_rapid_load() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        for _ in 0..5_000 {
            assert!(set.insert(next_activity_handle_id()));
        }
    }

    #[test]
    fn handle_wire_format_camelcase() {
        let h = ActivityHandle {
            id: "h".into(),
            interval_ms: 1000,
        };
        let v = serde_json::to_value(&h).unwrap();
        assert!(v.get("intervalMs").is_some());
    }
}
