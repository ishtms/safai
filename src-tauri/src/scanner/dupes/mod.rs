//! dupe finder. two decoupled pieces like the treemap:
//!
//! * [`pipeline`] pure 3-pass dedup. group by size -> 4 KB head hash ->
//!   full blake3. rayon for parallel hash passes, pipeline itself is std-only.
//! * [`stream`] streams groups to the UI. owns controller + cancel + registry.
//!
//! # algorithm
//!
//! 1. walk `root` with jwalk (parallel IO). regular files >= `min_bytes`.
//!    zero/tiny files skipped on purpose, every empty file hashes to the
//!    same blake3("") = af13... which is just noise.
//! 2. group by size. drop singletons, unique size can't be a dupe.
//! 3. for each size group, 4 KB head hash in parallel. regroup by
//!    (size, head_hash), drop singletons. kills most false positives
//!    without reading full files.
//! 4. remaining candidates get a full blake3 (streamed, not slurped).
//!    regroup by full hash. survivors are confirmed dupes.
//!
//! 3-pass structure matters on a real home dir: size pass prunes ~99%
//! instantly, head pass handles almost all remaining pairs with one 4 KB
//! read, only the last sliver pays for a full-file read.
//!
//! # why blake3
//!
//! crypto-strong (no collisions for our dataset sizes), SIMD beats memcpy
//! on most CPUs, no unsafe. MD5/SHA1 would work too, blake3 is just faster
//! on the hot path.
//!
//! # safety
//!
//! symlinks never followed or hashed. hardlinks to the same inode hash
//! identically but aren't real dupes (deleting one frees nothing), so we
//! dedup at (device, inode) on unix. windows would need
//! GetFileInformationByHandle, not in scope for worst case
//! hardlinks surface as a group and "keep one / delete rest" reclaims
//! nothing, which the cleaner's graveyard restore handles.

pub mod pipeline;
pub mod stream;

use std::path::Path;
use std::time::Instant;

use serde::Serialize;

pub use pipeline::{find_duplicates, DuplicateGroup, FindError, IGNORED_DIR_NAMES};
// DuplicateFile is already reachable via DuplicateGroup.files, re-export
// so tests / future callers can name it without reaching into pipeline
#[allow(unused_imports)]
pub use pipeline::DuplicateFile;
pub use stream::{
    next_dupes_handle_id, run_dupes_stream, DupesController, DupesEmit, DupesHandle,
    DupesRegistry, ScanPhase,
};

/// default min candidate size. files < 1 MiB produce thousands of tiny
/// groups on a real home dir (node_modules mirrors, browser caches, icon
/// variants, font metadata). row explosion cripples the UI for negligible
/// reclaimed bytes. user-tunable once lands.
pub const DEFAULT_MIN_BYTES: u64 = 1024 * 1024;

/// cap on files per size bucket. pathological bucket (every file exactly
/// 1 MB) would stall the UI. 10k is well past any realistic candidate set.
pub const MAX_CANDIDATES_PER_BUCKET: usize = 10_000;

/// wire response. returned by the sync API + emitted as `dupes://progress`
/// (per phase) and `dupes://done` (terminal) by the streaming variant.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateReport {
    /// echoed so the UI breadcrumb is stable
    pub root: String,
    /// confirmed groups, sorted desc by wasted_bytes. empty on progress
    /// events before final grouping
    pub groups: Vec<DuplicateGroup>,
    /// regular files the walker considered (post min-bytes filter)
    pub total_files_scanned: u64,
    /// groups.len(), duplicated so UI doesn't recompute
    pub total_groups: u64,
    /// sum of wasted_bytes across all groups. recoverable if user keeps
    /// one copy per group
    pub wasted_bytes: u64,
    pub duration_ms: u64,
    /// current pipeline phase. UI shows concrete status per phase.
    /// terminal done event sets this to [`ScanPhase::Done`]
    pub phase: ScanPhase,
    /// files still in play for next phase. starts at full count, size
    /// pass drops unique-size singletons, head pass narrows, final count
    /// ends up in total_groups
    pub candidates_remaining: u64,
}

