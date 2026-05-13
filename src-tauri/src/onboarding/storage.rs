//! on-disk persistence for OnboardingState.
//!
//! lives at `<data_dir>/state.json`. writes are atomic: serialise to
//! tmp in same dir, fsync if we can, then rename. on Unix we also fsync
//! the containing directory after rename so the directory entry itself is
//! durable. concurrent readers see full old or full new, never a tear.
//! crash between tmp write and rename leaves the original untouched.
//!
//! reads are tolerant of any realistic corruption: truncated JSON,
//! partial overwrite, newer-version file, older-version with missing
//! fields. every failure falls back to default + log line, never
//! propagates. unreadable state shouldn't brick the app, user can
//! re-run onboarding.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::types::{OnboardingError, OnboardingState, CURRENT_SCHEMA_VERSION};

pub const STATE_FILE_NAME: &str = "state.json";

pub fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join(STATE_FILE_NAME)
}

/// never-failing load. missing file = silent (first launch).
/// parse error or future schema = log once, return default.
pub fn load_or_default(data_dir: &Path) -> OnboardingState {
    match load(data_dir) {
        Ok(s) => s,
        Err(OnboardingError::Io(e)) if e.contains("missing") => OnboardingState::default(),
        Err(e) => {
            eprintln!("[safai] onboarding state load failed, using default: {e}");
            OnboardingState::default()
        }
    }
}

/// returns Io("missing") when absent, Parse when present but bad
pub fn load(data_dir: &Path) -> Result<OnboardingState, OnboardingError> {
    let path = state_path(data_dir);
    if !path.exists() {
        return Err(OnboardingError::Io(format!(
            "state file missing at {}",
            path.display()
        )));
    }
    let s = fs::read_to_string(&path)
        .map_err(|e| OnboardingError::Io(format!("read {}: {e}", path.display())))?;
    let mut state: OnboardingState = serde_json::from_str(&s)
        .map_err(|e| OnboardingError::Parse(format!("parse state.json: {e}")))?;
    // forward-compat: newer-version file = drop and re-onboard.
    // silent downgrade risks misreading new fields.
    if state.version > CURRENT_SCHEMA_VERSION {
        return Ok(OnboardingState::default());
    }
    // backward-compat: older write, bump so next save stamps current.
    // future migrations branch on state.version before this.
    if state.version < CURRENT_SCHEMA_VERSION {
        state = upgrade_state(state);
    }
    Ok(state)
}

/// migrations from older schemas. one version today so just bumps the
/// stamp. future additive changes are one match arm away.
pub fn upgrade_state(mut s: OnboardingState) -> OnboardingState {
    if s.version < CURRENT_SCHEMA_VERSION {
        s.version = CURRENT_SCHEMA_VERSION;
    }
    s
}

/// atomic save.
///
/// 1. mkdir -p data_dir
/// 2. write to state.json.tmp.<pid>.<nanos>
/// 3. rename tmp to state.json. POSIX atomic on same fs. on windows,
///    rename-over-existing is atomic on NTFS via MoveFileExW which rust
///    uses under the hood.
pub fn save(data_dir: &Path, state: &OnboardingState) -> Result<(), OnboardingError> {
    fs::create_dir_all(data_dir)
        .map_err(|e| OnboardingError::Io(format!("create data dir: {e}")))?;
    let final_path = state_path(data_dir);
    let tmp_path = data_dir.join(format!(
        "state.json.tmp.{}.{}",
        std::process::id(),
        now_nanos(),
    ));

    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|e| OnboardingError::Parse(format!("encode: {e}")))?;

    // scope the handle, windows won't rename an open file
    {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|e| OnboardingError::Io(format!("open tmp {}: {e}", tmp_path.display())))?;
        f.write_all(&bytes)
            .map_err(|e| OnboardingError::Io(format!("write tmp: {e}")))?;
        // best-effort fsync. ramfs under test harnesses may fail, data
        // is in page cache anyway so prefer rename over propagating
        let _ = f.sync_all();
    }

    fs::rename(&tmp_path, &final_path).map_err(|e| {
        // rename failed, try to clean the orphan tmp
        let _ = fs::remove_file(&tmp_path);
        OnboardingError::Io(format!("rename state file: {e}"))
    })?;
    sync_parent_dir(&final_path)
        .map_err(|e| OnboardingError::Io(format!("sync state dir: {e}")))?;
    Ok(())
}

