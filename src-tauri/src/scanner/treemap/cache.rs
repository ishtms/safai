//! in-memory cache of completed treemap scans.
//!
//! # why
//!
//! a full $HOME scan walks 250k files. the resulting [`TreeNode`] holds every
//! descendant up to max_depth. without a cache, each drill-down + back rewalks
//! from scratch even though the parent already has the data. this serves
//! descendants of any prev-scanned root in O(cached-nodes), pure RAM.
//!
//! # semantics
//!
//! * (root, tree, max_depth) stored on every successful scan via [`TreemapCache::store`]
//! * [`TreemapCache::serve`] descends from any cached ancestor root
//! * multiple roots covering target: longest (closest) wins
//! * can't distinguish "empty dir" from "folded at max_depth", so we miss
//!   conservatively when cached target is a dir with zero children + non-zero
//!   bytes. caller falls through to a real walk.
//! * clear is only meaningful on user-initiated rescans. otherwise data is
//!   immutable per-session and cache is correct forever.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use super::tree::TreeNode;
use super::{lay_out_children_of, now_unix, TreemapResponse};

/// memory scales with max_depth + tree fan-out. typical home scan = few hundred KB.
struct CachedScan {
    tree: TreeNode,
    scanned_at: u64,
    /// depth cap when built. kept so future heuristics (e.g. "only serve within
    /// N levels of cap") have context without a re-walk.
    #[allow(dead_code)]
    max_depth: usize,
}

/// process-wide cache, managed as a tauri State. Mutex serialises writes vs
/// reads. reads are short (descent + layout) so contention isn't an issue for
/// interactive navigation.
#[derive(Default)]
pub struct TreemapCache {
    inner: Mutex<HashMap<PathBuf, CachedScan>>,
}

impl TreemapCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// overwrites prior entry for the same root. rescan should replace stale data.
    pub fn store(&self, root: PathBuf, tree: TreeNode, max_depth: usize) {
        if let Ok(mut g) = self.inner.lock() {
            g.insert(
                root,
                CachedScan {
                    tree,
                    scanned_at: now_unix(),
                    max_depth,
                },
            );
        }
    }

    /// forget everything. called on explicit Rescan so user sees fresh numbers
    /// even if fs changed since snapshot.
    pub fn clear(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.clear();
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// build a TreemapResponse for target without a walk.
    ///
    /// 1. find longest cached root that equals target or is an ancestor
    /// 2. descend by remaining path components. missing component = drifted
    ///    out of tree, fall through to fresh walk.
    /// 3. non-empty dir = lay out children + biggest_folders + return. else
    ///    None so caller does a real scan.
    pub fn serve(&self, target: &Path, max_laid_out: usize) -> Option<TreemapResponse> {
        let g = self.inner.lock().ok()?;
        let (root, scan) = longest_ancestor(&g, target)?;

        // walk from cached root down by consuming each path component
        let rel = target.strip_prefix(root).ok()?;
        let mut node = &scan.tree;
        for comp in rel.components() {
            let name_os = match comp {
                Component::Normal(s) => s,
                // `..`/`.`/prefix/root under stripped-prefix is pathological,
                // miss so a real walk produces whatever fs actually says
                _ => return None,
            };
            let name = name_os.to_string_lossy();
            let child = node.children.iter().find(|c| c.name == name.as_ref())?;
            node = child;
        }

        // depth-cap check. populated dir w/ zero children = walk was truncated
        // at max_depth. empty grid would look like a bug. fall through to real
        // walk, costs the rescan user was trying to avoid but only when we've
        // genuinely run out of data.
        if node.is_dir && node.children.is_empty() && node.bytes > 0 {
            return None;
        }

        let tiles = lay_out_children_of(node, max_laid_out);
        let biggest = node.biggest_folders(16);
        Some(TreemapResponse {
            root: node.path.clone(),
            total_bytes: node.bytes,
            total_files: node.file_count,
            tiles,
            biggest,
            scanned_at: scan.scanned_at,
            // cache hit is essentially instant, report 0 so UI doesn't claim a
            // 3s scan when it came from RAM.
            duration_ms: 0,
        })
    }
}

