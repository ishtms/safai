//! shared types for the startup items manager.
//!
//! a startup item = anything the OS auto-launches on login/boot. sources:
//!
//! - linux: ~/.config/autostart/*.desktop (XDG) + user systemd units (enabled
//!   via symlink under <unit-dir>/default.target.wants/)
//! - mac: ~/Library/LaunchAgents/*.plist (rw) + /Library/LaunchAgents/*.plist
//!   (system-wide user agents, ro) + /Library/LaunchDaemons/*.plist (daemons, ro).
//!   system paths surface as "requires admin" in UI.
//! - windows: %APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\ (file-based)
//!   + HKCU\...\Run / HKLM\...\Run (registry, windows-only build)
//!
//! every item carries enough for UI toggle to round-trip without re-scanning
//! and enough for cleaner safety policy to classify `path`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// enum not string so wire format is stable + dispatcher can match. additive only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StartupSource {
    /// XDG autostart, *.desktop under ~/.config/autostart
    LinuxAutostart,
    /// user systemd, *.service under ~/.config/systemd/user/. enabled = symlink
    /// in default.target.wants/ (same as `systemctl --user enable`).
    LinuxSystemdUser,
    /// ~/Library/LaunchAgents/*.plist
    MacLaunchAgentUser,
    /// /Library/LaunchAgents/*.plist. read-only, toggle returns PermissionDenied
    /// so UI can show "requires admin".
    MacLaunchAgentSystem,
    /// /Library/LaunchDaemons/*.plist. read-only from non-root app.
    MacLaunchDaemon,
    /// file-based, fully testable cross-platform
    WindowsStartupFolder,
    /// HKCU\Software\Microsoft\Windows\CurrentVersion\Run. only populated on
    /// windows builds, others return empty.
    WindowsRunUser,
    /// HKLM\Software\Microsoft\Windows\CurrentVersion\Run
    WindowsRunMachine,
}

impl StartupSource {
    /// matches serde value
    pub fn slug(self) -> &'static str {
        match self {
            Self::LinuxAutostart => "linux-autostart",
            Self::LinuxSystemdUser => "linux-systemd-user",
            Self::MacLaunchAgentUser => "mac-launch-agent-user",
            Self::MacLaunchAgentSystem => "mac-launch-agent-system",
            Self::MacLaunchDaemon => "mac-launch-daemon",
            Self::WindowsStartupFolder => "windows-startup-folder",
            Self::WindowsRunUser => "windows-run-user",
            Self::WindowsRunMachine => "windows-run-machine",
        }
    }

    /// rw sources. /Library/... + HKLM are ro for a user-mode app.
    #[allow(dead_code)]
    pub fn is_toggleable(self) -> bool {
        matches!(
            self,
            Self::LinuxAutostart
                | Self::LinuxSystemdUser
                | Self::MacLaunchAgentUser
                | Self::WindowsStartupFolder
                | Self::WindowsRunUser
        )
    }

    /// home-rooted sources. UI "user vs system" badge reads off this.
    #[allow(dead_code)]
    pub fn is_user_scope(self) -> bool {
        matches!(
            self,
            Self::LinuxAutostart
                | Self::LinuxSystemdUser
                | Self::MacLaunchAgentUser
                | Self::WindowsStartupFolder
                | Self::WindowsRunUser
        )
    }
}

/// id is stable across scans (<source-slug>::<name>) so UI selection survives
/// rescans. path is the backing artefact (real fs path for current sources, or
/// a registry location formatted as string for UI "Reveal"). command is the
/// launch invocation verbatim so users see what runs before disabling.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StartupItem {
    pub id: String,
    pub name: String,
    pub description: String,
    pub command: String,
    pub source: StartupSource,
    pub path: String,
    pub enabled: bool,
    pub is_user: bool,
    /// must match a variant of the TS IconName union
    pub icon: String,
    /// rough tier. heavy patterns (Docker/Electron/sync clients) = high,
    /// everything else = low. drives colour badge + boot-time estimate.
    pub impact: StartupImpact,
}

