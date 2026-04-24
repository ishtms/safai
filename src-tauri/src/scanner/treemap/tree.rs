//! bounded-depth directory-size tree.
//!
//! # algorithm
//!
//! 1. jwalk::WalkDir streams every entry under root. jwalk pushes readdir+stat
//!    onto rayon's global pool so we get parallel IO without locks.
//! 2. consume entries on main thread, mutate tree directly. no mutex, no
//!    cross-thread builder. bookkeeping is O(entries), lock-free.
//! 3. for each file, climb ancestor chain from root down to max_depth (max),
//!    lazily creating nodes + adding bytes to every ancestor. at the cap the
//!    remaining sub-path folds into the last node. so a 7-level-deep file
//!    under max_depth=2 credits both top-level + second-level, creates no
//!    deeper nodes.
//!
//! # why climb per file
//!
//! classic alt: walk whole tree into memory then post-order sum. costs O(all
//! entries) of node memory even when UI only wants top-level totals. ours is
//! O(nodes within max_depth), which is what UI actually renders.
//!
//! # sorting
//!
//! children sorted desc by bytes before returning. treemap layout wants
//! pre-sorted input (squarified assumes decreasing weight).

use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use super::BiggestFolder;

/// walk failed before any result. variants reflect preflight checks on root.
#[derive(Debug)]
pub enum TreeBuildError {
    NotFound(String),
    Io(String),
}

impl fmt::Display for TreeBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TreeBuildError::NotFound(p) => write!(f, "root not found: {p}"),
            TreeBuildError::Io(m) => write!(f, "root io error: {m}"),
        }
    }
}

impl std::error::Error for TreeBuildError {}

impl From<TreeBuildError> for String {
    fn from(e: TreeBuildError) -> Self {
        e.to_string()
    }
}

/// wire type. frontend consumes directly for raw tree. drill-down is per-level
/// so it rarely sees more than one node's children at once.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeNode {
    pub name: String,
    pub path: String,
    pub bytes: u64,
    pub file_count: u64,
    pub is_dir: bool,
    /// direct children, sorted desc by bytes
    pub children: Vec<TreeNode>,
}

// --- builder rep ---
// HashMap for O(1) insert, flattened to sorted Vec on output.
//
// pub(super) so stream.rs can own a BuildNode + snapshot periodically without
// leaking the file-level API.

pub(super) struct BuildNode {
    pub(super) name: String,
    pub(super) path: PathBuf,
    pub(super) bytes: u64,
    pub(super) file_count: u64,
    pub(super) is_dir: bool,
    pub(super) children: HashMap<OsString, BuildNode>,
}

impl BuildNode {
    pub(super) fn new_dir(name: String, path: PathBuf) -> Self {
        Self {
            name,
            path,
            bytes: 0,
            file_count: 0,
            is_dir: true,
            children: HashMap::new(),
        }
    }

    pub(super) fn into_node(self) -> TreeNode {
        let mut children: Vec<TreeNode> =
            self.children.into_values().map(BuildNode::into_node).collect();
        children.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
        TreeNode {
            name: self.name,
            path: self.path.to_string_lossy().into_owned(),
            bytes: self.bytes,
            file_count: self.file_count,
            is_dir: self.is_dir,
            children,
        }
    }

    /// non-consuming. allocates a TreeNode tree at current state. O(nodes),
    /// cheap because max_depth bounds the tree. called once per progress tick.
    pub(super) fn snapshot(&self) -> TreeNode {
        let mut children: Vec<TreeNode> =
            self.children.values().map(BuildNode::snapshot).collect();
        children.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
        TreeNode {
            name: self.name.clone(),
            path: self.path.to_string_lossy().into_owned(),
            bytes: self.bytes,
            file_count: self.file_count,
            is_dir: self.is_dir,
            children,
        }
    }
}