/// sync entry point. used by tests + `find_duplicates` Tauri command.
/// streaming variant in [`stream`] reuses [`pipeline::find_duplicates`].
pub fn scan_duplicates(
    root: &Path,
    min_bytes: u64,
) -> Result<DuplicateReport, FindError> {
    let started = Instant::now();
    let (groups, files_scanned) = find_duplicates(root, min_bytes, None)?;
    let wasted: u64 = groups.iter().map(|g| g.wasted_bytes).sum();
    let total_groups = groups.len() as u64;
    Ok(DuplicateReport {
        root: root.to_string_lossy().into_owned(),
        total_groups,
        wasted_bytes: wasted,
        groups,
        total_files_scanned: files_scanned,
        duration_ms: started.elapsed().as_millis() as u64,
        phase: ScanPhase::Done,
        candidates_remaining: total_groups,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;

    fn write_bytes(root: &Path, rel: &str, content: &[u8]) {
        let full = root.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = File::create(&full).unwrap();
        f.write_all(content).unwrap();
    }

    fn payload(seed: u8, len: usize) -> Vec<u8> {
        (0..len).map(|i| seed.wrapping_add(i as u8)).collect()
    }

    #[test]
    fn finds_real_duplicate_pair() {
        let tmp = tempfile::tempdir().unwrap();
        let data = payload(1, 8 * 1024);
        write_bytes(tmp.path(), "a/one.bin", &data);
        write_bytes(tmp.path(), "b/two.bin", &data);
        let report = scan_duplicates(tmp.path(), 0).unwrap();
        assert_eq!(report.groups.len(), 1);
        let g = &report.groups[0];
        assert_eq!(g.files.len(), 2);
        assert_eq!(g.bytes_each, data.len() as u64);
        assert_eq!(g.wasted_bytes, data.len() as u64); // one wasted copy
        assert_eq!(report.wasted_bytes, data.len() as u64);
    }

    #[test]
    fn non_duplicates_produce_zero_groups() {
        let tmp = tempfile::tempdir().unwrap();
        // same size different payloads, head-hash catches it
        write_bytes(tmp.path(), "a.bin", &payload(1, 8 * 1024));
        write_bytes(tmp.path(), "b.bin", &payload(2, 8 * 1024));
        let report = scan_duplicates(tmp.path(), 0).unwrap();
        assert!(report.groups.is_empty());
        assert_eq!(report.wasted_bytes, 0);
    }

    #[test]
    fn unique_sizes_short_circuit_before_hashing() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..30 {
            write_bytes(tmp.path(), &format!("f{i}.bin"), &payload(i as u8, 1024 + i));
        }
        let report = scan_duplicates(tmp.path(), 0).unwrap();
        assert!(report.groups.is_empty());
    }

    #[test]
    fn min_bytes_filter_excludes_small_files() {
        let tmp = tempfile::tempdir().unwrap();
        let small = payload(9, 100);
        write_bytes(tmp.path(), "a.bin", &small);
        write_bytes(tmp.path(), "b.bin", &small);
        // below threshold, no dupes surfaced
        let report = scan_duplicates(tmp.path(), 4096).unwrap();
        assert!(report.groups.is_empty());
    }

    #[test]
    fn triplicate_group_reports_two_wasted_copies() {
        let tmp = tempfile::tempdir().unwrap();
        let data = payload(5, 16 * 1024);
        write_bytes(tmp.path(), "x/one.bin", &data);
        write_bytes(tmp.path(), "y/two.bin", &data);
        write_bytes(tmp.path(), "z/three.bin", &data);
        let report = scan_duplicates(tmp.path(), 0).unwrap();
        assert_eq!(report.groups.len(), 1);
        let g = &report.groups[0];
        assert_eq!(g.files.len(), 3);
        assert_eq!(g.wasted_bytes, 2 * data.len() as u64);
    }

    #[test]
    fn groups_sorted_desc_by_waste() {
        let tmp = tempfile::tempdir().unwrap();
        // small: 2 copies of 4 KB = 4 KB waste
        let small = payload(1, 4 * 1024);
        write_bytes(tmp.path(), "small/a.bin", &small);
        write_bytes(tmp.path(), "small/b.bin", &small);
        // bigger: 2 copies of 128 KB = 128 KB waste
        let big = payload(9, 128 * 1024);
        write_bytes(tmp.path(), "big/a.bin", &big);
        write_bytes(tmp.path(), "big/b.bin", &big);
        let report = scan_duplicates(tmp.path(), 0).unwrap();
        assert_eq!(report.groups.len(), 2);
        assert!(report.groups[0].wasted_bytes >= report.groups[1].wasted_bytes);
    }

    #[test]
    fn head_hash_pass_handles_collisions_in_tail() {
        // two files with same first 4 KB but different tail, full-hash
        // pass must reject
        let tmp = tempfile::tempdir().unwrap();
        let mut a = vec![42u8; 8 * 1024];
        let mut b = vec![42u8; 8 * 1024];
        a[7_000] = 1;
        b[7_000] = 2;
        write_bytes(tmp.path(), "a.bin", &a);
        write_bytes(tmp.path(), "b.bin", &b);
        let report = scan_duplicates(tmp.path(), 0).unwrap();
        assert!(report.groups.is_empty(), "tail differs, not dupes");
    }

    #[test]
    fn symlinks_are_ignored() {
        #[cfg(unix)]
        {
            let tmp = tempfile::tempdir().unwrap();
            let data = payload(3, 8 * 1024);
            write_bytes(tmp.path(), "real/a.bin", &data);
            // symlink to a.bin would otherwise become a second copy
            let link = tmp.path().join("alias.bin");
            std::os::unix::fs::symlink(tmp.path().join("real/a.bin"), &link).unwrap();
            let report = scan_duplicates(tmp.path(), 0).unwrap();
            assert!(report.groups.is_empty());
        }
    }

    #[test]
    fn hardlinks_are_deduplicated_not_surfaced() {
        #[cfg(unix)]
        {
            let tmp = tempfile::tempdir().unwrap();
            let data = payload(3, 8 * 1024);
            write_bytes(tmp.path(), "a.bin", &data);
            // second name for the same inode. deleting one doesn't reclaim
            // bytes so we must not surface it
            let link = tmp.path().join("a_link.bin");
            std::fs::hard_link(tmp.path().join("a.bin"), &link).unwrap();
            let report = scan_duplicates(tmp.path(), 0).unwrap();
            assert!(
                report.groups.is_empty(),
                "hardlinks share an inode, don't report them",
            );
        }
    }

    #[test]
    fn missing_root_returns_not_found() {
        let err =
            scan_duplicates(Path::new("/definitely/not/a/path/xyz-safai-dupes"), 0)
                .unwrap_err();
        assert!(matches!(err, FindError::NotFound(_)));
    }

    #[test]
    fn empty_directory_returns_empty_report() {
        let tmp = tempfile::tempdir().unwrap();
        let report = scan_duplicates(tmp.path(), 0).unwrap();
        assert_eq!(report.groups.len(), 0);
        assert_eq!(report.total_files_scanned, 0);
        assert_eq!(report.wasted_bytes, 0);
    }

    #[test]
    fn files_within_group_sorted_by_path() {
        // deterministic order in a group keeps UI's keep/delete selector
        // stable across rescans
        let tmp = tempfile::tempdir().unwrap();
        let data = payload(7, 8 * 1024);
        write_bytes(tmp.path(), "z/file.bin", &data);
        write_bytes(tmp.path(), "a/file.bin", &data);
        write_bytes(tmp.path(), "m/file.bin", &data);
        let report = scan_duplicates(tmp.path(), 0).unwrap();
        assert_eq!(report.groups.len(), 1);
        let paths: Vec<&str> = report.groups[0].files.iter().map(|f| f.path.as_str()).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted);
    }

    #[test]
    fn serializes_as_camelcase() {
        let tmp = tempfile::tempdir().unwrap();
        let data = payload(4, 4096);
        write_bytes(tmp.path(), "a.bin", &data);
        write_bytes(tmp.path(), "b.bin", &data);
        let report = scan_duplicates(tmp.path(), 0).unwrap();
        let v = serde_json::to_value(&report).unwrap();
        assert!(v.get("totalFilesScanned").is_some());
        assert!(v.get("totalGroups").is_some());
        assert!(v.get("wastedBytes").is_some());
        assert!(v.get("durationMs").is_some());
        if let Some(g) = v["groups"].as_array().and_then(|a| a.first()) {
            assert!(g.get("bytesEach").is_some());
            assert!(g.get("wastedBytes").is_some());
            if let Some(f) = g["files"].as_array().and_then(|a| a.first()) {
                assert!(f.get("path").is_some());
                assert!(f.get("bytes").is_some());
            }
        }
    }

    #[test]
    fn node_modules_subtree_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let data = payload(1, 8 * 1024);
        // two project dirs with identical artifacts under
        // node_modules/.../build/intermediates/. exactly what flooded the
        // first real-world run
        write_bytes(tmp.path(), "projA/node_modules/react/android/build/libreact.a", &data);
        write_bytes(tmp.path(), "projB/node_modules/react/android/build/libreact.a", &data);
        // and a pair of real user files outside node_modules
        let photo = payload(9, 12 * 1024);
        write_bytes(tmp.path(), "Pictures/trip.jpg", &photo);
        write_bytes(tmp.path(), "Desktop/trip.jpg", &photo);

        let report = scan_duplicates(tmp.path(), 0).unwrap();
        // node_modules pair must not surface, only the real photo pair
        assert_eq!(report.groups.len(), 1, "unexpected groups: {:?}", report.groups);
        let group = &report.groups[0];
        assert!(group.files.iter().all(|f| !f.path.contains("node_modules")));
        assert!(group.files.iter().all(|f| f.path.contains("trip.jpg")));
    }

    #[test]
    fn git_pack_objects_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let data = payload(2, 8 * 1024);
        write_bytes(tmp.path(), "repo/.git/objects/pack/pack.idx", &data);
        write_bytes(tmp.path(), "repo-mirror/.git/objects/pack/pack.idx", &data);
        let report = scan_duplicates(tmp.path(), 0).unwrap();
        assert!(report.groups.is_empty(), ".git contents must not be deduplicated");
    }

    #[test]
    fn pods_and_derived_data_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let data = payload(3, 8 * 1024);
        write_bytes(tmp.path(), "App/Pods/Target/lib.a", &data);
        write_bytes(tmp.path(), "App2/Pods/Target/lib.a", &data);
        write_bytes(tmp.path(), "Library/Developer/Xcode/DerivedData/X/Build/lib.o", &data);
        write_bytes(tmp.path(), "Library/Developer/Xcode/DerivedData/Y/Build/lib.o", &data);
        let report = scan_duplicates(tmp.path(), 0).unwrap();
        assert!(report.groups.is_empty());
    }

    #[test]
    fn perf_guard_synthetic_2k_tree() {
        // 1000 unique + 1000 dupes, must finish in < 15s on any CI box
        let tmp = tempfile::tempdir().unwrap();
        // 1000 unique files with different sizes so size pass prunes all
        for i in 0..1000 {
            write_bytes(
                tmp.path(),
                &format!("uniq/f{i}.bin"),
                &payload((i % 256) as u8, 4096 + i),
            );
        }
        // 500 dupe pairs of 8 KB identical content
        let data = payload(42, 8 * 1024);
        for i in 0..500 {
            write_bytes(tmp.path(), &format!("dup/a/f{i}.bin"), &data);
            write_bytes(tmp.path(), &format!("dup/b/f{i}.bin"), &data);
        }
        let started = std::time::Instant::now();
        let report = scan_duplicates(tmp.path(), 0).unwrap();
        let elapsed = started.elapsed();
        // all 500 pairs collapse to the same blake3 -> one group
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].files.len(), 1000);
        assert!(elapsed.as_secs() < 15, "too slow: {:?}", elapsed);
    }
}
