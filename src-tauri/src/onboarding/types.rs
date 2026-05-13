//! onboarding state types.
//!
//! OnboardingState is the source of truth for whether onboarding is
//! done, scan prefs, and schema version. every field is explicit. no
//! "null means default" because a forgotten migration would silently
//! downgrade user settings.

use serde::{Deserialize, Serialize};

/// bump whenever shape changes in a way that needs migration.
/// upgrade_state reads this and rewrites the file.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// root onboarding + prefs doc. lives at `<data_dir>/state.json`.
/// flat on purpose, deep nesting makes migrations harder for nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingState {
    /// forwards-compat: version > current = newer build wrote this,
    /// loader falls back to defaults rather than silently dropping
    /// unknown fields.
    pub version: u32,
    /// unix secs when onboarding completed, None = still in-flow
    pub completed_at: Option<u64>,
    /// last step reached. lets us resume mid-flow.
    pub last_step: OnboardingStep,
    /// per-permission verdicts captured during the permissions step.
    /// ready screen uses these to surface gaps ("FDA still not granted").
    pub permissions: Vec<PermissionRecord>,
    pub prefs: Preferences,
    /// opt-in default off, never upload without explicit opt-in
    pub telemetry_opt_in: bool,
    /// most recent scheduled scan fire / cadence anchor. used by the
    /// scheduler. serde default so previous state.json
    /// parses without a version bump.
    #[serde(default)]
    pub last_scheduled_at: Option<u64>,
}

impl Default for OnboardingState {
    fn default() -> Self {
        Self {
            version: CURRENT_SCHEMA_VERSION,
            completed_at: None,
            last_step: OnboardingStep::Welcome,
            permissions: Vec::new(),
            prefs: Preferences::default(),
            telemetry_opt_in: false,
            last_scheduled_at: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OnboardingStep {
    Welcome,
    Permissions,
    Prefs,
    Ready,
    /// distinct from Ready so the gate can tell "sitting on ready"
    /// from "finished, routed into the app"
    Done,
}

impl OnboardingStep {
    /// route fragment the UI navigates to
    #[allow(dead_code)] // tests + UI bindings
    pub fn slug(self) -> &'static str {
        match self {
            OnboardingStep::Welcome => "welcome",
            OnboardingStep::Permissions => "permissions",
            OnboardingStep::Prefs => "prefs",
            OnboardingStep::Ready => "ready",
            OnboardingStep::Done => "done",
        }
    }
}

/// user-facing scan + cleanup prefs. every field defaults conservative,
/// nothing deletes automatically and nothing phones home.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    /// run a scan on every launch. default off.
    pub auto_scan_on_launch: bool,
    /// background cadence, None = disabled. consumes this,
    /// declared early so the toggle doesn't need a schema bump.
    pub scheduled_scan: Option<ScheduleCadence>,
    /// smart scan categories, mirrors the dashboard grid
    pub included_categories: Vec<IncludedCategory>,
    /// large & old min bytes threshold
    pub large_min_bytes: u64,
    /// large & old days-idle threshold
    pub large_min_days_idle: u64,
    /// default on, power users can opt out
    pub confirm_before_clean: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            auto_scan_on_launch: false,
            scheduled_scan: None,
            included_categories: IncludedCategory::all_defaults(),
            large_min_bytes: 50 * 1024 * 1024, // 50 MB
            large_min_days_idle: 180,
            confirm_before_clean: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScheduleCadence {
    Daily,
    Weekly,
    Monthly,
}

/// string enum so new variants can land without a schema bump
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IncludedCategory {
    SystemJunk,
    Duplicates,
    LargeOld,
    Privacy,
    AppLeftovers,
    Trash,
}

impl IncludedCategory {
    pub fn all_defaults() -> Vec<Self> {
        vec![
            IncludedCategory::SystemJunk,
            IncludedCategory::Duplicates,
            IncludedCategory::LargeOld,
            IncludedCategory::Privacy,
            IncludedCategory::AppLeftovers,
            IncludedCategory::Trash,
        ]
    }
}

/// which OS permission a prompt targets
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionKind {
    /// mac. System Settings > Privacy & Security > Full Disk Access.
    /// without it we can't enumerate ~/Library/Mail, ~/Library/Safari,
    /// parts of ~/Library/Containers. privacy cleaner takes the hit.
    MacFullDiskAccess,
    /// mac. Files & Folders (Desktop/Documents/Downloads). softer gate
    /// but still trips the scanner on fresh installs.
    MacFilesAndFolders,
    /// linux. no hard block. records whether user acknowledged the
    /// "we scan your home" message so we don't re-nag.
    LinuxHomeAcknowledged,
    /// windows. UAC is per-op and we don't request elevation.
    WindowsHomeAcknowledged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionStatus {
    Granted,
    /// explicitly denied or skipped. remembered so we don't re-prompt
    /// every launch.
    Denied,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRecord {
    pub kind: PermissionKind,
    pub status: PermissionStatus,
    /// unix secs when answered. lets UI say "you answered 3 weeks ago,
    /// want to recheck?"
    pub answered_at: Option<u64>,
}

#[derive(Debug)]
pub enum OnboardingError {
    Io(String),
    Parse(String),
    /// e.g. asking for MacFullDiskAccess on linux
    UnsupportedPermission(PermissionKind),
}

impl std::fmt::Display for OnboardingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OnboardingError::Io(s) => write!(f, "io: {s}"),
            OnboardingError::Parse(s) => write!(f, "parse: {s}"),
            OnboardingError::UnsupportedPermission(k) => {
                write!(f, "permission {:?} is not supported on this platform", k)
            }
        }
    }
}

impl std::error::Error for OnboardingError {}

impl From<OnboardingError> for String {
    fn from(e: OnboardingError) -> Self {
        e.to_string()
    }
}

impl OnboardingState {
    pub fn apply_prefs(&mut self, prefs: Preferences) {
        self.prefs = prefs;
    }

