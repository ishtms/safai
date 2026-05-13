//! mac launchd enumerator + toggle.
//!
//! launchd dirs we care about:
//! - `~/Library/LaunchAgents/` - per-user, read-write. toggle flips `Disabled`.
//! - `/Library/LaunchAgents/`  - system-installed user-scope (Adobe updater etc).
//!   read-only, we enumerate but toggle returns PermissionDenied.
//! - `/Library/LaunchDaemons/` - root-scope. read-only.
//!
//! # plist parsing
//!
//! launch agents can be XML or binary plists. Enumeration uses the `plist`
//! crate so both formats are parsed through the same typed path. The legacy
//! XML helpers below remain for targeted rewrite tests and comments, but the
//! production listing/toggle path is not string-based.
//!
//! Shape we care about:
//!
//! ```text
//! <plist version="1.0">
//!     <dict>
//!         <key>Label</key>         <string>com.example.foo</string>
//!         <key>ProgramArguments</key>
//!         <array>
//!             <string>/usr/local/bin/foo</string>
//!             <string>--serve</string>
//!         </array>
//!         <key>Program</key>       <string>/path/to/binary</string>
//!         <key>Disabled</key>      <true/>
//!     </dict>
//! </plist>
//! ```
//!
//! anything else (missing keys, bad data, wrong root) is rejected gracefully;
//! owner gets zero-or-skipped, never a scan crash.
//!
//! # toggle
//!
//! `Disabled=true` is load-bearing, launchd honours it at login. `launchctl
//! disable` writes to an override db (`/var/db/com.apple.xpc.launchd/...`) we
//! can't touch without root on sealed volumes, but editing the plist's Disabled
//! works for per-user agents without extra privs and survives reboots.

use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use super::types::{impact_for_command, make_item_id, path_for_wire, StartupItem, StartupSource};

pub fn user_launch_agents(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
}
pub fn system_launch_agents() -> PathBuf {
    PathBuf::from("/Library/LaunchAgents")
}
pub fn system_launch_daemons() -> PathBuf {
    PathBuf::from("/Library/LaunchDaemons")
}

// ---------------- enumeration ----------------

/// sorted by label for stable wire order
pub fn list_user_agents(home: &Path) -> Vec<StartupItem> {
    list_plist_dir(
        &user_launch_agents(home),
        StartupSource::MacLaunchAgentUser,
        true,
    )
}

/// read-only, items have is_user=false
pub fn list_system_agents() -> Vec<StartupItem> {
    list_plist_dir(
        &system_launch_agents(),
        StartupSource::MacLaunchAgentSystem,
        false,
    )
}

pub fn list_launch_daemons() -> Vec<StartupItem> {
    list_plist_dir(
        &system_launch_daemons(),
        StartupSource::MacLaunchDaemon,
        false,
    )
}

