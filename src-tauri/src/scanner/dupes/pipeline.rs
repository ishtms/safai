//! 3-pass dedup pipeline. algorithm + rationale are in mod.rs. this is
//! the pure impl, no IO state beyond fs + rayon, returns
//! (groups, files_scanned).

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rayon::prelude::*;
use serde::Serialize;

use crate::scanner::fs_guard;

use super::MAX_CANDIDATES_PER_BUCKET;

/// head-pass byte count. 4 KB = one page everywhere, single readahead block
const HEAD_HASH_BYTES: usize = 4096;

/// full-hash streaming buf. 64 KB balances syscall count vs cache pressure
/// from parallel hashers
const FULL_HASH_BUF: usize = 64 * 1024;

/// files per progress tick while walking. duplicate progress events carry
/// only counters until the terminal result, so this stays cheap even on
/// large home directories.
pub const PROGRESS_EVERY: u64 = 1_000;

/// one file in a [`DuplicateGroup`]. absolute path, unix-seconds mtime.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateFile {
    pub path: String,
    pub bytes: u64,
    /// unix seconds. None = mtime unreadable (usually perms on parent dir)
    pub modified: Option<u64>,
}

/// confirmed dupe set, every file is byte-identical. wasted_bytes =
/// (files.len() - 1) * bytes_each
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateGroup {
    /// short prefix of full blake3, handy as a React key
    pub id: String,
    /// full 256-bit blake3 lowercase hex
    pub hash: String,
    pub bytes_each: u64,
    pub files: Vec<DuplicateFile>,
    pub wasted_bytes: u64,
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
            FindError::Io(m) => write!(f, "duplicates io error: {m}"),
        }
    }
}

impl std::error::Error for FindError {}

impl From<FindError> for String {
    fn from(e: FindError) -> Self {
        e.to_string()
    }
}

/// per-candidate metadata collected on the walk pass so later passes
/// don't re-stat. hardlink dedup via (dev, ino) happens in
/// collect_candidates so by the time a Candidate exists the inode key
/// isn't needed on the struct.
struct Candidate {
    path: PathBuf,
    size: u64,
    modified: Option<u64>,
}

/// duplicate pipeline progress. streaming variant turns these into
/// `dupes://progress` events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// walker is still running, counts are best-effort live totals
    Walking,
    /// walker done, with final walked-file and candidate counts
    WalkDone,
    /// size bucketing done, count = files still in play after dropping
    /// unique-size singletons
    SizeGrouped,
    /// head-hash done, count = what's left to full-hash
    HeadHashed,
    /// pipeline done, count = final group count
    Done,
}

/// thread-safe progress callback. args are:
/// `(phase, total_regular_files_walked, candidates_remaining_or_seen)`.
///
/// During `Walking` and `WalkDone`, the third value is the number of files
/// that cleared the minimum-byte filter so far. During later phases it is
/// the number of files still in play after pruning.
pub type PhaseCallback<'a> = &'a (dyn Fn(Phase, u64, u64) + Send + Sync);