    /// overwrites any existing record for the same kind
    pub fn record_permission(&mut self, kind: PermissionKind, status: PermissionStatus, now: u64) {
        if let Some(existing) = self.permissions.iter_mut().find(|r| r.kind == kind) {
            existing.status = status;
            existing.answered_at = Some(now);
            return;
        }
        self.permissions.push(PermissionRecord {
            kind,
            status,
            answered_at: Some(now),
        });
    }

    /// idempotent, second call keeps original timestamp
    pub fn mark_complete(&mut self, now: u64) {
        if self.completed_at.is_none() {
            self.completed_at = Some(now);
        }
        self.last_step = OnboardingStep::Done;
    }

    #[allow(dead_code)] // tests + tauri command surface
    pub fn is_onboarded(&self) -> bool {
        self.completed_at.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_is_not_onboarded() {
        let s = OnboardingState::default();
        assert!(!s.is_onboarded());
        assert_eq!(s.version, CURRENT_SCHEMA_VERSION);
        assert_eq!(s.last_step, OnboardingStep::Welcome);
        assert!(s.permissions.is_empty());
        assert!(!s.telemetry_opt_in);
    }

    #[test]
    fn default_preferences_are_conservative() {
        let p = Preferences::default();
        assert!(!p.auto_scan_on_launch, "auto-scan must default OFF");
        assert!(p.confirm_before_clean, "confirm dialog must default ON");
        assert!(p.scheduled_scan.is_none(), "no schedule by default");
        assert!(!p.included_categories.is_empty());
        // match backend's defaults so Large & Old stays consistent
        assert_eq!(p.large_min_bytes, 50 * 1024 * 1024);
        assert_eq!(p.large_min_days_idle, 180);
    }

    #[test]
    fn state_serialises_camel_case() {
        let s = OnboardingState::default();
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"completedAt\""), "{json}");
        assert!(json.contains("\"lastStep\""));
        assert!(json.contains("\"telemetryOptIn\""));
        assert!(json.contains("\"includedCategories\""));
        assert!(json.contains("\"largeMinBytes\""));
    }

