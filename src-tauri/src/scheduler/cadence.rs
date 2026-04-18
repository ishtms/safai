//! pure cadence math for the scheduler.
//!
//! every fn here is a pure function of its inputs. no I/O, no wallclock
//! coupling, no ambient state. runner loop hits these every tick and
//! tests run every branch against a mock clock.

use crate::onboarding::types::ScheduleCadence;

pub const SECS_PER_DAY: u64 = 24 * 60 * 60;

/// monthly = 30 days approx. no real calendar math because:
/// * user mental model is "about once a month"
/// * cron-style "1st at 03:00" needs wake/sleep state we can't cleanly
///   observe on all platforms
/// * we already handle "app closed for 2 weeks then reopened" via
///   last_run vs now, cron wouldn't
#[inline]
pub fn cadence_interval_secs(c: ScheduleCadence) -> u64 {
    match c {
        ScheduleCadence::Daily => SECS_PER_DAY,
        ScheduleCadence::Weekly => 7 * SECS_PER_DAY,
        ScheduleCadence::Monthly => 30 * SECS_PER_DAY,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextDue {
    /// no cadence configured. runner should long-sleep for a cadence
    /// change instead of polling.
    Idle,
    /// never run before. runner anchors last_scheduled_at = now and
    /// waits one interval. separate variant from In so the anchor
    /// happens exactly once instead of Idle forever.
    AnchorAndWait(u64),
    /// fire now, stamp last_scheduled_at, sleep one interval
    Overdue,
    In(u64),
}

/// clock-skew safe via saturating_sub, backwards NTP jump won't wrap
/// into a giant delay.
#[inline]
pub fn compute_next_due(
    cadence: Option<ScheduleCadence>,
    last_run: Option<u64>,
    now: u64,
) -> NextDue {
    let Some(c) = cadence else {
        return NextDue::Idle;
    };
    let interval = cadence_interval_secs(c);
    match last_run {
        None => NextDue::AnchorAndWait(interval),
        Some(last) => {
            // saturating_sub protects against backwards clock jumps
            let elapsed = now.saturating_sub(last);
            if elapsed >= interval {
                NextDue::Overdue
            } else {
                NextDue::In(interval - elapsed)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals_match_published_values() {
        assert_eq!(cadence_interval_secs(ScheduleCadence::Daily), 86_400);
        assert_eq!(cadence_interval_secs(ScheduleCadence::Weekly), 604_800);
        assert_eq!(cadence_interval_secs(ScheduleCadence::Monthly), 2_592_000);
    }

    #[test]
    fn no_cadence_is_idle() {
        assert_eq!(compute_next_due(None, None, 1000), NextDue::Idle);
        assert_eq!(compute_next_due(None, Some(1), 1000), NextDue::Idle);
    }

    #[test]
    fn never_run_anchors_and_waits_one_interval() {
        assert_eq!(
            compute_next_due(Some(ScheduleCadence::Daily), None, 1000),
            NextDue::AnchorAndWait(SECS_PER_DAY),
        );
        assert_eq!(
            compute_next_due(Some(ScheduleCadence::Weekly), None, 1000),
            NextDue::AnchorAndWait(7 * SECS_PER_DAY),
        );
    }

    #[test]
    fn within_interval_returns_remaining_time() {
        let now = 1_000_000;
        let last = now - 3600; // 1h ago
        assert_eq!(
            compute_next_due(Some(ScheduleCadence::Daily), Some(last), now),
            NextDue::In(SECS_PER_DAY - 3600),
        );
    }

    #[test]
    fn exactly_at_boundary_is_overdue() {
        let now = 1_000_000;
        let last = now - SECS_PER_DAY;
        assert_eq!(
            compute_next_due(Some(ScheduleCadence::Daily), Some(last), now),
            NextDue::Overdue,
        );
    }

    #[test]
    fn past_boundary_is_overdue() {
        let now = 1_000_000;
        let last = now - 2 * SECS_PER_DAY;
        assert_eq!(
            compute_next_due(Some(ScheduleCadence::Daily), Some(last), now),
            NextDue::Overdue,
        );
    }

    #[test]
    fn backwards_clock_never_panics_and_is_bounded() {
        // last > now: clock jumped backwards. must not wrap into a
        // huge delay or we'd defer the next scan for years.
        let now = 100;
        let last = 1_000_000;
        let due = compute_next_due(Some(ScheduleCadence::Daily), Some(last), now);
        // saturating_sub -> 0 elapsed -> full interval remaining
        assert_eq!(due, NextDue::In(SECS_PER_DAY));
    }

    #[test]
    fn zero_now_with_zero_last_is_overdue() {
        // fresh epoch, elapsed=0 < interval, wait full interval. guards
        // the edge case of mock clock at 0 + last=0 in state file.
        let due = compute_next_due(Some(ScheduleCadence::Daily), Some(0), 0);
        assert_eq!(due, NextDue::In(SECS_PER_DAY));
    }

    #[test]
    fn weekly_and_monthly_math_matches_days() {
        let now = 10_000_000;
        // 3 days into a weekly, 4 remaining
        let due = compute_next_due(
            Some(ScheduleCadence::Weekly),
            Some(now - 3 * SECS_PER_DAY),
            now,
        );
        assert_eq!(due, NextDue::In(4 * SECS_PER_DAY));

        // 29 days into a monthly, 1 remaining
        let due = compute_next_due(
            Some(ScheduleCadence::Monthly),
            Some(now - 29 * SECS_PER_DAY),
            now,
        );
        assert_eq!(due, NextDue::In(SECS_PER_DAY));
    }

    #[test]
    fn one_second_before_boundary_is_not_overdue() {
        let now = 1_000_000;
        let last = now - (SECS_PER_DAY - 1);
        assert_eq!(
            compute_next_due(Some(ScheduleCadence::Daily), Some(last), now),
            NextDue::In(1),
        );
    }
}
