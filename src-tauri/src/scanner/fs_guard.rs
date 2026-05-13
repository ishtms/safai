//! filesystem identity helpers shared by walkers.
//!
//! The scanners default to staying on the filesystem that owns their root.
//! This prevents a home/cache/browser scan from walking into mounted external
//! disks, network shares, bind mounts, or APFS firmlinks that happen to sit
//! under an otherwise valid root.

use std::fs::Metadata;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceId(u128);

/// Device/volume identity for the root path. None means "unsupported or
/// unreadable"; callers should fail open rather than skipping the whole scan.
pub fn root_device_id(path: &Path) -> Option<DeviceId> {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| metadata_device_id(&m))
}

/// True when `metadata` is on `root_device`, or when no guard is available.
#[inline]
pub fn is_on_root_device(root_device: Option<DeviceId>, metadata: &Metadata) -> bool {
    match root_device {
        Some(root) => metadata_device_id(metadata) == Some(root),
        None => true,
    }
}

#[cfg(unix)]
fn metadata_device_id(meta: &Metadata) -> Option<DeviceId> {
    use std::os::unix::fs::MetadataExt;
    Some(DeviceId(meta.dev() as u128))
}

#[cfg(windows)]
fn metadata_device_id(meta: &Metadata) -> Option<DeviceId> {
    use std::os::windows::fs::MetadataExt;
    meta.volume_serial_number().map(|v| DeviceId(v as u128))
}

#[cfg(not(any(unix, windows)))]
fn metadata_device_id(_meta: &Metadata) -> Option<DeviceId> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_device_matches_child_metadata_in_same_temp_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("f");
        std::fs::write(&file, b"x").unwrap();

        let root_dev = root_device_id(tmp.path());
        let meta = std::fs::metadata(&file).unwrap();

        assert!(is_on_root_device(root_dev, &meta));
    }

    #[test]
    fn missing_guard_fails_open() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("f");
        std::fs::write(&file, b"x").unwrap();
        let meta = std::fs::metadata(&file).unwrap();

        assert!(is_on_root_device(None, &meta));
    }
}
