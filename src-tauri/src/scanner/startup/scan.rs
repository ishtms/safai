//! startup scan orchestrator.
//!
//! runs every per-OS enumerator concurrently under std::thread::scope (same as
//! /9), merges into one [`StartupReport`]. sort: impact (High -> Medium
//! -> Low) then name, so UI default surfaces the boot hogs first.
//!
//! dispatch lives in [`toggle_startup`]. UI hands us id + desired state + path
//! (echoed from last scan), we validate path matches the source and call through.

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use super::types::{StartupImpact, StartupItem, StartupReport, StartupSource, ToggleResult};
use super::{linux, mac, windows};

pub use super::super::junk::catalog::Os;
pub use super::super::junk::catalog::current_os;

/// baseline boot seconds for "OS booting itself" on top of startup items.
/// rough mid-range laptop average. "before" is always >= this so UI comparison
/// bar has something to render when zero items enabled.
pub const BASELINE_BOOT_SECS: f32 = 8.0;

/// hermetic, no env consulted. tauri cmd resolves $HOME / %USERPROFILE% at the boundary.
pub fn scan_startup(home: &Path, os: Os) -> StartupReport {
    let started = Instant::now();

    let items: Vec<StartupItem> = std::thread::scope(|s| {
        // fan out. shaves ms even on small homes, matters on busy macs with
        // hundreds of launch agents.
        let h_linux_auto = s.spawn(|| {
            if matches!(os, Os::Linux) {
                linux::list_autostart(home)
            } else {
                Vec::new()
            }
        });
        let h_linux_systemd = s.spawn(|| {
            if matches!(os, Os::Linux) {
                linux::list_systemd_user(home)
            } else {
                Vec::new()
            }
        });
        let h_mac_user = s.spawn(|| {
            if matches!(os, Os::Mac) {
                mac::list_user_agents(home)
            } else {
                Vec::new()
            }
        });
        let h_mac_system = s.spawn(|| {
            if matches!(os, Os::Mac) {
                mac::list_system_agents()
            } else {
                Vec::new()
            }
        });
        let h_mac_daemons = s.spawn(|| {
            if matches!(os, Os::Mac) {
                mac::list_launch_daemons()
            } else {
                Vec::new()
            }
        });
        let h_win_folder = s.spawn(|| {
            if matches!(os, Os::Windows) {
                windows::list_startup_folder(home)
            } else {
                Vec::new()
            }
        });
        let h_win_run_user = s.spawn(|| {
            if matches!(os, Os::Windows) {
                windows::list_registry_run_user()
            } else {
                Vec::new()
            }
        });
        let h_win_run_machine = s.spawn(|| {
            if matches!(os, Os::Windows) {
                windows::list_registry_run_machine()
            } else {
                Vec::new()
            }
        });

        let mut merged = Vec::new();
        for handle in [
            h_linux_auto,
            h_linux_systemd,
            h_mac_user,
            h_mac_system,
            h_mac_daemons,
            h_win_folder,
            h_win_run_user,
            h_win_run_machine,
        ] {
            if let Ok(v) = handle.join() {
                merged.extend(v);
            }
        }
        merged
    });

    let mut items = items;
    items.sort_by(|a, b| {
        // High -> Medium -> Low so user sees "what's slowing your boot" first.
        // tiebreak: enabled first (those actually matter for boot time), then name.
        impact_rank(a.impact)
            .cmp(&impact_rank(b.impact))
            .then((!a.enabled).cmp(&!b.enabled))
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then(a.id.cmp(&b.id))
    });

    StartupReport {
        baseline_seconds: BASELINE_BOOT_SECS,
        items,
        duration_ms: started.elapsed().as_millis() as u64,
        scanned_at: now_unix(),
        platform: match os {
            Os::Mac => "mac",
            Os::Linux => "linux",
            Os::Windows => "windows",
        }
        .to_string(),
    }
}

fn impact_rank(i: StartupImpact) -> u8 {
    match i {
        StartupImpact::High => 0,
        StartupImpact::Medium => 1,
        StartupImpact::Low => 2,
    }
}

/// dispatches to per-source impl. validates path matches source first so a
/// compromised frontend can't point this at /etc/hosts.
pub fn toggle_startup(
    home: &Path,
    source: StartupSource,
    path: &Path,
    enabled: bool,
) -> Result<ToggleResult, String> {
    if !source.is_toggleable() {
        return Err(format!(
            "source {} is read-only from safai",
            source.slug(),
        ));
    }
    validate_path_matches_source(home, source, path)?;
    match source {
        StartupSource::LinuxAutostart => linux::toggle_autostart(path, enabled)?,
        StartupSource::LinuxSystemdUser => {
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| "invalid systemd unit path".to_string())?;
            linux::toggle_systemd_user(home, name, enabled)?;
        }
        StartupSource::MacLaunchAgentUser => mac::toggle_user_agent(path, enabled)?,
        StartupSource::WindowsStartupFolder => {
            windows::toggle_startup_folder(path, enabled)?
        }
        StartupSource::WindowsRunUser => {
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| "invalid registry item path".to_string())?;
            windows::toggle_registry_run_user(name, enabled)?;
        }
        _ => unreachable!("guarded by is_toggleable"),
    }
    Ok(ToggleResult {
        id: super::types::make_item_id(
            source,
            path.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default(),
        ),
        enabled,
    })
}

