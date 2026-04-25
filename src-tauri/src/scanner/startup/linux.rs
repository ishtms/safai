//! linux startup enumerator + toggle.
//!
//! two XDG sources:
//!
//! 1. autostart `.desktop` files under `<XDG_CONFIG_HOME>/autostart`. per the
//!    freedesktop.org autostart spec, every `.desktop` in there launches at login.
//!    disable = either `Hidden=true` or `X-GNOME-Autostart-enabled=false`. we
//!    normalize to `Hidden=true` since it's the spec-blessed key.
//!
//! 2. systemd `--user` units. "enabled" when a symlink under
//!    `<unit-dir>/default.target.wants/` points to the canonical file. toggle
//!    creates/removes that symlink (same as `systemctl --user enable|disable`).
//!    we do it ourselves to stay hermetic and testable without systemd running.
//!
//! both sources are real files cleaner could act on, but we mutate them
//! in place (autostart) or via symlink flips (systemd) not move-to-graveyard.
//! user disabling a startup item wants it to stop launching, not vanish.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs as unix_fs;
use std::path::{Path, PathBuf};

use super::types::{
    impact_for_command, make_item_id, path_for_wire, StartupItem, StartupSource,
};

pub fn autostart_dir(home: &Path) -> PathBuf {
    home.join(".config/autostart")
}

/// user-scope systemd base dir. runtime also searches /etc/systemd/user and
/// /usr/lib/systemd/user but those are distro-owned, we don't mutate them.
/// scoped to user-only here.
pub fn systemd_user_dir(home: &Path) -> PathBuf {
    home.join(".config/systemd/user")
}

/// systemd's "enabled at boot" dir. unit `foo.service` is enabled iff
/// `default.target.wants/foo.service` is a symlink back to the canonical unit.
/// could also check `*.target.wants/` (timers etc) but default.target is where
/// login-scoped units land, matches "what launches when i log in".
pub fn systemd_wants_dir(home: &Path) -> PathBuf {
    systemd_user_dir(home).join("default.target.wants")
}

// ---------------- autostart enumeration ----------------

/// returns sorted-by-name list for stable wire order.
///
/// unreadable entries, non-UTF-8 names, malformed files skipped silently. spec
/// allows a lot of vendor weirdness (Ubuntu ships `Name[de]=...` locale keys,
/// KDE emits X-KDE-autostart-condition predicates) so one odd file never kills
/// the whole scan.
pub fn list_autostart(home: &Path) -> Vec<StartupItem> {
    let dir = autostart_dir(home);
    let Ok(read) = fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut items: Vec<StartupItem> = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
            continue;
        }
        // plain files only. symlinks could redirect a toggle to an unrelated file.
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        if !meta.file_type().is_file() {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else { continue };
        let parsed = parse_desktop(&text);

        let name = parsed
            .get("Name")
            .cloned()
            .or_else(|| {
                path.file_stem().and_then(|s| s.to_str()).map(str::to_owned)
            })
            .unwrap_or_default();
        let command = parsed.get("Exec").cloned().unwrap_or_default();
        let description = parsed
            .get("Comment")
            .cloned()
            .or_else(|| parsed.get("GenericName").cloned())
            .unwrap_or_default();

        let enabled = is_autostart_enabled(&parsed);
        let id = make_item_id(
            StartupSource::LinuxAutostart,
            path.file_stem().and_then(|s| s.to_str()).unwrap_or(&name),
        );
        items.push(StartupItem {
            id,
            name,
            description,
            command: command.clone(),
            source: StartupSource::LinuxAutostart,
            path: path_for_wire(&path),
            enabled,
            is_user: true,
            icon: "power".to_string(),
            impact: impact_for_command(&command),
        });
    }
    items.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
    items
}

