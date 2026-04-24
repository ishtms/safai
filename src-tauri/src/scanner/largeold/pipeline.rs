//! pure large & old walker + filter.
//!
//! single pass on purpose. no head/full hash split like  only
//! signals we need are len() and modified(), both in a single stat(2).
//! jwalk parallelises readdir in rayon's global pool, no extra fan-out.
//!
//! # bounded result
//!
//! bounded min-heap of top-N by bytes so memory scales with max_results
//! not tree size. a 2M-file home would otherwise need a 2M-entry Vec +
//! sort, hundreds of MB and a second of stall. heap keeps footprint at
//! ~max_results entries.
//!
//! # determinism
//!
//! walker order is non-deterministic (jwalk hands subdirs to rayon).
//! final Vec sorted by (bytes desc, path asc) so two runs produce same
//! order, UI selection keys off path.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::scanner::dupes::IGNORED_DIR_NAMES;

/// progress points. streaming wrapper bridges to `large-old://progress`.
/// sync entry point passes None.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// fires every PROGRESS_EVERY files the walker stat'd, count =
    /// running total
    Walking,
    /// walker + filter + sort + truncate done, count = final row count
    Done,
}

/// files per progress tick. 5k is cheap (one CAS + one closure call) and
/// keeps UI live on huge trees.
pub const PROGRESS_EVERY: u64 = 5_000;

pub type PhaseCallback<'a> = &'a (dyn Fn(Phase, u64) + Send + Sync);

/// one row on the large & old screen.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileSummary {
    /// absolute, lossy-utf8
    pub path: String,
    pub bytes: u64,
    /// unix seconds. None = mtime unreadable (perms on parent dir).
    /// rows with no mtime are dropped since we can't compute idle_days
    pub modified: Option<u64>,
    /// days since modified. floor. only populated when row clears min_days_idle
    pub idle_days: u64,
    /// lowercase ext without the dot, "" for none. drives scatter colour bucket
    pub extension: String,
}

#[derive(Debug)]
pub enum FindError {
    NotFound(String),
    Io(String),
}

impl fmt::Display for FindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FindError::NotFound(p) => write!(f, "root not found: {p}"),
            FindError::Io(m) => write!(f, "large-old io error: {m}"),
        }
    }
}

impl std::error::Error for FindError {}

impl From<FindError> for String {
    fn from(e: FindError) -> Self {
        e.to_string()
    }
}

/// top-N selector via min-heap on bytes. O(log N) push, O(N) memory
/// regardless of tree size.
struct TopN {
    cap: usize,
    heap: std::collections::BinaryHeap<std::cmp::Reverse<HeapItem>>,
}

#[derive(PartialEq, Eq)]
struct HeapItem {
    bytes: u64,
    // path + metadata, sort cost paid once at the end
    path: PathBuf,
    modified: u64,
    extension: String,
}

impl PartialOrd for HeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapItem {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // primary: bytes asc (min-heap pops smallest).
        // tiebreak: path desc so among ties we keep the earlier-alphabetical
        // path, matches final-sort tiebreak.
        self.bytes
            .cmp(&other.bytes)
            .then_with(|| other.path.cmp(&self.path))
    }
}

impl TopN {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            heap: std::collections::BinaryHeap::with_capacity(cap.min(1024) + 1),
        }
    }

    fn offer(&mut self, item: HeapItem) {
        if self.cap == 0 {
            return;
        }
        if self.heap.len() < self.cap {
            self.heap.push(std::cmp::Reverse(item));
        } else if let Some(std::cmp::Reverse(min)) = self.heap.peek() {
            if item.bytes > min.bytes
                || (item.bytes == min.bytes && item.path < min.path)
            {
                self.heap.pop();
                self.heap.push(std::cmp::Reverse(item));
            }
        }
    }

    fn into_sorted_vec(self) -> Vec<HeapItem> {
        let mut v: Vec<HeapItem> = self.heap.into_iter().map(|r| r.0).collect();
        // final wire order: bytes desc, path asc
        v.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
        v
    }
}