/// coarse on purpose. UI renders three tiers w/ colour tokens, boot-time
/// estimate = counts * per-tier weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StartupImpact {
    Low,
    Medium,
    High,
}

impl StartupImpact {
    /// seconds per enabled item at this tier. calibrated from Apple's
    /// startup-item benchmarks (low=launchctl daemon, medium=small bg service,
    /// high=Electron/Chromium/sync client).
    #[allow(dead_code)]
    pub fn boot_seconds(self) -> f32 {
        match self {
            Self::Low => 0.3,
            Self::Medium => 0.9,
            Self::High => 2.4,
        }
    }
}

/// top-level response from `startup_scan`
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupReport {
    pub items: Vec<StartupItem>,
    /// "no items" boot time, added to every estimate so "before" is never zero.
    /// conservative across OS families: login window + shell init.
    pub baseline_seconds: f32,
    pub duration_ms: u64,
    pub scanned_at: u64,
    /// "mac" | "linux" | "windows"
    pub platform: String,
}

/// returned so UI reflects the new state without re-scan. rescan still fires
/// to catch cascading changes.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToggleResult {
    pub id: String,
    pub enabled: bool,
}

/// pure, cheap to test. HIGH_MARKERS catches top offenders (Electron/Chromium
/// apps, sync clients, Docker, VS Code) via ASCII substring match. else Low.
/// Medium for well-known bg services (cloud storage, IDE helpers) where
/// footprint is real but smaller than a full Electron shell.
pub fn impact_for_command(command: &str) -> StartupImpact {
    const HIGH_MARKERS: &[&str] = &[
        "Electron",
        "electron-",
        "Docker",
        "docker-desktop",
        "Slack",
        "Discord",
        "Spotify",
        "Zoom",
        "Teams",
        "Code Helper",
        "Visual Studio Code",
        "Chrome",
        "Chromium",
        "Firefox",
        "Edge",
        "Brave",
        "Obsidian",
        "Notion",
        "Figma",
    ];
    const MEDIUM_MARKERS: &[&str] = &[
        "Dropbox",
        "OneDrive",
        "iCloud",
        "GoogleDrive",
        "Backblaze",
        "syncthing",
        "cloudd",
        "bird",
        "com.1password",
        "Bitwarden",
        "tailscaled",
    ];
    let haystack = command;
    for m in HIGH_MARKERS {
        if haystack.contains(m) {
            return StartupImpact::High;
        }
    }
    for m in MEDIUM_MARKERS {
        if haystack.contains(m) {
            return StartupImpact::Medium;
        }
    }
    StartupImpact::Low
}

/// stable item id from (source, name). name gets path-sep stripped so colons
/// (rare but possible on mac LaunchAgents) can't forge a different id.
pub fn make_item_id(source: StartupSource, name: &str) -> String {
    let safe = name.replace(['/', '\\', ':'], "-");
    format!("{}::{}", source.slug(), safe)
}

