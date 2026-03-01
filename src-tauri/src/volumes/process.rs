//! pure transform from RawVolume to UI-facing Volume.
//!
//! no sysinfo / OS calls so pseudo-fs filtering, bind-mount dedup, and
//! primary-volume pick are covered by hermetic unit tests.

use super::types::{RawVolume, Volume, VolumeKind};

/// separate from cfg!(target_os=...) so tests exercise all 3 branches
/// on one host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Mac,
    Linux,
    Windows,
    Other,
}

impl Platform {
    pub fn host() -> Self {
        if cfg!(target_os = "macos") {
            Platform::Mac
        } else if cfg!(target_os = "linux") {
            Platform::Linux
        } else if cfg!(target_os = "windows") {
            Platform::Windows
        } else {
            Platform::Other
        }
    }
}

/// 1. drop total_bytes==0 (pseudo/virtual)
/// 2. drop known noise (snap squashfs, /proc|/sys|/run|/dev, docker
///    overlays, etc)
/// 3. dedupe by mount, keep larger total
/// 4. compute used_bytes via saturating_sub
/// 5. pick one primary
/// 6. sort: primary first, then total_bytes desc
pub fn process(raw: Vec<RawVolume>, platform: Platform) -> Vec<Volume> {
    let mut kept: Vec<RawVolume> = raw
        .into_iter()
        .filter(|v| v.total_bytes > 0)
        .filter(|v| !is_pseudo(v, platform))
        .collect();

    dedupe_by_mount(&mut kept);

    let primary_idx = pick_primary(&kept, platform);

    let mut out: Vec<Volume> = kept
        .into_iter()
        .enumerate()
        .map(|(i, r)| to_volume(r, Some(i) == primary_idx, platform))
        .collect();

    // primary first, then largest
    out.sort_by(|a, b| {
        b.is_primary
            .cmp(&a.is_primary)
            .then(b.total_bytes.cmp(&a.total_bytes))
    });

    out
}

fn to_volume(r: RawVolume, is_primary: bool, platform: Platform) -> Volume {
    let used = r.total_bytes.saturating_sub(r.free_bytes);
    // kind=Unknown + removable=true -> upgrade to Removable for a
    // different badge. small inference, centralised so every caller
    // gets it.
    let kind = if r.is_removable && r.kind == VolumeKind::Unknown {
        VolumeKind::Removable
    } else {
        r.kind
    };
    Volume {
        name: friendly_name(&r, platform),
        mount_point: r.mount_point,
        total_bytes: r.total_bytes,
        free_bytes: r.free_bytes.min(r.total_bytes),
        used_bytes: used,
        file_system: r.file_system,
        kind,
        is_removable: r.is_removable,
        is_primary,
    }
}

/// mac name is the volume label ("Macintosh HD"), use it.
/// linux name is the device path ("/dev/nvme0n1p2"), terrible UX, so
/// we derive from the mount with nicer aliases. win labels like "C:"
/// are fine as-is.
fn friendly_name(r: &RawVolume, platform: Platform) -> String {
    match platform {
        Platform::Linux | Platform::Other => match r.mount_point.as_str() {
            "/" => "Root (/)".to_string(),
            "/home" => "Home (/home)".to_string(),
            "/boot" | "/boot/efi" => format!("Boot ({})", r.mount_point),
            mp if mp.starts_with("/mnt/") || mp.starts_with("/media/") => {
                // strip /mnt/ or /media/<user>/ prefix, keep last name
                let last = mp.rsplit('/').find(|s| !s.is_empty()).unwrap_or(mp);
                last.to_string()
            }
            mp => mp.to_string(),
        },
        Platform::Mac | Platform::Windows => {
            if r.name.trim().is_empty() {
                r.mount_point.clone()
            } else {
                r.name.clone()
            }
        }
    }
}

fn is_pseudo(v: &RawVolume, platform: Platform) -> bool {
    // known pseudo fs is noise everywhere
    let fs = v.file_system.to_ascii_lowercase();
    if matches!(
        fs.as_str(),
        "squashfs" | "tmpfs" | "devtmpfs" | "proc" | "sysfs" | "overlay" | "autofs" | "fuse.snapfuse"
    ) {
        return true;
    }

    match platform {
        Platform::Linux => {
            let mp = v.mount_point.as_str();
            mp.starts_with("/proc")
                || mp.starts_with("/sys")
                || mp.starts_with("/run")
                || mp.starts_with("/dev")
                || mp.starts_with("/snap/")
                || mp == "/snap"
                || mp.starts_with("/var/snap/")
                || mp.starts_with("/var/lib/docker/")
                || mp.starts_with("/var/lib/containers/")
                || mp.starts_with("/boot/efi")
        }
        Platform::Mac => {
            // mac mounts preboot/recovery/VM under /System/Volumes/*,
            // junk. user-facing one is / (Data sibling of /).
            let mp = v.mount_point.as_str();
            mp.starts_with("/System/Volumes/") && mp != "/System/Volumes/Data"
        }
        Platform::Windows | Platform::Other => false,
    }
}

