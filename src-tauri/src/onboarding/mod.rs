//! onboarding state machine + persistence.
//!
//! UI shows 4 steps: welcome -> permissions -> prefs -> ready. user
//! can back up, skip where allowed, or bail. every transition saves so
//! a kill-9 mid-flow leaves a consistent file.
//!
//! layout:
//! - types: on-disk OnboardingState + Preferences + per-permission
//!   record. serde camelCase, schema version stamped for migrations.
//! - storage: atomic load/save at `<data_dir>/state.json`. tolerant of
//!   any realistic corruption, falls back to defaults on parse fail so
//!   a bad state file can't brick the app.
//! - permissions: per-OS catalog of user-facing gates + System Settings
//!   deep-link URLs. best-effort detect_status probe for macOS FDA.
//!
//! separate from cleaner on purpose. cleaner owns deletion+undo state,
//! this owns "what the user agreed to". corrupt state.json can't block
//! a restore, two files side-by-side but independent.

pub mod permissions;
pub mod storage;
pub mod types;

pub use permissions::{applicable_for, detect_status, open_settings, settings_url, Platform};
#[allow(unused_imports)]
pub use storage::{load, load_or_default, reset, save, state_path, STATE_FILE_NAME};
#[allow(unused_imports)]
pub use types::{
    IncludedCategory, OnboardingError, OnboardingState, OnboardingStep, PermissionKind,
    PermissionRecord, PermissionStatus, Preferences, ScheduleCadence, CURRENT_SCHEMA_VERSION,
};

#[cfg(test)]
mod facade_tests {
    //! e2e for the onboarding module. units live in types/storage/
    //! permissions, these verify the sequence (welcome -> prefs ->
    //! permissions -> complete) survives disk round-trips the way UI
    //! drives it.

    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[test]
    fn fresh_install_walks_through_all_four_steps() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();

        // nothing on disk
        let s = load_or_default(data);
        assert!(!s.is_onboarded());
        assert_eq!(s.last_step, OnboardingStep::Welcome);

        // click through to permissions
        let mut s = s;
        s.last_step = OnboardingStep::Permissions;
        save(data, &s).unwrap();

        // mac grants FDA, skips Files & Folders
        let mut s = load(data).unwrap();
        s.record_permission(
            PermissionKind::MacFullDiskAccess,
            PermissionStatus::Granted,
            10,
        );
        s.record_permission(
            PermissionKind::MacFilesAndFolders,
            PermissionStatus::Denied,
            11,
        );
        s.last_step = OnboardingStep::Prefs;
        save(data, &s).unwrap();

        // tweak prefs
        let mut s = load(data).unwrap();
        s.prefs.auto_scan_on_launch = true;
        s.prefs.scheduled_scan = Some(ScheduleCadence::Weekly);
        s.telemetry_opt_in = true;
        s.last_step = OnboardingStep::Ready;
        save(data, &s).unwrap();

        // complete
        let mut s = load(data).unwrap();
        s.mark_complete(100);
        save(data, &s).unwrap();

