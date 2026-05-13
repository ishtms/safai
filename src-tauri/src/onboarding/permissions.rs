//! platform-aware permission prompts.
//!
//! onboarding surfaces any gates the host OS puts between safai and the
//! user's home. today the only hard gate is mac FDA, the rest are UX
//! "just so you know" screens.
//!
//! two jobs:
//!
//! 1. discovery: which permissions matter on the current OS, and
//!    best-effort whether they're granted. for mac FDA we probe a
//!    known-protected path. negatives mean "denied or genuinely absent",
//!    advisory not authoritative.
//! 2. deep-linking: url that `open` on mac, `start` on win, or xdg-open
//!    on linux lands the user in the right Settings pane. we build the
//!    url only, the command layer launches.

use std::path::Path;

use super::types::{PermissionKind, PermissionStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Mac/Windows only constructed on their own builds
pub enum Platform {
    Mac,
    Linux,
    Windows,
}

impl Platform {
    pub fn current() -> Self {
        #[cfg(target_os = "macos")]
        {
            Platform::Mac
        }
        #[cfg(target_os = "windows")]
        {
            Platform::Windows
        }
        #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
        {
            Platform::Linux
        }
    }
}

/// permissions the onboarding flow should surface. ordered, hard gate
/// (mac FDA) first, softer confirmations after.
pub fn applicable_for(platform: Platform) -> Vec<PermissionKind> {
    match platform {
        Platform::Mac => vec![
            PermissionKind::MacFullDiskAccess,
            PermissionKind::MacFilesAndFolders,
        ],
        Platform::Linux => vec![PermissionKind::LinuxHomeAcknowledged],
        Platform::Windows => vec![PermissionKind::WindowsHomeAcknowledged],
    }
}

/// deep-link url for a permission, or None if no settings target.
/// linux/windows home-acknowledged are purely informational.
/// urls are hardcoded, Apple-documented, stable across macOS versions.
/// building them programmatically isn't worth the typo risk.
pub fn settings_url(kind: PermissionKind) -> Option<&'static str> {
    match kind {
        PermissionKind::MacFullDiskAccess => Some(
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles",
        ),
        PermissionKind::MacFilesAndFolders => Some(
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_FilesAndFolders",
        ),
        PermissionKind::LinuxHomeAcknowledged | PermissionKind::WindowsHomeAcknowledged => None,
    }
}

/// best-effort detection. for mac FDA we probe ~/Library/Mail, non-ok
/// becomes Unknown. can't tell "denied" from "app not installed" on
/// negatives. caller blends with persisted user choice.
pub fn detect_status(kind: PermissionKind, home: &Path) -> PermissionStatus {
    match kind {
        PermissionKind::MacFullDiskAccess => detect_mac_fda(home),
        PermissionKind::MacFilesAndFolders => detect_mac_files(home),
        PermissionKind::LinuxHomeAcknowledged | PermissionKind::WindowsHomeAcknowledged => {
            // nothing to probe
            PermissionStatus::Unknown
        }
    }
}

/// read_dir on an FDA-protected path. OK=granted, EPERM/EACCES=denied.
/// non-mac always Unknown.
fn detect_mac_fda(home: &Path) -> PermissionStatus {
    #[cfg(target_os = "macos")]
    {
        // ~/Library/Mail is Apple's canonical FDA probe. Safari too,
        // installed everywhere. even on a mail-less machine Mail reads
        // EACCES not ENOENT when FDA denied.
        let probes = [home.join("Library/Mail"), home.join("Library/Safari")];
        for p in probes {
            match std::fs::read_dir(&p) {
                Ok(_) => return PermissionStatus::Granted,
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    return PermissionStatus::Denied;
                }
                Err(_) => {}
            }
        }
        PermissionStatus::Unknown
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = home;
        PermissionStatus::Unknown
    }
}

