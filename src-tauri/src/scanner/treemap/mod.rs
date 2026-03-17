//! disk usage treemap.
//!
//! two decoupled pieces:
//!
//! * [`tree`] - parallel jwalk walker, aggregates root into a bounded-depth
//!   size tree. bounded so a 50k-file home doesn't produce a 50k-node graph.
//!   below max_depth = folded into ancestor at exactly that depth.
//!
//! * [`layout`] - pure std squarified treemap (van Wijk / Bruls 1999).
//!   weighted items + bounds -> near-square child rects. pure+total = trivial
//!   to test, runs in microseconds.
//!
//! [`compute_treemap`] composes the two: walk -> aggregate -> lay out top-level
//! children. drill-down = frontend re-calls with a deeper root. UI just draws rects.

pub mod cache;
pub mod layout;
pub mod stream;
pub mod tree;

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

pub use cache::TreemapCache;
pub use layout::{squarify, Rect};
pub use stream::{
    next_treemap_handle_id, preflight_root, run_treemap_stream, TreemapController, TreemapEmit,
    TreemapHandle, TreemapRegistry,
};
pub use tree::{build_tree, TreeBuildError, TreeNode};

/// tail-fold cap. beyond this = single "...and N more" slot. node_modules
/// with 2000 children = unclickable pixel rects otherwise.
pub const DEFAULT_MAX_LAID_OUT: usize = 64;

/// min fraction of parent area to get its own rect. smaller = folded into
/// "other" bucket. matches WinDirStat/DaisyDisk defaults.
pub const MIN_LAID_OUT_FRACTION: f64 = 0.0025;

/// UI draws this as <svg><rect>. coords in [0,1] x [0,1] so UI renders at any
/// pixel size without re-asking rust.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TreemapTile {
    /// path into tree. empty = root's direct children. `["docs"]` = docs child
    /// of laid-out node. 2-element never produced here, we only lay out one
    /// level. frontend drills down by re-calling with new root.
    pub key: String,
    pub name: String,
    /// abs path, or "...other" for the synthetic bucket
    pub path: String,
    pub bytes: u64,
    pub file_count: u64,
    pub is_dir: bool,
    pub rect: Rect,
    /// true for the synthetic "...and N more". UI renders muted + disables drill-down.
    pub is_other: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreemapResponse {
    /// abs path of walked root, echoed so UI breadcrumb is stable
    pub root: String,
    /// total bytes in subtree including folded entries. matches sum(tiles.bytes).
    pub total_bytes: u64,
    pub total_files: u64,
    /// positioned rects in unit square, ready to render
    pub tiles: Vec<TreemapTile>,
    /// top-N biggest folders for sidebar
    pub biggest: Vec<BiggestFolder>,
    pub duration_ms: u64,
}

/// distinct from [`TreemapTile`] so the list can surface folders 2+ levels deep
/// (tiles are laid out one level only).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BiggestFolder {
    pub path: String,
    pub name: String,
    pub bytes: u64,
    pub file_count: u64,
    /// depth beneath scan root, 1 = direct children
    pub depth: u32,
}