/// shared worker. public for the orchestrator + test-on-linux.
pub fn list_plist_dir(dir: &Path, source: StartupSource, is_user: bool) -> Vec<StartupItem> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut items: Vec<StartupItem> = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("plist") {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.file_type().is_file() {
            continue;
        }
        let Some(parsed) = parse_plist_file(&path) else {
            continue;
        };

        let name = parsed
            .label
            .clone()
            .or_else(|| path.file_stem().and_then(|s| s.to_str()).map(str::to_owned))
            .unwrap_or_default();
        let command = parsed.command();
        let description = parsed.label.clone().unwrap_or_default();
        let enabled = !parsed.disabled;

        let id = make_item_id(source, &name);
        items.push(StartupItem {
            id,
            name,
            description,
            command: command.clone(),
            source,
            path: path_for_wire(&path),
            enabled,
            is_user,
            icon: match source {
                StartupSource::MacLaunchDaemon => "shield2",
                _ => "power",
            }
            .to_string(),
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

// ---------------- toggle ----------------

/// flip Disabled. atomic write, preserves other keys.
pub fn toggle_user_agent(path: &Path, enabled: bool) -> Result<(), String> {
    let mut value = plist::Value::from_file(path).map_err(|e| format!("read plist: {e}"))?;
    set_plist_disabled(&mut value, !enabled)
        .ok_or_else(|| format!("plist has no dictionary body: {}", path.display()))?;
    atomic_write_plist(path, &value).map_err(|e| format!("write plist: {e}"))
}

// ---------------- plist parsing ----------------

/// minimal subset. ignores everything launchd needs to actually run the agent,
/// UI only wants label + command + disabled.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedPlist {
    pub label: Option<String>,
    /// first <string> inside ProgramArguments, or the Program key value when
    /// ProgramArguments is absent.
    pub program: Option<String>,
    /// remaining ProgramArguments entries, for "full command" in UI
    pub args: Vec<String>,
    pub disabled: bool,
}

impl ParsedPlist {
    /// cmd line for the UI. program + args space-joined, launchd doesn't shell-escape either.
    pub fn command(&self) -> String {
        match &self.program {
            Some(p) if self.args.is_empty() => p.clone(),
            Some(p) => {
                let mut out = String::with_capacity(p.len() + 32);
                out.push_str(p);
                for a in &self.args {
                    out.push(' ');
                    out.push_str(a);
                }
                out
            }
            None => String::new(),
        }
    }
}

fn parse_plist_file(path: &Path) -> Option<ParsedPlist> {
    let value = plist::Value::from_file(path).ok()?;
    parse_plist_value(&value)
}

fn parse_plist_value(value: &plist::Value) -> Option<ParsedPlist> {
    let plist::Value::Dictionary(dict) = value else {
        return None;
    };

    let mut out = ParsedPlist::default();
    out.label = dict.get("Label").and_then(plist_string);
    if let Some(program) = dict.get("Program").and_then(plist_string) {
        out.program = Some(program);
    }
    if let Some(args) = dict.get("ProgramArguments").and_then(plist_string_array) {
        let mut it = args.into_iter();
        if let Some(first) = it.next() {
            out.program = Some(first);
            out.args = it.collect();
        }
    }
    out.disabled = dict.get("Disabled").and_then(plist_bool).unwrap_or(false);

    Some(out)
}

fn plist_string(value: &plist::Value) -> Option<String> {
    match value {
        plist::Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn plist_bool(value: &plist::Value) -> Option<bool> {
    match value {
        plist::Value::Boolean(b) => Some(*b),
        _ => None,
    }
}

fn plist_string_array(value: &plist::Value) -> Option<Vec<String>> {
    let plist::Value::Array(values) = value else {
        return None;
    };
    let mut out = Vec::new();
    for value in values {
        if let Some(s) = plist_string(value) {
            out.push(s);
        }
    }
    Some(out)
}

fn set_plist_disabled(value: &mut plist::Value, disabled: bool) -> Option<()> {
    let plist::Value::Dictionary(dict) = value else {
        return None;
    };
    dict.insert("Disabled".into(), plist::Value::Boolean(disabled));
    Some(())
}

/// returns None if no <dict> (malformed XML, wrong root). never panics.
/// not a general plist parser, only knows keys launch agents use. nested dicts
/// other than EnvironmentVariables ignored gracefully.
pub fn parse_plist(text: &str) -> Option<ParsedPlist> {
    if let Ok(value) = plist::Value::from_reader(Cursor::new(text.as_bytes())) {
        if let Some(parsed) = parse_plist_value(&value) {
            return Some(parsed);
        }
    }

    let dict_start = text.find("<dict>")?;
    let dict_end = text.rfind("</dict>")?;
    if dict_end <= dict_start {
        return None;
    }
    let body = &text[dict_start + "<dict>".len()..dict_end];

    let mut out = ParsedPlist::default();
    let mut cursor = 0usize;
    // nesting tracker so <dict>/<array> values for uninteresting keys (e.g.
    // EnvironmentVariables) don't confuse the key-value matcher.
    while let Some(key_offset) = find_tag(&body[cursor..], "key") {
        let abs = cursor + key_offset;
        let (key_text, after_key) = read_tag_body(body, abs, "key")?;
        cursor = after_key;
        // skip ws to the value tag
        let val_start = skip_ws(body, cursor);
        if val_start >= body.len() {
            break;
        }
        match key_text.as_str() {
            "Label" => {
                if let Some((s, next)) = read_string(body, val_start) {
                    out.label = Some(s);
                    cursor = next;
                } else {
                    cursor = skip_value(body, val_start);
                }
            }
            "Program" => {
                if let Some((s, next)) = read_string(body, val_start) {
                    if out.program.is_none() {
                        out.program = Some(s);
                    }
                    cursor = next;
                } else {
                    cursor = skip_value(body, val_start);
                }
            }
            "ProgramArguments" => {
                if let Some((strings, next)) = read_string_array(body, val_start) {
                    let mut it = strings.into_iter();
                    if let Some(first) = it.next() {
                        // ProgramArguments wins over Program, launchd's precedence
                        out.program = Some(first);
                        out.args = it.collect();
                    }
                    cursor = next;
                } else {
                    cursor = skip_value(body, val_start);
                }
            }
            "Disabled" => {
                if let Some((b, next)) = read_bool(body, val_start) {
                    out.disabled = b;
                    cursor = next;
                } else {
                    cursor = skip_value(body, val_start);
                }
            }
            _ => {
                cursor = skip_value(body, val_start);
            }
        }
    }

    Some(out)
}

/// rewrites so <key>Disabled</key> matches `disabled`. None if no top-level
/// <dict>. preserves other keys + ordering + comments + unknown keys.
pub fn rewrite_plist_disabled(text: &str, disabled: bool) -> Option<String> {
    let dict_start = text.find("<dict>")?;
    let dict_end = text.rfind("</dict>")?;
    if dict_end <= dict_start {
        return None;
    }
    let prefix = &text[..dict_start + "<dict>".len()];
    let body = &text[dict_start + "<dict>".len()..dict_end];
    let suffix = &text[dict_end..];

    // find existing <key>Disabled</key>
    let lower = body.to_ascii_lowercase();
    let new_body = if let Some(pos) = find_disabled_key(&lower, body) {
        // replace the following <true/>/<false/>
        let after_key = pos + "<key>Disabled</key>".len();
        let val_start = skip_ws(body, after_key);
        let new_tag = if disabled { "<true/>" } else { "<false/>" };
        let end = if let Some((_, end)) = read_bool_span(body, val_start) {
            end
        } else {
            // malformed, drop in a fresh one
            val_start
        };
        let mut out = String::with_capacity(body.len() + 16);
        out.push_str(&body[..val_start]);
        out.push_str(new_tag);
        out.push_str(&body[end..]);
        out
    } else {
        // append <key>Disabled</key><true/> before closing dict. newline + 2-space
        // indent matches `plutil -convert xml1` formatting.
        let mut body = body.to_owned();
        // trim trailing whitespace so the injection sits cleanly before </dict>
        while body.ends_with(|c: char| c == ' ' || c == '\t') {
            body.pop();
        }
        let newline = if body.ends_with('\n') { "" } else { "\n" };
        body.push_str(newline);
        body.push_str(&format!(
            "    <key>Disabled</key>\n    {}\n",
            if disabled { "<true/>" } else { "<false/>" }
        ));
        body
    };

    Some(format!("{prefix}{new_body}{suffix}"))
}

// ---------------- low-level tag helpers ----------------

fn find_tag(hay: &str, tag: &str) -> Option<usize> {
    let needle = format!("<{tag}>");
    hay.find(&needle)
}

fn find_disabled_key(lower: &str, body: &str) -> Option<usize> {
    // plist keys are case-sensitive but we want to be robust to third-party
    // scripts writing <key>disabled</key>. prefer canonical, fall back to lower.
    // returns index into original body.
    if let Some(p) = body.find("<key>Disabled</key>") {
        return Some(p);
    }
    let needle = "<key>disabled</key>";
    let idx = lower.find(needle)?;
    // confirm length matches so subsequent slicing stays in bounds
    if body[idx..].to_ascii_lowercase().starts_with(needle) {
        Some(idx)
    } else {
        None
    }
}

fn read_tag_body<'a>(hay: &'a str, abs_offset: usize, tag: &str) -> Option<(String, usize)> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = abs_offset + open.len();
    let rest = &hay[start..];
    let end_rel = rest.find(&close)?;
    let raw = &rest[..end_rel];
    Some((decode_entities(raw.trim()), start + end_rel + close.len()))
}

fn read_string(hay: &str, offset: usize) -> Option<(String, usize)> {
    let after_ws = skip_ws(hay, offset);
    let rest = &hay[after_ws..];
    if rest.starts_with("<string/>") {
        return Some((String::new(), after_ws + "<string/>".len()));
    }
    if !rest.starts_with("<string>") {
        return None;
    }
    let body_start = after_ws + "<string>".len();
    let body = &hay[body_start..];
    let end_rel = body.find("</string>")?;
    let text = decode_entities(&body[..end_rel]);
    Some((text, body_start + end_rel + "</string>".len()))
}

fn read_bool(hay: &str, offset: usize) -> Option<(bool, usize)> {
    let after_ws = skip_ws(hay, offset);
    let rest = &hay[after_ws..];
    if rest.starts_with("<true/>") {
        return Some((true, after_ws + "<true/>".len()));
    }
    if rest.starts_with("<false/>") {
        return Some((false, after_ws + "<false/>".len()));
    }
    // older dialects also accept <true></true>/<false></false>
    if rest.starts_with("<true>") {
        let len = "<true></true>".len();
        if rest.starts_with("<true></true>") {
            return Some((true, after_ws + len));
        }
    }
    if rest.starts_with("<false>") {
        let len = "<false></false>".len();
        if rest.starts_with("<false></false>") {
            return Some((false, after_ws + len));
        }
    }
    None
}

fn read_bool_span(hay: &str, offset: usize) -> Option<(bool, usize)> {
    read_bool(hay, offset)
}

fn read_string_array(hay: &str, offset: usize) -> Option<(Vec<String>, usize)> {
    let after_ws = skip_ws(hay, offset);
    let rest = &hay[after_ws..];
    if rest.starts_with("<array/>") {
        return Some((Vec::new(), after_ws + "<array/>".len()));
    }
    if !rest.starts_with("<array>") {
        return None;
    }
    let mut cursor = after_ws + "<array>".len();
    let mut out = Vec::new();
    loop {
        let start = skip_ws(hay, cursor);
        let rest2 = &hay[start..];
        if rest2.starts_with("</array>") {
            return Some((out, start + "</array>".len()));
        }
        if rest2.starts_with("<string>") || rest2.starts_with("<string/>") {
            let (s, next) = read_string(hay, start)?;
            out.push(s);
            cursor = next;
            continue;
        }
        // unknown element, skip one defensively. no progress = bail.
        let next = skip_value(hay, start);
        if next == start {
            return None;
        }
        cursor = next;
    }
}

fn skip_ws(hay: &str, offset: usize) -> usize {
    let bytes = hay.as_bytes();
    let mut i = offset;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b' ' || b == b'\n' || b == b'\r' || b == b'\t' {
            i += 1;
        } else {
            break;
        }
    }
    i
}

/// skip one value. handles self-closing (<true/>) + paired (<string>...</string>)
/// with nesting. deliberately minimal, escape hatch for uninteresting keys.
/// no sensible skip = returns offset unchanged (caller guards infinite loops).
fn skip_value(hay: &str, offset: usize) -> usize {
    let start = skip_ws(hay, offset);
    let rest = &hay[start..];
    if rest.is_empty() {
        return start;
    }
    // self-closing element
    if let Some(end) = rest.find("/>") {
        let tag_end = end + 2;
        // only if no `<` before `/>` (so it's actually self-closing, not a
        // nested `<dict>\n<key.../>`).
        let prefix = &rest[..end];
        if !prefix.contains('<') {
            return start + tag_end;
        }
    }
    // paired element
    if !rest.starts_with('<') {
        return start;
    }
    let tag_end = match rest.find('>') {
        Some(p) => p,
        None => return start,
    };
    let tag_name = rest[1..tag_end].split_whitespace().next().unwrap_or("");
    if tag_name.is_empty() {
        return start;
    }
    let open = format!("<{tag_name}>");
    let close = format!("</{tag_name}>");
    // walk forward counting depth
    let mut depth = 1usize;
    let mut cursor = start + tag_end + 1;
    while cursor < hay.len() && depth > 0 {
        let slice = &hay[cursor..];
        let next_open = slice.find(&open);
        let next_close = slice.find(&close);
        match (next_open, next_close) {
            (_, None) => return start, // malformed
            (Some(o), Some(c)) if o < c => {
                depth += 1;
                cursor += o + open.len();
            }
            (_, Some(c)) => {
                depth -= 1;
                cursor += c + close.len();
            }
        }
    }
    cursor
}

fn decode_entities(s: &str) -> String {
    // agents rarely embed entities but handle &amp;/&lt;/&gt;/&quot;/&apos; defensively
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'&' {
            out.push(bytes[i] as char);
            i += 1;
            continue;
        }
        let rest = &s[i..];
        if let Some(end) = rest.find(';') {
            let ent = &rest[1..end];
            match ent {
                "amp" => out.push('&'),
                "lt" => out.push('<'),
                "gt" => out.push('>'),
                "quot" => out.push('"'),
                "apos" => out.push('\''),
                _ => out.push_str(&rest[..=end]),
            }
            i += end + 1;
        } else {
            out.push('&');
            i += 1;
        }
    }
    out
}