/// parse a `.desktop` file into a flat key -> value map.
///
/// freedesktop.org rules:
/// - `#` comments + blank lines skipped
/// - section headers `[Desktop Entry]` detected but only first section kept.
///   autostart ignores secondary action sections like `[Desktop Action ...]`.
/// - locale-suffixed keys (`Name[de]=...`) dropped, only bare keys
/// - backslash escapes (`\n`, `\t`, `\\`) expanded. users embed literal tabs
///   in Exec sometimes.
pub fn parse_desktop(text: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut in_main = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(section) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            // first section only, typically [Desktop Entry]
            if in_main {
                break;
            }
            in_main = section == "Desktop Entry";
            continue;
        }
        if !in_main {
            continue;
        }
        let Some(eq) = line.find('=') else { continue };
        let key_part = line[..eq].trim();
        let value = line[eq + 1..].trim_start();
        // drop locale variants
        if key_part.contains('[') {
            continue;
        }
        let unescaped = unescape_desktop(value);
        out.insert(key_part.to_string(), unescaped);
    }
    out
}

fn unescape_desktop(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some('s') => out.push(' '),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// does this autostart entry launch at next login. covers every disable
/// mechanism the spec + vendors use so toggle round-trips whatever the original
/// author wrote:
///
/// - `Hidden=true` - spec-defined disable. our default form.
/// - `X-GNOME-Autostart-enabled=false` - GNOME-specific toggle some "Startup
///   Applications" tools write.
/// - `NoDisplay=true` - launcher visibility only, still autostarts per spec. leave alone.
/// - `Type=Application` required (spec excludes other types from autostart).
fn is_autostart_enabled(parsed: &BTreeMap<String, String>) -> bool {
    // XDG autostart requires Type=Application. anything else (Link, Directory,
    // missing) = session runner skips it, so we surface it as disabled.
    match parsed.get("Type") {
        Some(t) if t.eq_ignore_ascii_case("application") => {}
        _ => return false,
    }
    if parsed
        .get("Hidden")
        .map(|v| parse_bool(v))
        .unwrap_or(false)
    {
        return false;
    }
    if let Some(v) = parsed.get("X-GNOME-Autostart-enabled") {
        if !parse_bool(v) {
            return false;
        }
    }
    true
}

fn parse_bool(s: &str) -> bool {
    matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes")
}

/// flip enabled state. rewrites Hidden, leaves other keys alone (3rd-party
/// metadata survives). atomic via tmp+rename. missing file = error so UI rescans.
pub fn toggle_autostart(path: &Path, enabled: bool) -> Result<(), String> {
    let text = fs::read_to_string(path).map_err(|e| format!("read autostart file: {e}"))?;
    let new_text = rewrite_desktop_hidden(&text, !enabled);
    atomic_write(path, &new_text).map_err(|e| format!("write autostart file: {e}"))
}

/// pure rewriter, public for tests.
///
/// preserves rest of file byte-for-byte where possible. Hidden in first section:
/// updated in place. otherwise: appended after last key of [Desktop Entry].
/// also coerces any X-GNOME-Autostart-enabled to match so the two can't disagree.
pub fn rewrite_desktop_hidden(text: &str, hidden: bool) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let mut section: Option<&str> = None;
    let mut touched_hidden = false;
    let mut main_section_end: Option<usize> = None;

    for (i, raw) in lines.clone().iter().enumerate() {
        let trimmed = raw.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if section == Some("Desktop Entry") {
                // hit the next section, mark prior line as insert point if
                // we haven't written Hidden yet, then stop scanning.
                main_section_end.get_or_insert(i);
                break;
            }
            section = if trimmed == "[Desktop Entry]" {
                Some("Desktop Entry")
            } else {
                Some("")
            };
            continue;
        }
        if section != Some("Desktop Entry") {
            continue;
        }
        if let Some(eq) = raw.find('=') {
            let key = raw[..eq].trim();
            if key == "Hidden" {
                lines[i] = format!("Hidden={}", if hidden { "true" } else { "false" });
                touched_hidden = true;
            } else if key == "X-GNOME-Autostart-enabled" {
                // coerce to match so the two mechanisms can't contradict
                lines[i] = format!(
                    "X-GNOME-Autostart-enabled={}",
                    if hidden { "false" } else { "true" }
                );
            }
        }
    }

    if !touched_hidden {
        // append Hidden=... inside [Desktop Entry], or at EOF if no header
        let insert_at = main_section_end.unwrap_or(lines.len());
        lines.insert(
            insert_at,
            format!("Hidden={}", if hidden { "true" } else { "false" }),
        );
    }

    // preserve trailing newline shape
    let joined = lines.join("\n");
    if text.ends_with('\n') && !joined.ends_with('\n') {
        format!("{joined}\n")
    } else {
        joined
    }
}