/// walks + aggregates to max_depth, lays out top-level children as a squarified
/// treemap, picks top-N folders for sidebar.
///
/// max_depth clamped: 0 makes no sense, >12 balloons memory. frontend passes 4-6.
pub fn compute_treemap(
    root: &Path,
    max_depth: usize,
    max_laid_out: usize,
) -> Result<TreemapResponse, TreeBuildError> {
    let started = Instant::now();
    let max_depth = max_depth.clamp(1, 12);
    let max_laid_out = max_laid_out.clamp(4, 512);

    let tree = build_tree(root, max_depth)?;

    let biggest = tree.biggest_folders(16);

    // lay out root's direct children. UI treats tile click as "re-invoke with
    // this path as root".
    let tiles = lay_out_children(&tree, max_laid_out);

    Ok(TreemapResponse {
        root: tree.path.clone(),
        total_bytes: tree.bytes,
        total_files: tree.file_count,
        tiles,
        biggest,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

/// tail-fold: child below MIN_LAID_OUT_FRACTION of parent area, or past
/// max_laid_out, merges into "...other" so small slices aren't pixel noise.
pub(crate) fn lay_out_children_of(tree: &TreeNode, max_laid_out: usize) -> Vec<TreemapTile> {
    lay_out_children(tree, max_laid_out)
}

fn lay_out_children(tree: &TreeNode, max_laid_out: usize) -> Vec<TreemapTile> {
    // children arrive pre-sorted desc by bytes from tree::build_tree
    if tree.children.is_empty() || tree.bytes == 0 {
        return Vec::new();
    }

    let parent_bytes = tree.bytes as f64;
    let threshold = (parent_bytes * MIN_LAID_OUT_FRACTION).max(1.0);

    let mut visible: Vec<&TreeNode> = Vec::new();
    let mut other_bytes: u64 = 0;
    let mut other_files: u64 = 0;
    let mut other_count: usize = 0;

    for (idx, child) in tree.children.iter().enumerate() {
        let below_threshold = (child.bytes as f64) < threshold;
        let over_budget = idx >= max_laid_out;
        if below_threshold || over_budget {
            other_bytes = other_bytes.saturating_add(child.bytes);
            other_files = other_files.saturating_add(child.file_count);
            other_count += 1;
        } else {
            visible.push(child);
        }
    }

    let mut weights: Vec<(String, f64)> = visible
        .iter()
        .map(|c| (c.name.clone(), c.bytes as f64))
        .collect();
    if other_bytes > 0 {
        weights.push((other_key(other_count), other_bytes as f64));
    }

    let rects = squarify(&weights, Rect::unit());

    let mut tiles: Vec<TreemapTile> = Vec::with_capacity(rects.len());
    for (i, rect) in rects.into_iter().enumerate() {
        if i < visible.len() {
            let child = visible[i];
            tiles.push(TreemapTile {
                key: child.name.clone(),
                name: child.name.clone(),
                path: child.path.clone(),
                bytes: child.bytes,
                file_count: child.file_count,
                is_dir: child.is_dir,
                rect,
                is_other: false,
            });
        } else {
            tiles.push(TreemapTile {
                key: "__other__".into(),
                name: format!("…{} more", other_count),
                path: PathBuf::from(&tree.path)
                    .join("__other__")
                    .to_string_lossy()
                    .into_owned(),
                bytes: other_bytes,
                file_count: other_files,
                is_dir: false,
                rect,
                is_other: true,
            });
        }
    }
    tiles
}

fn other_key(count: usize) -> String {
    format!("…{} more", count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_file(root: &Path, rel: &str, size: usize) {
        let full = root.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let f = fs::File::create(&full).unwrap();
        f.set_len(size as u64).unwrap();
    }

    #[test]
    fn treemap_happy_path_sums_match() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "a/1.bin", 1000);
        write_file(tmp.path(), "a/2.bin", 500);
        write_file(tmp.path(), "b/x.bin", 2000);
        write_file(tmp.path(), "c.bin", 300);

        let res = compute_treemap(tmp.path(), 4, 64).unwrap();
        assert_eq!(res.total_bytes, 1000 + 500 + 2000 + 300);
        assert_eq!(res.total_files, 4);
        // tiles sum = parent bytes
        let tile_bytes: u64 = res.tiles.iter().map(|t| t.bytes).sum();
        assert_eq!(tile_bytes, res.total_bytes);
    }

    #[test]
    fn biggest_folders_picks_largest_first() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "small/a.bin", 10);
        write_file(tmp.path(), "medium/a.bin", 100);
        write_file(tmp.path(), "huge/a.bin", 10_000);
        write_file(tmp.path(), "huge/b.bin", 5_000);

        let res = compute_treemap(tmp.path(), 4, 64).unwrap();
        assert!(!res.biggest.is_empty());
        assert_eq!(res.biggest[0].name, "huge");
    }

    #[test]
    fn small_children_fold_into_other_bucket() {
        let tmp = tempfile::tempdir().unwrap();
        // one dominant child, many tinies
        write_file(tmp.path(), "big/a.bin", 1_000_000);
        for i in 0..40 {
            write_file(tmp.path(), &format!("tiny_{i}/a.bin"), 10);
        }
        let res = compute_treemap(tmp.path(), 4, 64).unwrap();
        // "big" present, most tinies collapse
        assert!(res.tiles.iter().any(|t| t.name == "big"));
        assert!(res.tiles.iter().any(|t| t.is_other));
        // visible + other << 41 raw children
        assert!(res.tiles.len() < 20, "got {} tiles", res.tiles.len());
    }

    #[test]
    fn max_laid_out_bounds_visible_tiles() {
        let tmp = tempfile::tempdir().unwrap();
        // equal-sized so none fall under fraction threshold, only max_laid_out applies
        for i in 0..12 {
            write_file(tmp.path(), &format!("dir_{i:02}/a.bin"), 10_000);
        }
        let res = compute_treemap(tmp.path(), 4, 4).unwrap();
        // 4 visible + 1 other
        let visible = res.tiles.iter().filter(|t| !t.is_other).count();
        let other = res.tiles.iter().filter(|t| t.is_other).count();
        assert_eq!(visible, 4, "expected 4 visible, got {visible}");
        assert_eq!(other, 1);
    }

    #[test]
    fn nonexistent_root_returns_error() {
        let err = compute_treemap(
            Path::new("/definitely/does/not/exist/safai-t-6-xyz"),
            4,
            64,
        )
        .unwrap_err();
        assert!(matches!(err, TreeBuildError::NotFound(_)));
    }

    #[test]
    fn empty_directory_returns_empty_tiles() {
        let tmp = tempfile::tempdir().unwrap();
        let res = compute_treemap(tmp.path(), 4, 64).unwrap();
        assert_eq!(res.total_bytes, 0);
        assert_eq!(res.total_files, 0);
        assert!(res.tiles.is_empty());
        assert!(res.biggest.is_empty());
    }

    #[test]
    fn file_root_returns_single_self_node() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("lonely.bin");
        fs::File::create(&f).unwrap().set_len(1234).unwrap();
        let res = compute_treemap(&f, 4, 64).unwrap();
        assert_eq!(res.total_bytes, 1234);
        assert_eq!(res.total_files, 1);
        // no children to lay out
        assert!(res.tiles.is_empty());
    }

    #[test]
    fn tiles_stay_within_unit_square() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..10 {
            write_file(tmp.path(), &format!("d{i}/a.bin"), 100 + (i as usize) * 10);
        }
        let res = compute_treemap(tmp.path(), 4, 64).unwrap();
        for t in &res.tiles {
            assert!(t.rect.x >= 0.0 && t.rect.x <= 1.0);
            assert!(t.rect.y >= 0.0 && t.rect.y <= 1.0);
            assert!(t.rect.x + t.rect.w <= 1.0 + 1e-6);
            assert!(t.rect.y + t.rect.h <= 1.0 + 1e-6);
        }
    }

    #[test]
    fn deep_tree_respects_max_depth() {
        let tmp = tempfile::tempdir().unwrap();
        // create a/b/c/d/e.bin
        write_file(tmp.path(), "a/b/c/d/e.bin", 1000);
        // max_depth=2: "a" is top child, b/c/d/e.bin bytes fold into one node at depth 2
        let res = compute_treemap(tmp.path(), 2, 64).unwrap();
        assert_eq!(res.total_bytes, 1000);
        // one top-level tile: "a"
        assert_eq!(res.tiles.len(), 1);
        assert_eq!(res.tiles[0].name, "a");
    }

    #[test]
    fn serializes_as_camelcase() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "x/y.bin", 100);
        let res = compute_treemap(tmp.path(), 3, 64).unwrap();
        let v = serde_json::to_value(&res).unwrap();
        assert!(v.get("totalBytes").is_some());
        assert!(v.get("totalFiles").is_some());
        assert!(v.get("durationMs").is_some());
        if let Some(t) = v["tiles"].as_array().and_then(|a| a.first()) {
            assert!(t.get("fileCount").is_some());
            assert!(t.get("isDir").is_some());
            assert!(t.get("isOther").is_some());
            assert!(t["rect"].get("w").is_some());
        }
    }
}