fn atomic_write(path: &Path, contents: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("launch-agent");
    let tmp = dir.join(format!(".{file_name}.safai-tmp"));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

fn atomic_write_plist(path: &Path, value: &plist::Value) -> std::io::Result<()> {
    let mut bytes = Vec::new();
    value
        .to_writer_xml(&mut bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    atomic_write_bytes(path, &bytes)
}

fn atomic_write_bytes(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("launch-agent");
    let tmp = dir.join(format!(".{file_name}.safai-tmp"));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(contents)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    fn write_binary_plist(path: &Path, label: &str, disabled: bool) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut dict = plist::Dictionary::new();
        dict.insert("Label".into(), plist::Value::String(label.to_string()));
        dict.insert(
            "ProgramArguments".into(),
            plist::Value::Array(vec![
                plist::Value::String("/usr/local/bin/foo".to_string()),
                plist::Value::String("--serve".to_string()),
            ]),
        );
        dict.insert("Disabled".into(), plist::Value::Boolean(disabled));
        let value = plist::Value::Dictionary(dict);
        let mut file = fs::File::create(path).unwrap();
        value.to_writer_binary(&mut file).unwrap();
    }

    fn sample_plist(label: &str, disabled: bool) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/foo</string>
        <string>--serve</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>Disabled</key>
    {bool}
</dict>
</plist>
"#,
            bool = if disabled { "<true/>" } else { "<false/>" },
        )
    }

    // ---------- parse_plist ----------

    #[test]
    fn parse_plist_extracts_label_and_program_arguments() {
        let p = parse_plist(&sample_plist("com.example.foo", false)).unwrap();
        assert_eq!(p.label.as_deref(), Some("com.example.foo"));
        assert_eq!(p.program.as_deref(), Some("/usr/local/bin/foo"));
        assert_eq!(p.args, vec!["--serve".to_string()]);
        assert!(!p.disabled);
        assert_eq!(p.command(), "/usr/local/bin/foo --serve");
    }

    #[test]
    fn parse_plist_honours_disabled_true() {
        let p = parse_plist(&sample_plist("com.example.foo", true)).unwrap();
        assert!(p.disabled);
    }

    #[test]
    fn parse_plist_falls_back_to_program_key() {
        let text = r#"<plist version="1.0"><dict>
            <key>Label</key><string>x</string>
            <key>Program</key><string>/opt/app</string>
        </dict></plist>"#;
        let p = parse_plist(text).unwrap();
        assert_eq!(p.program.as_deref(), Some("/opt/app"));
        assert!(p.args.is_empty());
        assert_eq!(p.command(), "/opt/app");
    }

    #[test]
    fn parse_plist_program_arguments_trump_program() {
        // both present = launchd runs ProgramArguments
        let text = r#"<plist><dict>
            <key>Label</key><string>x</string>
            <key>Program</key><string>/opt/ignored</string>
            <key>ProgramArguments</key>
            <array><string>/opt/real</string><string>-a</string></array>
        </dict></plist>"#;
        let p = parse_plist(text).unwrap();
        assert_eq!(p.program.as_deref(), Some("/opt/real"));
        assert_eq!(p.args, vec!["-a".to_string()]);
    }

    #[test]
    fn parse_plist_returns_none_on_invalid_binary_plist() {
        // invalid bplist00 magic, can't read, don't crash
        let bytes = b"bplist00\x01\x02\x03";
        // only need parse_plist to reject the magic-prefixed text
        let text = String::from_utf8_lossy(bytes).into_owned();
        assert!(parse_plist(&text).is_none());
    }

    #[test]
    fn parse_plist_rejects_malformed() {
        assert!(parse_plist("not xml at all").is_none());
        assert!(parse_plist("<plist></plist>").is_none());
    }

    #[test]
    fn parse_plist_handles_unknown_keys() {
        // skip nested EnvironmentVariables dict gracefully
        let text = r#"<plist><dict>
            <key>Label</key><string>x</string>
            <key>EnvironmentVariables</key>
            <dict>
                <key>PATH</key><string>/usr/bin</string>
                <key>FOO</key><string>bar</string>
            </dict>
            <key>Program</key><string>/opt/app</string>
            <key>Disabled</key><true/>
        </dict></plist>"#;
        let p = parse_plist(text).unwrap();
        assert_eq!(p.label.as_deref(), Some("x"));
        assert_eq!(p.program.as_deref(), Some("/opt/app"));
        assert!(p.disabled);
    }

    #[test]
    fn parse_plist_decodes_entities() {
        let text = r#"<plist><dict>
            <key>Label</key><string>A &amp; B</string>
        </dict></plist>"#;
        let p = parse_plist(text).unwrap();
        assert_eq!(p.label.as_deref(), Some("A & B"));
    }

    // ---------- rewrite_plist_disabled ----------

    #[test]
    fn rewrite_flips_existing_disabled() {
        let input = sample_plist("a", false);
        let out = rewrite_plist_disabled(&input, true).unwrap();
        let p = parse_plist(&out).unwrap();
        assert!(p.disabled);
        // roundtrip: flip back gives original semantics
        let back = rewrite_plist_disabled(&out, false).unwrap();
        let p2 = parse_plist(&back).unwrap();
        assert!(!p2.disabled);
    }

    #[test]
    fn rewrite_adds_disabled_when_missing() {
        let input = r#"<plist><dict>
            <key>Label</key><string>x</string>
            <key>ProgramArguments</key><array><string>/x</string></array>
        </dict></plist>"#;
        let out = rewrite_plist_disabled(input, true).unwrap();
        let p = parse_plist(&out).unwrap();
        assert!(p.disabled);
        assert_eq!(p.label.as_deref(), Some("x"));
    }

    #[test]
    fn rewrite_preserves_other_keys() {
        let input = sample_plist("keep.me", false);
        let out = rewrite_plist_disabled(&input, true).unwrap();
        assert!(out.contains("<string>keep.me</string>"));
        assert!(out.contains("<string>/usr/local/bin/foo</string>"));
        assert!(out.contains("<string>--serve</string>"));
        assert!(out.contains("RunAtLoad"));
    }

    #[test]
    fn rewrite_returns_none_without_dict() {
        assert!(rewrite_plist_disabled("not xml", true).is_none());
        assert!(rewrite_plist_disabled("<plist></plist>", false).is_none());
    }

    // ---------- list_plist_dir ----------

    #[test]
    fn list_plist_dir_sorts_and_populates() {
        let dir = TempDir::new().unwrap();
        let agents = dir.path().join("Library/LaunchAgents");
        fs::create_dir_all(&agents).unwrap();
        write(
            &agents.join("com.z.example.plist"),
            &sample_plist("com.z.example", false),
        );
        write(
            &agents.join("com.a.example.plist"),
            &sample_plist("com.a.example", true),
        );
        let items = list_plist_dir(&agents, StartupSource::MacLaunchAgentUser, true);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "com.a.example");
        assert!(!items[0].enabled);
        assert_eq!(items[1].name, "com.z.example");
        assert!(items[1].enabled);
        for i in &items {
            assert_eq!(i.source, StartupSource::MacLaunchAgentUser);
            assert!(i.is_user);
        }
    }

    #[test]
    fn list_plist_dir_parses_xml_and_binary_plists() {
        let dir = TempDir::new().unwrap();
        let agents = dir.path().join("Library/LaunchAgents");
        fs::create_dir_all(&agents).unwrap();
        write(&agents.join("a.plist"), &sample_plist("a", false));
        write(&agents.join("notes.txt"), "no");
        write_binary_plist(&agents.join("binary.plist"), "binary.agent", true);
        let items = list_plist_dir(&agents, StartupSource::MacLaunchAgentUser, true);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "a");
        assert_eq!(items[1].name, "binary.agent");
        assert!(!items[1].enabled);
    }

    #[cfg(unix)]
    #[test]
    fn list_plist_dir_skips_symlinks() {
        let dir = TempDir::new().unwrap();
        let agents = dir.path().join("Library/LaunchAgents");
        fs::create_dir_all(&agents).unwrap();
        let real = dir.path().join("target.plist");
        write(&real, &sample_plist("sneaky", false));
        std::os::unix::fs::symlink(&real, agents.join("link.plist")).unwrap();
        let items = list_plist_dir(&agents, StartupSource::MacLaunchAgentUser, true);
        assert!(items.iter().all(|i| i.name != "sneaky"));
    }

    #[test]
    fn list_plist_dir_missing_dir_returns_empty() {
        let dir = TempDir::new().unwrap();
        let items = list_plist_dir(
            &dir.path().join("nope"),
            StartupSource::MacLaunchAgentUser,
            true,
        );
        assert!(items.is_empty());
    }

    // ---------- toggle_user_agent ----------

    #[test]
    fn toggle_user_agent_round_trips() {
        let dir = TempDir::new().unwrap();
        let agents = dir.path().join("Library/LaunchAgents");
        fs::create_dir_all(&agents).unwrap();
        let plist = agents.join("com.example.toggle.plist");
        write(&plist, &sample_plist("com.example.toggle", false));

        toggle_user_agent(&plist, false).unwrap();
        let items = list_plist_dir(&agents, StartupSource::MacLaunchAgentUser, true);
        assert_eq!(items[0].enabled, false);
        toggle_user_agent(&plist, true).unwrap();
        let items2 = list_plist_dir(&agents, StartupSource::MacLaunchAgentUser, true);
        assert!(items2[0].enabled);
    }

    #[test]
    fn toggle_user_agent_on_plist_without_disabled_key_adds_it() {
        let dir = TempDir::new().unwrap();
        let agents = dir.path().join("Library/LaunchAgents");
        fs::create_dir_all(&agents).unwrap();
        let plist = agents.join("x.plist");
        let text = r#"<?xml version="1.0"?><plist><dict>
    <key>Label</key><string>x</string>
    <key>ProgramArguments</key><array><string>/x</string></array>
</dict></plist>
"#;
        write(&plist, text);
        toggle_user_agent(&plist, false).unwrap();
        let p = parse_plist(&fs::read_to_string(&plist).unwrap()).unwrap();
        assert!(p.disabled);
    }

    #[test]
    fn toggle_user_agent_handles_binary_plist() {
        let dir = TempDir::new().unwrap();
        let agents = dir.path().join("Library/LaunchAgents");
        fs::create_dir_all(&agents).unwrap();
        let plist = agents.join("binary.plist");
        write_binary_plist(&plist, "binary.agent", false);

        toggle_user_agent(&plist, false).unwrap();
        let items = list_plist_dir(&agents, StartupSource::MacLaunchAgentUser, true);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "binary.agent");
        assert!(!items[0].enabled);
    }

    #[test]
    fn impact_for_launch_agent_commands() {
        let text = sample_plist("com.example.slack", false).replace(
            "/usr/local/bin/foo",
            "/Applications/Slack.app/Contents/MacOS/Slack",
        );
        let p = parse_plist(&text).unwrap();
        assert_eq!(
            impact_for_command(&p.command()),
            super::super::types::StartupImpact::High
        );
    }
}
