//! background scan scheduler.
//!
//! single dedicated thread. periodically checks if the user cadence
//! (daily/weekly/monthly) has elapsed since last fire, runs a callback
//! wired by the tauri layer ("kick off a scan").
//!
//! layout:
//! * cadence: pure math for next-due
//! * runner: SchedulerController + run_scheduler_loop + tick. every
//!   side effect is parameterised so tests drive with a mock clock
//!
//! no cron library. next-due math is 2 lines, sleep loop is 5.
//! `cron` / `tokio-cron-scheduler` would add a tree bigger than the
//! module itself. activity stream already ships the condvar
//! sleep pattern.

pub mod cadence;
pub mod runner;

pub use cadence::cadence_interval_secs;
#[allow(unused_imports)]
pub use cadence::{compute_next_due, NextDue};
pub use runner::{run_scheduler_loop, SchedulerController, SystemClock};
#[allow(unused_imports)]
pub use runner::{tick, Clock, IDLE_SLEEP_SECS, MAX_SLEEP_SECS};

use std::sync::Arc;

/// wire type for "what the scheduler is doing next". Settings reads
/// this for the "Next scheduled scan: ..." readout.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SchedulerStatus {
    pub cadence: Option<crate::onboarding::types::ScheduleCadence>,
    pub last_run_at: Option<u64>,
    /// None when disabled or never-anchored. otherwise last_run_at + interval.
    pub next_run_at: Option<u64>,
    pub seconds_until_next: Option<u64>,
}

impl SchedulerStatus {
    /// pure, caller loads state separately
    pub fn derive(
        cadence: Option<crate::onboarding::types::ScheduleCadence>,
        last_run_at: Option<u64>,
        now: u64,
    ) -> Self {
        let (next_run_at, seconds_until_next) = match cadence {
            None => (None, None),
            Some(c) => {
                let interval = cadence_interval_secs(c);
                match last_run_at {
                    None => (None, Some(interval)),
                    Some(last) => {
                        let next = last.saturating_add(interval);
                        let remaining = next.saturating_sub(now);
                        (Some(next), Some(remaining))
                    }
                }
            }
        };
        Self {
            cadence,
            last_run_at,
            next_run_at,
            seconds_until_next,
        }
    }
}

/// handle held as tauri managed state. commands layer uses it to
/// change cadence or cancel from any thread.
pub struct Scheduler {
    pub controller: Arc<SchedulerController>,
}

impl Scheduler {
    pub fn new(controller: Arc<SchedulerController>) -> Self {
        Self { controller }
    }
}

#[cfg(test)]
mod facade_tests {
    //! e2e through the public API. verifies the state-change sequence
    //! commands layer depends on: cadence change wakes the loop, fire
    //! stamps last_scheduled_at, cadence change resets the anchor
    //! (settings_update's behaviour).

