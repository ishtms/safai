//! thin adapter over sysinfo::Disks. kept boring on purpose, all
//! interpretation lives in process.

use super::process::{process, Platform};
use super::types::{RawVolume, Volume, VolumeKind};

/// blocking snapshot. sysinfo's refreshed list does one round of
/// platform syscalls (DiskArbitration / /proc/mounts+statvfs /
/// GetLogicalDrives) and returns in a few ms even with many mounts.
pub fn list_volumes() -> Vec<Volume> {
    let raw = collect_raw();
    process(raw, Platform::host())
}

fn collect_raw() -> Vec<RawVolume> {
    use sysinfo::{DiskKind, Disks};

    let disks = Disks::new_with_refreshed_list();
    disks
        .iter()
        .map(|d| {
            let kind = match d.kind() {
                DiskKind::SSD => VolumeKind::Ssd,
                DiskKind::HDD => VolumeKind::Hdd,
                _ => VolumeKind::Unknown,
            };
            RawVolume {
                name: d.name().to_string_lossy().into_owned(),
                mount_point: d.mount_point().to_string_lossy().into_owned(),
                total_bytes: d.total_space(),
                free_bytes: d.available_space(),
                file_system: d.file_system().to_string_lossy().into_owned(),
                kind,
                is_removable: d.is_removable(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// smoke test, host sysinfo through process must not panic and must
    /// either be empty or elect exactly one primary
    #[test]
    fn host_listing_is_well_formed() {
        let out = list_volumes();
        let primaries = out.iter().filter(|v| v.is_primary).count();
        if out.is_empty() {
            assert_eq!(primaries, 0);
        } else {
            assert_eq!(primaries, 1, "exactly one primary when any disk is present");
        }
        for v in &out {
            assert!(v.total_bytes > 0, "filter should have dropped empty disks");
            assert!(v.free_bytes <= v.total_bytes, "free clamped to total");
            assert_eq!(v.used_bytes + v.free_bytes, v.total_bytes);
        }
    }
}