impl TreeNode {
    /// top-N folders by size anywhere in the tree. iterative walk with bounded
    /// min-heap, O(nodes * log N) with N=16. microseconds on a bounded tree.
    pub fn biggest_folders(&self, n: usize) -> Vec<BiggestFolder> {
        if n == 0 || self.children.is_empty() {
            return Vec::new();
        }
        use std::cmp::Reverse;
        use std::collections::BinaryHeap;

        // min-heap on bytes, keep N largest by popping smallest
        let mut heap: BinaryHeap<Reverse<(u64, u64, String, String, u32)>> = BinaryHeap::new();
        // (node, depth)
        let mut stack: Vec<(&TreeNode, u32)> =
            self.children.iter().map(|c| (c, 1u32)).collect();

        while let Some((node, depth)) = stack.pop() {
            if node.is_dir && node.bytes > 0 {
                // name tiebreak = deterministic sort
                let key = (
                    node.bytes,
                    node.file_count,
                    node.name.clone(),
                    node.path.clone(),
                    depth,
                );
                if heap.len() < n {
                    heap.push(Reverse(key));
                } else if let Some(Reverse(smallest)) = heap.peek() {
                    if key > *smallest {
                        heap.pop();
                        heap.push(Reverse(key));
                    }
                }
            }
            for child in &node.children {
                stack.push((child, depth + 1));
            }
        }

        let mut out: Vec<BiggestFolder> = heap
            .into_iter()
            .map(|Reverse((bytes, file_count, name, path, depth))| BiggestFolder {
                path,
                name,
                bytes,
                file_count,
                depth,
            })
            .collect();
        out.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
        out
    }
}

/// see module docs for algorithm
pub fn build_tree(root: &Path, max_depth: usize) -> Result<TreeNode, TreeBuildError> {
    let meta = std::fs::symlink_metadata(root).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            TreeBuildError::NotFound(root.to_string_lossy().into_owned())
        } else {
            TreeBuildError::Io(format!("{}: {e}", root.to_string_lossy()))
        }
    })?;

    let root_name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.to_string_lossy().into_owned());

    // If the user handed us a single file, short-circuit to a leaf.
    if meta.is_file() {
        return Ok(TreeNode {
            name: root_name,
            path: root.to_string_lossy().into_owned(),
            bytes: super::super::meta_ext::allocated_bytes(&meta),
            file_count: 1,
            is_dir: false,
            children: Vec::new(),
        });
    }

    let mut root_node = BuildNode::new_dir(root_name, root.to_path_buf());

    // Walk everything under `root`. `follow_links(false)` prevents looping
    // through mount points / recursive symlinks; `skip_hidden(false)`
    // because dotfiles can be arbitrarily large on Linux/mac.
    let walker = jwalk::WalkDir::new(root)
        .skip_hidden(false)
        .follow_links(false);

    for entry in walker {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        // jwalk's first yield is the root itself — skip.
        if path == root {
            continue;
        }
        // Only files contribute bytes. Directory sizes are meaningless on
        // most FS (they report inode table size, not contents).
        let ft = entry.file_type();
        if ft.is_symlink() {
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        let bytes = entry
            .metadata()
            .map(|m| super::super::meta_ext::allocated_bytes(&m))
            .unwrap_or(0);

        let rel = match path.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        insert_file(&mut root_node, root, rel, bytes, max_depth);
    }

    Ok(root_node.into_node())
}

