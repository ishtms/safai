//! windows startup enumerator + toggle.
//!
//! windows surfaces startup items from:
//!
//! - startup folder (file-based). `%APPDATA%\Microsoft\Windows\Start
//!   Menu\Programs\Startup\*.lnk`. anything under this dir launches at login.
//!   disable = move out, we rename with `.disabled` suffix so restore is a
//!   simple rename.
//!
//! - registry Run keys. `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
//!   (user) + `HKLM\...\Run` (machine). needs winreg, only populated on windows
//!   builds. non-windows returns empty so tests still compile.
//!
//! startup folder path is canonically %APPDATA% (Roaming). %LOCALAPPDATA% isn't
//! standard, everything there is per-machine / cached.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::types::{impact_for_command, make_item_id, path_for_wire, StartupItem, StartupSource};

pub const DISABLED_SUFFIX: &str = ".disabled";
#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const DISABLED_RUN_KEY: &str = r"Software\Safai\DisabledStartupRun";

/// user-scope Startup folder under %APPDATA%. On Windows we use APPDATA
/// directly so redirected/roaming profiles resolve correctly. Non-Windows
/// tests keep the deterministic `home`-relative layout.
pub fn startup_folder(home: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let roaming = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Roaming"));
        startup_folder_from_roaming(&roaming)
    }
    #[cfg(not(windows))]
    {
        startup_folder_from_roaming(&home.join("AppData/Roaming"))
    }
}

fn startup_folder_from_roaming(roaming: &Path) -> PathBuf {
    roaming.join("Microsoft/Windows/Start Menu/Programs/Startup")
}

/// .lnk + .disabled both recognised. .disabled shows as enabled=false so UI
/// can flip back without re-scanning.
pub fn list_startup_folder(home: &Path) -> Vec<StartupItem> {
    let dir = startup_folder(home);
    let Ok(read) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut items: Vec<StartupItem> = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.file_type().is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()).map(str::to_owned) else {
            continue;
        };
        // skip our own atomic-writer tmp files
        if name.starts_with(".safai-tmp") {
            continue;
        }
        let (display_name, enabled) = match name.strip_suffix(DISABLED_SUFFIX) {
            Some(stem) => (strip_ext(stem).to_string(), false),
            None => (strip_ext(&name).to_string(), true),
        };
        // command = the shortcut path itself. resolving to the underlying exe
        // would need a .lnk parser (Windows Shell link binary format). surfacing
        // the path matches what Explorer shows + what UI knows how to reveal.
        let command = path_for_wire(&path);
        let id = make_item_id(StartupSource::WindowsStartupFolder, &display_name);
        items.push(StartupItem {
            id,
            name: display_name,
            description: String::new(),
            command: command.clone(),
            source: StartupSource::WindowsStartupFolder,
            path: path_for_wire(&path),
            enabled,
            is_user: true,
            icon: "power".to_string(),
            impact: impact_for_command(&command),
        });
    }
    items.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    items
}

fn strip_ext(name: &str) -> &str {
    // drop final extension for display. multiple dots fine, only last segment
    // removed so "My.App.v2.lnk" -> "My.App.v2".
    if let Some(dot) = name.rfind('.') {
        if dot > 0 {
            return &name[..dot];
        }
    }
    name
}

/// disable: rename `foo.lnk` -> `foo.lnk.disabled`.
/// enable:  rename `foo.lnk.disabled` -> `foo.lnk`. refuses to clobber an
/// existing `foo.lnk` so we don't nuke a fresh file the user dropped in.
pub fn toggle_startup_folder(path: &Path, enabled: bool) -> Result<(), String> {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return Err(format!("startup item not found: {}", path.display()));
    };
    if !meta.file_type().is_file() {
        return Err(format!(
            "startup item is not a regular file: {}",
            path.display()
        ));
    }
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| "invalid startup item name".to_string())?;
    let parent = path
        .parent()
        .ok_or_else(|| "startup item has no parent directory".to_string())?;

    if enabled {
        // rename foo.lnk.disabled -> foo.lnk
        let Some(base) = file_name.strip_suffix(DISABLED_SUFFIX) else {
            // already enabled
            return Ok(());
        };
        let target = parent.join(base);
        if target.exists() {
            return Err(format!(
                "cannot enable: a file already exists at {}",
                target.display()
            ));
        }
        fs::rename(path, target).map_err(|e| format!("rename: {e}"))
    } else {
        if file_name.ends_with(DISABLED_SUFFIX) {
            // idempotent
            return Ok(());
        }
        let target = parent.join(format!("{file_name}{DISABLED_SUFFIX}"));
        if target.exists() {
            // stale .disabled from prior run. keep user's newer file, drop the shadow.
            fs::remove_file(&target).map_err(|e| format!("remove stale disabled file: {e}"))?;
        }
        fs::rename(path, target).map_err(|e| format!("rename: {e}"))
    }
}

