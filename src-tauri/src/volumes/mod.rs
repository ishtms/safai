//! cross-platform disk telemetry.
//!
//! two layers so the interesting logic is testable without the OS:
//!
//! * source: adapter over sysinfo::Disks. returns RawVolume exactly as
//!   reported, no interpretation.
//! * process: pure fn. RawVolume + platform -> Volume. dedup, pseudo-fs
//!   filter, primary-disk pick, ordering.
//!
//! commands.rs just glues them together.

mod process;
mod source;
mod types;

use std::path::Path;
use std::path::PathBuf;

pub use source::list_volumes;
pub use types::Volume;

/// volume whose mount_point is the longest prefix of `path`. falls back
/// to the primary volume, then None when no volumes.
///
/// used by the scanner to pick "which volume should reconcile against
/// the walk bytes". $HOME is almost always on primary, but on weird
/// setups (separate /home mount on linux) the longest-prefix match
/// wins.
pub fn volume_for_path(volumes: &[Volume], path: &Path) -> Option<Volume> {
    let path_key = normalize_for_match(path);
    let mut best: Option<&Volume> = None;
    for v in volumes {
        if v.mount_point.is_empty() {
            continue;
        }
        let mount_key = normalize_for_match(Path::new(&v.mount_point));
        if !path_key.starts_with(&mount_key) {
            continue;
        }
        // tie-break by normalized mount length so "/home" beats "/"
        best = match best {
            None => Some(v),
            Some(b)
                if mount_sort_len(&mount_key)
                    > mount_sort_len(&normalize_for_match(Path::new(&b.mount_point))) =>
            {
                Some(v)
            }
            Some(b) => Some(b),
        };
    }
    best.or_else(|| volumes.iter().find(|v| v.is_primary))
        .cloned()
}

fn normalize_for_match(path: &Path) -> PathBuf {
    let p = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    normalize_platform_path(&p)
}

#[cfg(windows)]
fn normalize_platform_path(path: &Path) -> PathBuf {
    // Windows paths are case-insensitive and accept both separators. Lowercase
    // the string form before Path::starts_with so C:\Users and c:/users match,
    // including UNC prefixes.
    PathBuf::from(
        path.to_string_lossy()
            .replace('/', "\\")
            .to_ascii_lowercase(),
    )
}

#[cfg(not(windows))]
fn normalize_platform_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

fn mount_sort_len(path: &Path) -> usize {
    path.components().count() * 1024 + path.to_string_lossy().len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::volumes::types::VolumeKind;

    fn vol(mp: &str, is_primary: bool) -> Volume {
        Volume {
            name: mp.into(),
            mount_point: mp.into(),
            total_bytes: 1_000,
            free_bytes: 500,
            used_bytes: 500,
            file_system: "ext4".into(),
            kind: VolumeKind::Ssd,
            is_removable: false,
            is_primary,
        }
    }

    #[test]
    fn longest_prefix_wins_over_root() {
        let vols = vec![vol("/", true), vol("/home", false)];
        let got = volume_for_path(&vols, Path::new("/home/alice")).unwrap();
        assert_eq!(got.mount_point, "/home");
    }

    #[test]
    fn prefix_match_is_component_aware() {
        let vols = vec![vol("/", true), vol("/home", false)];
        let got = volume_for_path(&vols, Path::new("/home2/alice")).unwrap();
        assert_eq!(got.mount_point, "/");
    }

    #[test]
    fn trailing_separators_match_same_mount() {
        let vols = vec![vol("/", true), vol("/home/", false)];
        let got = volume_for_path(&vols, Path::new("/home/alice")).unwrap();
        assert_eq!(got.mount_point, "/home/");
    }

    #[test]
    fn falls_back_to_primary_when_no_prefix_matches() {
        let vols = vec![vol("/data", false), vol("/", true)];
        let got = volume_for_path(&vols, Path::new("/nomatch")).unwrap();
        // root is always a prefix of /nomatch, so it wins here. test the
        // no-match + no-root case instead
        assert_eq!(got.mount_point, "/");

        let vols = vec![vol("D:\\", false), vol("C:\\", true)];
        let got = volume_for_path(&vols, Path::new("E:\\foo")).unwrap();
        assert_eq!(got.mount_point, "C:\\", "fell back to primary");
    }

    #[test]
    fn empty_volumes_yields_none() {
        assert!(volume_for_path(&[], Path::new("/")).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_matching_is_case_insensitive() {
        let vols = vec![vol("C:\\", true), vol("C:\\Users", false)];
        let got = volume_for_path(&vols, Path::new("c:/users/alice")).unwrap();
        assert_eq!(got.mount_point, "C:\\Users");
    }

    #[cfg(windows)]
    #[test]
    fn windows_unc_paths_match_by_component() {
        let vols = vec![
            vol("C:\\", true),
            vol("\\\\server\\share", false),
            vol("\\\\server\\share2", false),
        ];
        let got = volume_for_path(&vols, Path::new("\\\\SERVER\\share\\dir")).unwrap();
        assert_eq!(got.mount_point, "\\\\server\\share");
    }
}