/// longest key that is target itself or an ancestor. None if no match.
fn longest_ancestor<'a>(
    map: &'a HashMap<PathBuf, CachedScan>,
    target: &Path,
) -> Option<(&'a PathBuf, &'a CachedScan)> {
    map.iter()
        .filter(|(root, _)| root.as_path() == target || target.starts_with(root))
        .max_by_key(|(root, _)| root.components().count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::treemap::tree::build_tree;
    use std::fs;
    use std::sync::Arc;
    use std::thread;

    fn write_file(root: &Path, rel: &str, size: u64) {
        let full = root.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let f = fs::File::create(&full).unwrap();
        f.set_len(size).unwrap();
    }

    #[test]
    fn round_trip_same_root_serves_cached_tiles() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "a/f.bin", 100);
        write_file(tmp.path(), "b/f.bin", 200);

        let tree = build_tree(tmp.path(), 4).unwrap();
        let cache = TreemapCache::new();
        cache.store(tmp.path().to_path_buf(), tree, 4);

        let resp = cache.serve(tmp.path(), 64).expect("hit");
        assert_eq!(resp.total_bytes, 300);
        assert_eq!(resp.total_files, 2);
        // 2 top-level children laid out
        assert_eq!(resp.tiles.len(), 2);
    }

    #[test]
    fn descendant_is_resolved_from_ancestor_cache() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "docs/big/huge.bin", 10_000);
        write_file(tmp.path(), "docs/small.bin", 100);
        write_file(tmp.path(), "other/x.bin", 5);

        let tree = build_tree(tmp.path(), 4).unwrap();
        let cache = TreemapCache::new();
        cache.store(tmp.path().to_path_buf(), tree, 4);

        // `docs` subtree should come from root cache, no fs touch
        let resp = cache.serve(&tmp.path().join("docs"), 64).expect("hit");
        assert_eq!(resp.total_bytes, 10_100);
        assert_eq!(resp.total_files, 2);
        // big + small.bin visible
        assert!(resp.tiles.iter().any(|t| t.name == "big"));
        assert!(resp.tiles.iter().any(|t| t.name == "small.bin"));
    }

    #[test]
    fn longest_ancestor_wins_when_multiple_roots_cover_target() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "docs/a/f.bin", 111);
        write_file(tmp.path(), "docs/a/g.bin", 222);

        let root_tree = build_tree(tmp.path(), 2).unwrap();
        // deeper scan of docs/a has f.bin + g.bin as children. root scan at
        // max_depth=2 would fold them.
        let deep_tree = build_tree(&tmp.path().join("docs").join("a"), 4).unwrap();

        let cache = TreemapCache::new();
        cache.store(tmp.path().to_path_buf(), root_tree, 2);
        cache.store(tmp.path().join("docs").join("a"), deep_tree, 4);

        // serving docs/a uses the deeper cache so we see file-level children
        let resp = cache
            .serve(&tmp.path().join("docs").join("a"), 64)
            .expect("hit");
        assert!(resp.tiles.iter().any(|t| t.name == "f.bin"));
        assert!(resp.tiles.iter().any(|t| t.name == "g.bin"));
    }

    #[test]
    fn miss_returns_none_when_target_is_outside_every_cached_root() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "a/f.bin", 10);
        let tree = build_tree(tmp.path(), 3).unwrap();
        let cache = TreemapCache::new();
        cache.store(tmp.path().to_path_buf(), tree, 3);

        // unrelated path
        assert!(cache
            .serve(Path::new("/not/under/tmp/whatever"), 64)
            .is_none());
    }

    #[test]
    fn miss_returns_none_when_cached_dir_has_no_children_but_bytes() {
        // depth-cap: build_tree(max_depth=1) folds a/b/c.bin into `a` with zero
        // children. serving `a` would return empty tiles, misleading when the
        // dir is really populated.
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "a/b/c.bin", 100);

        let tree = build_tree(tmp.path(), 1).unwrap();
        let cache = TreemapCache::new();
        cache.store(tmp.path().to_path_buf(), tree, 1);

        // target = root still has `a` as child, hit
        assert!(cache.serve(tmp.path(), 64).is_some());
        // target = `a`, in cache but children empty + bytes > 0. miss, caller rescans.
        assert!(cache.serve(&tmp.path().join("a"), 64).is_none());
    }

    #[test]
    fn empty_dir_serves_hit_even_with_zero_children() {
        let tmp = tempfile::tempdir().unwrap();
        let tree = build_tree(tmp.path(), 4).unwrap();
        let cache = TreemapCache::new();
        cache.store(tmp.path().to_path_buf(), tree, 4);

        // empty dir: zero children + zero bytes = valid hit. cap didn't
        // truncate, the dir is genuinely empty.
        let resp = cache.serve(tmp.path(), 64).expect("hit");
        assert_eq!(resp.total_bytes, 0);
        assert!(resp.tiles.is_empty());
    }

    #[test]
    fn clear_forgets_everything() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "a/f.bin", 10);
        let tree = build_tree(tmp.path(), 3).unwrap();
        let cache = TreemapCache::new();
        cache.store(tmp.path().to_path_buf(), tree, 3);
        assert_eq!(cache.len(), 1);
        cache.clear();
        assert_eq!(cache.len(), 0);
        assert!(cache.serve(tmp.path(), 64).is_none());
    }

    #[test]
    fn overwriting_a_root_replaces_the_prior_entry() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "a/f.bin", 10);
        let t1 = build_tree(tmp.path(), 3).unwrap();

        // add file + rebuild, simulates rescan after fs change
        write_file(tmp.path(), "a/g.bin", 1_000);
        let t2 = build_tree(tmp.path(), 3).unwrap();

        let cache = TreemapCache::new();
        cache.store(tmp.path().to_path_buf(), t1, 3);
        cache.store(tmp.path().to_path_buf(), t2, 3);

        let resp = cache.serve(tmp.path(), 64).expect("hit");
        assert_eq!(resp.total_bytes, 1_010);
    }

    #[test]
    fn serve_is_safe_under_concurrent_store() {
        // smoke check: reader hammering `serve` doesn't deadlock/crash vs writer
        // hammering `store`. mutex guarantees no torn reads.
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "a/f.bin", 10);
        let seed = build_tree(tmp.path(), 3).unwrap();
        let cache = Arc::new(TreemapCache::new());
        cache.store(tmp.path().to_path_buf(), seed.clone(), 3);

        let cache_w = cache.clone();
        let root = tmp.path().to_path_buf();
        let writer = thread::spawn(move || {
            for _ in 0..200 {
                cache_w.store(root.clone(), seed.clone(), 3);
            }
        });

        let cache_r = cache.clone();
        let root = tmp.path().to_path_buf();
        let reader = thread::spawn(move || {
            let mut hits = 0usize;
            for _ in 0..200 {
                if cache_r.serve(&root, 64).is_some() {
                    hits += 1;
                }
            }
            hits
        });

        writer.join().unwrap();
        let hits = reader.join().unwrap();
        // nearly every read should land. slack for scheduler jitter (we don't
        // clear here so in practice it's all hits).
        assert!(hits > 100, "expected most reads to hit, got {hits}");
    }

    #[test]
    fn serve_returns_zero_duration_ms() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "a/f.bin", 10);
        let tree = build_tree(tmp.path(), 3).unwrap();
        let cache = TreemapCache::new();
        cache.store(tmp.path().to_path_buf(), tree, 3);
        let resp = cache.serve(tmp.path(), 64).expect("hit");
        // cache hits are RAM-local, don't claim scan duration
        assert_eq!(resp.duration_ms, 0);
    }
}
