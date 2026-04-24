//! streaming treemap walker.
//!
//! compute_treemap (in mod.rs) is the sync version, fine for tests + sub-second
//! roots. 250k+ file homes would stare at "0 B" for a bit, so this streams:
//!
//! * walker owns a single [`BuildNode`] on its thread
//! * every [`PROGRESS_THROTTLE`] (~150ms) snapshots tree + emits a
//!   `treemap://progress` with full [`TreemapResponse`]. snapshot is cheap,
//!   O(nodes within max_depth).
//! * terminal `treemap://done` carries final sorted response after walk ends
//!   (or is cancelled).
//!
//! [`TreemapRegistry`] (managed by tauri) tracks running walks so UI can cancel.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::tree::{insert_file, BuildNode, TreeBuildError};
use super::{lay_out_children_of, BiggestFolder, TreemapResponse};

/// min interval between snapshots. bigger than the scanner's 50ms because a
/// full snapshot + layout is heavier than a counter. 150ms feels live while
/// letting IO dominate.
pub const PROGRESS_THROTTLE: Duration = Duration::from_millis(150);

pub struct TreemapController {
    cancelled: AtomicBool,
    files_scanned: AtomicU64,
    bytes_scanned: AtomicU64,
    started: Instant,
}

impl TreemapController {
    pub fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            files_scanned: AtomicU64::new(0),
            bytes_scanned: AtomicU64::new(0),
            started: Instant::now(),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

impl Default for TreemapController {
    fn default() -> Self {
        Self::new()
    }
}

/// tests use a Vec recorder, tauri adapter bridges to AppHandle::emit
pub trait TreemapEmit: Send + Sync {
    fn emit_progress(&self, handle_id: &str, resp: &TreemapResponse);
    fn emit_done(&self, handle_id: &str, resp: &TreemapResponse);
    /// called once on non-cancelled completion with the full tree. tauri adapter
    /// seeds [`super::TreemapCache`] so drill-down + back can serve from RAM
    /// without re-walking. default no-op for tests/non-caching consumers.
    fn on_done_tree(&self, _handle_id: &str, _tree: &super::tree::TreeNode, _max_depth: usize) {}
}

/// UI uses id to correlate streamed events against its requested walk
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreemapHandle {
    pub id: String,
    pub root: String,
}

/// preflight before spawning walker. runs on tauri cmd pool so NotFound surfaces
/// sync - the streaming protocol doesn't carry error events.
pub fn preflight_root(root: &Path) -> Result<PathBuf, TreeBuildError> {
    match std::fs::symlink_metadata(root) {
        Ok(_) => Ok(root.to_path_buf()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(TreeBuildError::NotFound(
            root.to_string_lossy().into_owned(),
        )),
        Err(e) => Err(TreeBuildError::Io(format!(
            "{}: {e}",
            root.to_string_lossy()
        ))),
    }
}

/// drive one walk to completion. blocks, spawn in a dedicated thread.
///
/// every PROGRESS_THROTTLE (+ on completion) calls emit_progress with a
/// freshly-laid-out response. last emission is `done`.
///
/// cancel is checked between every jwalk entry. cancel flips the atomic, walker
/// exits on next tick + emits final `done` with whatever was aggregated.
pub fn run_treemap_stream<E: TreemapEmit>(
    handle_id: String,
    root: PathBuf,
    max_depth: usize,
    max_laid_out: usize,
    ctrl: Arc<TreemapController>,
    emit: E,
) {
    let max_depth = max_depth.clamp(1, 12);
    let max_laid_out = max_laid_out.clamp(4, 512);

    let root_name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.to_string_lossy().into_owned());

    // file root, nothing to walk
    if let Ok(meta) = std::fs::symlink_metadata(&root) {
        if meta.is_file() {
            let resp = TreemapResponse {
                root: root.to_string_lossy().into_owned(),
                total_bytes: super::super::meta_ext::allocated_bytes(&meta),
                total_files: 1,
                tiles: Vec::new(),
                biggest: Vec::new(),
                duration_ms: ctrl.started.elapsed().as_millis() as u64,
            };
            emit.emit_done(&handle_id, &resp);
            return;
        }
    }

    let mut root_node = BuildNode::new_dir(root_name, root.clone());
    let mut last_emit = Instant::now()
        .checked_sub(PROGRESS_THROTTLE)
        .unwrap_or_else(Instant::now);

    let walker = jwalk::WalkDir::new(&root)
        .skip_hidden(false)
        .follow_links(false);

    for entry in walker {
        if ctrl.is_cancelled() {
            break;
        }
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path == root {
            continue;
        }
        let ft = entry.file_type();
        if ft.is_symlink() || !ft.is_file() {
            continue;
        }
        let bytes = entry
            .metadata()
            .map(|m| super::super::meta_ext::allocated_bytes(&m))
            .unwrap_or(0);
        let rel = match path.strip_prefix(&root) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        insert_file(&mut root_node, &root, &rel, bytes, max_depth);

        ctrl.files_scanned.fetch_add(1, Ordering::Relaxed);
        ctrl.bytes_scanned.fetch_add(bytes, Ordering::Relaxed);

        // throttled emit. first check fires immediately since last_emit was
        // initialised PROGRESS_THROTTLE in the past.
        let now = Instant::now();
        if now.duration_since(last_emit) >= PROGRESS_THROTTLE {
            last_emit = now;
            let resp = build_response(&root_node, &ctrl, max_laid_out);
            emit.emit_progress(&handle_id, &resp);
        }
    }

    // one final snapshot reused for `done` event + on_done_tree cache hook so
    // we don't pay for a second walk.
    let final_tree = root_node.snapshot();
    let resp = response_from_tree(&final_tree, &ctrl, max_laid_out);
    emit.emit_done(&handle_id, &resp);
    if !ctrl.is_cancelled() {
        emit.on_done_tree(&handle_id, &final_tree, max_depth);
    }
}

fn build_response(
    root_node: &BuildNode,
    ctrl: &TreemapController,
    max_laid_out: usize,
) -> TreemapResponse {
    response_from_tree(&root_node.snapshot(), ctrl, max_laid_out)
}

fn response_from_tree(
    tree: &super::tree::TreeNode,
    ctrl: &TreemapController,
    max_laid_out: usize,
) -> TreemapResponse {
    let biggest: Vec<BiggestFolder> = tree.biggest_folders(16);
    let tiles = lay_out_children_of(tree, max_laid_out);
    TreemapResponse {
        root: tree.path.clone(),
        total_bytes: tree.bytes,
        total_files: tree.file_count,
        tiles,
        biggest,
        duration_ms: ctrl.started.elapsed().as_millis() as u64,
    }
}

// ---------- registry ----------

/// active walks. usually one at a time, kept so UI can cancel cleanly.
#[derive(Default)]
pub struct TreemapRegistry {
    inner: Mutex<std::collections::HashMap<String, Arc<TreemapController>>>,
}

impl TreemapRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, id: String, ctrl: Arc<TreemapController>) {
        if let Ok(mut g) = self.inner.lock() {
            g.insert(id, ctrl);
        }
    }

    pub fn get(&self, id: &str) -> Option<Arc<TreemapController>> {
        self.inner.lock().ok().and_then(|g| g.get(id).cloned())
    }

    pub fn remove(&self, id: &str) -> Option<Arc<TreemapController>> {
        self.inner.lock().ok().and_then(|mut g| g.remove(id))
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }
}