        // re-read, persisted
        let final_s = load(data).unwrap();
        assert!(final_s.is_onboarded());
        assert_eq!(final_s.completed_at, Some(100));
        assert_eq!(final_s.last_step, OnboardingStep::Done);
        assert!(final_s.prefs.auto_scan_on_launch);
        assert!(final_s.telemetry_opt_in);
        assert_eq!(final_s.permissions.len(), 2);
    }

    #[test]
    fn resume_after_crash_preserves_last_step_and_partial_answers() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = OnboardingState::default();
        s.last_step = OnboardingStep::Prefs;
        s.record_permission(
            PermissionKind::MacFullDiskAccess,
            PermissionStatus::Granted,
            1,
        );
        save(tmp.path(), &s).unwrap();

        // relaunch
        let resumed = load_or_default(tmp.path());
        assert_eq!(resumed.last_step, OnboardingStep::Prefs);
        assert_eq!(resumed.permissions.len(), 1);
        assert!(!resumed.is_onboarded(), "still mid-flow, not complete");
    }

    #[test]
    fn reset_then_load_returns_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = OnboardingState::default();
        s.mark_complete(1);
        save(tmp.path(), &s).unwrap();

        reset(tmp.path()).unwrap();
        let after = load_or_default(tmp.path());
        assert!(!after.is_onboarded());
    }

    #[test]
    fn concurrent_saves_never_produce_torn_reads() {
        // two threads saving distinct state. file must always parse and
        // always reflect one full write, never a half-mixture.
        let tmp = tempfile::tempdir().unwrap();
        let data: Arc<std::path::PathBuf> = Arc::new(tmp.path().to_path_buf());
        save(&data, &OnboardingState::default()).unwrap();
        let parse_errors = Arc::new(AtomicU32::new(0));

        let reader_errors = parse_errors.clone();
        let reader_path = data.clone();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader_stop = stop.clone();
        let reader = std::thread::spawn(move || {
            while !reader_stop.load(Ordering::Relaxed) {
                if let Ok(text) =
                    std::fs::read_to_string(reader_path.join(STATE_FILE_NAME))
                {
                    if !text.is_empty() && serde_json::from_str::<OnboardingState>(&text).is_err() {
                        reader_errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        });

        let w1_data = data.clone();
        let w1 = std::thread::spawn(move || {
            for i in 0..100 {
                let mut s = OnboardingState::default();
                s.prefs.large_min_bytes = i;
                save(&w1_data, &s).unwrap();
            }
        });
        let w2_data = data.clone();
        let w2 = std::thread::spawn(move || {
            for i in 0..100 {
                let mut s = OnboardingState::default();
                s.prefs.large_min_days_idle = i;
                save(&w2_data, &s).unwrap();
            }
        });
        w1.join().unwrap();
        w2.join().unwrap();
        stop.store(true, Ordering::Relaxed);
        reader.join().unwrap();
        assert_eq!(
            parse_errors.load(Ordering::Relaxed),
            0,
            "reader saw a torn write",
        );
    }

    #[test]
    fn permissions_applicable_matches_current_platform_shape() {
        let p = Platform::current();
        let list = applicable_for(p);
        // always at least one item so the permissions step has
        // something to render
        assert!(!list.is_empty());
    }

    #[test]
    fn detect_status_is_safe_on_a_fake_home() {
        let tmp = tempfile::tempdir().unwrap();
        // every kind must return without panic when probes are missing
        for k in [
            PermissionKind::MacFullDiskAccess,
            PermissionKind::MacFilesAndFolders,
            PermissionKind::LinuxHomeAcknowledged,
            PermissionKind::WindowsHomeAcknowledged,
        ] {
            let _ = detect_status(k, tmp.path());
        }
    }

    #[test]
    fn save_commutes_for_independent_fields() {
        // saving prefs -> permissions -> telemetry (any order) must
        // converge to the same state as a single save with all three
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();

        let mut a = OnboardingState::default();
        a.prefs.auto_scan_on_launch = true;
        save(data, &a).unwrap();
        let mut a = load(data).unwrap();
        a.record_permission(
            PermissionKind::MacFullDiskAccess,
            PermissionStatus::Granted,
            5,
        );
        save(data, &a).unwrap();
        let mut a = load(data).unwrap();
        a.telemetry_opt_in = true;
        save(data, &a).unwrap();
        let a_final = load(data).unwrap();

        // same in one shot
        reset(data).unwrap();
        let mut b = OnboardingState::default();
        b.prefs.auto_scan_on_launch = true;
        b.record_permission(
            PermissionKind::MacFullDiskAccess,
            PermissionStatus::Granted,
            5,
        );
        b.telemetry_opt_in = true;
        save(data, &b).unwrap();
        let b_final = load(data).unwrap();

        assert_eq!(a_final, b_final);
    }

    #[test]
    fn load_or_default_never_panics_on_garbage_file() {
        let tmp = tempfile::tempdir().unwrap();
        // random bytes, definitely not JSON
        std::fs::write(
            state_path(tmp.path()),
            &[0u8, 1, 2, 3, 255, 128, 64, 0, b'{', b'x'],
        )
        .unwrap();
        let s = load_or_default(tmp.path());
        // defaults, cleanly
        assert_eq!(s, OnboardingState::default());
    }
}

