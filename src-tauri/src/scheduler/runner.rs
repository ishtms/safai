//! scheduler loop + controller.
//!
//! controller owns the cancel flag, user cadence, and a condvar that
//! lets the loop break a long sleep on cadence change. loop is a thin
//! wrapper around tick so tests can drive behaviour without spinning
//! an OS thread.
//!
//! Clock trait lets tests step time forward without real waits. prod
//! reads SystemTime::now per tick.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::onboarding::storage;
use crate::onboarding::types::{OnboardingError, OnboardingState, ScheduleCadence};
use crate::onboarding::OnboardingStore;

use super::cadence::{cadence_interval_secs, compute_next_due, NextDue, SECS_PER_DAY};

/// longest park. two reasons:
///   * condvar should wake us on cadence change but OS spurious-wake
///     behaviour varies, so fall back to polling inside a human window
///   * hibernation may not advance Instant-based timeouts, so this
///     bounds the "missed scan on laptop wake" lag
pub const MAX_SLEEP_SECS: u64 = 15 * 60;

/// idle sleep when no cadence. long enough to not burn CPU, short
/// enough that a first cadence change shows up in a human window even
/// if the condvar notify got lost.
pub const IDLE_SLEEP_SECS: u64 = MAX_SLEEP_SECS;

pub trait Clock: Send + Sync {
    fn now_secs(&self) -> u64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_secs(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

pub trait SchedulerStateStore {
    fn access_state<F, R>(&self, access: F) -> Result<R, OnboardingError>
    where
        F: FnOnce(&mut OnboardingState) -> (R, bool);
}

impl SchedulerStateStore for &Path {
    fn access_state<F, R>(&self, access: F) -> Result<R, OnboardingError>
    where
        F: FnOnce(&mut OnboardingState) -> (R, bool),
    {
        let mut state = storage::load_or_default(self);
        let (result, should_save) = access(&mut state);
        if should_save {
            storage::save(self, &state)?;
        }
        Ok(result)
    }
}

impl SchedulerStateStore for PathBuf {
    fn access_state<F, R>(&self, access: F) -> Result<R, OnboardingError>
    where
        F: FnOnce(&mut OnboardingState) -> (R, bool),
    {
        self.as_path().access_state(access)
    }
}

impl SchedulerStateStore for Arc<OnboardingStore> {
    fn access_state<F, R>(&self, access: F) -> Result<R, OnboardingError>
    where
        F: FnOnce(&mut OnboardingState) -> (R, bool),
    {
        self.access(access)
    }
}

impl<T> SchedulerStateStore for &T
where
    T: SchedulerStateStore + ?Sized,
{
    fn access_state<F, R>(&self, access: F) -> Result<R, OnboardingError>
    where
        F: FnOnce(&mut OnboardingState) -> (R, bool),
    {
        (**self).access_state(access)
    }
}

/// shared cancel + cadence state + interruptible sleep. mutexes are
/// uncontended (few updates per user action), std::sync::Mutex is
/// fine, no parking_lot.
pub struct SchedulerController {
    cancel_flag: AtomicBool,
    cadence: Mutex<Option<ScheduleCadence>>,
    /// same shape as activity controller
    wait: (Mutex<()>, Condvar),
}

impl SchedulerController {
    pub fn new(initial: Option<ScheduleCadence>) -> Self {
        Self {
            cancel_flag: AtomicBool::new(false),
            cadence: Mutex::new(initial),
            wait: (Mutex::new(()), Condvar::new()),
        }
    }

    pub fn cadence(&self) -> Option<ScheduleCadence> {
        self.cadence.lock().ok().and_then(|g| *g)
    }

    /// wake the loop to re-evaluate. noop on same-value to avoid a
    /// pointless round-trip through the wait pair.
    pub fn set_cadence(&self, c: Option<ScheduleCadence>) -> bool {
        let changed = {
            let Ok(mut g) = self.cadence.lock() else {
                return false;
            };
            if *g == c {
                false
            } else {
                *g = c;
                true
            }
        };
        if changed {
            self.notify();
        }
        changed
    }

    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::Release);
        self.notify();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel_flag.load(Ordering::Acquire)
    }

    /// wake without cadence change. used by "run now" to short-circuit
    /// sleep.
    pub fn notify(&self) {
        let (lock, cv) = &self.wait;
        let _g = lock.lock().unwrap();
        cv.notify_all();
    }