// ---------------- registry (windows only) ----------------

/// non-windows stub so orchestrator can call unconditionally
#[cfg(not(windows))]
pub fn list_registry_run_user() -> Vec<StartupItem> {
    Vec::new()
}

#[cfg(not(windows))]
pub fn list_registry_run_machine() -> Vec<StartupItem> {
    Vec::new()
}

/// non-windows returns error so linux tests exercise the path
#[cfg(not(windows))]
pub fn toggle_registry_run_user(_name: &str, _enabled: bool) -> Result<(), String> {
    Err("registry toggling is only supported on Windows builds".into())
}

#[cfg(windows)]
pub fn list_registry_run_user() -> Vec<StartupItem> {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let mut items = list_registry_run_key(
        &hkcu,
        StartupSource::WindowsRunUser,
        true,
        "HKCU",
        RUN_KEY,
        true,
    );
    items.extend(list_registry_run_key(
        &hkcu,
        StartupSource::WindowsRunUser,
        true,
        "HKCU",
        DISABLED_RUN_KEY,
        false,
    ));
    items.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    items
}

#[cfg(windows)]
pub fn list_registry_run_machine() -> Vec<StartupItem> {
    let hklm = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE);
    let mut items = list_registry_run_key(
        &hklm,
        StartupSource::WindowsRunMachine,
        false,
        "HKLM",
        RUN_KEY,
        true,
    );
    items.extend(list_registry_run_key(
        &hklm,
        StartupSource::WindowsRunMachine,
        false,
        "HKLM",
        r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run",
        true,
    ));
    dedup_registry_items(&mut items);
    items.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    items
}

#[cfg(windows)]
pub fn toggle_registry_run_user(name: &str, enabled: bool) -> Result<(), String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (run_key, _) = hkcu
        .create_subkey_with_flags(RUN_KEY, KEY_READ | KEY_SET_VALUE)
        .map_err(|e| format!("open HKCU Run key: {e}"))?;
    let (disabled_key, _) = hkcu
        .create_subkey_with_flags(DISABLED_RUN_KEY, KEY_READ | KEY_SET_VALUE)
        .map_err(|e| format!("open Safai disabled Run key: {e}"))?;

    if enabled {
        let value = disabled_key
            .get_raw_value(name)
            .map_err(|e| format!("disabled registry value not found: {e}"))?;
        run_key
            .set_raw_value(name, &value)
            .map_err(|e| format!("restore Run value: {e}"))?;
        disabled_key
            .delete_value(name)
            .map_err(|e| format!("delete disabled Run backup: {e}"))?;
    } else {
        let value = run_key
            .get_raw_value(name)
            .map_err(|e| format!("Run registry value not found: {e}"))?;
        disabled_key
            .set_raw_value(name, &value)
            .map_err(|e| format!("backup Run value: {e}"))?;
        run_key
            .delete_value(name)
            .map_err(|e| format!("delete Run value: {e}"))?;
    }
    Ok(())
}

#[cfg(windows)]
fn list_registry_run_key(
    root: &winreg::RegKey,
    source: StartupSource,
    is_user: bool,
    hive_label: &str,
    subkey: &str,
    enabled: bool,
) -> Vec<StartupItem> {
    use winreg::enums::KEY_READ;

    let Ok(key) = root.open_subkey_with_flags(subkey, KEY_READ) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for value in key.enum_values().flatten() {
        let (name, _raw) = value;
        let command = match key.get_value::<String, _>(&name) {
            Ok(command) => command,
            Err(_) => continue,
        };
        let path = format!(r"{hive_label}\{subkey}\{name}");
        let id = make_item_id(source, &name);
        let impact = impact_for_command(&command);
        items.push(StartupItem {
            id,
            name,
            description: String::new(),
            command,
            source,
            path,
            enabled,
            is_user,
            icon: if is_user { "power" } else { "shield2" }.to_string(),
            impact,
        });
    }
    items
}

#[cfg(windows)]
fn dedup_registry_items(items: &mut Vec<StartupItem>) {
    let mut seen = std::collections::HashSet::<String>::new();
    items.retain(|item| seen.insert(item.name.to_ascii_lowercase()));
}