/// like detect_mac_fda but for Files & Folders. probes ~/Desktop,
/// accessible by default, so PermissionDenied = user explicitly locked
/// us out.
fn detect_mac_files(home: &Path) -> PermissionStatus {
    #[cfg(target_os = "macos")]
    {
        match std::fs::read_dir(home.join("Desktop")) {
            Ok(_) => PermissionStatus::Granted,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => PermissionStatus::Denied,
            Err(_) => PermissionStatus::Unknown,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = home;
        PermissionStatus::Unknown
    }
}

/// spawn Settings at the URL for kind. errors if no deep-link or spawn
/// fails. fire-and-forget, user goes to settings and comes back.
pub fn open_settings(kind: PermissionKind) -> Result<(), String> {
    let Some(url) = settings_url(kind) else {
        return Err(format!(
            "no settings deep-link for {:?} on this platform",
            kind,
        ));
    };
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("open failed: {e}"))
    }
    #[cfg(target_os = "windows")]
    {
        // `start <url>` runs through cmd.exe. empty quoted title arg
        // stops `start` from parsing the URL as a window title
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("start failed: {e}"))
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let _ = url;
        // no safai-relevant deep-link on linux. surface an error so
        // future UI can say "nothing to do"
        Err(format!("deep-linking {:?} is not supported on Linux", kind,))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applicable_for_mac_lists_both_privacy_gates() {
        let v = applicable_for(Platform::Mac);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], PermissionKind::MacFullDiskAccess);
        assert_eq!(v[1], PermissionKind::MacFilesAndFolders);
    }

    #[test]
    fn applicable_for_linux_is_home_acknowledged_only() {
        let v = applicable_for(Platform::Linux);
        assert_eq!(v, vec![PermissionKind::LinuxHomeAcknowledged]);
    }

    #[test]
    fn applicable_for_windows_is_home_acknowledged_only() {
        let v = applicable_for(Platform::Windows);
        assert_eq!(v, vec![PermissionKind::WindowsHomeAcknowledged]);
    }

    #[test]
    fn settings_url_for_fda_is_the_apple_documented_scheme() {
        let url = settings_url(PermissionKind::MacFullDiskAccess).unwrap();
        assert!(url.starts_with("x-apple.systempreferences:"), "{url}");
        assert!(url.contains("Privacy_AllFiles"));
    }

    #[test]
    fn settings_url_for_files_and_folders_is_distinct_from_fda() {
        let fda = settings_url(PermissionKind::MacFullDiskAccess).unwrap();
        let ff = settings_url(PermissionKind::MacFilesAndFolders).unwrap();
        assert_ne!(fda, ff);
        assert!(ff.contains("Privacy_FilesAndFolders"));
    }

    #[test]
    fn settings_url_is_none_for_linux_windows_acknowledged() {
        assert!(settings_url(PermissionKind::LinuxHomeAcknowledged).is_none());
        assert!(settings_url(PermissionKind::WindowsHomeAcknowledged).is_none());
    }

    #[test]
    fn detect_status_returns_unknown_for_pure_ack_kinds() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            detect_status(PermissionKind::LinuxHomeAcknowledged, tmp.path()),
            PermissionStatus::Unknown,
        );
        assert_eq!(
            detect_status(PermissionKind::WindowsHomeAcknowledged, tmp.path()),
            PermissionStatus::Unknown,
        );
    }

    #[test]
    fn detect_status_does_not_crash_on_missing_home() {
        // empty tempdir as fake $HOME. probes should fail-open to
        // Unknown (non-mac) or Denied/Unknown (mac), never panic
        let tmp = tempfile::tempdir().unwrap();
        for k in [
            PermissionKind::MacFullDiskAccess,
            PermissionKind::MacFilesAndFolders,
        ] {
            let _ = detect_status(k, tmp.path());
        }
    }

    #[test]
    fn open_settings_rejects_kinds_without_a_url() {
        let err = open_settings(PermissionKind::LinuxHomeAcknowledged).unwrap_err();
        assert!(err.contains("no settings deep-link"));
    }

    #[test]
    fn current_platform_matches_build_target() {
        let p = Platform::current();
        #[cfg(target_os = "macos")]
        assert_eq!(p, Platform::Mac);
        #[cfg(target_os = "windows")]
        assert_eq!(p, Platform::Windows);
        #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
        assert_eq!(p, Platform::Linux);
    }
}