/// run the pipeline. returns confirmed groups + total regular file count
/// the walker saw.
///
/// cancel is optional, streaming variant threads it in so the UI's
/// Cancel button short-circuits hash passes. sync API passes None.
///
/// on_phase is optional, streaming variant bridges phase ticks to
/// `dupes://progress` events for live status.
pub fn find_duplicates(
    root: &Path,
    min_bytes: u64,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<(Vec<DuplicateGroup>, u64), FindError> {
    find_duplicates_with_progress(root, min_bytes, cancel, None)
}

pub fn find_duplicates_with_progress(
    root: &Path,
    min_bytes: u64,
    cancel: Option<Arc<AtomicBool>>,
    on_phase: Option<PhaseCallback<'_>>,
) -> Result<(Vec<DuplicateGroup>, u64), FindError> {
    // preflight. surface NotFound sync instead of silently producing
    // an empty result
    std::fs::symlink_metadata(root).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            FindError::NotFound(root.to_string_lossy().into_owned())
        } else {
            FindError::Io(format!("{}: {e}", root.to_string_lossy()))
        }
    })?;

    let (candidates, files_scanned) =
        collect_candidates(root, min_bytes, cancel.as_ref(), on_phase);
    let candidate_count = candidates.len() as u64;
    if let Some(cb) = on_phase {
        cb(Phase::WalkDone, files_scanned, candidate_count);
    }

    if is_cancelled(cancel.as_ref()) {
        return Ok((Vec::new(), files_scanned));
    }

    // pass 1: size buckets, HashMap<u64, Vec<Candidate>>
    let size_buckets = bucket_by(candidates, |c| c.size);
    let size_candidates: Vec<Vec<Candidate>> = size_buckets
        .into_values()
        .filter(|v| v.len() >= 2)
        .map(|v| cap_bucket(v))
        .collect();
    let size_left: u64 = size_candidates.iter().map(|v| v.len() as u64).sum();
    if let Some(cb) = on_phase {
        cb(Phase::SizeGrouped, files_scanned, size_left);
    }

    if is_cancelled(cancel.as_ref()) {
        return Ok((Vec::new(), files_scanned));
    }

    // pass 2: head-hash prune. parallel across buckets + across files
    // within a bucket. rayon flat-map = one workload per file
    let head_hashed: Vec<(Candidate, [u8; 32])> = size_candidates
        .into_par_iter()
        .flat_map_iter(|bucket| {
            bucket.into_iter().filter_map(|c| {
                if is_cancelled(cancel.as_ref()) {
                    return None;
                }
                match head_hash(&c.path) {
                    Ok(h) => Some((c, h)),
                    Err(_) => None,
                }
            })
        })
        .collect();

    // rebucket by (size, head_hash), drop singletons
    let head_buckets = bucket_by(head_hashed, |(c, h)| (c.size, *h));
    let candidates_for_full: Vec<Vec<Candidate>> = head_buckets
        .into_values()
        .filter(|v| v.len() >= 2)
        .map(|v| v.into_iter().map(|(c, _)| c).collect::<Vec<_>>())
        .collect();
    let full_left: u64 = candidates_for_full.iter().map(|v| v.len() as u64).sum();
    if let Some(cb) = on_phase {
        cb(Phase::HeadHashed, files_scanned, full_left);
    }

    if is_cancelled(cancel.as_ref()) {
        return Ok((Vec::new(), files_scanned));
    }

    // pass 3: full-hash in parallel, each worker streams with a bounded buf
    let full_hashed: Vec<(Candidate, [u8; 32])> = candidates_for_full
        .into_par_iter()
        .flat_map_iter(|bucket| {
            bucket.into_iter().filter_map(|c| {
                if is_cancelled(cancel.as_ref()) {
                    return None;
                }
                match full_hash(&c.path) {
                    Ok(h) => Some((c, h)),
                    Err(_) => None,
                }
            })
        })
        .collect();

    // final grouping by full hash
    let full_buckets = bucket_by(full_hashed, |(_, h)| *h);

    let mut groups: Vec<DuplicateGroup> = full_buckets
        .into_iter()
        .filter(|(_, v)| v.len() >= 2)
        .map(|(h, mut v)| {
            // sort by path so wire order is deterministic, UI's keep/delete
            // selector depends on it
            v.sort_by(|a, b| a.0.path.cmp(&b.0.path));
            let bytes_each = v[0].0.size;
            let files: Vec<DuplicateFile> = v
                .into_iter()
                .map(|(c, _)| DuplicateFile {
                    path: c.path.to_string_lossy().into_owned(),
                    bytes: c.size,
                    modified: c.modified,
                })
                .collect();
            let wasted = bytes_each.saturating_mul((files.len() as u64).saturating_sub(1));
            let hex = hex32(&h);
            let id: String = hex.chars().take(12).collect();
            DuplicateGroup {
                id,
                hash: hex,
                bytes_each,
                files,
                wasted_bytes: wasted,
            }
        })
        .collect();

    // groups desc by wasted_bytes, hash tiebreak for determinism
    groups.sort_by(|a, b| {
        b.wasted_bytes
            .cmp(&a.wasted_bytes)
            .then_with(|| a.hash.cmp(&b.hash))
    });

    if let Some(cb) = on_phase {
        cb(Phase::Done, files_scanned, groups.len() as u64);
    }

    Ok((groups, files_scanned))
}