    use super::*;
    use crate::onboarding::storage;
    use crate::onboarding::types::{OnboardingState, ScheduleCadence};
    use runner::MockClock;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn cadence_change_to_none_prevents_further_fires() {
        // user turns off scheduling after it was on
        let tmp = tempfile::tempdir().unwrap();
        let mut seed = OnboardingState::default();
        seed.last_scheduled_at = Some(1_000_000 - 2 * cadence::SECS_PER_DAY);
        storage::save(tmp.path(), &seed).unwrap();

        let ctrl = Arc::new(SchedulerController::new(Some(ScheduleCadence::Daily)));
        // flip to None before first tick
        ctrl.set_cadence(None);

        let clock = MockClock::new(1_000_000);
        let fires = AtomicU64::new(0);
        let _dur = runner::tick(tmp.path(), &ctrl, &clock, || {
            fires.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(fires.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn clearing_last_run_on_cadence_change_resets_the_anchor() {
        // settings_update clears last_scheduled_at on cadence change.
        // tick that sees the cleared anchor should hit AnchorAndWait,
        // not fire off a stale pre-change timestamp.
        let tmp = tempfile::tempdir().unwrap();
        let mut seed = OnboardingState::default();
        seed.prefs.scheduled_scan = Some(ScheduleCadence::Weekly);
        // was daily, ran yesterday, now weekly. settings_update cleared
        // last_scheduled_at on the flip.
        seed.last_scheduled_at = None;
        storage::save(tmp.path(), &seed).unwrap();

        let ctrl = Arc::new(SchedulerController::new(Some(ScheduleCadence::Weekly)));
        let clock = MockClock::new(1_000_000);
        let fires = AtomicU64::new(0);
        runner::tick(tmp.path(), &ctrl, &clock, || {
            fires.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(fires.load(Ordering::Relaxed), 0);
        let after = storage::load_or_default(tmp.path());
        assert_eq!(after.last_scheduled_at, Some(1_000_000));
    }

    #[test]
    fn two_ticks_over_a_full_interval_fire_exactly_twice() {
        // anchor -> +24h -> tick (fire) -> +24h -> tick (fire)
        let tmp = tempfile::tempdir().unwrap();
        let ctrl = SchedulerController::new(Some(ScheduleCadence::Daily));
        let clock = MockClock::new(1_000_000);
        let fires = AtomicU64::new(0);

        // anchor
        runner::tick(tmp.path(), &ctrl, &clock, || {
            fires.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(fires.load(Ordering::Relaxed), 0, "anchor must not fire");

        // +1d, fire
        clock.advance(cadence::SECS_PER_DAY);
        runner::tick(tmp.path(), &ctrl, &clock, || {
            fires.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(fires.load(Ordering::Relaxed), 1);

        // +1d, fire again
        clock.advance(cadence::SECS_PER_DAY);
        runner::tick(tmp.path(), &ctrl, &clock, || {
            fires.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(fires.load(Ordering::Relaxed), 2);

        // no advance, no fire (debounce)
        runner::tick(tmp.path(), &ctrl, &clock, || {
            fires.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(fires.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn state_survives_controller_replacement() {
        // app restart: drop controller, state.json persists, new
        // controller must honour persisted last_scheduled_at
        let tmp = tempfile::tempdir().unwrap();

        let clock = MockClock::new(1_000_000);
        let c1 = SchedulerController::new(Some(ScheduleCadence::Daily));
        let fires = AtomicU64::new(0);
        runner::tick(tmp.path(), &c1, &clock, || {
            fires.fetch_add(1, Ordering::Relaxed);
        });
        // anchor stamped at 1_000_000
        drop(c1);

        // relaunch, +12h, not due yet
        let c2 = SchedulerController::new(Some(ScheduleCadence::Daily));
        clock.advance(cadence::SECS_PER_DAY / 2);
        runner::tick(tmp.path(), &c2, &clock, || {
            fires.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(fires.load(Ordering::Relaxed), 0);

        // +12h more, one full interval from anchor, fires
        clock.advance(cadence::SECS_PER_DAY / 2);
        runner::tick(tmp.path(), &c2, &clock, || {
            fires.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(fires.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn scheduler_status_matches_tick_view() {
        // settings uses SchedulerStatus::derive, scheduler thread uses
        // tick. both must agree on next-due given the same state.
        let tmp = tempfile::tempdir().unwrap();
        let mut seed = OnboardingState::default();
        seed.last_scheduled_at = Some(1_000_000);
        seed.prefs.scheduled_scan = Some(ScheduleCadence::Weekly);
        storage::save(tmp.path(), &seed).unwrap();

        let now = 1_000_000 + 24 * 3600;
        let status = SchedulerStatus::derive(
            seed.prefs.scheduled_scan,
            seed.last_scheduled_at,
            now,
        );
        let expected_next = 1_000_000 + 7 * 24 * 3600;
        assert_eq!(status.next_run_at, Some(expected_next));
        assert_eq!(status.seconds_until_next, Some(6 * 24 * 3600));

        let clock = MockClock::new(now);
        let ctrl = SchedulerController::new(seed.prefs.scheduled_scan);
        let fires = AtomicU64::new(0);
        let dur = runner::tick(tmp.path(), &ctrl, &clock, || {
            fires.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(fires.load(Ordering::Relaxed), 0);
        // tick sleep is clamped to MAX_SLEEP_SECS, not the raw 6 days.
        // intentional, see runner::MAX_SLEEP_SECS.
        assert_eq!(dur, Duration::from_secs(runner::MAX_SLEEP_SECS));
    }

    #[test]
    fn many_ticks_with_no_cadence_never_touch_state_file() {
        // Idle tick must not persist anything. cadence=None is the
        // default, writing on every tick would trash disks for free.
        let tmp = tempfile::tempdir().unwrap();
        let ctrl = SchedulerController::new(None);
        let clock = MockClock::new(1_000_000);
        for _ in 0..50 {
            runner::tick(tmp.path(), &ctrl, &clock, || {});
        }
        assert!(
            !storage::state_path(tmp.path()).exists(),
            "idle ticks must not create state.json",
        );
    }
}

#[cfg(test)]
mod status_tests {
    use super::*;
    use crate::onboarding::types::ScheduleCadence;

    #[test]
    fn status_disabled_when_no_cadence() {
        let s = SchedulerStatus::derive(None, None, 0);
        assert_eq!(s.cadence, None);
        assert_eq!(s.next_run_at, None);
        assert_eq!(s.seconds_until_next, None);
    }

    #[test]
    fn status_never_run_reports_one_interval_remaining() {
        let s = SchedulerStatus::derive(Some(ScheduleCadence::Daily), None, 1_000_000);
        assert_eq!(s.next_run_at, None);
        assert_eq!(s.seconds_until_next, Some(86_400));
    }

    #[test]
    fn status_computes_next_from_last_plus_interval() {
        let s = SchedulerStatus::derive(
            Some(ScheduleCadence::Daily),
            Some(1_000_000),
            1_000_000 + 3600,
        );
        assert_eq!(s.next_run_at, Some(1_000_000 + 86_400));
        assert_eq!(s.seconds_until_next, Some(86_400 - 3600));
    }

    #[test]
    fn status_overdue_reports_zero_remaining() {
        let now = 2_000_000;
        let last = now - 90_000; // past 1-day boundary
        let s = SchedulerStatus::derive(Some(ScheduleCadence::Daily), Some(last), now);
        assert_eq!(s.seconds_until_next, Some(0));
    }

    #[test]
    fn status_round_trips_through_serde_as_camel_case() {
        let s = SchedulerStatus::derive(
            Some(ScheduleCadence::Weekly),
            Some(123),
            456,
        );
        let j = serde_json::to_string(&s).unwrap();
        assert!(j.contains("lastRunAt"), "{j}");
        assert!(j.contains("nextRunAt"), "{j}");
        assert!(j.contains("secondsUntilNext"), "{j}");
        let back: SchedulerStatus = serde_json::from_str(&j).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn status_saturating_sub_protects_backwards_clock() {
        // last_run is in the future, clock went backwards
        let s = SchedulerStatus::derive(
            Some(ScheduleCadence::Daily),
            Some(2_000_000),
            1_000_000,
        );
        // invariant: no underflow panic
        assert!(s.seconds_until_next.is_some());
        assert!(s.next_run_at.is_some());
    }
}