/// reject paths outside the declared source's tree. lexical prefix (no
/// canonicalize) so symlinks can't trick us, same as safety.
fn validate_path_matches_source(
    home: &Path,
    source: StartupSource,
    path: &Path,
) -> Result<(), String> {
    let roots: Vec<PathBuf> = match source {
        StartupSource::LinuxAutostart => vec![linux::autostart_dir(home)],
        StartupSource::LinuxSystemdUser => vec![linux::systemd_user_dir(home)],
        StartupSource::MacLaunchAgentUser => vec![mac::user_launch_agents(home)],
        StartupSource::WindowsStartupFolder => vec![windows::startup_folder(home)],
        StartupSource::WindowsRunUser => return Ok(()),
        _ => return Err(format!("source {} is read-only", source.slug())),
    };
    if !roots.iter().any(|r| path.starts_with(r)) {
        return Err(format!(
            "path {} is not under the expected root for source {}",
            path.display(),
            source.slug(),
        ));
    }
    // reject `..` segments. catalog root + `..` can escape lexically even after starts_with.
    if path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(format!("path {} contains parent-directory components", path.display()));
    }
    Ok(())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    fn linux_home(dir: &TempDir) -> PathBuf {
        let home = dir.path().to_path_buf();
        let autostart = home.join(".config/autostart");
        fs::create_dir_all(&autostart).unwrap();
        write(
            &autostart.join("slack.desktop"),
            "[Desktop Entry]\nType=Application\nName=Slack\nExec=/opt/Slack/slack\n",
        );
        write(
            &autostart.join("random.desktop"),
            "[Desktop Entry]\nType=Application\nName=Random\nExec=/usr/bin/random\nHidden=true\n",
        );
        let systemd = home.join(".config/systemd/user");
        fs::create_dir_all(&systemd).unwrap();
        write(
            &systemd.join("syncthing.service"),
            "[Unit]\nDescription=Syncthing\n[Service]\nExecStart=/usr/bin/syncthing\n",
        );
        home
    }

    #[test]
    fn scan_linux_merges_autostart_and_systemd() {
        let dir = TempDir::new().unwrap();
        let home = linux_home(&dir);
        let report = scan_startup(&home, Os::Linux);
        assert_eq!(report.platform, "linux");
        let names: Vec<&str> = report.items.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"Slack"));
        assert!(names.contains(&"Random"));
        assert!(names.contains(&"syncthing.service"));
    }

    #[test]
    fn scan_orders_high_impact_first() {
        let dir = TempDir::new().unwrap();
        let home = linux_home(&dir);
        let report = scan_startup(&home, Os::Linux);
        // Slack = high, Random + syncthing = low. Slack must come first.
        let slack_idx = report.items.iter().position(|i| i.name == "Slack").unwrap();
        let random_idx = report.items.iter().position(|i| i.name == "Random").unwrap();
        assert!(slack_idx < random_idx);
    }

    #[test]
    fn scan_empty_home_returns_zero_items() {
        let dir = TempDir::new().unwrap();
        let report = scan_startup(dir.path(), Os::Linux);
        assert!(report.items.is_empty());
        assert_eq!(report.platform, "linux");
        assert_eq!(report.baseline_seconds, BASELINE_BOOT_SECS);
    }

    #[test]
    fn scan_mac_enumerates_user_agents() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().to_path_buf();
        let agents = mac::user_launch_agents(&home);
        fs::create_dir_all(&agents).unwrap();
        let plist = r#"<?xml version="1.0"?><plist><dict>
            <key>Label</key><string>com.example.foo</string>
            <key>ProgramArguments</key><array><string>/opt/foo</string></array>
            <key>Disabled</key><false/>
        </dict></plist>"#;
        fs::write(agents.join("com.example.foo.plist"), plist).unwrap();
        let report = scan_startup(&home, Os::Mac);
        assert_eq!(report.platform, "mac");
        assert!(
            report.items.iter().any(|i| i.name == "com.example.foo" && i.enabled),
            "items: {:?}",
            report.items,
        );
    }

    #[test]
    fn scan_windows_enumerates_startup_folder() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().to_path_buf();
        let folder = windows::startup_folder(&home);
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("Spotify.lnk"), b"x").unwrap();
        let report = scan_startup(&home, Os::Windows);
        assert_eq!(report.platform, "windows");
        assert!(report.items.iter().any(|i| i.name == "Spotify"));
    }

    #[test]
    fn scan_is_deterministic_across_runs() {
        let dir = TempDir::new().unwrap();
        let home = linux_home(&dir);
        let a = scan_startup(&home, Os::Linux);
        let b = scan_startup(&home, Os::Linux);
        let a_ids: Vec<&str> = a.items.iter().map(|i| i.id.as_str()).collect();
        let b_ids: Vec<&str> = b.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(a_ids, b_ids);
    }

    #[test]
    fn scan_duration_is_recorded() {
        let dir = TempDir::new().unwrap();
        let home = linux_home(&dir);
        let r = scan_startup(&home, Os::Linux);
        // always < 1s on synth tree. upper-bound assert catches accidental
        // blocking-fs stalls sneaking in.
        assert!(r.duration_ms < 5_000, "scan took {} ms", r.duration_ms);
    }

    // ---------- toggle_startup ----------

    #[test]
    fn toggle_startup_round_trips_autostart() {
        let dir = TempDir::new().unwrap();
        let home = linux_home(&dir);
        let slack = linux::autostart_dir(&home).join("slack.desktop");
        let before = scan_startup(&home, Os::Linux);
        let slack_before = before.items.iter().find(|i| i.name == "Slack").unwrap().enabled;
        assert!(slack_before);
        let res = toggle_startup(
            &home,
            StartupSource::LinuxAutostart,
            &slack,
            false,
        )
        .unwrap();
        assert!(!res.enabled);
        let after = scan_startup(&home, Os::Linux);
        let slack_after = after.items.iter().find(|i| i.name == "Slack").unwrap().enabled;
        assert!(!slack_after);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn toggle_startup_round_trips_systemd() {
        let dir = TempDir::new().unwrap();
        let home = linux_home(&dir);
        let unit = linux::systemd_user_dir(&home).join("syncthing.service");
        toggle_startup(
            &home,
            StartupSource::LinuxSystemdUser,
            &unit,
            true,
        )
        .unwrap();
        let items = linux::list_systemd_user(&home);
        assert!(items.iter().find(|i| i.name == "syncthing.service").unwrap().enabled);
    }

    #[test]
    fn toggle_startup_rejects_path_outside_source_root() {
        let dir = TempDir::new().unwrap();
        let home = linux_home(&dir);
        let somewhere_else = dir.path().join("evil.desktop");
        write(
            &somewhere_else,
            "[Desktop Entry]\nType=Application\nName=Evil\nExec=/x\n",
        );
        let err = toggle_startup(
            &home,
            StartupSource::LinuxAutostart,
            &somewhere_else,
            false,
        )
        .unwrap_err();
        assert!(err.contains("not under the expected root"), "got: {err}");
    }

    #[test]
    fn toggle_startup_rejects_parent_dir_components() {
        let dir = TempDir::new().unwrap();
        let home = linux_home(&dir);
        let sneaky = linux::autostart_dir(&home).join("../../../etc/hosts");
        let err = toggle_startup(
            &home,
            StartupSource::LinuxAutostart,
            &sneaky,
            false,
        )
        .unwrap_err();
        assert!(err.contains("parent-directory") || err.contains("not under"));
    }

    #[test]
    fn toggle_startup_refuses_readonly_sources() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().to_path_buf();
        let err = toggle_startup(
            &home,
            StartupSource::MacLaunchDaemon,
            Path::new("/Library/LaunchDaemons/com.apple.foo.plist"),
            false,
        )
        .unwrap_err();
        assert!(err.contains("read-only"), "got: {err}");
    }

    #[test]
    fn toggle_startup_mac_user_agent() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().to_path_buf();
        let agents = mac::user_launch_agents(&home);
        fs::create_dir_all(&agents).unwrap();
        let plist_path = agents.join("com.example.toggle.plist");
        fs::write(
            &plist_path,
            r#"<?xml version="1.0"?><plist><dict>
    <key>Label</key><string>com.example.toggle</string>
    <key>ProgramArguments</key><array><string>/x</string></array>
    <key>Disabled</key><false/>
</dict></plist>"#,
        )
        .unwrap();
        toggle_startup(
            &home,
            StartupSource::MacLaunchAgentUser,
            &plist_path,
            false,
        )
        .unwrap();
        let items = mac::list_user_agents(&home);
        assert!(!items[0].enabled);
    }

    #[test]
    fn toggle_startup_windows_folder() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().to_path_buf();
        let folder = windows::startup_folder(&home);
        fs::create_dir_all(&folder).unwrap();
        let lnk = folder.join("App.lnk");
        fs::write(&lnk, b"x").unwrap();
        toggle_startup(
            &home,
            StartupSource::WindowsStartupFolder,
            &lnk,
            false,
        )
        .unwrap();
        assert!(!lnk.exists());
        assert!(folder.join("App.lnk.disabled").exists());
    }

    #[test]
    fn report_wire_shape_is_camel_case() {
        let dir = TempDir::new().unwrap();
        let home = linux_home(&dir);
        let r = scan_startup(&home, Os::Linux);
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"baselineSeconds\""), "{json}");
        assert!(json.contains("\"durationMs\""));
        assert!(json.contains("\"scannedAt\""));
        // item shape
        assert!(json.contains("\"isUser\""));
        assert!(json.contains("\"impact\""));
    }
}