/// subtrees we skip during the dupe walk. these hold package-manager or
/// build-system content that's byte-identical across copies by design
/// (npm installs the same tarball, xcode compiles the same .o, .git
/// dedupes pack objects). cleaning wastes time, the next install/build/gc
/// regenerates identical bytes and the gap can break active work.
///
/// kept deliberately tight: every name here is unambiguous (nobody names
/// their photos folder `node_modules`). broader rules (build, dist, bin)
/// collide with legit user folders, leave those for a user-tunable
/// setting once lands.
pub const IGNORED_DIR_NAMES: &[&str] = &[
    "node_modules", // npm / pnpm / yarn
    ".git",         // git objects + packfiles
    ".hg",          // mercurial
    ".svn",         // subversion
    ".bzr",         // bazaar
    "Pods",         // CocoaPods (iOS)
    "DerivedData",  // Xcode build intermediates
    "__pycache__",  // python bytecode
    ".venv",        // python virtualenv (PEP 405 conventional)
    "venv",         // python virtualenv (older convention)
    "vendor",       // composer / go modules / bundler
    ".gradle",      // gradle daemon + wrapper cache
    ".tox",         // tox test envs
    "target",       // rust cargo / maven build dir
    "Cellar",       // homebrew (macOS)
    ".pnpm-store",  // pnpm content-addressed store
    ".yarn",        // yarn PnP / cache
];

/// walk root, return regular files >= min_bytes. skips symlinks,
/// inode-dedups hardlinks on unix, prunes [`IGNORED_DIR_NAMES`] subtrees
/// so build intermediates never surface as dupes.
fn collect_candidates(
    root: &Path,
    min_bytes: u64,
    cancel: Option<&Arc<AtomicBool>>,
    on_phase: Option<PhaseCallback<'_>>,
) -> (Vec<Candidate>, u64) {
    let root_dev = fs_guard::root_device_id(root);
    let walker = jwalk::WalkDir::new(root)
        .skip_hidden(false)
        .follow_links(false)
        .process_read_dir(move |_depth, _path, _state, children| {
            // drop children matching skip-list + their subtrees here.
            // doing it in process_read_dir prevents jwalk from ever
            // queueing the subtree, way faster than post-walk filter
            children.retain(|res| match res {
                Ok(entry) => {
                    let name = entry.file_name();
                    if IGNORED_DIR_NAMES
                        .iter()
                        .any(|ignored| name == std::ffi::OsStr::new(ignored))
                    {
                        return false;
                    }
                    match entry.metadata() {
                        Ok(meta) => fs_guard::is_on_root_device(root_dev, &meta),
                        Err(_) => true,
                    }
                }
                Err(_) => true, // let outer loop surface the error
            });
        });

    let mut seen_inodes: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    // touch seen_inodes on non-unix to keep the borrow checker happy
    // when inode_key_from always returns None (windows)
    let _ = &mut seen_inodes;
    let mut out: Vec<Candidate> = Vec::new();
    let mut files_scanned: u64 = 0;

    let maybe_emit_walk = |files_scanned: u64, candidates_seen: usize| {
        if files_scanned == 1 || files_scanned % PROGRESS_EVERY == 0 {
            if let Some(cb) = on_phase {
                cb(Phase::Walking, files_scanned, candidates_seen as u64);
            }
        }
    };

    for entry in walker {
        if is_cancelled(cancel) {
            break;
        }
        let Ok(entry) = entry else { continue };
        let ft = entry.file_type();
        if ft.is_symlink() || !ft.is_file() {
            continue;
        }
        files_scanned = files_scanned.saturating_add(1);
        let path = entry.path();
        // sparse container files (Docker.raw etc) would otherwise look
        // like dupes by logical size. skip them.
        if super::super::meta_ext::is_sparse_container_path(&path) {
            maybe_emit_walk(files_scanned, out.len());
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => {
                maybe_emit_walk(files_scanned, out.len());
                continue;
            }
        };
        let size = super::super::meta_ext::allocated_bytes(&meta);
        if size < min_bytes {
            maybe_emit_walk(files_scanned, out.len());
            continue;
        }
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        // hardlink dedup: keep one path per inode
        if let Some(k) = inode_key_from(&meta) {
            if !seen_inodes.insert(k) {
                maybe_emit_walk(files_scanned, out.len());
                continue;
            }
        }
        out.push(Candidate {
            path,
            size,
            modified,
        });
        maybe_emit_walk(files_scanned, out.len());
    }

    (out, files_scanned)
}

fn inode_key_from(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        return Some((meta.dev(), meta.ino()));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        return Some((meta.volume_serial_number()? as u64, meta.file_index()?));
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = meta;
        None
    }
}

fn bucket_by<T, K: std::hash::Hash + Eq>(
    items: impl IntoIterator<Item = T>,
    key_fn: impl Fn(&T) -> K,
) -> HashMap<K, Vec<T>> {
    let mut map: HashMap<K, Vec<T>> = HashMap::new();
    for item in items {
        let k = key_fn(&item);
        map.entry(k).or_default().push(item);
    }
    map
}