    /// park up to dur. returns immediately on cancel or notify.
    pub fn sleep_cancellable(&self, dur: Duration) {
        let (lock, cv) = &self.wait;
        if let Ok(guard) = lock.lock() {
            let _ = cv.wait_timeout(guard, dur);
        }
    }
}

/// one tick. reads persisted state, decides whether to fire, returns
/// how long to sleep. isolated from the loop for testability.
///
/// side effects:
/// * AnchorAndWait: stamp last_scheduled_at = now + persist
/// * Overdue: fire(), stamp, persist
///
/// persist failures log + swallow. scheduler must keep ticking even
/// when the state file is briefly unwritable.
pub fn tick<S, F>(
    state_store: S,
    ctrl: &SchedulerController,
    clock: &dyn Clock,
    fire: F,
) -> Duration
where
    S: SchedulerStateStore,
    F: FnOnce(),
{
    let cadence = ctrl.cadence();
    let now = clock.now_secs();

    let mut fire = Some(fire);
    let mut planned_sleep_secs = IDLE_SLEEP_SECS;
    let sleep_secs = match state_store.access_state(|state| {
        let decision = compute_next_due(cadence, state.last_scheduled_at, now);
        let out = match decision {
            NextDue::Idle => (IDLE_SLEEP_SECS, false),
            NextDue::AnchorAndWait(interval) => {
                state.last_scheduled_at = Some(now);
                (interval, true)
            }
            NextDue::Overdue => {
                if let Some(fire) = fire.take() {
                    fire();
                }
                state.last_scheduled_at = Some(now);
                // Overdue implies cadence Some, sleep a full interval
                (
                    cadence.map(cadence_interval_secs).unwrap_or(SECS_PER_DAY),
                    true,
                )
            }
            NextDue::In(s) => (s, false),
        };
        planned_sleep_secs = out.0;
        out
    }) {
        Ok(secs) => secs,
        Err(e) => {
            eprintln!("[safai] scheduler state save failed (non-fatal): {e}");
            planned_sleep_secs
        }
    };

    // clamp so hibernation / spurious wakes can't defer by days
    Duration::from_secs(sleep_secs.min(MAX_SLEEP_SECS).max(1))
}

/// runs until cancel. blocks, spawn on a dedicated thread.
pub fn run_scheduler_loop<S, C, F>(
    state_store: S,
    ctrl: Arc<SchedulerController>,
    clock: C,
    fire: F,
) where
    S: SchedulerStateStore + Send + 'static,
    C: Clock + 'static,
    F: Fn() + Send + Sync + 'static,
{
    while !ctrl.is_cancelled() {
        let sleep = tick(&state_store, &ctrl, &clock, || fire());
        if ctrl.is_cancelled() {
            break;
        }
        ctrl.sleep_cancellable(sleep);
    }
}

/// test-only clock tests can step forward deterministically
#[cfg(test)]
pub struct MockClock {
    pub now: std::sync::atomic::AtomicU64,
}

#[cfg(test)]
impl MockClock {
    pub fn new(start: u64) -> Self {
        Self {
            now: std::sync::atomic::AtomicU64::new(start),
        }
    }

    pub fn advance(&self, secs: u64) {
        self.now.fetch_add(secs, Ordering::AcqRel);
    }

    pub fn set(&self, t: u64) {
        self.now.store(t, Ordering::Release);
    }
}