#[allow(dead_code)]
fn atomic_write(path: &Path, contents: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(".safai-tmp.startup");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn startup_folder_path_is_under_home() {
        let home = Path::new("/Users/test");
        let got = startup_folder(home);
        #[cfg(not(windows))]
        assert!(got.starts_with(home));
        #[cfg(windows)]
        assert!(got.ends_with("Startup"));
        assert!(got.ends_with("Startup"));
    }

    #[test]
    fn list_startup_folder_returns_enabled_items() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let folder = startup_folder(home);
        fs::create_dir_all(&folder).unwrap();
        write(&folder.join("Dropbox.lnk"), b"fake-shortcut");
        write(&folder.join("MyBat.bat"), b"@echo off\n");
        let items = list_startup_folder(home);
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"Dropbox"));
        assert!(names.contains(&"MyBat"));
        for i in &items {
            assert!(i.enabled);
            assert_eq!(i.source, StartupSource::WindowsStartupFolder);
        }
    }

    #[test]
    fn list_startup_folder_marks_disabled_suffix() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let folder = startup_folder(home);
        fs::create_dir_all(&folder).unwrap();
        write(&folder.join("ExampleApp.lnk.disabled"), b"x");
        let items = list_startup_folder(home);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "ExampleApp");
        assert!(!items[0].enabled);
    }

    #[test]
    fn list_startup_folder_skips_non_files() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let folder = startup_folder(home);
        fs::create_dir_all(&folder).unwrap();
        fs::create_dir_all(folder.join("subdir")).unwrap();
        write(&folder.join("a.lnk"), b"x");
        let items = list_startup_folder(home);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "a");
    }

    #[test]
    fn list_startup_folder_missing_returns_empty() {
        let dir = TempDir::new().unwrap();
        let items = list_startup_folder(dir.path());
        assert!(items.is_empty());
    }

    #[test]
    fn toggle_startup_folder_disable_renames_file() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let folder = startup_folder(home);
        fs::create_dir_all(&folder).unwrap();
        let shortcut = folder.join("Foo.lnk");
        write(&shortcut, b"x");
        toggle_startup_folder(&shortcut, false).unwrap();
        assert!(!shortcut.exists());
        assert!(folder.join("Foo.lnk.disabled").exists());
    }

    #[test]
    fn toggle_startup_folder_enable_reverses_rename() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let folder = startup_folder(home);
        fs::create_dir_all(&folder).unwrap();
        let disabled = folder.join("Foo.lnk.disabled");
        write(&disabled, b"x");
        toggle_startup_folder(&disabled, true).unwrap();
        assert!(!disabled.exists());
        assert!(folder.join("Foo.lnk").exists());
    }

    #[test]
    fn toggle_startup_folder_refuses_to_clobber() {
        // If enabling would overwrite a live shortcut, error out.
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let folder = startup_folder(home);
        fs::create_dir_all(&folder).unwrap();
        let live = folder.join("Foo.lnk");
        write(&live, b"live");
        let disabled = folder.join("Foo.lnk.disabled");
        write(&disabled, b"old");
        let err = toggle_startup_folder(&disabled, true).unwrap_err();
        assert!(err.contains("already exists"), "got: {err}");
        assert!(live.exists());
        assert!(disabled.exists());
    }

    #[test]
    fn toggle_startup_folder_disable_replaces_stale_disabled() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let folder = startup_folder(home);
        fs::create_dir_all(&folder).unwrap();
        let live = folder.join("Foo.lnk");
        write(&live, b"new");
        let stale = folder.join("Foo.lnk.disabled");
        write(&stale, b"stale");
        toggle_startup_folder(&live, false).unwrap();
        assert!(!live.exists());
        let moved = fs::read_to_string(&stale).unwrap();
        assert_eq!(moved, "new");
    }

    #[test]
    fn toggle_startup_folder_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let folder = startup_folder(home);
        fs::create_dir_all(&folder).unwrap();
        let live = folder.join("X.lnk");
        write(&live, b"x");
        toggle_startup_folder(&live, true).unwrap();
        assert!(live.exists());
        // disable twice = first disables, second is no-op
        toggle_startup_folder(&live, false).unwrap();
        let disabled = folder.join("X.lnk.disabled");
        assert!(disabled.exists());
        toggle_startup_folder(&disabled, false).unwrap();
        assert!(disabled.exists());
    }

    #[test]
    fn toggle_rejects_missing_path() {
        let dir = TempDir::new().unwrap();
        let err = toggle_startup_folder(&dir.path().join("nope.lnk"), false).unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn registry_stubs_return_empty_on_non_windows() {
        // stubs must return empty, not panic/error, so enumeration isn't blocked
        let u = list_registry_run_user();
        let m = list_registry_run_machine();
        assert!(u.is_empty());
        assert!(m.is_empty());
    }

    #[cfg(not(windows))]
    #[test]
    fn registry_toggle_errors_off_platform() {
        assert!(toggle_registry_run_user("x", true).is_err());
    }

    #[test]
    fn strip_ext_handles_multiple_dots() {
        assert_eq!(strip_ext("foo.bar.lnk"), "foo.bar");
        assert_eq!(strip_ext("foo"), "foo");
        assert_eq!(strip_ext(".hidden"), ".hidden");
    }
}