fn cap_bucket(mut bucket: Vec<Candidate>) -> Vec<Candidate> {
    if bucket.len() > MAX_CANDIDATES_PER_BUCKET {
        // sort by path so the kept subset is deterministic, not
        // dependent on walker order
        bucket.sort_by(|a, b| a.path.cmp(&b.path));
        bucket.truncate(MAX_CANDIDATES_PER_BUCKET);
    }
    bucket
}

fn head_hash(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut f = File::open(path)?;
    let mut buf = [0u8; HEAD_HASH_BYTES];
    let n = read_up_to(&mut f, &mut buf)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&buf[..n]);
    Ok(*hasher.finalize().as_bytes())
}

fn full_hash(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut f = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; FULL_HASH_BUF];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn read_up_to(f: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut off = 0;
    while off < buf.len() {
        let n = f.read(&mut buf[off..])?;
        if n == 0 {
            break;
        }
        off += n;
    }
    Ok(off)
}

fn hex32(h: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in h {
        // lowercase hex. format! allocates per byte, hand-rolled is O(32)
        const HEX: &[u8; 16] = b"0123456789abcdef";
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

fn is_cancelled(c: Option<&Arc<AtomicBool>>) -> bool {
    c.map(|a| a.load(Ordering::Acquire)).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn write_bytes(root: &Path, rel: &str, content: &[u8]) {
        let full = root.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(&full).unwrap();
        f.write_all(content).unwrap();
    }

    #[test]
    fn head_hash_matches_full_hash_for_sub_4k_file() {
        let tmp = tempfile::tempdir().unwrap();
        let small = vec![7u8; 1024];
        write_bytes(tmp.path(), "a.bin", &small);
        let h1 = head_hash(&tmp.path().join("a.bin")).unwrap();
        let h2 = full_hash(&tmp.path().join("a.bin")).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn head_hash_equal_for_shared_prefix_full_hash_different() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = vec![3u8; 8 * 1024];
        let mut b = vec![3u8; 8 * 1024];
        a[5000] = 99;
        b[5000] = 100;
        write_bytes(tmp.path(), "a.bin", &a);
        write_bytes(tmp.path(), "b.bin", &b);
        let ah = head_hash(&tmp.path().join("a.bin")).unwrap();
        let bh = head_hash(&tmp.path().join("b.bin")).unwrap();
        assert_eq!(ah, bh, "first 4 KB identical");
        let af = full_hash(&tmp.path().join("a.bin")).unwrap();
        let bf = full_hash(&tmp.path().join("b.bin")).unwrap();
        assert_ne!(af, bf, "full contents differ");
    }

    #[test]
    fn cancel_flag_short_circuits_hashing() {
        let tmp = tempfile::tempdir().unwrap();
        let data = vec![1u8; 16 * 1024];
        for i in 0..20 {
            write_bytes(tmp.path(), &format!("a{i}.bin"), &data);
            write_bytes(tmp.path(), &format!("b{i}.bin"), &data);
        }
        let flag = Arc::new(AtomicBool::new(true));
        let (groups, _) = find_duplicates(tmp.path(), 0, Some(flag)).unwrap();
        assert!(groups.is_empty(), "cancelled run, no groups");
    }

    #[test]
    fn hex32_round_trip() {
        let h = [
            0u8, 1, 2, 15, 16, 255, 254, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0,
        ];
        let s = hex32(&h);
        assert!(s.starts_with("0001020f10fffe10"));
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn cap_bucket_enforces_ceiling_deterministically() {
        let mut bucket = Vec::new();
        for i in 0..(MAX_CANDIDATES_PER_BUCKET + 50) {
            bucket.push(Candidate {
                path: PathBuf::from(format!("/tmp/f{i:08}.bin")),
                size: 1024,
                modified: None,
            });
        }
        let capped = cap_bucket(bucket);
        assert_eq!(capped.len(), MAX_CANDIDATES_PER_BUCKET);
        // deterministic subset, first capped item is the lexical min
        assert!(capped[0].path.to_string_lossy().contains("f00000000"));
    }

    #[test]
    fn head_hash_handles_missing_file_as_error() {
        let err = head_hash(Path::new("/definitely/not/here/x.bin")).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn empty_file_hashes_to_blake3_empty() {
        let tmp = tempfile::tempdir().unwrap();
        write_bytes(tmp.path(), "empty.bin", b"");
        let h = full_hash(&tmp.path().join("empty.bin")).unwrap();
        let want = *blake3::hash(b"").as_bytes();
        assert_eq!(h, want);
    }
}