#[cfg(test)]
impl Clock for MockClock {
    fn now_secs(&self) -> u64 {
        self.now.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onboarding::types::{OnboardingState, ScheduleCadence};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    fn fresh_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    // ------- Controller -------

    #[test]
    fn controller_default_cadence_is_none() {
        let c = SchedulerController::new(None);
        assert_eq!(c.cadence(), None);
        assert!(!c.is_cancelled());
    }

    #[test]
    fn controller_set_cadence_signals_change() {
        let c = SchedulerController::new(None);
        assert!(c.set_cadence(Some(ScheduleCadence::Daily)));
        assert_eq!(c.cadence(), Some(ScheduleCadence::Daily));
        // same value = noop, returns false
        assert!(!c.set_cadence(Some(ScheduleCadence::Daily)));
    }

    #[test]
    fn controller_cancel_is_idempotent() {
        let c = SchedulerController::new(None);
        c.cancel();
        c.cancel();
        assert!(c.is_cancelled());
    }

    #[test]
    fn controller_sleep_wakes_on_cancel() {
        let c = Arc::new(SchedulerController::new(None));
        let cc = c.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            cc.cancel();
        });
        let start = Instant::now();
        c.sleep_cancellable(Duration::from_secs(5));
        assert!(
            start.elapsed() < Duration::from_millis(600),
            "expected sleep to wake near-instantly on cancel, got {:?}",
            start.elapsed(),
        );
    }

    #[test]
    fn controller_sleep_wakes_on_cadence_change() {
        let c = Arc::new(SchedulerController::new(None));
        let cc = c.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            cc.set_cadence(Some(ScheduleCadence::Daily));
        });
        let start = Instant::now();
        c.sleep_cancellable(Duration::from_secs(10));
        assert!(
            start.elapsed() < Duration::from_millis(600),
            "expected sleep to wake on cadence change, got {:?}",
            start.elapsed(),
        );
    }

    #[test]
    fn controller_notify_wakes_sleep() {
        // explicit notify ("run now" nudge) must also break the sleep
        let c = Arc::new(SchedulerController::new(Some(ScheduleCadence::Weekly)));
        let cc = c.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            cc.notify();
        });
        let start = Instant::now();
        c.sleep_cancellable(Duration::from_secs(10));
        assert!(
            start.elapsed() < Duration::from_millis(600),
            "expected notify() to wake sleep, got {:?}",
            start.elapsed(),
        );
    }

    // ------- tick(): idle branch -------

    #[test]
    fn tick_idle_when_no_cadence_set() {
        let d = fresh_dir();
        let ctrl = SchedulerController::new(None);
        let clock = MockClock::new(1_000_000);
        let fires = AtomicU64::new(0);
        let dur = tick(d.path(), &ctrl, &clock, || {
            fires.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(fires.load(Ordering::Relaxed), 0);
        assert_eq!(dur, Duration::from_secs(MAX_SLEEP_SECS));
        // idle branch must not create a state file
        assert!(!storage::state_path(d.path()).exists());
    }

    // ------- tick(): AnchorAndWait branch -------

    #[test]
    fn tick_anchors_first_time_and_persists() {
        let d = fresh_dir();
        let ctrl = SchedulerController::new(Some(ScheduleCadence::Daily));
        let clock = MockClock::new(1_000_000);
        let fires = AtomicU64::new(0);
        let dur = tick(d.path(), &ctrl, &clock, || {
            fires.fetch_add(1, Ordering::Relaxed);
        });
        // first anchor tick must not fire
        assert_eq!(fires.load(Ordering::Relaxed), 0);
        // must persist so next tick doesn't re-anchor
        let state = storage::load_or_default(d.path());
        assert_eq!(state.last_scheduled_at, Some(1_000_000));
        // clamp = min(interval, MAX_SLEEP_SECS)
        assert_eq!(dur, Duration::from_secs(MAX_SLEEP_SECS));
    }

    // ------- tick(): Overdue branch -------

    #[test]
    fn tick_fires_and_stamps_when_overdue() {
        let d = fresh_dir();
        // seed "last run 2 days ago"
        let mut seed = OnboardingState::default();
        seed.last_scheduled_at = Some(1_000_000 - 2 * SECS_PER_DAY);
        storage::save(d.path(), &seed).unwrap();

        let ctrl = SchedulerController::new(Some(ScheduleCadence::Daily));
        let clock = MockClock::new(1_000_000);
        let fires = AtomicU64::new(0);
        let _dur = tick(d.path(), &ctrl, &clock, || {
            fires.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(fires.load(Ordering::Relaxed), 1);
        let state = storage::load_or_default(d.path());
        assert_eq!(state.last_scheduled_at, Some(1_000_000));
    }

    #[test]
    fn tick_does_not_fire_twice_in_a_row() {
        // after fire, last = now. next tick at same now sees
        // elapsed=0 < interval, must not refire. debounce guard.
        let d = fresh_dir();
        let mut seed = OnboardingState::default();
        seed.last_scheduled_at = Some(1_000_000 - 2 * SECS_PER_DAY);
        storage::save(d.path(), &seed).unwrap();

        let ctrl = SchedulerController::new(Some(ScheduleCadence::Daily));
        let clock = MockClock::new(1_000_000);
        let fires = AtomicU64::new(0);
        tick(d.path(), &ctrl, &clock, || {
            fires.fetch_add(1, Ordering::Relaxed);
        });
        tick(d.path(), &ctrl, &clock, || {
            fires.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(
            fires.load(Ordering::Relaxed),
            1,
            "second tick at same now must not re-fire",
        );
    }

    // ------- tick(): In branch -------

    #[test]
    fn tick_returns_remaining_time_when_not_due() {
        let d = fresh_dir();
        let mut seed = OnboardingState::default();
        seed.last_scheduled_at = Some(1_000_000 - 3_600); // 1h ago
        storage::save(d.path(), &seed).unwrap();

        let ctrl = SchedulerController::new(Some(ScheduleCadence::Daily));
        let clock = MockClock::new(1_000_000);
        let fires = AtomicU64::new(0);
        let dur = tick(d.path(), &ctrl, &clock, || {
            fires.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(fires.load(Ordering::Relaxed), 0);
        // raw remaining 23h, sleep clamps to MAX_SLEEP_SECS
        assert_eq!(dur, Duration::from_secs(MAX_SLEEP_SECS));
    }

    #[test]
    fn tick_returns_short_sleep_near_boundary() {
        let d = fresh_dir();
        let mut seed = OnboardingState::default();
        // last run is (daily - 45s) ago, 45s remaining
        seed.last_scheduled_at = Some(1_000_000 - (SECS_PER_DAY - 45));
        storage::save(d.path(), &seed).unwrap();

        let ctrl = SchedulerController::new(Some(ScheduleCadence::Daily));
        let clock = MockClock::new(1_000_000);
        let dur = tick(d.path(), &ctrl, &clock, || {});
        assert_eq!(dur, Duration::from_secs(45));
    }

    #[test]
    fn tick_never_returns_zero_sleep() {
        // boundary -> Overdue -> fire + sleep one interval, clamped to
        // MAX_SLEEP_SECS. must never be 0 or we'd busy-loop.
        let d = fresh_dir();
        let ctrl = SchedulerController::new(Some(ScheduleCadence::Daily));
        let clock = MockClock::new(1_000_000);
        let dur = tick(d.path(), &ctrl, &clock, || {});
        assert!(dur >= Duration::from_secs(1));
    }

    // ------- tick(): clock-skew & corruption -------

    #[test]
    fn tick_tolerates_backwards_clock() {
        let d = fresh_dir();
        let mut seed = OnboardingState::default();
        seed.last_scheduled_at = Some(2_000_000);
        storage::save(d.path(), &seed).unwrap();

        let ctrl = SchedulerController::new(Some(ScheduleCadence::Daily));
        let clock = MockClock::new(1_000_000); // backwards
        let fires = AtomicU64::new(0);
        let dur = tick(d.path(), &ctrl, &clock, || {
            fires.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(fires.load(Ordering::Relaxed), 0);
        // skew treated as 0 elapsed so no fire. clamped.
        assert_eq!(dur, Duration::from_secs(MAX_SLEEP_SECS));
    }

    #[test]
    fn tick_survives_corrupt_state_file() {
        let d = fresh_dir();
        std::fs::write(
            storage::state_path(d.path()),
            b"not valid json and never will be",
        )
        .unwrap();

        let ctrl = SchedulerController::new(Some(ScheduleCadence::Daily));
        let clock = MockClock::new(1_000_000);
        let fires = AtomicU64::new(0);
        let _dur = tick(d.path(), &ctrl, &clock, || {
            fires.fetch_add(1, Ordering::Relaxed);
        });
        // corrupt -> defaults (last=None) -> AnchorAndWait. no fire.
        assert_eq!(fires.load(Ordering::Relaxed), 0);
        let state = storage::load_or_default(d.path());
        assert_eq!(state.last_scheduled_at, Some(1_000_000));
    }

    // ------- tick(): state preservation -------

    #[test]
    fn tick_preserves_unrelated_state_fields() {
        // scheduler must only write last_scheduled_at. prefs,
        // permissions, completion timestamp must be preserved.
        let d = fresh_dir();
        let mut seed = OnboardingState::default();
        seed.mark_complete(555);
        seed.prefs.auto_scan_on_launch = true;
        seed.telemetry_opt_in = true;
        seed.record_permission(
            crate::onboarding::types::PermissionKind::MacFullDiskAccess,
            crate::onboarding::types::PermissionStatus::Granted,
            42,
        );
        storage::save(d.path(), &seed).unwrap();

        let ctrl = SchedulerController::new(Some(ScheduleCadence::Daily));
        let clock = MockClock::new(1_000_000);
        tick(d.path(), &ctrl, &clock, || {});

        let after = storage::load_or_default(d.path());
        assert_eq!(after.completed_at, Some(555));
        assert!(after.prefs.auto_scan_on_launch);
        assert!(after.telemetry_opt_in);
        assert_eq!(after.permissions.len(), 1);
        assert_eq!(after.last_scheduled_at, Some(1_000_000));
    }

    // ------- tick(): cadence change mid-schedule -------

    #[test]
    fn tick_respects_cadence_change() {
        let d = fresh_dir();
        let mut seed = OnboardingState::default();
        seed.last_scheduled_at = Some(1_000_000 - 2 * SECS_PER_DAY);
        storage::save(d.path(), &seed).unwrap();

        // was daily, now weekly. 2d elapsed > daily but < weekly, so
        // NOT overdue.
        let ctrl = SchedulerController::new(Some(ScheduleCadence::Weekly));
        let clock = MockClock::new(1_000_000);
        let fires = AtomicU64::new(0);
        tick(d.path(), &ctrl, &clock, || {
            fires.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(fires.load(Ordering::Relaxed), 0);
    }

    // ------- Loop -------

    #[test]
    fn loop_exits_promptly_on_cancel() {
        let d = fresh_dir();
        let ctrl = Arc::new(SchedulerController::new(Some(ScheduleCadence::Daily)));
        let cc = ctrl.clone();
        let path = d.path().to_path_buf();
        let fires = Arc::new(AtomicU64::new(0));
        let fires_c = fires.clone();
        let clock = MockClock::new(1_000_000);
        let t = std::thread::spawn(move || {
            run_scheduler_loop(path, cc, clock, move || {
                fires_c.fetch_add(1, Ordering::Relaxed);
            });
        });
        std::thread::sleep(Duration::from_millis(50));
        ctrl.cancel();
        let joined = t.join();
        assert!(joined.is_ok(), "loop thread must join cleanly");
    }

    #[test]
    fn loop_fires_once_when_overdue() {
        // seed so first tick is Overdue, run loop briefly, expect 1 fire
        let d = fresh_dir();
        let mut seed = OnboardingState::default();
        seed.last_scheduled_at = Some(1_000_000 - 2 * SECS_PER_DAY);
        storage::save(d.path(), &seed).unwrap();

        let ctrl = Arc::new(SchedulerController::new(Some(ScheduleCadence::Daily)));
        let cc = ctrl.clone();
        let path = d.path().to_path_buf();
        let fires = Arc::new(AtomicU64::new(0));
        let fires_c = fires.clone();
        let clock = MockClock::new(1_000_000);
        let t = std::thread::spawn(move || {
            run_scheduler_loop(path, cc, clock, move || {
                fires_c.fetch_add(1, Ordering::Relaxed);
            });
        });
        // let the first tick run
        std::thread::sleep(Duration::from_millis(100));
        ctrl.cancel();
        t.join().unwrap();
        assert_eq!(
            fires.load(Ordering::Relaxed),
            1,
            "loop must fire exactly once when overdue then debounce"
        );
    }

    #[test]
    fn set_cadence_from_thread_wakes_loop() {
        // cadence change must break current sleep, otherwise
        // Never -> Daily waits MAX_SLEEP_SECS before anchoring
        let d = fresh_dir();
        let ctrl = Arc::new(SchedulerController::new(None));
        let cc = ctrl.clone();
        let path = d.path().to_path_buf();
        let fires = Arc::new(AtomicU64::new(0));
        let fires_c = fires.clone();
        let clock = MockClock::new(1_000_000);
        let t = std::thread::spawn(move || {
            run_scheduler_loop(path, cc, clock, move || {
                fires_c.fetch_add(1, Ordering::Relaxed);
            });
        });
        std::thread::sleep(Duration::from_millis(30));
        ctrl.set_cadence(Some(ScheduleCadence::Daily));
        std::thread::sleep(Duration::from_millis(80));
        ctrl.cancel();
        t.join().unwrap();
        // second tick should have anchored by now
        let state = storage::load_or_default(d.path());
        assert!(
            state.last_scheduled_at.is_some(),
            "cadence change should have triggered an anchoring tick"
        );
    }

    #[test]
    fn backing_file_save_failure_does_not_propagate() {
        // scheduler must keep running if save fails. we plant a dir
        // named state.json to force save to fail.
        let d = fresh_dir();
        std::fs::create_dir_all(storage::state_path(d.path())).unwrap();

        let ctrl = SchedulerController::new(Some(ScheduleCadence::Daily));
        let clock = MockClock::new(1_000_000);
        let fires = AtomicU64::new(0);
        let dur = tick(d.path(), &ctrl, &clock, || {
            fires.fetch_add(1, Ordering::Relaxed);
        });
        // even with persist failing, tick must return a sane sleep
        assert!(dur >= Duration::from_secs(1));
        // anchor branch, no fire, no panic
        assert_eq!(fires.load(Ordering::Relaxed), 0);
    }
}