    #[test]
    fn state_round_trips_through_serde() {
        let mut s = OnboardingState::default();
        s.record_permission(
            PermissionKind::MacFullDiskAccess,
            PermissionStatus::Granted,
            42,
        );
        s.telemetry_opt_in = true;
        s.mark_complete(100);
        let json = serde_json::to_string(&s).unwrap();
        let back: OnboardingState = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn steps_serialise_kebab_case() {
        assert_eq!(
            serde_json::to_string(&OnboardingStep::Welcome).unwrap(),
            "\"welcome\"",
        );
        assert_eq!(
            serde_json::to_string(&OnboardingStep::Permissions).unwrap(),
            "\"permissions\"",
        );
        assert_eq!(
            serde_json::to_string(&OnboardingStep::Done).unwrap(),
            "\"done\"",
        );
    }

    #[test]
    fn record_permission_overwrites_existing() {
        let mut s = OnboardingState::default();
        s.record_permission(
            PermissionKind::MacFullDiskAccess,
            PermissionStatus::Denied,
            1,
        );
        s.record_permission(
            PermissionKind::MacFullDiskAccess,
            PermissionStatus::Granted,
            2,
        );
        assert_eq!(s.permissions.len(), 1);
        assert_eq!(s.permissions[0].status, PermissionStatus::Granted);
        assert_eq!(s.permissions[0].answered_at, Some(2));
    }

    #[test]
    fn record_permission_keeps_distinct_kinds_separate() {
        let mut s = OnboardingState::default();
        s.record_permission(
            PermissionKind::MacFullDiskAccess,
            PermissionStatus::Granted,
            1,
        );
        s.record_permission(
            PermissionKind::MacFilesAndFolders,
            PermissionStatus::Denied,
            2,
        );
        assert_eq!(s.permissions.len(), 2);
    }

    #[test]
    fn mark_complete_is_idempotent() {
        let mut s = OnboardingState::default();
        s.mark_complete(100);
        s.mark_complete(200); // must not overwrite
        assert_eq!(s.completed_at, Some(100));
        assert_eq!(s.last_step, OnboardingStep::Done);
        assert!(s.is_onboarded());
    }

    #[test]
    fn apply_prefs_replaces_field() {
        let mut s = OnboardingState::default();
        let mut p = Preferences::default();
        p.auto_scan_on_launch = true;
        p.scheduled_scan = Some(ScheduleCadence::Weekly);
        s.apply_prefs(p);
        assert!(s.prefs.auto_scan_on_launch);
        assert_eq!(s.prefs.scheduled_scan, Some(ScheduleCadence::Weekly));
    }

    #[test]
    fn onboarding_step_slug_is_stable() {
        assert_eq!(OnboardingStep::Welcome.slug(), "welcome");
        assert_eq!(OnboardingStep::Permissions.slug(), "permissions");
        assert_eq!(OnboardingStep::Prefs.slug(), "prefs");
        assert_eq!(OnboardingStep::Ready.slug(), "ready");
        assert_eq!(OnboardingStep::Done.slug(), "done");
    }

    #[test]
    fn included_categories_defaults_cover_six_buckets() {
        let all = IncludedCategory::all_defaults();
        assert_eq!(all.len(), 6);
        assert!(all.contains(&IncludedCategory::SystemJunk));
        assert!(all.contains(&IncludedCategory::Duplicates));
        assert!(all.contains(&IncludedCategory::Trash));
    }

    #[test]
    fn permission_kind_round_trips_all_variants() {
        let kinds = [
            PermissionKind::MacFullDiskAccess,
            PermissionKind::MacFilesAndFolders,
            PermissionKind::LinuxHomeAcknowledged,
            PermissionKind::WindowsHomeAcknowledged,
        ];
        for k in kinds {
            let j = serde_json::to_string(&k).unwrap();
            let back: PermissionKind = serde_json::from_str(&j).unwrap();
            assert_eq!(k, back);
        }
    }
}