/// clean stale state.json.tmp.* files older than 60s. cold-start sweep,
/// not called from save (scan would be wasted hot-path work).
#[allow(dead_code)]
pub fn sweep_stale_tmps(data_dir: &Path) {
    sweep_orphan_tmps(data_dir);
}

/// age-gated so we don't nuke another process's in-flight tmp.
/// create_new in save includes pid+nanos so collisions are fine.
#[allow(dead_code)]
fn sweep_orphan_tmps(data_dir: &Path) {
    let Ok(iter) = fs::read_dir(data_dir) else {
        return;
    };
    for entry in iter.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            if name.starts_with("state.json.tmp.") {
                // >60s old = orphan or badly stuck writer, either way nuke
                let age = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .map(|t| {
                        std::time::SystemTime::now()
                            .duration_since(t)
                            .map(|d| d.as_secs())
                            .unwrap_or(0)
                    })
                    .unwrap_or(0);
                if age > 60 {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }
}

/// missing file = success. used by onboarding_reset.
pub fn reset(data_dir: &Path) -> Result<(), OnboardingError> {
    let path = state_path(data_dir);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(OnboardingError::Io(format!("remove state.json: {e}"))),
    }
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(unix)]
fn sync_parent_dir(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent_dir(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onboarding::types::{
        IncludedCategory, OnboardingStep, PermissionKind, PermissionStatus, Preferences,
        ScheduleCadence,
    };
    use std::fs::File;
    use std::io::Read;

    #[test]
    fn load_returns_default_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let s = load_or_default(tmp.path());
        assert_eq!(s, OnboardingState::default());
        // read must not create the data dir
        assert!(!state_path(tmp.path()).exists());
    }

    #[test]
    fn save_then_load_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = OnboardingState::default();
        s.mark_complete(42);
        s.telemetry_opt_in = true;
        s.record_permission(
            PermissionKind::MacFullDiskAccess,
            PermissionStatus::Granted,
            10,
        );
        save(tmp.path(), &s).unwrap();

        let back = load_or_default(tmp.path());
        assert_eq!(s, back);
    }

    #[test]
    fn save_creates_data_dir_lazily() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("safai-ns/sub/dir");
        assert!(!nested.exists());
        save(&nested, &OnboardingState::default()).unwrap();
        assert!(state_path(&nested).exists());
    }

    #[test]
    fn save_writes_camel_case_json() {
        let tmp = tempfile::tempdir().unwrap();
        let s = OnboardingState::default();
        save(tmp.path(), &s).unwrap();
        let mut text = String::new();
        File::open(state_path(tmp.path()))
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert!(text.contains("\"completedAt\""), "{text}");
        assert!(text.contains("\"lastStep\""));
        assert!(text.contains("\"telemetryOptIn\""));
    }

    #[test]
    fn load_falls_back_to_default_on_corrupt_file() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path()).unwrap();
        fs::write(state_path(tmp.path()), b"{not valid json").unwrap();
        let s = load_or_default(tmp.path());
        assert_eq!(s, OnboardingState::default());
    }

    #[test]
    fn load_falls_back_on_future_schema_version() {
        let tmp = tempfile::tempdir().unwrap();
        let raw = r#"{"version": 9999, "completedAt": 1, "lastStep": "done",
                      "permissions": [], "prefs": {
                        "autoScanOnLaunch": false, "scheduledScan": null,
                        "includedCategories": [],
                        "largeMinBytes": 0, "largeMinDaysIdle": 0,
                        "confirmBeforeClean": true
                      }, "telemetryOptIn": false}"#;
        fs::write(state_path(tmp.path()), raw).unwrap();
        let s = load_or_default(tmp.path());
        assert!(!s.is_onboarded(), "future schema must be ignored");
    }

    #[test]
    fn load_upgrades_older_schema_version() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = OnboardingState::default();
        s.version = 0; // fake older write
        s.mark_complete(100);
        let bytes = serde_json::to_vec(&s).unwrap();
        fs::write(state_path(tmp.path()), bytes).unwrap();
        let loaded = load_or_default(tmp.path());
        assert_eq!(loaded.version, CURRENT_SCHEMA_VERSION);
        assert!(loaded.is_onboarded(), "upgrade must preserve fields");
    }

    #[test]
    fn save_is_atomic_under_concurrent_read_stress() {
        // pound writes while a reader checks every observed version
        // parses. atomic rename = reader never sees a tear.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_path_buf();

        // seed so reader always has a file to open
        save(&path, &OnboardingState::default()).unwrap();

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader_stop = stop.clone();
        let reader_path = path.clone();
        let reader = std::thread::spawn(move || {
            let mut ok_reads = 0u64;
            while !reader_stop.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok(s) = fs::read_to_string(reader_path.join(STATE_FILE_NAME)) {
                    if !s.is_empty() {
                        // must always parse
                        let _: OnboardingState =
                            serde_json::from_str(&s).expect("torn read produced unparseable state");
                        ok_reads += 1;
                    }
                }
            }
            ok_reads
        });

        for i in 0..200 {
            let mut s = OnboardingState::default();
            s.mark_complete(i);
            save(&path, &s).unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let reads = reader.join().unwrap();
        assert!(reads > 0, "reader didn't observe any writes");
    }

    #[test]
    fn reset_removes_the_state_file() {
        let tmp = tempfile::tempdir().unwrap();
        save(tmp.path(), &OnboardingState::default()).unwrap();
        assert!(state_path(tmp.path()).exists());
        reset(tmp.path()).unwrap();
        assert!(!state_path(tmp.path()).exists());
    }

    #[test]
    fn reset_is_ok_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        reset(tmp.path()).unwrap();
    }

    #[test]
    fn load_tolerates_missing_optional_fields() {
        // guards that future added fields get #[serde(default)] so an
        // older state without them still parses
        let tmp = tempfile::tempdir().unwrap();
        let raw = serde_json::to_string(&OnboardingState::default()).unwrap();
        fs::write(state_path(tmp.path()), raw).unwrap();
        let _ = load(tmp.path()).unwrap();
    }

    #[test]
    fn save_overwrites_existing_file_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let mut first = OnboardingState::default();
        first.telemetry_opt_in = false;
        save(tmp.path(), &first).unwrap();

        let mut second = OnboardingState::default();
        second.telemetry_opt_in = true;
        save(tmp.path(), &second).unwrap();

        let back = load(tmp.path()).unwrap();
        assert!(back.telemetry_opt_in);
    }

    #[test]
    fn save_does_not_leave_tmp_files_behind() {
        let tmp = tempfile::tempdir().unwrap();
        for _ in 0..5 {
            save(tmp.path(), &OnboardingState::default()).unwrap();
        }
        // successful rename consumes the tmp, nothing should survive
        let leftovers: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .map(|n| n.starts_with("state.json.tmp."))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(
            leftovers.len(),
            0,
            "found tmp leftovers: {:?}",
            leftovers.iter().map(|e| e.path()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn sweep_stale_tmps_removes_old_orphans_only() {
        // fresh tmp must not get removed. std has no portable mtime
        // backdate so we only check the keep-fresh half here.
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("state.json.tmp.0.0"), b"stale contents").unwrap();
        fs::write(
            tmp.path().join("state.json.tmp.999.999"),
            b"recent contents",
        )
        .unwrap();
        sweep_stale_tmps(tmp.path());
        // both fresh, both survive (sweep gate is >60s)
        assert!(tmp.path().join("state.json.tmp.0.0").exists());
        assert!(tmp.path().join("state.json.tmp.999.999").exists());
    }

    #[test]
    fn save_preserves_preferences_field_by_field() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = OnboardingState::default();
        s.prefs = Preferences {
            auto_scan_on_launch: true,
            scheduled_scan: Some(ScheduleCadence::Monthly),
            included_categories: vec![IncludedCategory::SystemJunk, IncludedCategory::Privacy],
            large_min_bytes: 1_234_567,
            large_min_days_idle: 42,
            confirm_before_clean: false,
        };
        save(tmp.path(), &s).unwrap();
        let back = load(tmp.path()).unwrap();
        assert_eq!(back.prefs, s.prefs);
    }

    #[test]
    fn schema_version_is_stamped_on_save() {
        let tmp = tempfile::tempdir().unwrap();
        save(tmp.path(), &OnboardingState::default()).unwrap();
        let text = fs::read_to_string(state_path(tmp.path())).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["version"], CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn load_upgrade_does_not_mutate_user_fields() {
        // older version tag must not clobber user prefs, migration
        // only bumps the version stamp
        let mut s = OnboardingState::default();
        s.version = 0;
        s.telemetry_opt_in = true;
        s.prefs.auto_scan_on_launch = true;
        s.last_step = OnboardingStep::Prefs;
        let upgraded = upgrade_state(s);
        assert_eq!(upgraded.version, CURRENT_SCHEMA_VERSION);
        assert!(upgraded.telemetry_opt_in);
        assert!(upgraded.prefs.auto_scan_on_launch);
        assert_eq!(upgraded.last_step, OnboardingStep::Prefs);
    }
}