fn dedupe_by_mount(list: &mut Vec<RawVolume>) {
    // same mount twice (linux bind mounts) -> keep the larger
    list.sort_by(|a, b| a.mount_point.cmp(&b.mount_point).then(b.total_bytes.cmp(&a.total_bytes)));
    let mut last: Option<String> = None;
    list.retain(|v| {
        let dup = last.as_ref().is_some_and(|m| m == &v.mount_point);
        if !dup {
            last = Some(v.mount_point.clone());
        }
        !dup
    });
}

fn pick_primary(list: &[RawVolume], platform: Platform) -> Option<usize> {
    if list.is_empty() {
        return None;
    }

    let by_mount = |target: &str| list.iter().position(|v| v.mount_point == target);

    // 1. obvious root mount
    let rooted = match platform {
        Platform::Mac | Platform::Linux | Platform::Other => by_mount("/"),
        Platform::Windows => {
            // system drive, usually C:. sysinfo reports with trailing \
            by_mount("C:\\").or_else(|| by_mount("C:/"))
        }
    };
    if let Some(i) = rooted {
        return Some(i);
    }

    // 2. largest non-removable
    if let Some((i, _)) = list
        .iter()
        .enumerate()
        .filter(|(_, v)| !v.is_removable)
        .max_by_key(|(_, v)| v.total_bytes)
    {
        return Some(i);
    }

    // 3. single largest
    list.iter()
        .enumerate()
        .max_by_key(|(_, v)| v.total_bytes)
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(
        name: &str,
        mount: &str,
        total: u64,
        free: u64,
        fs: &str,
        kind: VolumeKind,
        removable: bool,
    ) -> RawVolume {
        RawVolume {
            name: name.into(),
            mount_point: mount.into(),
            total_bytes: total,
            free_bytes: free,
            file_system: fs.into(),
            kind,
            is_removable: removable,
        }
    }

    const GB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn strips_pseudo_filesystems_on_linux() {
        let input = vec![
            raw("/", "/", 500 * GB, 200 * GB, "ext4", VolumeKind::Ssd, false),
            raw("", "/proc", 0, 0, "proc", VolumeKind::Unknown, false),
            raw("", "/run/user/1000", 3 * GB, 3 * GB, "tmpfs", VolumeKind::Unknown, false),
            raw("", "/snap/core22/x", GB / 10, 0, "squashfs", VolumeKind::Unknown, false),
            raw("", "/dev/shm", GB, GB, "tmpfs", VolumeKind::Unknown, false),
        ];
        let out = process(input, Platform::Linux);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].mount_point, "/");
    }

    #[test]
    fn strips_system_volumes_on_mac_but_keeps_data() {
        let input = vec![
            raw("Macintosh HD", "/", 500 * GB, 200 * GB, "apfs", VolumeKind::Ssd, false),
            raw("Data", "/System/Volumes/Data", 500 * GB, 200 * GB, "apfs", VolumeKind::Ssd, false),
            raw("Preboot", "/System/Volumes/Preboot", GB, 0, "apfs", VolumeKind::Ssd, false),
            raw("Recovery", "/System/Volumes/Recovery", GB, 0, "apfs", VolumeKind::Ssd, false),
            raw("VM", "/System/Volumes/VM", GB, 0, "apfs", VolumeKind::Ssd, false),
        ];
        let out = process(input, Platform::Mac);
        // preboot/recovery/VM filtered, /Data kept (APFS sibling of /)
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|v| v.mount_point == "/"));
        assert!(out.iter().any(|v| v.mount_point == "/System/Volumes/Data"));
    }

    #[test]
    fn dedupes_by_mount_keeping_largest() {
        // Mac here so friendly-name doesn't mask which raw entry
        // survived. total_bytes + label prove "b" won.
        let input = vec![
            raw("a", "/Volumes/Data", 100 * GB, 50 * GB, "apfs", VolumeKind::Ssd, false),
            raw("b", "/Volumes/Data", 200 * GB, 90 * GB, "apfs", VolumeKind::Ssd, false),
        ];
        let out = process(input, Platform::Mac);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].total_bytes, 200 * GB);
        assert_eq!(out[0].name, "b");
    }

    #[test]
    fn primary_is_root_on_unix() {
        let input = vec![
            raw("Backup", "/mnt/backup", 2000 * GB, 100 * GB, "ext4", VolumeKind::Hdd, false),
            raw("Macintosh HD", "/", 500 * GB, 200 * GB, "apfs", VolumeKind::Ssd, false),
        ];
        let out = process(input, Platform::Mac);
        assert_eq!(out[0].mount_point, "/", "primary sorts to index 0");
        assert!(out[0].is_primary);
        assert!(!out[1].is_primary);
    }

    #[test]
    fn primary_is_c_drive_on_windows() {
        let input = vec![
            raw("Data", "D:\\", 2000 * GB, 1000 * GB, "NTFS", VolumeKind::Hdd, false),
            raw("System", "C:\\", 500 * GB, 200 * GB, "NTFS", VolumeKind::Ssd, false),
        ];
        let out = process(input, Platform::Windows);
        assert_eq!(out[0].mount_point, "C:\\");
        assert!(out[0].is_primary);
    }

    #[test]
    fn primary_falls_back_to_largest_non_removable() {
        let input = vec![
            raw("USB", "/mnt/usb", 128 * GB, 0, "vfat", VolumeKind::Removable, true),
            raw("Scratch", "/mnt/scratch", 200 * GB, 50 * GB, "ext4", VolumeKind::Hdd, false),
            raw("Big", "/mnt/archive", 4000 * GB, 500 * GB, "ext4", VolumeKind::Hdd, false),
        ];
        let out = process(input, Platform::Linux);
        let primary = out.iter().find(|v| v.is_primary).unwrap();
        assert_eq!(primary.mount_point, "/mnt/archive");
    }

    #[test]
    fn primary_falls_back_to_largest_removable_when_all_removable() {
        let input = vec![
            raw("A", "/mnt/a", 32 * GB, 10 * GB, "vfat", VolumeKind::Removable, true),
            raw("B", "/mnt/b", 128 * GB, 80 * GB, "vfat", VolumeKind::Removable, true),
        ];
        let out = process(input, Platform::Linux);
        let primary = out.iter().find(|v| v.is_primary).unwrap();
        assert_eq!(primary.mount_point, "/mnt/b");
    }

    #[test]
    fn empty_list_yields_no_primary() {
        let out = process(Vec::new(), Platform::Linux);
        assert!(out.is_empty());
    }

    #[test]
    fn all_pseudo_yields_empty() {
        let input = vec![
            raw("", "/proc", 0, 0, "proc", VolumeKind::Unknown, false),
            raw("", "/sys", 0, 0, "sysfs", VolumeKind::Unknown, false),
        ];
        let out = process(input, Platform::Linux);
        assert!(out.is_empty());
    }

    #[test]
    fn used_bytes_saturating_when_free_exceeds_total() {
        let input = vec![raw("/", "/", 100 * GB, 150 * GB, "ext4", VolumeKind::Ssd, false)];
        let out = process(input, Platform::Linux);
        assert_eq!(out[0].used_bytes, 0, "no underflow");
        assert_eq!(out[0].free_bytes, 100 * GB, "free clamped to total");
    }

    #[test]
    fn used_plus_free_equals_total_in_normal_case() {
        let input = vec![raw("/", "/", 500 * GB, 123 * GB, "apfs", VolumeKind::Ssd, false)];
        let out = process(input, Platform::Mac);
        assert_eq!(out[0].used_bytes + out[0].free_bytes, out[0].total_bytes);
    }

    #[test]
    fn linux_uses_friendly_mount_labels() {
        // sysinfo gives device paths on linux, hostile to display.
        // we derive a label from the mount.
        let input = vec![
            raw("/dev/nvme0n1p2", "/", 500 * GB, 200 * GB, "ext4", VolumeKind::Ssd, false),
            raw("/dev/sda1", "/home", 2000 * GB, 1000 * GB, "ext4", VolumeKind::Hdd, false),
            raw("/dev/nvme0n1p1", "/boot", GB, GB / 2, "vfat", VolumeKind::Ssd, false),
            raw("/dev/sdb1", "/media/user/USB", 32 * GB, 20 * GB, "vfat", VolumeKind::Removable, true),
        ];
        let out = process(input, Platform::Linux);
        let by_mount: std::collections::HashMap<_, _> =
            out.iter().map(|v| (v.mount_point.clone(), v.name.clone())).collect();
        assert_eq!(by_mount["/"], "Root (/)");
        assert_eq!(by_mount["/home"], "Home (/home)");
        assert_eq!(by_mount["/boot"], "Boot (/boot)");
        assert_eq!(by_mount["/media/user/USB"], "USB");
    }

    #[test]
    fn mac_keeps_volume_label() {
        let input = vec![raw(
            "Macintosh HD",
            "/",
            500 * GB,
            200 * GB,
            "apfs",
            VolumeKind::Ssd,
            false,
        )];
        let out = process(input, Platform::Mac);
        assert_eq!(out[0].name, "Macintosh HD");
    }

    #[test]
    fn mac_empty_label_falls_back_to_mount_point() {
        let input = vec![raw("", "/", 100 * GB, 50 * GB, "apfs", VolumeKind::Ssd, false)];
        let out = process(input, Platform::Mac);
        assert_eq!(out[0].name, "/");
    }

    #[test]
    fn unknown_kind_with_removable_flag_is_upgraded() {
        let input = vec![raw(
            "USB",
            "/mnt/usb",
            32 * GB,
            10 * GB,
            "vfat",
            VolumeKind::Unknown,
            true,
        )];
        let out = process(input, Platform::Linux);
        assert_eq!(out[0].kind, VolumeKind::Removable);
    }

    #[test]
    fn at_most_one_primary() {
        let input = vec![
            raw("/", "/", 500 * GB, 200 * GB, "apfs", VolumeKind::Ssd, false),
            raw("Backup", "/Volumes/Backup", 2000 * GB, 1000 * GB, "apfs", VolumeKind::Hdd, false),
            raw("Scratch", "/Volumes/Scratch", 1000 * GB, 500 * GB, "apfs", VolumeKind::Ssd, false),
        ];
        let out = process(input, Platform::Mac);
        let primaries = out.iter().filter(|v| v.is_primary).count();
        assert_eq!(primaries, 1);
    }

    #[test]
    fn sort_primary_first_then_size_desc() {
        let input = vec![
            raw("Tiny", "/mnt/tiny", 10 * GB, 5 * GB, "ext4", VolumeKind::Ssd, false),
            raw("Root", "/", 500 * GB, 100 * GB, "ext4", VolumeKind::Ssd, false),
            raw("Huge", "/mnt/huge", 4000 * GB, 1000 * GB, "ext4", VolumeKind::Hdd, false),
        ];
        let out = process(input, Platform::Linux);
        assert_eq!(out[0].mount_point, "/");
        assert_eq!(out[1].mount_point, "/mnt/huge");
        assert_eq!(out[2].mount_point, "/mnt/tiny");
    }

    #[test]
    fn serializes_as_camelcase_with_kebab_kind() {
        let input = vec![raw("/", "/", 100 * GB, 50 * GB, "ext4", VolumeKind::Ssd, false)];
        let out = process(input, Platform::Linux);
        let v = serde_json::to_value(&out[0]).unwrap();
        assert!(v.get("totalBytes").is_some());
        assert!(v.get("freeBytes").is_some());
        assert!(v.get("usedBytes").is_some());
        assert!(v.get("mountPoint").is_some());
        assert!(v.get("fileSystem").is_some());
        assert!(v.get("isRemovable").is_some());
        assert!(v.get("isPrimary").is_some());
        assert_eq!(v["kind"], "ssd");
    }

    #[test]
    fn handles_large_disk_list_without_quadratic_blowup() {
        // 1000 bind-mount dupes + 100 unique. sort is O(n log n),
        // dedup O(n). nested-loop regression will time out CI.
        let mut input: Vec<RawVolume> = (0..100)
            .map(|i| {
                raw(
                    &format!("d{i}"),
                    &format!("/mnt/d{i}"),
                    (i as u64 + 1) * GB,
                    (i as u64 + 1) * GB / 2,
                    "ext4",
                    VolumeKind::Hdd,
                    false,
                )
            })
            .collect();
        // bind-mount dupes of /mnt/d0
        for _ in 0..1000 {
            input.push(raw("d0", "/mnt/d0", GB, GB / 2, "ext4", VolumeKind::Hdd, false));
        }
        let out = process(input, Platform::Linux);
        assert_eq!(out.len(), 100);
    }
}