/// kept in one place so every source surfaces the same shape (lossy UTF-8
/// string, no trailing slash normalization).
pub fn path_for_wire(p: &PathBuf) -> String {
    p.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_slugs_are_unique_and_stable() {
        use std::collections::HashSet;
        let all = [
            StartupSource::LinuxAutostart,
            StartupSource::LinuxSystemdUser,
            StartupSource::MacLaunchAgentUser,
            StartupSource::MacLaunchAgentSystem,
            StartupSource::MacLaunchDaemon,
            StartupSource::WindowsStartupFolder,
            StartupSource::WindowsRunUser,
            StartupSource::WindowsRunMachine,
        ];
        let slugs: HashSet<&str> = all.iter().map(|s| s.slug()).collect();
        assert_eq!(slugs.len(), all.len());
    }

    #[test]
    fn source_slug_matches_serde() {
        for (src, want) in [
            (StartupSource::LinuxAutostart, "linux-autostart"),
            (StartupSource::MacLaunchAgentUser, "mac-launch-agent-user"),
            (
                StartupSource::WindowsStartupFolder,
                "windows-startup-folder",
            ),
        ] {
            assert_eq!(src.slug(), want);
            let json = serde_json::to_string(&src).unwrap();
            assert_eq!(json, format!("\"{want}\""));
        }
    }

    #[test]
    fn toggleable_sources_match_user_scope_exactly() {
        // is_toggleable = can the app mutate it (no sudo). is_user_scope = scope.
        // today they agree, test locks it in so a future ro user-scope source
        // (e.g. ro snap autostart) gets caught.
        for src in [
            StartupSource::LinuxAutostart,
            StartupSource::LinuxSystemdUser,
            StartupSource::MacLaunchAgentUser,
            StartupSource::MacLaunchAgentSystem,
            StartupSource::MacLaunchDaemon,
            StartupSource::WindowsStartupFolder,
            StartupSource::WindowsRunUser,
            StartupSource::WindowsRunMachine,
        ] {
            assert_eq!(src.is_toggleable(), src.is_user_scope(), "{src:?}");
        }
    }

    #[test]
    fn system_mac_sources_are_readonly() {
        assert!(!StartupSource::MacLaunchAgentSystem.is_toggleable());
        assert!(!StartupSource::MacLaunchDaemon.is_toggleable());
        assert!(!StartupSource::WindowsRunMachine.is_toggleable());
    }

    #[test]
    fn impact_classifier_flags_heavy_apps() {
        assert_eq!(
            impact_for_command("/Applications/Docker.app/…"),
            StartupImpact::High
        );
        assert_eq!(impact_for_command("/opt/Slack/slack"), StartupImpact::High);
        assert_eq!(
            impact_for_command("/usr/share/code/Code Helper"),
            StartupImpact::High,
        );
        assert_eq!(
            impact_for_command("/Library/Application Support/Firefox/firefox"),
            StartupImpact::High,
        );
    }

    #[test]
    fn impact_classifier_flags_sync_clients() {
        assert_eq!(
            impact_for_command("/Applications/Dropbox.app/Contents/MacOS/Dropbox"),
            StartupImpact::Medium,
        );
        assert_eq!(
            impact_for_command("/usr/bin/tailscaled --state=/var/lib/tailscale"),
            StartupImpact::Medium,
        );
    }

    #[test]
    fn impact_classifier_defaults_to_low() {
        assert_eq!(impact_for_command("/usr/bin/true"), StartupImpact::Low);
        assert_eq!(impact_for_command(""), StartupImpact::Low);
        assert_eq!(
            impact_for_command("some random shell script"),
            StartupImpact::Low
        );
    }

    #[test]
    fn impact_boot_seconds_are_ordered() {
        assert!(StartupImpact::Low.boot_seconds() < StartupImpact::Medium.boot_seconds());
        assert!(StartupImpact::Medium.boot_seconds() < StartupImpact::High.boot_seconds());
        // positive, an item should never speed up boot
        assert!(StartupImpact::Low.boot_seconds() > 0.0);
    }

    #[test]
    fn make_item_id_is_injection_safe() {
        assert_eq!(
            make_item_id(StartupSource::LinuxAutostart, "weird/name"),
            "linux-autostart::weird-name",
        );
        assert_eq!(
            make_item_id(StartupSource::MacLaunchAgentUser, "com.example:foo"),
            "mac-launch-agent-user::com.example-foo",
        );
    }

    #[test]
    fn startup_item_serialises_camel_case() {
        let item = StartupItem {
            id: "a".into(),
            name: "b".into(),
            description: "c".into(),
            command: "d".into(),
            source: StartupSource::LinuxAutostart,
            path: "/x".into(),
            enabled: true,
            is_user: true,
            icon: "power".into(),
            impact: StartupImpact::Low,
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains("\"isUser\":true"), "got: {json}");
        assert!(json.contains("\"impact\":\"low\""));
        assert!(json.contains("\"source\":\"linux-autostart\""));
    }
}
