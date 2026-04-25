//! path safety policy. decides if a path is trashable or if the cleaner
//! refuses.
//!
//! do-no-harm foundation. even if scanner or UI somehow submits a path
//! that would nuke the OS, cleaner says no. every path is classified
//! twice, once in preview and again in commit (graveyard re-reads
//! protection from the stored plan so a tampered plan can't sneak
//! through).
//!
//! classifier is lexical, never touches disk. canonicalize would fail
//! on missing paths or silently resolve symlinks, both wrong here.
//! normalise . / .. in memory and compare against a hard blocklist plus
//! $HOME.

use std::path::{Component, Path, PathBuf};

/// never-trash paths. match = refuse, strict ancestor of one = refuse
/// (deleting / would wipe /usr). mac + linux + win merged so tests stay
/// portable.
const SYSTEM_BLOCKLIST: &[&str] = &[
    // POSIX roots
    "/",
    "/bin",
    "/sbin",
    "/usr",
    "/etc",
    "/var",
    "/lib",
    "/lib32",
    "/lib64",
    "/boot",
    "/dev",
    "/proc",
    "/sys",
    "/root",
    "/home",
    "/srv",
    "/opt",
    "/run",
    // macOS
    "/System",
    "/Library",
    "/Applications",
    "/private",
    "/Users",
    "/Volumes",
    "/cores",
    "/Network",
    // win. literals are posix-style. backslashes get canonicalised to
    // forward slashes before compare, so C:\Windows == C:/Windows
    "C:/",
    "C:/Windows",
    "C:/Program Files",
    "C:/Program Files (x86)",
    "C:/Users",
    "C:/ProgramData",
    "C:/System Volume Information",
];

/// primary user folders under $HOME. losing these would be catastrophic
/// even when empty.
const PRIMARY_USER_FOLDERS: &[&str] = &[
    "Documents",
    "Desktop",
    "Downloads",
    "Pictures",
    "Music",
    "Videos",
    "Movies", // macOS uses Movies instead of Videos
    "Public",
];

/// Some(reason) if the cleaner refuses to trash this, None if safe.
/// reason is user-facing, flows to UI as protectedReason.
pub fn classify(home: &Path, path: &Path) -> Option<&'static str> {
    if !path.is_absolute() {
        return Some("path must be absolute");
    }

    let norm = normalize(path);
    let home_norm = normalize(home);

    // .. escaped the root, something's weird
    if norm.as_os_str().is_empty() {
        return Some("path resolved to empty");
    }

    // exact $HOME/<name>
    for name in PRIMARY_USER_FOLDERS {
        if norm == home_norm.join(name) {
            return Some("refusing to delete a primary user folder");
        }
    }

    if norm == home_norm {
        return Some("refusing to delete the home directory");
    }
    // ancestor of home = would wipe home
    if is_strict_ancestor(&norm, &home_norm) {
        return Some("refusing to delete an ancestor of the home directory");
    }

    // system blocklist, both directions:
    //   1. norm IS a blocklisted path
    //   2. norm is a strict ancestor of one (would wipe it)
    let norm_str = to_forward_slashes(&norm);
    for crit in SYSTEM_BLOCKLIST {
        let crit_path = PathBuf::from(crit);
        let crit_norm = normalize(&crit_path);
        if norm == crit_norm {
            return Some(reason_for(crit));
        }
        if is_strict_ancestor(&norm, &crit_norm) {
            return Some(reason_for(crit));
        }
        // fallback string match for win literals like C:/ that arrive
        // with weird casing or mixed separators
        if norm_str.eq_ignore_ascii_case(crit) {
            return Some(reason_for(crit));
        }
    }

    None
}

fn reason_for(crit: &str) -> &'static str {
    match crit {
        "/" | "C:/" => "refusing to delete a filesystem root",
        _ => "refusing to delete a system directory",
    }
}