// ---------------- systemd user enumeration ----------------

/// user-scope units under `~/.config/systemd/user/`. "enabled" = symlink exists
/// in default.target.wants/, same as `systemctl is-enabled`.
pub fn list_systemd_user(home: &Path) -> Vec<StartupItem> {
    let dir = systemd_user_dir(home);
    let Ok(read) = fs::read_dir(&dir) else { return Vec::new() };
    let wants = systemd_wants_dir(home);

    let mut items: Vec<StartupItem> = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        // only user-owned service/socket/timer. .target/.mount are structural
        // and would confuse the UI.
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !matches!(ext, "service" | "socket" | "timer") {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        // skip wants-dir symlinks which sit under the same tree
        if !meta.file_type().is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|s| s.to_str()).map(str::to_owned) else {
            continue;
        };
        let Ok(text) = fs::read_to_string(&path) else { continue };
        let (description, exec_start) = parse_systemd_unit(&text);
        let enabled = wants.join(&file_name).is_symlink();
        let id = make_item_id(StartupSource::LinuxSystemdUser, &file_name);
        items.push(StartupItem {
            id,
            name: file_name.clone(),
            description,
            command: exec_start.clone(),
            source: StartupSource::LinuxSystemdUser,
            path: path_for_wire(&path),
            enabled,
            is_user: true,
            icon: "pulse".to_string(),
            impact: impact_for_command(&exec_start),
        });
    }
    items.sort_by(|a, b| a.name.cmp(&b.name));
    items
}

/// pulls Description= + ExecStart=. returns ("", "") for missing, UI handles
/// empty fine. no locale suffixes in systemd so parse is straightforward.
pub fn parse_systemd_unit(text: &str) -> (String, String) {
    let mut section: Option<&str> = None;
    let mut description = String::new();
    let mut exec_start = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if let Some(sec) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = Some(match sec {
                "Unit" => "Unit",
                "Service" => "Service",
                _ => "",
            });
            continue;
        }
        let Some(eq) = line.find('=') else { continue };
        let key = line[..eq].trim();
        let value = line[eq + 1..].trim();
        match (section, key) {
            (Some("Unit"), "Description") => description = value.to_string(),
            (Some("Service"), "ExecStart") => {
                if exec_start.is_empty() {
                    exec_start = value.trim_start_matches('-').trim_start_matches('@').to_string();
                }
            }
            _ => {}
        }
    }
    (description, exec_start)
}

/// flip enabled. creates/removes default.target.wants/<unit> symlink. pure fs,
/// no systemctl dep, works in tests + containers without systemd.
///
/// captures persistent "launches at login" state. if systemd is running and the
/// unit is already live, caller still needs `systemctl --user daemon-reload` for
/// the change to take effect live, but login-time behaviour is correct.
pub fn toggle_systemd_user(home: &Path, unit_name: &str, enabled: bool) -> Result<(), String> {
    // UI hands us a bare basename. reject anything resembling traversal so a
    // compromised frontend can't smuggle in an arbitrary symlink target.
    if unit_name.is_empty()
        || unit_name.contains('/')
        || unit_name.contains('\\')
        || unit_name.contains("..")
    {
        return Err(format!("invalid systemd unit name: {unit_name}"));
    }
    let base_dir = systemd_user_dir(home);
    let target = base_dir.join(unit_name);
    if !target.exists() {
        return Err(format!("systemd unit not found: {unit_name}"));
    }
    let wants = systemd_wants_dir(home);
    fs::create_dir_all(&wants).map_err(|e| format!("create wants dir: {e}"))?;
    let link = wants.join(unit_name);
    if enabled {
        if link.exists() || link.is_symlink() {
            // idempotent
            return Ok(());
        }
        #[cfg(unix)]
        {
            unix_fs::symlink(&target, &link).map_err(|e| format!("create symlink: {e}"))
        }
        #[cfg(not(unix))]
        {
            // never reached, scan dispatch gates by Os::Linux. stub keeps it compilable
            let _ = (&target, &link);
            Err("systemd toggle unsupported on this platform".to_string())
        }
    } else {
        // symlink_metadata so broken symlinks still get recognised
        match fs::symlink_metadata(&link) {
            Ok(_) => fs::remove_file(&link).map_err(|e| format!("remove symlink: {e}")),
            Err(_) => Ok(()),
        }
    }
}