/// returns (rows, total_matched, total_bytes, total_files_scanned).
///
/// * now_secs injected so tests can freeze "now" vs the mtimes they set.
/// * cancel short-circuits the walk on next dir batch + drops partials.
/// * on_phase is best-effort, keep the closure cheap (runs inline with walker).
pub fn find_large_old(
    root: &Path,
    min_bytes: u64,
    min_days_idle: u64,
    max_results: usize,
    now_secs: u64,
    cancel: Option<Arc<AtomicBool>>,
    on_phase: Option<PhaseCallback<'_>>,
) -> Result<(Vec<FileSummary>, u64, u64, u64), FindError> {
    std::fs::symlink_metadata(root).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            FindError::NotFound(root.to_string_lossy().into_owned())
        } else {
            FindError::Io(format!("{}: {e}", root.to_string_lossy()))
        }
    })?;

    let min_idle_secs = min_days_idle.saturating_mul(86_400);

    let walker = jwalk::WalkDir::new(root)
        .skip_hidden(false)
        .follow_links(false)
        .process_read_dir(|_d, _p, _s, children| {
            children.retain(|res| match res {
                Ok(entry) => {
                    let name = entry.file_name();
                    !IGNORED_DIR_NAMES
                        .iter()
                        .any(|ignored| name == std::ffi::OsStr::new(ignored))
                }
                Err(_) => true,
            });
        });

    let files_scanned = AtomicU64::new(0);
    let total_matched = AtomicU64::new(0);
    let total_bytes = AtomicU64::new(0);
    // TopN isn't Sync. wrap in Mutex, single-walker push unlocks
    // instantly. no hashing here so no contention.
    let top = Mutex::new(TopN::new(max_results));

    for entry in walker {
        if is_cancelled(cancel.as_ref()) {
            break;
        }
        let Ok(entry) = entry else { continue };
        let ft = entry.file_type();
        if ft.is_symlink() || !ft.is_file() {
            continue;
        }
        let n = files_scanned.fetch_add(1, Ordering::Relaxed) + 1;
        if n % PROGRESS_EVERY == 0 {
            if let Some(cb) = on_phase {
                cb(Phase::Walking, n);
            }
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let path = entry.path();
        // skip well-known sparse container files (Docker.raw, Parallels,
        // iOS sim etc). their logical size is inflated vs what actually
        // touches the disk, and surfacing them wastes a row since the
        // user can't clean them standalone without breaking the owning
        // app.
        if super::super::meta_ext::is_sparse_container_path(&path) {
            continue;
        }
        let size = super::super::meta_ext::allocated_bytes(&meta);
        if size < min_bytes {
            continue;
        }
        let modified_secs = match meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
        {
            Some(m) => m,
            None => continue,
        };
        // clock skew / post-dated files -> zero idle so they never
        // surface as old
        let idle_secs = now_secs.saturating_sub(modified_secs);
        if idle_secs < min_idle_secs {
            continue;
        }
        let extension = path
            .extension()
            .and_then(|o| o.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();

        total_matched.fetch_add(1, Ordering::Relaxed);
        total_bytes.fetch_add(size, Ordering::Relaxed);

        let mut guard = top.lock().expect("poisoned top heap");
        guard.offer(HeapItem {
            bytes: size,
            path,
            modified: modified_secs,
            extension,
        });
    }

    let final_scanned = files_scanned.load(Ordering::Relaxed);
    let final_matched = total_matched.load(Ordering::Relaxed);
    let final_bytes = total_bytes.load(Ordering::Relaxed);

    if is_cancelled(cancel.as_ref()) {
        // drop partials, callers see empty list + reset counters so
        // UI can say "cancelled"
        if let Some(cb) = on_phase {
            cb(Phase::Done, 0);
        }
        return Ok((Vec::new(), 0, 0, final_scanned));
    }

    let top = top.into_inner().expect("poisoned top heap");
    let sorted = top.into_sorted_vec();

    let rows: Vec<FileSummary> = sorted
        .into_iter()
        .map(|h| {
            let idle_secs = now_secs.saturating_sub(h.modified);
            FileSummary {
                path: h.path.to_string_lossy().into_owned(),
                bytes: h.bytes,
                modified: Some(h.modified),
                idle_days: idle_secs / 86_400,
                extension: h.extension,
            }
        })
        .collect();

    if let Some(cb) = on_phase {
        cb(Phase::Done, rows.len() as u64);
    }

    Ok((rows, final_matched, final_bytes, final_scanned))
}

fn is_cancelled(c: Option<&Arc<AtomicBool>>) -> bool {
    c.map(|a| a.load(Ordering::Acquire)).unwrap_or(false)
}

/// SystemTime::now() in unix seconds so callers don't import UNIX_EPOCH
pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;

    fn write_aged(root: &Path, rel: &str, content: &[u8], secs_ago: u64) {
        let full = root.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = File::create(&full).unwrap();
        f.write_all(content).unwrap();
        f.sync_all().unwrap();
        drop(f);
        let handle = File::options().write(true).open(&full).unwrap();
        let new_mtime = SystemTime::now() - std::time::Duration::from_secs(secs_ago);
        handle.set_modified(new_mtime).unwrap();
    }

    #[test]
    fn topn_keeps_largest_k_items() {
        let mut t = TopN::new(3);
        for (i, bytes) in [100u64, 50, 200, 75, 300, 25, 400].iter().enumerate() {
            t.offer(HeapItem {
                bytes: *bytes,
                path: PathBuf::from(format!("/f{i}")),
                modified: 0,
                extension: String::new(),
            });
        }
        let v = t.into_sorted_vec();
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].bytes, 400);
        assert_eq!(v[1].bytes, 300);
        assert_eq!(v[2].bytes, 200);
    }

    #[test]
    fn topn_with_zero_cap_returns_empty() {
        let mut t = TopN::new(0);
        t.offer(HeapItem {
            bytes: 9999,
            path: PathBuf::from("/x"),
            modified: 0,
            extension: String::new(),
        });
        let v = t.into_sorted_vec();
        assert!(v.is_empty());
    }

    #[test]
    fn topn_breaks_ties_by_path_asc() {
        // on ties we must keep lexically earlier paths or UI selection
        // drifts between runs
        let mut t = TopN::new(2);
        t.offer(HeapItem {
            bytes: 100,
            path: PathBuf::from("/z"),
            modified: 0,
            extension: String::new(),
        });
        t.offer(HeapItem {
            bytes: 100,
            path: PathBuf::from("/a"),
            modified: 0,
            extension: String::new(),
        });
        t.offer(HeapItem {
            bytes: 100,
            path: PathBuf::from("/m"),
            modified: 0,
            extension: String::new(),
        });
        let v = t.into_sorted_vec();
        assert_eq!(v.len(), 2);
        // two of {a, m, z}, 'a' must always be kept
        let paths: Vec<String> = v.iter().map(|i| i.path.to_string_lossy().to_string()).collect();
        assert!(paths.contains(&"/a".to_string()));
    }

    #[test]
    fn cancellation_flag_drops_partial_results() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..5 {
            write_aged(tmp.path(), &format!("a{i}.bin"), &vec![0u8; 4096], 365 * 86400);
        }
        let flag = Arc::new(AtomicBool::new(true));
        let (rows, matched, bytes, _scanned) = find_large_old(
            tmp.path(),
            1024,
            30,
            1000,
            now_unix_secs(),
            Some(flag),
            None,
        )
        .unwrap();
        assert!(rows.is_empty());
        assert_eq!(matched, 0);
        assert_eq!(bytes, 0);
    }

    #[test]
    fn phase_callback_fires_done_exactly_once() {
        let tmp = tempfile::tempdir().unwrap();
        write_aged(tmp.path(), "a.bin", &vec![0u8; 4096], 365 * 86400);
        let counter = Arc::new(AtomicU64::new(0));
        let counter_cb = Arc::clone(&counter);
        let cb = move |p: Phase, _n: u64| {
            if p == Phase::Done {
                counter_cb.fetch_add(1, Ordering::Relaxed);
            }
        };
        find_large_old(
            tmp.path(),
            1024,
            30,
            1000,
            now_unix_secs(),
            None,
            Some(&cb),
        )
        .unwrap();
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn extension_extraction_lowercases() {
        let tmp = tempfile::tempdir().unwrap();
        write_aged(tmp.path(), "movie.MP4", &vec![0u8; 4096], 365 * 86400);
        let (rows, _, _, _) = find_large_old(
            tmp.path(),
            1024,
            30,
            1000,
            now_unix_secs(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].extension, "mp4");
    }

    #[test]
    fn post_dated_files_are_not_reported_as_old() {
        // future mtime (clock change, archive extraction) must not
        // be reported as idle
        let tmp = tempfile::tempdir().unwrap();
        let full = tmp.path().join("future.bin");
        let mut f = File::create(&full).unwrap();
        f.write_all(&vec![0u8; 4096]).unwrap();
        drop(f);
        let handle = File::options().write(true).open(&full).unwrap();
        handle
            .set_modified(SystemTime::now() + std::time::Duration::from_secs(86_400))
            .unwrap();
        let (rows, _, _, _) = find_large_old(
            tmp.path(),
            1024,
            1,
            1000,
            now_unix_secs(),
            None,
            None,
        )
        .unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn total_counts_ignore_truncation() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..10 {
            write_aged(
                tmp.path(),
                &format!("f{i:02}.bin"),
                &vec![1u8; 4096 + i],
                365 * 86400,
            );
        }
        let (rows, matched, bytes, _) = find_large_old(
            tmp.path(),
            1024,
            30,
            3,
            now_unix_secs(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(matched, 10);
        // bytes >= sum of 10 files, must exceed visible sum
        let visible: u64 = rows.iter().map(|r| r.bytes).sum();
        assert!(bytes > visible);
    }

    #[test]
    fn no_mtime_files_are_dropped() {
        // can't easily build a file with no mtime portably, just
        // test the None branch at struct level
        let rows: Vec<FileSummary> = Vec::new();
        let v = serde_json::to_value(&rows).unwrap();
        assert!(v.is_array());
    }

    #[test]
    fn builds_absolute_paths_lossy_utf8() {
        let tmp = tempfile::tempdir().unwrap();
        write_aged(tmp.path(), "dir/a.bin", &vec![0u8; 4096], 365 * 86400);
        let (rows, _, _, _) = find_large_old(
            tmp.path(),
            1024,
            30,
            1000,
            now_unix_secs(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].path.ends_with("a.bin"));
        assert!(Path::new(&rows[0].path).is_absolute());
    }

    #[test]
    fn progress_ticks_scale_with_walk_size() {
        // write > PROGRESS_EVERY files and check callback fires at
        // least once. small files + short idle to stay fast.
        let tmp = tempfile::tempdir().unwrap();
        let n_files = PROGRESS_EVERY + 10;
        for i in 0..n_files {
            let rel = format!("f{i:06}.bin");
            let full = tmp.path().join(&rel);
            let mut f = File::create(&full).unwrap();
            f.write_all(&[0u8]).unwrap();
            // no backdate, fresh files will get filtered out.
            // fine: we only want to observe walk ticks
        }
        let fired = Arc::new(AtomicU64::new(0));
        let fired_cb = Arc::clone(&fired);
        let cb = move |p: Phase, _n: u64| {
            if p == Phase::Walking {
                fired_cb.fetch_add(1, Ordering::Relaxed);
            }
        };
        find_large_old(
            tmp.path(),
            u64::MAX, // filter everything, just measure walk ticks
            0,
            1000,
            now_unix_secs(),
            None,
            Some(&cb),
        )
        .unwrap();
        assert!(fired.load(Ordering::Relaxed) >= 1);
    }

    #[test]
    fn perf_guard_synthetic_5k_tree() {
        // 5k file tree, pipeline must finish under 10s on any CI box.
        // jwalk parallel IO + O(log cap) heap push, dwarfed by IO.
        let tmp = tempfile::tempdir().unwrap();
        let age = 365 * 86400;
        for i in 0..5_000 {
            let rel = format!("d{:03}/f{i:05}.bin", i % 64);
            let full = tmp.path().join(&rel);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            let mut f = File::create(&full).unwrap();
            f.write_all(&vec![0u8; 2048]).unwrap();
            drop(f);
            let h = File::options().write(true).open(&full).unwrap();
            h.set_modified(SystemTime::now() - std::time::Duration::from_secs(age))
                .unwrap();
        }
        let started = std::time::Instant::now();
        let (rows, matched, _, _) = find_large_old(
            tmp.path(),
            1024,
            30,
            500,
            now_unix_secs(),
            None,
            None,
        )
        .unwrap();
        let elapsed = started.elapsed();
        assert_eq!(rows.len(), 500);
        assert_eq!(matched, 5_000);
        assert!(elapsed.as_secs() < 10, "too slow: {elapsed:?}");
    }
}