/// lexical normalize. collapses . and .. without touching disk.
/// unlike fs::canonicalize: works on missing paths, doesn't resolve
/// symlinks (critical, or a symlink inside a cache could delete its
/// target outside the cache).
pub fn normalize(p: &Path) -> PathBuf {
    let mut out: Vec<Component> = Vec::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {} // skip .
            Component::ParentDir => {
                // only pop if last is Normal, otherwise .. stays (can't
                // climb past root / prefix)
                if matches!(out.last(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            other => out.push(other),
        }
    }
    let mut result = PathBuf::new();
    for c in out {
        result.push(c.as_os_str());
    }
    result
}

/// strict = not equal, descendant lives under ancestor. expects
/// already-normalised input.
pub fn is_strict_ancestor(ancestor: &Path, descendant: &Path) -> bool {
    if ancestor == descendant {
        return false;
    }
    let a: Vec<_> = ancestor.components().collect();
    let d: Vec<_> = descendant.components().collect();
    if a.len() >= d.len() {
        return false;
    }
    a.iter().zip(d.iter()).all(|(x, y)| x == y)
}

fn to_forward_slashes(p: &Path) -> String {
    let s = p.to_string_lossy();
    s.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // posix-style absolute paths aren't is_absolute() on windows, classify
    // would short-circuit with "path must be absolute" before exercising
    // the actual blocklist logic. prefix C: on windows so the path is a
    // real absolute and the classifier runs end-to-end. blocklist already
    // has both posix and C:\ entries.
    fn abs(unix_style: &str) -> PathBuf {
        #[cfg(windows)]
        { PathBuf::from(format!("C:{unix_style}")) }
        #[cfg(not(windows))]
        { PathBuf::from(unix_style) }
    }

    fn home_of(name: &str) -> PathBuf {
        abs(&format!("/home/{name}"))
    }

    // ---------- normalize ----------

    #[test]
    fn normalize_collapses_dot_and_parent() {
        assert_eq!(
            normalize(&PathBuf::from("/a/b/./c/../d")),
            PathBuf::from("/a/b/d")
        );
    }

    #[test]
    fn normalize_keeps_absolute_root() {
        assert_eq!(normalize(&PathBuf::from("/")), PathBuf::from("/"));
    }

    #[test]
    fn normalize_does_not_touch_disk() {
        // missing path still normalises
        let p = PathBuf::from("/definitely/does/not/exist/../here");
        assert_eq!(normalize(&p), PathBuf::from("/definitely/does/not/here"));
    }

    // ---------- is_strict_ancestor ----------

    #[test]
    fn ancestor_is_strict() {
        assert!(is_strict_ancestor(
            &PathBuf::from("/a"),
            &PathBuf::from("/a/b/c")
        ));
        assert!(!is_strict_ancestor(
            &PathBuf::from("/a"),
            &PathBuf::from("/a")
        ));
        assert!(!is_strict_ancestor(
            &PathBuf::from("/a/b"),
            &PathBuf::from("/a")
        ));
    }

    #[test]
    fn unrelated_paths_are_not_ancestors() {
        assert!(!is_strict_ancestor(
            &PathBuf::from("/a"),
            &PathBuf::from("/b/c")
        ));
    }

    // ---------- classify: hard blocks ----------

    #[test]
    fn root_is_always_blocked() {
        assert!(classify(&home_of("a"), &PathBuf::from("/")).is_some());
    }

    #[test]
    fn system_directories_are_blocked() {
        let home = home_of("a");
        for p in ["/usr", "/etc", "/bin", "/System", "/Library", "/Volumes"] {
            assert!(
                classify(&home, &PathBuf::from(p)).is_some(),
                "expected {p} to be blocked"
            );
        }
    }

    #[test]
    fn ancestor_of_system_path_is_blocked() {
        // /bin/.. normalises to / which is blocked
        let home = home_of("a");
        assert!(classify(&home, &PathBuf::from("/bin/..")).is_some());
    }

    // ---------- classify: home dir protection ----------

    #[test]
    fn home_itself_is_blocked() {
        let home = home_of("adrian");
        assert!(classify(&home, &home).is_some());
    }

    #[test]
    fn primary_user_folders_are_blocked() {
        let home = home_of("adrian");
        for name in ["Documents", "Desktop", "Downloads", "Pictures", "Music"] {
            let p = home.join(name);
            assert!(
                classify(&home, &p).is_some(),
                "expected {} to be blocked",
                p.display()
            );
        }
    }

    #[test]
    fn ancestor_of_home_is_blocked() {
        // /home is blocklisted on unix but this exercises the explicit
        // ancestor-of-$HOME branch on mac/win where $HOME lives under
        // /Users or C:/Users
        let home = PathBuf::from("/Users/adrian");
        assert!(classify(&home, &PathBuf::from("/Users")).is_some());
    }

    // ---------- classify: paths that should pass ----------

    #[test]
    fn inside_user_caches_is_safe() {
        let home = home_of("adrian");
        let p = home.join(".cache/spotify/data");
        assert_eq!(classify(&home, &p), None);
    }

    #[test]
    fn tmp_subpath_is_safe() {
        let home = home_of("adrian");
        // /tmp isn't blocklisted, it's a legit junk catalog base on
        // linux. /tmp itself also passes, policy only guards system
        // paths
        assert_eq!(classify(&home, &abs("/tmp/leftover.lock")), None);
    }

    #[test]
    fn trash_child_is_safe() {
        // ~/.Trash/deleted.pdf inside home, not a primary folder
        let home = home_of("adrian");
        let p = home.join(".Trash/deleted.pdf");
        assert_eq!(classify(&home, &p), None);
    }

    // ---------- classify: pathological inputs ----------

    #[test]
    fn relative_paths_are_rejected() {
        let home = home_of("adrian");
        assert!(classify(&home, &PathBuf::from("relative/thing")).is_some());
    }

    #[test]
    fn dotdot_escaping_to_root_is_blocked() {
        let home = home_of("adrian");
        // /home/adrian/.cache/../../.. -> /
        let p = PathBuf::from("/home/adrian/.cache/../../..");
        assert!(classify(&home, &p).is_some());
    }

    #[test]
    fn dotdot_inside_home_is_fine() {
        let home = home_of("adrian");
        // /home/adrian/.cache/x/.. -> /home/adrian/.cache, still home
        let p = abs("/home/adrian/.cache/x/..");
        assert_eq!(classify(&home, &p), None);
    }

    #[test]
    fn primary_folder_name_inside_subdir_is_safe() {
        // nested Documents folder that isn't the $HOME/Documents one
        let home = home_of("adrian");
        let p = home.join(".cache/Documents/stuff.bin");
        assert_eq!(classify(&home, &p), None);
    }

    #[test]
    fn reason_strings_are_human_readable() {
        let home = home_of("a");
        let r = classify(&home, &abs("/")).unwrap();
        assert!(r.starts_with("refusing"));
        let r = classify(&home, &home.join("Documents")).unwrap();
        assert!(r.contains("primary user folder"));
    }
}