/// short non-crypto id. format mirrors `next_handle_id` in scanner::run for
/// consistent wire vocab.
pub fn next_treemap_handle_id() -> String {
    use std::sync::atomic::AtomicU64;
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("tree-{pid:x}-{now:x}-{n:x}")
}

// ---------- tests ----------

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

    #[derive(Default)]
    struct Recorder {
        progress: Mutex<Vec<TreemapResponse>>,
        done: Mutex<Option<TreemapResponse>>,
    }

    struct ArcEmit(Arc<Recorder>);
    impl TreemapEmit for ArcEmit {
        fn emit_progress(&self, _id: &str, resp: &TreemapResponse) {
            self.0.progress.lock().unwrap().push(resp.clone());
        }
        fn emit_done(&self, _id: &str, resp: &TreemapResponse) {
            *self.0.done.lock().unwrap() = Some(resp.clone());
        }
    }

    #[test]
    fn done_event_fires_exactly_once_with_full_totals() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "a/1.bin", 100);
        write_file(tmp.path(), "b/2.bin", 200);
        write_file(tmp.path(), "c.bin", 50);

        let ctrl = Arc::new(TreemapController::new());
        let rec = Arc::new(Recorder::default());
        run_treemap_stream(
            "h1".into(),
            tmp.path().to_path_buf(),
            4,
            64,
            ctrl,
            ArcEmit(rec.clone()),
        );

        let done = rec.done.lock().unwrap();
        let final_resp = done.as_ref().unwrap();
        assert_eq!(final_resp.total_bytes, 350);
        assert_eq!(final_resp.total_files, 3);
    }

    #[test]
    fn cancelled_walk_still_emits_done_with_partial_total() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..2_000 {
            write_file(tmp.path(), &format!("d{}/f{}.bin", i % 20, i), 1);
        }

        let ctrl = Arc::new(TreemapController::new());
        ctrl.cancel(); // pre-cancel

        let rec = Arc::new(Recorder::default());
        run_treemap_stream(
            "h-cancel".into(),
            tmp.path().to_path_buf(),
            4,
            64,
            ctrl,
            ArcEmit(rec.clone()),
        );

        let done = rec.done.lock().unwrap();
        let final_resp = done.as_ref().unwrap();
        // walker exits on first cancel check, so total_files is small
        assert!(final_resp.total_files < 2_000);
    }

    #[test]
    fn progress_responses_are_monotonic_in_totals() {
        // enough files to trigger at least one progress tick
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..30_000 {
            write_file(tmp.path(), &format!("bucket{}/f{}.bin", i % 60, i), 1);
        }

        let ctrl = Arc::new(TreemapController::new());
        let rec = Arc::new(Recorder::default());
        run_treemap_stream(
            "h-mono".into(),
            tmp.path().to_path_buf(),
            3,
            64,
            ctrl,
            ArcEmit(rec.clone()),
        );

        let progress = rec.progress.lock().unwrap();
        // non-decreasing across snapshots
        for pair in progress.windows(2) {
            assert!(
                pair[1].total_bytes >= pair[0].total_bytes,
                "regression: {} then {}",
                pair[0].total_bytes,
                pair[1].total_bytes,
            );
            assert!(pair[1].total_files >= pair[0].total_files);
        }

        // done >= last progress snapshot (covers leftovers past final throttle window)
        let done = rec.done.lock().unwrap();
        let final_resp = done.as_ref().unwrap();
        if let Some(last_prog) = progress.last() {
            assert!(final_resp.total_files >= last_prog.total_files);
        }
        assert_eq!(final_resp.total_files, 30_000);
    }

    #[test]
    fn file_root_short_circuits() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("lonely.bin");
        fs::File::create(&p).unwrap().set_len(42).unwrap();

        let ctrl = Arc::new(TreemapController::new());
        let rec = Arc::new(Recorder::default());
        run_treemap_stream(
            "h-file".into(),
            p,
            4,
            64,
            ctrl,
            ArcEmit(rec.clone()),
        );

        let prog = rec.progress.lock().unwrap();
        let done = rec.done.lock().unwrap();
        assert!(prog.is_empty()); // no ticks on short-circuit
        let f = done.as_ref().unwrap();
        assert_eq!(f.total_bytes, 42);
        assert_eq!(f.total_files, 1);
    }

    #[test]
    fn registry_round_trip() {
        let reg = TreemapRegistry::new();
        let id = next_treemap_handle_id();
        let ctrl = Arc::new(TreemapController::new());
        reg.insert(id.clone(), ctrl.clone());
        assert_eq!(reg.len(), 1);
        reg.get(&id).unwrap().cancel();
        assert!(ctrl.is_cancelled());
        assert!(reg.remove(&id).is_some());
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn handle_ids_unique_under_rapid_allocation() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        for _ in 0..5_000 {
            assert!(set.insert(next_treemap_handle_id()));
        }
    }

    #[test]
    fn empty_directory_still_emits_done() {
        let tmp = tempfile::tempdir().unwrap();
        let ctrl = Arc::new(TreemapController::new());
        let rec = Arc::new(Recorder::default());
        run_treemap_stream(
            "h-empty".into(),
            tmp.path().to_path_buf(),
            4,
            64,
            ctrl,
            ArcEmit(rec.clone()),
        );
        let done = rec.done.lock().unwrap();
        let f = done.as_ref().unwrap();
        assert_eq!(f.total_bytes, 0);
        assert_eq!(f.total_files, 0);
        assert!(f.tiles.is_empty());
    }
}
