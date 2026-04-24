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
    let p = path.to_string_lossy();
    let mut best: Option<&Volume> = None;
    for v in volumes {
        let mp = v.mount_point.as_str();
        if mp.is_empty() || !p.starts_with(mp) {
            continue;
        }
        // tie-break by mount_point length so "/home" beats "/"
        best = match best {
            None => Some(v),
            Some(b) if mp.len() > b.mount_point.len() => Some(v),
            Some(b) => Some(b),
        };
    }
    best.or_else(|| volumes.iter().find(|v| v.is_primary))
        .cloned()
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
}