// ---------------- shared helpers ----------------

/// atomic overwrite. tmp file + fsync + rename so a crash mid-write leaves
/// the original in place.
fn atomic_write(path: &Path, contents: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("startup-item");
    let tmp = dir.join(format!(".{file_name}.safai-tmp"));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    // fs::rename is atomic within an fs. autostart lives inside $HOME/.config,
    // never crosses device boundaries in practice.
    fs::rename(&tmp, path)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    // ---------- parse_desktop ----------

    #[test]
    fn parse_desktop_basic_keys() {
        let text = "[Desktop Entry]\n\
                    Type=Application\n\
                    Name=Example\n\
                    Exec=/usr/bin/example --foo\n\
                    Comment=An example\n";
        let m = parse_desktop(text);
        assert_eq!(m.get("Type"), Some(&"Application".to_string()));
        assert_eq!(m.get("Name"), Some(&"Example".to_string()));
        assert_eq!(m.get("Exec"), Some(&"/usr/bin/example --foo".to_string()));
        assert_eq!(m.get("Comment"), Some(&"An example".to_string()));
    }

    #[test]
    fn parse_desktop_skips_locale_keys() {
        let text = "[Desktop Entry]\n\
                    Name=Example\n\
                    Name[de]=Beispiel\n\
                    Name[fr]=Exemple\n";
        let m = parse_desktop(text);
        assert_eq!(m.get("Name"), Some(&"Example".to_string()));
        assert!(!m.keys().any(|k| k.contains('[')));
    }

    #[test]
    fn parse_desktop_honours_first_section_only() {
        let text = "[Desktop Entry]\nName=Main\nExec=/a\n\
                    [Desktop Action Open]\nName=Action\nExec=/b\n";
        let m = parse_desktop(text);
        assert_eq!(m.get("Exec"), Some(&"/a".to_string()));
    }

    #[test]
    fn parse_desktop_skips_comments_and_blanks() {
        let text = "# leading comment\n\
                    [Desktop Entry]\n\
                    # in-section comment\n\
                    \n\
                    Name=X\n";
        let m = parse_desktop(text);
        assert_eq!(m.get("Name"), Some(&"X".to_string()));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn parse_desktop_unescapes_strings() {
        let text = "[Desktop Entry]\nComment=line1\\nline2\\ttab\\\\bs\n";
        let m = parse_desktop(text);
        assert_eq!(m.get("Comment"), Some(&"line1\nline2\ttab\\bs".to_string()));
    }

    // ---------- is_autostart_enabled ----------

    #[test]
    fn autostart_enabled_is_true_by_default() {
        let m = parse_desktop("[Desktop Entry]\nType=Application\nName=X\nExec=/x\n");
        assert!(is_autostart_enabled(&m));
    }

    #[test]
    fn autostart_hidden_true_disables() {
        let m = parse_desktop(
            "[Desktop Entry]\nType=Application\nName=X\nExec=/x\nHidden=true\n",
        );
        assert!(!is_autostart_enabled(&m));
    }

    #[test]
    fn autostart_gnome_false_disables() {
        let m = parse_desktop(
            "[Desktop Entry]\nType=Application\nName=X\nExec=/x\nX-GNOME-Autostart-enabled=false\n",
        );
        assert!(!is_autostart_enabled(&m));
    }

    #[test]
    fn autostart_non_application_type_disables() {
        let m = parse_desktop("[Desktop Entry]\nType=Link\nName=X\nURL=http://x\n");
        assert!(!is_autostart_enabled(&m));
    }

    #[test]
    fn autostart_nodisplay_does_not_disable() {
        // NoDisplay = launcher visibility only, spec says it still autostarts
        let m = parse_desktop(
            "[Desktop Entry]\nType=Application\nName=X\nExec=/x\nNoDisplay=true\n",
        );
        assert!(is_autostart_enabled(&m));
    }

    // ---------- list_autostart ----------

    #[test]
    fn list_autostart_returns_sorted_items() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let autostart = home.join(".config/autostart");
        fs::create_dir_all(&autostart).unwrap();
        write(
            &autostart.join("zed.desktop"),
            "[Desktop Entry]\nType=Application\nName=Zed\nExec=/usr/bin/zed\n",
        );
        write(
            &autostart.join("alpha.desktop"),
            "[Desktop Entry]\nType=Application\nName=Alpha\nExec=/usr/bin/alpha\nHidden=true\n",
        );
        let items = list_autostart(home);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "Alpha");
        assert_eq!(items[1].name, "Zed");
        assert!(!items[0].enabled);
        assert!(items[1].enabled);
        for i in &items {
            assert_eq!(i.source, StartupSource::LinuxAutostart);
            assert!(i.is_user);
        }
    }

    #[test]
    fn list_autostart_ignores_non_desktop_files() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let autostart = home.join(".config/autostart");
        fs::create_dir_all(&autostart).unwrap();
        write(&autostart.join("not.txt"), "hello");
        write(&autostart.join("a.desktop.bak"), "junk");
        write(
            &autostart.join("real.desktop"),
            "[Desktop Entry]\nType=Application\nName=Real\nExec=/x\n",
        );
        let items = list_autostart(home);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Real");
    }

    #[test]
    fn list_autostart_ignores_symlinks() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let autostart = home.join(".config/autostart");
        fs::create_dir_all(&autostart).unwrap();
        let real = home.join("elsewhere.desktop");
        write(
            &real,
            "[Desktop Entry]\nType=Application\nName=Linked\nExec=/x\n",
        );
        let link = autostart.join("sneaky.desktop");
        unix_fs::symlink(&real, &link).unwrap();
        let items = list_autostart(home);
        assert!(
            items.iter().all(|i| i.name != "Linked"),
            "symlink leaked into list: {items:?}",
        );
    }

    #[test]
    fn list_autostart_empty_dir_returns_empty() {
        let dir = TempDir::new().unwrap();
        let items = list_autostart(dir.path());
        assert!(items.is_empty());
    }

    #[test]
    fn list_autostart_malformed_file_falls_back_to_filename() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let autostart = home.join(".config/autostart");
        fs::create_dir_all(&autostart).unwrap();
        write(&autostart.join("weird.desktop"), "completely broken content");
        let items = list_autostart(home);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "weird");
        // no [Desktop Entry] = no Type = non-Application = disabled. safe default.
        assert!(!items[0].enabled);
    }

    // ---------- rewrite_desktop_hidden ----------

    #[test]
    fn rewrite_sets_hidden_when_missing() {
        let input = "[Desktop Entry]\nType=Application\nName=X\nExec=/x\n";
        let out = rewrite_desktop_hidden(input, true);
        assert!(out.contains("Hidden=true"));
        let m = parse_desktop(&out);
        assert!(!is_autostart_enabled(&m));
    }

    #[test]
    fn rewrite_flips_existing_hidden() {
        let input = "[Desktop Entry]\nType=Application\nName=X\nExec=/x\nHidden=true\n";
        let out = rewrite_desktop_hidden(input, false);
        assert!(out.contains("Hidden=false"));
        assert!(!out.contains("Hidden=true"));
        let m = parse_desktop(&out);
        assert!(is_autostart_enabled(&m));
    }

    #[test]
    fn rewrite_coerces_gnome_key_to_agree() {
        // both keys existing = must not disagree after toggle
        let input = "[Desktop Entry]\nType=Application\nName=X\nExec=/x\nX-GNOME-Autostart-enabled=true\n";
        let out = rewrite_desktop_hidden(input, true);
        let m = parse_desktop(&out);
        assert!(!is_autostart_enabled(&m));
        assert_eq!(m.get("X-GNOME-Autostart-enabled"), Some(&"false".to_string()));
        assert_eq!(m.get("Hidden"), Some(&"true".to_string()));
    }

    #[test]
    fn rewrite_preserves_other_keys() {
        let input = "[Desktop Entry]\n\
                     Type=Application\n\
                     Name=X\n\
                     Exec=/x\n\
                     Comment=Some comment\n\
                     X-CustomVendor=preserved\n";
        let out = rewrite_desktop_hidden(input, true);
        let m = parse_desktop(&out);
        assert_eq!(m.get("Comment"), Some(&"Some comment".to_string()));
        assert_eq!(m.get("X-CustomVendor"), Some(&"preserved".to_string()));
    }

    #[test]
    fn rewrite_preserves_trailing_newline() {
        let with_nl = "[Desktop Entry]\nType=Application\nName=X\nExec=/x\n";
        let without_nl = "[Desktop Entry]\nType=Application\nName=X\nExec=/x";
        assert!(rewrite_desktop_hidden(with_nl, true).ends_with('\n'));
        assert!(!rewrite_desktop_hidden(without_nl, true).ends_with('\n'));
    }

    // ---------- toggle_autostart ----------

    #[test]
    fn toggle_autostart_round_trips() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("app.desktop");
        write(
            &path,
            "[Desktop Entry]\nType=Application\nName=App\nExec=/app\n",
        );
        toggle_autostart(&path, false).unwrap();
        let items_off = list_autostart_for_dir(&path);
        assert!(!items_off.enabled);
        toggle_autostart(&path, true).unwrap();
        let items_on = list_autostart_for_dir(&path);
        assert!(items_on.enabled);
    }

    /// read a single .desktop into a StartupItem same way list_autostart does.
    /// test helper, keeps toggle tests focused.
    fn list_autostart_for_dir(path: &Path) -> StartupItem {
        let text = fs::read_to_string(path).unwrap();
        let parsed = parse_desktop(&text);
        let name = parsed.get("Name").cloned().unwrap_or_default();
        let enabled = is_autostart_enabled(&parsed);
        StartupItem {
            id: make_item_id(StartupSource::LinuxAutostart, &name),
            name,
            description: parsed.get("Comment").cloned().unwrap_or_default(),
            command: parsed.get("Exec").cloned().unwrap_or_default(),
            source: StartupSource::LinuxAutostart,
            path: path_for_wire(&path.to_path_buf()),
            enabled,
            is_user: true,
            icon: "power".into(),
            impact: impact_for_command(parsed.get("Exec").map(String::as_str).unwrap_or("")),
        }
    }

    #[test]
    fn toggle_autostart_is_atomic_against_concurrent_read() {
        // tmp+rename means a concurrent reader sees either full old or full new,
        // never half-written. fire toggle while a reader hammers in a loop and
        // verify every observed content is parseable.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("app.desktop");
        write(
            &path,
            "[Desktop Entry]\nType=Application\nName=App\nExec=/app\n",
        );

        let p = path.clone();
        let handle = std::thread::spawn(move || {
            for _ in 0..200 {
                if let Ok(text) = fs::read_to_string(&p) {
                    let m = parse_desktop(&text);
                    // Name always present. half-written file would sometimes miss it.
                    assert_eq!(m.get("Name"), Some(&"App".to_string()));
                }
            }
        });
        for i in 0..100 {
            toggle_autostart(&path, i % 2 == 0).unwrap();
        }
        handle.join().unwrap();
    }

    // ---------- systemd ----------

    #[test]
    fn parse_systemd_unit_picks_description_and_execstart() {
        let text = "[Unit]\nDescription=Run my thing\nAfter=network.target\n\
                    [Service]\nType=simple\nExecStart=/usr/bin/myd --flag\n\
                    [Install]\nWantedBy=default.target\n";
        let (desc, exec) = parse_systemd_unit(text);
        assert_eq!(desc, "Run my thing");
        assert_eq!(exec, "/usr/bin/myd --flag");
    }

    #[test]
    fn parse_systemd_strips_prefix_markers() {
        // systemd allows `ExecStart=-/bin/foo` (ignore failure) and
        // `ExecStart=@/bin/foo` (argv0 override). strip so UI shows the real command.
        let text = "[Service]\nExecStart=-/bin/foo --x\n";
        let (_, exec) = parse_systemd_unit(text);
        assert_eq!(exec, "/bin/foo --x");

        let text = "[Service]\nExecStart=@/bin/foo argv0\n";
        let (_, exec) = parse_systemd_unit(text);
        assert_eq!(exec, "/bin/foo argv0");
    }

    #[test]
    fn list_systemd_user_detects_enabled_via_symlink() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let ud = systemd_user_dir(home);
        fs::create_dir_all(&ud).unwrap();
        let unit = ud.join("syncthing.service");
        write(
            &unit,
            "[Unit]\nDescription=Syncthing\n[Service]\nExecStart=/usr/bin/syncthing\n",
        );
        // no symlink = disabled
        let items = list_systemd_user(home);
        assert_eq!(items.len(), 1);
        assert!(!items[0].enabled);

        // enable via wants symlink
        let wants = systemd_wants_dir(home);
        fs::create_dir_all(&wants).unwrap();
        unix_fs::symlink(&unit, wants.join("syncthing.service")).unwrap();
        let items2 = list_systemd_user(home);
        assert!(items2[0].enabled);
    }

    #[test]
    fn list_systemd_user_accepts_service_socket_timer() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let ud = systemd_user_dir(home);
        fs::create_dir_all(&ud).unwrap();
        for (name, body) in [
            ("a.service", "[Service]\nExecStart=/a\n"),
            ("b.socket", "[Socket]\nListenStream=/tmp/s\n"),
            ("c.timer", "[Timer]\nOnUnitActiveSec=1h\n"),
            ("d.mount", "[Mount]\nWhat=/dev/x\n"),
            ("e.target", "[Unit]\nDescription=t\n"),
        ] {
            write(&ud.join(name), body);
        }
        let items = list_systemd_user(home);
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"a.service"));
        assert!(names.contains(&"b.socket"));
        assert!(names.contains(&"c.timer"));
        assert!(!names.contains(&"d.mount"));
        assert!(!names.contains(&"e.target"));
    }

    #[test]
    fn toggle_systemd_user_round_trips() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let ud = systemd_user_dir(home);
        fs::create_dir_all(&ud).unwrap();
        let unit = ud.join("x.service");
        write(&unit, "[Service]\nExecStart=/x\n");

        toggle_systemd_user(home, "x.service", true).unwrap();
        assert!(systemd_wants_dir(home).join("x.service").is_symlink());
        let items = list_systemd_user(home);
        assert!(items[0].enabled);

        // idempotent
        toggle_systemd_user(home, "x.service", true).unwrap();
        toggle_systemd_user(home, "x.service", false).unwrap();
        assert!(!systemd_wants_dir(home).join("x.service").exists());

        // idempotent
        toggle_systemd_user(home, "x.service", false).unwrap();
    }

    #[test]
    fn toggle_systemd_rejects_traversal_and_missing_unit() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        fs::create_dir_all(systemd_user_dir(home)).unwrap();
        assert!(toggle_systemd_user(home, "../evil", true).is_err());
        assert!(toggle_systemd_user(home, "a/b.service", true).is_err());
        assert!(toggle_systemd_user(home, "", true).is_err());
        assert!(toggle_systemd_user(home, "missing.service", true).is_err());
    }
}
