//! metadata -> bytes helpers.
//!
//! `meta.len()` is the *logical* size. for sparse files (Docker.raw, vm
//! images, some iOS sim dmgs) that's wildly inflated vs what actually
//! touches the disk. allocated_bytes prefers the physical block count on
//! unix so Docker.raw stops reading as 900+ GB when the disk is 500.

use std::fs::Metadata;

/// bytes actually allocated on disk for `meta`. falls back to logical
/// len() on non-unix or when blocks() is zero (some fs types like
/// network mounts under-report).
///
/// windows has no equivalent of st_blocks in stable std, so we accept the
/// inflation there. ntfs sparse files are rare on user disks anyway.
#[inline]
pub fn allocated_bytes(meta: &Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let physical = meta.blocks().saturating_mul(512);
        if physical > 0 {
            // return min of physical + logical. some apfs cases report
            // physical > logical when the fs hasn't flushed; clamp so we
            // never over-report a small file as huge.
            return physical.min(meta.len()).max(1.min(meta.len()));
        }
        meta.len()
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

/// true when a path looks like a well-known sparse container file that
/// should not be summed as junk or surfaced as "large & old". these files
/// are real allocations inside a user app (docker vm disk, parallels, iOS
/// sim) but logical size is wildly inflated, and deleting them breaks
/// the owning app.
pub fn is_sparse_container_path(path: &std::path::Path) -> bool {
    let s = path.to_string_lossy();
    // case-insensitive contains is overkill, these are fixed casings
    const NEEDLES: &[&str] = &[
        // docker desktop (mac)
        "com.docker.docker/Data/vms/",
        "/Docker.raw",
        // docker machine (older, still seen)
        "/.docker/machine/machines/",
        // parallels
        ".pvm/",
        ".hdd/",
        // vmware fusion
        ".vmwarevm/",
        ".vmdk",
        // virtualbox
        ".vdi",
        ".vhd",
        // ios simulator runtime images
        "CoreSimulator/Devices/",
        "CoreSimulator/Images/",
    ];
    NEEDLES.iter().any(|n| s.contains(n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn allocated_bytes_basic_roundtrip() {
        // smoke: write a small file, both paths should agree within 4 KiB
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("f.bin");
        std::fs::write(&p, b"hello world").unwrap();
        let meta = std::fs::metadata(&p).unwrap();
        let a = allocated_bytes(&meta);
        assert!(a >= 1, "expected non-zero");
        // allocated may be a block (4k) but never larger than the file rounded up reasonably
        assert!(a <= 64 * 1024, "small file should not allocate more than a block or two");
    }

    #[test]
    fn sparse_container_matches_docker() {
        let p = Path::new("/Users/x/Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw");
        assert!(is_sparse_container_path(p));
    }

    #[test]
    fn sparse_container_matches_parallels() {
        let p = Path::new("/Users/x/Parallels/Windows 11.pvm/Windows 11-0.hdd");
        assert!(is_sparse_container_path(p));
    }

    #[test]
    fn sparse_container_matches_ios_sim() {
        let p = Path::new(
            "/Users/x/Library/Developer/CoreSimulator/Devices/ABC/data/Containers",
        );
        assert!(is_sparse_container_path(p));
    }

    #[test]
    fn sparse_container_rejects_normal() {
        let p = Path::new("/Users/x/Documents/letter.txt");
        assert!(!is_sparse_container_path(p));
    }
}