pub(super) fn insert_file(
    root: &mut BuildNode,
    root_path: &Path,
    rel: &Path,
    bytes: u64,
    max_depth: usize,
) {
    // Always credit the root with this file's bytes — keeps `tree.bytes`
    // equal to the true subtree total regardless of depth cap.
    root.bytes = root.bytes.saturating_add(bytes);
    root.file_count = root.file_count.saturating_add(1);

    let components: Vec<&std::ffi::OsStr> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s),
            // `..` in a rel path under the scan root should not happen for
            // a clean jwalk; defensively skip so we can't escape root.
            _ => None,
        })
        .collect();

    if components.is_empty() {
        return;
    }

    let mut cur = root;
    let mut path_so_far = root_path.to_path_buf();

    for (i, comp) in components.iter().enumerate() {
        if i >= max_depth {
            // Fold: credit bytes to the deepest allowed ancestor (`cur`)
            // and stop creating nodes. Note `cur.bytes` was already
            // incremented when we visited this node, so we're done.
            return;
        }

        path_so_far.push(comp);
        let is_last = i == components.len() - 1;
        let key = comp.to_os_string();
        let name = comp.to_string_lossy().into_owned();

        let child_path = path_so_far.clone();
        let child = cur.children.entry(key).or_insert_with(|| {
            let mut n = BuildNode::new_dir(name, child_path);
            // If this is the file itself, it's not a directory.
            if is_last {
                n.is_dir = false;
            }
            n
        });

        child.bytes = child.bytes.saturating_add(bytes);
        child.file_count = child.file_count.saturating_add(1);
        cur = child;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_file(root: &Path, rel: &str, size: u64) {
        let full = root.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let f = fs::File::create(&full).unwrap();
        f.set_len(size).unwrap();
    }

    #[test]
    fn single_file_root_is_leaf() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("lonely.bin");
        fs::File::create(&p).unwrap().set_len(42).unwrap();
        let t = build_tree(&p, 4).unwrap();
        assert_eq!(t.bytes, 42);
        assert_eq!(t.file_count, 1);
        assert!(!t.is_dir);
        assert!(t.children.is_empty());
    }

    #[test]
    fn empty_directory_has_zero_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let t = build_tree(tmp.path(), 4).unwrap();
        assert_eq!(t.bytes, 0);
        assert_eq!(t.file_count, 0);
        assert!(t.is_dir);
        assert!(t.children.is_empty());
    }

    #[test]
    fn children_are_sorted_desc_by_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "small/a.bin", 10);
        write_file(tmp.path(), "medium/a.bin", 100);
        write_file(tmp.path(), "huge/a.bin", 10_000);
        let t = build_tree(tmp.path(), 3).unwrap();
        assert_eq!(t.children[0].name, "huge");
        assert_eq!(t.children[1].name, "medium");
        assert_eq!(t.children[2].name, "small");
    }

    #[test]
    fn subtree_bytes_match_file_sum() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "a/b/c/d.bin", 1000);
        write_file(tmp.path(), "a/e.bin", 500);
        write_file(tmp.path(), "f/g.bin", 2000);
        let t = build_tree(tmp.path(), 10).unwrap();
        assert_eq!(t.bytes, 3500);
        assert_eq!(t.file_count, 3);
        let a = t.children.iter().find(|c| c.name == "a").unwrap();
        assert_eq!(a.bytes, 1500);
        let f = t.children.iter().find(|c| c.name == "f").unwrap();
        assert_eq!(f.bytes, 2000);
    }

    #[test]
    fn max_depth_folds_deep_bytes_into_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "a/b/c/d/e.bin", 1000);
        // max_depth=2 → "a" and "a/b" exist, nothing below.
        let t = build_tree(tmp.path(), 2).unwrap();
        assert_eq!(t.bytes, 1000);
        let a = t.children.iter().find(|c| c.name == "a").unwrap();
        assert_eq!(a.bytes, 1000);
        assert_eq!(a.children.len(), 1);
        assert_eq!(a.children[0].name, "b");
        // "b" has the bytes but no children — they were folded in.
        assert_eq!(a.children[0].bytes, 1000);
        assert!(a.children[0].children.is_empty());
    }

    #[test]
    fn symlinks_are_not_followed_or_counted() {
        #[cfg(unix)]
        {
            let tmp = tempfile::tempdir().unwrap();
            write_file(tmp.path(), "real/file.bin", 100);
            // Create a symlink pointing back at the root — would loop
            // without `follow_links(false)`.
            let link = tmp.path().join("loop");
            std::os::unix::fs::symlink(tmp.path(), &link).unwrap();
            let t = build_tree(tmp.path(), 5).unwrap();
            // Only the real file's bytes count.
            assert_eq!(t.bytes, 100);
            assert_eq!(t.file_count, 1);
        }
    }

    #[test]
    fn missing_root_returns_not_found() {
        let err =
            build_tree(Path::new("/definitely/not/a/path/xyz-safai-6"), 3).unwrap_err();
        assert!(matches!(err, TreeBuildError::NotFound(_)));
    }

    #[test]
    fn biggest_folders_picks_descendants_too() {
        let tmp = tempfile::tempdir().unwrap();
        // A deeply nested huge folder; its ancestor is also huge but the
        // leaf folder itself is interesting to surface separately.
        write_file(tmp.path(), "projects/myapp/node_modules/big.bin", 50_000);
        write_file(tmp.path(), "projects/myapp/src/small.bin", 500);
        write_file(tmp.path(), "projects/other/file.bin", 100);
        let t = build_tree(tmp.path(), 10).unwrap();
        let biggest = t.biggest_folders(5);
        assert!(!biggest.is_empty());
        // "projects" (ancestor) + "myapp" + "node_modules" all qualify.
        let names: Vec<&str> = biggest.iter().map(|b| b.name.as_str()).collect();
        assert!(names.contains(&"projects"));
        assert!(names.contains(&"node_modules"));
        // Depth of node_modules is 3 under the tmp root.
        let nm = biggest.iter().find(|b| b.name == "node_modules").unwrap();
        assert_eq!(nm.depth, 3);
    }

    #[test]
    fn biggest_folders_respects_n_cap() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..20 {
            write_file(tmp.path(), &format!("d_{i:02}/f.bin"), 100 + i as u64);
        }
        let t = build_tree(tmp.path(), 2).unwrap();
        let top = t.biggest_folders(5);
        assert_eq!(top.len(), 5);
        // Desc by bytes.
        for pair in top.windows(2) {
            assert!(pair[0].bytes >= pair[1].bytes);
        }
    }

    #[test]
    fn file_in_root_creates_leaf_child() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "top.bin", 777);
        let t = build_tree(tmp.path(), 3).unwrap();
        assert_eq!(t.bytes, 777);
        assert_eq!(t.children.len(), 1);
        assert_eq!(t.children[0].name, "top.bin");
        assert!(!t.children[0].is_dir);
        assert_eq!(t.children[0].bytes, 777);
    }

    #[test]
    fn perf_guard_synthetic_10k_tree() {
        // Build a synthetic 10k-file tree in tmp and assert the walk +
        // aggregate finishes in a reasonable wall time on CI-class hw.
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..10_000 {
            let dir = i % 100;
            write_file(tmp.path(), &format!("bucket_{dir:03}/f_{i:05}.bin"), 1);
        }
        let started = std::time::Instant::now();
        let t = build_tree(tmp.path(), 4).unwrap();
        let elapsed = started.elapsed();
        assert_eq!(t.file_count, 10_000);
        assert_eq!(t.bytes, 10_000);
        // Very loose ceiling — on a modern laptop this is well under 1s.
        // 10s is enough slack that a loaded CI box won't flake.
        assert!(
            elapsed.as_secs() < 10,
            "synthetic 10k walk too slow: {:?}",
            elapsed,
        );
    }

    #[test]
    fn subtree_bytes_greater_than_zero_cap_doesnt_lose_bytes() {
        // Regression: even at max_depth=1 (only direct children get nodes),
        // total bytes at the root must still equal the real sum.
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "a/b/c.bin", 100);
        write_file(tmp.path(), "a/d/e.bin", 200);
        let t = build_tree(tmp.path(), 1).unwrap();
        assert_eq!(t.bytes, 300);
        assert_eq!(t.children.len(), 1);
        assert_eq!(t.children[0].name, "a");
        assert_eq!(t.children[0].bytes, 300);
        // No grandchildren at depth 1.
        assert!(t.children[0].children.is_empty());
    }
}
