// tauri command surface. commands stay thin, real work lives in
// modules so it can be tested without a tauri runtime.

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager, State};

use crate::cleaner::{
    self, Cleaner, DeletePlan, DeleteResult, GraveyardStats, PurgeResult, RestoreResult,
};
use crate::onboarding::{
    self, OnboardingState, OnboardingStep, PermissionKind, PermissionStatus, Preferences,
};
use crate::scheduler::{self, Scheduler, SchedulerStatus};
use crate::scanner::activity::{
    self, kill_pid as activity_kill_pid, next_activity_handle_id, run_activity_stream,
    sample_activity, ActivityController, ActivityEmit, ActivityHandle, ActivityRegistry,
    ActivitySnapshot, SysinfoProbe, SystemProbe,
};
use crate::scanner::dupes::{
    self, next_dupes_handle_id, run_dupes_stream, scan_duplicates, DupesController, DupesEmit,
    DupesHandle, DupesRegistry, DuplicateReport,
};
use crate::scanner::junk::{self, JunkReport};
use crate::scanner::malware::{
    self, next_malware_handle_id, run_malware_stream, scan_malware, MalwareController,
    MalwareEmit, MalwareHandle, MalwareOptions, MalwareRegistry, MalwareReport,
};
use crate::scanner::privacy::{self, PrivacyReport};
use crate::scanner::startup::{
    self, StartupReport, StartupSource, ToggleResult,
};
use crate::scanner::largeold::{
    self, next_large_old_handle_id, run_large_old_stream, scan_large_old, LargeOldController,
    LargeOldEmit, LargeOldHandle, LargeOldRegistry, LargeOldReport,
};
use crate::scanner::run::{
    next_handle_id, run_scan, Emit, ScanController, ScanEvent, ScanProgress, ScanRegistry,
    ScanRoot, ScanState, VolumeSnapshot,
};
use crate::scanner::treemap::{
    self, next_treemap_handle_id, preflight_root, run_treemap_stream, TreemapCache,
    TreemapController, TreemapEmit, TreemapHandle, TreemapRegistry, TreemapResponse,
};
use crate::scanner::treemap::tree::TreeNode;
use crate::scanner::{
    empty_summary, summary_from_scan, LastScanFacts, LastScanStore, SmartScanSummary,
};
use crate::volumes::{self, Volume};

#[tauri::command]
pub fn ping() -> &'static str {
    "pong"
}

/// dashboard roll-up. empty if nothing has completed yet this session.
#[tauri::command]
pub fn smart_scan_summary(store: State<'_, LastScanStore>) -> SmartScanSummary {
    match store.get() {
        Some(facts) => summary_from_scan(facts),
        None => empty_summary(),
    }
}

/// blocking but sysinfo refresh is single-digit ms, fine for
/// fetch-on-mount. sorted primary first.
#[tauri::command]
pub fn list_volumes() -> Vec<Volume> {
    volumes::list_volumes()
}

// ---------------- streaming scan ----------------

/// channel names. const so lib/scanner.ts and this file can't drift.
pub const EVENT_SCAN_EVENT: &str = "scan://event";
pub const EVENT_SCAN_PROGRESS: &str = "scan://progress";
pub const EVENT_SCAN_DONE: &str = "scan://done";

/// widens AppHandle into the Emit trait. emit_done also stashes final
/// totals in LastScanStore so the dashboard has real numbers next
/// time.
struct AppEmitter {
    app: AppHandle,
}

impl Emit for AppEmitter {
    fn emit_event(&self, ev: &ScanEvent) {
        let _ = self.app.emit(EVENT_SCAN_EVENT, ev);
    }
    fn emit_progress(&self, p: &ScanProgress) {
        let _ = self.app.emit(EVENT_SCAN_PROGRESS, p);
    }
    fn emit_done(&self, p: &ScanProgress) {
        // cancelled scans still captured, partial is more honest than
        // "Never"
        let store = self.app.state::<LastScanStore>();
        store.set(LastScanFacts {
            flagged_bytes: p.flagged_bytes,
            flagged_items: p.flagged_items,
            scanned_at: now_unix(),
            bytes_accounted: p.bytes_scanned,
            volume_used_bytes: p.volume_used_bytes,
            volume_total_bytes: p.volume_total_bytes,
        });
        let _ = self.app.emit(EVENT_SCAN_DONE, p);
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanHandle {
    pub id: String,
    /// resolved roots, echoed so UI can show the marquee without
    /// recomputing defaults
    pub roots: Vec<String>,
}

/// None/empty roots = $HOME plus per-OS system paths. returns
/// immediately, walk runs on a dedicated thread.
#[tauri::command]
pub fn start_scan(
    app: AppHandle,
    registry: State<'_, ScanRegistry>,
    roots: Option<Vec<String>>,
) -> Result<ScanHandle, String> {
    let scan_roots: Vec<ScanRoot> = roots
        .filter(|v| !v.is_empty())
        // explicit roots from the UI are always User - UI only lets you
        // pick a folder you own
        .map(|v| v.into_iter().map(|s| ScanRoot::user(PathBuf::from(s))).collect())
        .unwrap_or_else(default_roots);

    if scan_roots.is_empty() {
        return Err("no scan roots resolved".into());
    }

    let id = next_handle_id();
    let ctrl = Arc::new(ScanController::new());

    // capture the primary volume snapshot once so every progress/done
    // emission carries the reconciliation numbers. sysinfo round-trip is
    // single-digit ms, fine on the command thread.
    let snap = capture_volume_snapshot(&scan_roots);
    ctrl.set_volume_snapshot(snap);

    registry.insert(id.clone(), ctrl.clone());

    let emitter = AppEmitter { app: app.clone() };
    let roots_echo: Vec<String> = scan_roots
        .iter()
        .map(|r| r.path.to_string_lossy().into_owned())
        .collect();

    let id_thread = id.clone();
    std::thread::Builder::new()
        .name(format!("safai-scan-{id}"))
        .spawn(move || {
            run_scan(id_thread, scan_roots, ctrl, emitter);
        })
        .map_err(|e| format!("failed to spawn scan thread: {e}"))?;

    Ok(ScanHandle {
        id,
        roots: roots_echo,
    })
}

/// find the volume that owns the first user root ($HOME on default
/// scans). zeros when sysinfo returned empty (sandbox, test).
fn capture_volume_snapshot(roots: &[ScanRoot]) -> VolumeSnapshot {
    let volumes = volumes::list_volumes();
    // first User root is $HOME on a default scan, or the UI-selected
    // folder on a custom scan. either way it's the volume to reconcile
    // against.
    let anchor = roots
        .iter()
        .find(|r| matches!(r.kind, crate::scanner::run::RootKind::User))
        .map(|r| r.path.as_path())
        .unwrap_or_else(|| {
            // all-System shouldn't happen from the UI but stay robust
            roots
                .first()
                .map(|r| r.path.as_path())
                .unwrap_or(std::path::Path::new("/"))
        });
    match volumes::volume_for_path(&volumes, anchor) {
        Some(v) => VolumeSnapshot {
            used_bytes: v.used_bytes,
            total_bytes: v.total_bytes,
        },
        None => VolumeSnapshot::default(),
    }
}

/// idempotent
#[tauri::command]
pub fn cancel_scan(registry: State<'_, ScanRegistry>, handle_id: String) -> bool {
    match registry.get(&handle_id) {
        Some(ctrl) => {
            ctrl.set_state(ScanState::Cancelled);
            true
        }
        None => false,
    }
}

#[tauri::command]
pub fn pause_scan(registry: State<'_, ScanRegistry>, handle_id: String) -> bool {
    match registry.get(&handle_id) {
        Some(ctrl) => {
            ctrl.set_state(ScanState::Paused);
            true
        }
        None => false,
    }
}

#[tauri::command]
pub fn resume_scan(registry: State<'_, ScanRegistry>, handle_id: String) -> bool {
    match registry.get(&handle_id) {
        Some(ctrl) => {
            ctrl.set_state(ScanState::Running);
            true
        }
        None => false,
    }
}

/// late-mount safety net, event stream only covers post-subscribe
#[tauri::command]
pub fn scan_snapshot(
    registry: State<'_, ScanRegistry>,
    handle_id: String,
) -> Option<ScanProgress> {
    registry.get(&handle_id).map(|c| c.snapshot())
}

/// controllers linger after done so late snapshot still returns final
/// numbers. UI calls this when navigating away.
#[tauri::command]
pub fn forget_scan(registry: State<'_, ScanRegistry>, handle_id: String) -> bool {
    registry.remove(&handle_id).is_some()
}

// ---------------- system junk ----------------

/// tauri runs commands on a blocking pool. scan is rayon/jwalk parallel
/// so big ~/.cache still wraps in ~1s.
#[tauri::command]
pub fn junk_scan() -> Result<JunkReport, String> {
    let home = home_dir().ok_or_else(|| "could not resolve user home directory".to_string())?;
    Ok(junk::scan_junk(&home, junk::current_os()))
}

// ---------------- startup items ----------------

/// ~30ms on mac with 120 launch agents, single-digit ms on linux/win
#[tauri::command]
pub fn startup_scan() -> Result<StartupReport, String> {
    let home = home_dir().ok_or_else(|| "could not resolve user home directory".to_string())?;
    Ok(startup::scan_startup(&home, startup::current_os()))
}

/// frontend echoes source+path from startup_scan. orchestrator validates
/// path lives under expected root so a compromised renderer can't
/// redirect the toggle.
#[tauri::command]
pub fn startup_toggle(
    source: StartupSource,
    path: String,
    enabled: bool,
) -> Result<ToggleResult, String> {
    let home = home_dir().ok_or_else(|| "could not resolve user home directory".to_string())?;
    startup::toggle_startup(&home, source, &PathBuf::from(&path), enabled)
}

// ---------------- privacy cleaner ----------------

/// sync, browser catalog is narrow. tens of ms.
#[tauri::command]
pub fn privacy_scan() -> Result<PrivacyReport, String> {
    let home = home_dir().ok_or_else(|| "could not resolve user home directory".to_string())?;
    Ok(privacy::scan_privacy(&home, privacy::current_os()))
}

// ---------------- malware scan ----------------

pub const EVENT_MALWARE_PROGRESS: &str = "malware://progress";
pub const EVENT_MALWARE_DONE: &str = "malware://done";

/// missing fields fall back to MalwareOptions::default()
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MalwareScanArgs {
    pub max_hash_bytes: Option<u64>,
    pub recent_window_secs: Option<u64>,
    pub max_findings: Option<usize>,
}

impl MalwareScanArgs {
    fn into_options(self) -> MalwareOptions {
        let mut o = MalwareOptions::default();
        if let Some(v) = self.max_hash_bytes {
            o.max_hash_bytes = v;
        }
        if let Some(v) = self.recent_window_secs {
            o.recent_window_secs = v;
        }
        if let Some(v) = self.max_findings {
            o.max_findings = v;
        }
        o
    }
}

/// sync. UI uses the streaming variant below.
#[tauri::command]
pub fn malware_scan(opts: Option<MalwareScanArgs>) -> Result<MalwareReport, String> {
    let home = home_dir().ok_or_else(|| "could not resolve user home directory".to_string())?;
    let opts = opts.unwrap_or_default().into_options();
    Ok(scan_malware(&home, malware::current_os(), &opts))
}

struct AppMalwareEmitter {
    app: AppHandle,
}

impl MalwareEmit for AppMalwareEmitter {
    fn emit_progress(&self, _id: &str, resp: &MalwareReport) {
        let _ = self.app.emit(EVENT_MALWARE_PROGRESS, resp);
    }
    fn emit_done(&self, _id: &str, resp: &MalwareReport) {
        let _ = self.app.emit(EVENT_MALWARE_DONE, resp);
    }
}

/// final report on malware://done
#[tauri::command]
pub fn start_malware(
    app: AppHandle,
    registry: State<'_, MalwareRegistry>,
    opts: Option<MalwareScanArgs>,
) -> Result<MalwareHandle, String> {
    let home = home_dir().ok_or_else(|| "could not resolve user home directory".to_string())?;
    let opts = opts.unwrap_or_default().into_options();
    let os = malware::current_os();
    let id = next_malware_handle_id();
    let ctrl = Arc::new(MalwareController::new());
    registry.insert(id.clone(), ctrl.clone());

    let emitter = AppMalwareEmitter { app: app.clone() };
    let id_thread = id.clone();
    std::thread::Builder::new()
        .name(format!("safai-malware-{id}"))
        .spawn(move || {
            run_malware_stream(id_thread, home, os, opts, ctrl, emitter);
        })
        .map_err(|e| format!("failed to spawn malware thread: {e}"))?;

    Ok(MalwareHandle {
        id,
        platform: crate::scanner::malware::types::platform_tag(os).to_string(),
    })
}

#[tauri::command]
pub fn cancel_malware(registry: State<'_, MalwareRegistry>, handle_id: String) -> bool {
    match registry.get(&handle_id) {
        Some(ctrl) => {
            ctrl.cancel();
            true
        }
        None => false,
    }
}

#[tauri::command]
pub fn forget_malware(registry: State<'_, MalwareRegistry>, handle_id: String) -> bool {
    registry.remove(&handle_id).is_some()
}

// ---------------- safe deletion engine ----------------

/// process-wide cleaner. called once from run() and managed by tauri.
/// also runs a TTL sweep, failures log but don't block startup.
pub fn build_cleaner() -> Cleaner {
    let home = home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let data = cleaner::default_data_dir(&home);
    let c = Cleaner::new(data, home);
    match c.sweep_stale(cleaner::DEFAULT_GRAVEYARD_TTL_SECS) {
        Ok(r) if !r.purged.is_empty() => {
            eprintln!(
                "[safai] graveyard startup sweep purged {} batch(es), freed {} bytes",
                r.purged.len(),
                r.bytes_freed
            );
        }
        Ok(_) => {}
        Err(e) => eprintln!("[safai] graveyard startup sweep failed: {e}"),
    }
    c
}

/// preview/commit split closes a TOCTOU window. commit only trusts
/// paths already classified safe, so a compromised frontend can't
/// smuggle new paths in.
#[tauri::command]
pub fn preview_delete(cleaner: State<'_, Cleaner>, paths: Vec<String>) -> DeletePlan {
    let pbs: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    cleaner.preview(pbs)
}

#[tauri::command]
pub fn commit_delete(cleaner: State<'_, Cleaner>, token: String) -> Result<DeleteResult, String> {
    cleaner.commit(&token).map_err(Into::into)
}

/// undo last clean
#[tauri::command]
pub fn restore_last(cleaner: State<'_, Cleaner>) -> Result<RestoreResult, String> {
    cleaner.restore_last().map_err(Into::into)
}

#[tauri::command]
pub fn graveyard_stats(cleaner: State<'_, Cleaner>) -> Result<GraveyardStats, String> {
    cleaner.graveyard_stats().map_err(Into::into)
}

/// irreversible, UI must confirm first
#[tauri::command]
pub fn purge_graveyard(cleaner: State<'_, Cleaner>) -> Result<PurgeResult, String> {
    cleaner.purge_all().map_err(Into::into)
}

// ---------------- disk usage treemap ----------------

/// 4 levels: "Applications > Xcode > Contents > Frameworks". enough
/// for biggest-folders sidebar while keeping the tree bounded.
pub const DEFAULT_TREEMAP_DEPTH: usize = 4;

pub const EVENT_TREEMAP_PROGRESS: &str = "treemap://progress";
pub const EVENT_TREEMAP_DONE: &str = "treemap://done";

/// sync. UI uses start_treemap.
#[tauri::command]
pub fn compute_treemap(
    root: Option<String>,
    depth: Option<usize>,
    max_tiles: Option<usize>,
) -> Result<TreemapResponse, String> {
    let root_path: PathBuf = match root.filter(|s| !s.is_empty()) {
        Some(s) => PathBuf::from(s),
        None => home_dir().ok_or_else(|| "could not resolve user home directory".to_string())?,
    };
    let depth = depth.unwrap_or(DEFAULT_TREEMAP_DEPTH);
    let max_tiles = max_tiles.unwrap_or(treemap::DEFAULT_MAX_LAID_OUT);
    treemap::compute_treemap(&root_path, depth, max_tiles).map_err(Into::into)
}

/// owns Arc<TreemapCache> so on_done_tree can seed the cache at walk
/// end. makes drill-down + back-nav free of a rescan.
struct AppTreemapEmitter {
    app: AppHandle,
    cache: Arc<TreemapCache>,
    root: PathBuf,
}

impl TreemapEmit for AppTreemapEmitter {
    fn emit_progress(&self, _id: &str, resp: &TreemapResponse) {
        let _ = self.app.emit(EVENT_TREEMAP_PROGRESS, resp);
    }
    fn emit_done(&self, _id: &str, resp: &TreemapResponse) {
        let _ = self.app.emit(EVENT_TREEMAP_DONE, resp);
    }
    fn on_done_tree(&self, _id: &str, tree: &TreeNode, max_depth: usize) {
        // walker owns the tree, clone into cache. one-time cost
        // buys every future drill-down a free ride.
        self.cache.store(self.root.clone(), tree.clone(), max_depth);
    }
}

/// TreemapResponse payloads are self-contained, UI swaps in the
/// latest. no handle id in events, UI runs one treemap at a time.
#[tauri::command]
pub fn start_treemap(
    app: AppHandle,
    registry: State<'_, TreemapRegistry>,
    cache: State<'_, Arc<TreemapCache>>,
    root: Option<String>,
    depth: Option<usize>,
    max_tiles: Option<usize>,
) -> Result<TreemapHandle, String> {
    let root_path: PathBuf = match root.filter(|s| !s.is_empty()) {
        Some(s) => PathBuf::from(s),
        None => home_dir().ok_or_else(|| "could not resolve user home directory".to_string())?,
    };
    // preflight so UI sees NotFound synchronously, no confusing it
    // with "still walking"
    preflight_root(&root_path).map_err(|e| e.to_string())?;

    let depth = depth.unwrap_or(DEFAULT_TREEMAP_DEPTH);
    let max_tiles = max_tiles.unwrap_or(treemap::DEFAULT_MAX_LAID_OUT);

    let id = next_treemap_handle_id();
    let ctrl = Arc::new(TreemapController::new());
    registry.insert(id.clone(), ctrl.clone());

    let emitter = AppTreemapEmitter {
        app: app.clone(),
        cache: cache.inner().clone(),
        root: root_path.clone(),
    };
    let id_thread = id.clone();
    let root_echo = root_path.to_string_lossy().into_owned();
    let root_for_walk = root_path.clone();

    std::thread::Builder::new()
        .name(format!("safai-treemap-{id}"))
        .spawn(move || {
            run_treemap_stream(id_thread, root_for_walk, depth, max_tiles, ctrl, emitter);
        })
        .map_err(|e| format!("failed to spawn treemap thread: {e}"))?;

    Ok(TreemapHandle {
        id,
        root: root_echo,
    })
}

/// serves from the cache seeded by a prior start_treemap. UI calls
/// this before drill/pop/home so nav is instant when the path is a
/// descendant of an already-walked root.
///
/// returns None when no cached scan covers path, or when the cached
/// node is a dir with 0 children + non-zero bytes (depth cap truncated,
/// real walk needed). UI falls back to start_treemap.
#[tauri::command]
pub fn serve_treemap_subtree(
    cache: State<'_, Arc<TreemapCache>>,
    path: Option<String>,
    max_tiles: Option<usize>,
) -> Result<Option<TreemapResponse>, String> {
    let target: PathBuf = match path.filter(|s| !s.is_empty()) {
        Some(s) => PathBuf::from(s),
        None => home_dir().ok_or_else(|| "could not resolve user home directory".to_string())?,
    };
    let max_tiles = max_tiles.unwrap_or(treemap::DEFAULT_MAX_LAID_OUT);
    Ok(cache.serve(&target, max_tiles))
}

/// Rescan button
#[tauri::command]
pub fn invalidate_treemap_cache(cache: State<'_, Arc<TreemapCache>>) {
    cache.clear();
}

/// idempotent. walker still emits a final treemap://done with partial
/// aggregates when the flag flips.
#[tauri::command]
pub fn cancel_treemap(registry: State<'_, TreemapRegistry>, handle_id: String) -> bool {
    match registry.get(&handle_id) {
        Some(ctrl) => {
            ctrl.cancel();
            true
        }
        None => false,
    }
}

/// controllers linger so late cancel still returns true. UI calls
/// this when done with events.
#[tauri::command]
pub fn forget_treemap(registry: State<'_, TreemapRegistry>, handle_id: String) -> bool {
    registry.remove(&handle_id).is_some()
}

// ---------------- duplicate finder ----------------

/// only emits done today. progress is reserved for future
/// per-pass ticks and already wired through AppEmit so enabling later
/// is a pipeline change only.
#[allow(dead_code)]
pub const EVENT_DUPES_PROGRESS: &str = "dupes://progress";
pub const EVENT_DUPES_DONE: &str = "dupes://done";

/// sync. UI uses start_duplicates.
#[tauri::command]
pub fn find_duplicates(
    root: Option<String>,
    min_bytes: Option<u64>,
) -> Result<DuplicateReport, String> {
    let root_path: PathBuf = match root.filter(|s| !s.is_empty()) {
        Some(s) => PathBuf::from(s),
        None => home_dir().ok_or_else(|| "could not resolve user home directory".to_string())?,
    };
    let min = min_bytes.unwrap_or(dupes::DEFAULT_MIN_BYTES);
    scan_duplicates(&root_path, min).map_err(Into::into)
}

struct AppDupesEmitter {
    app: AppHandle,
}

impl DupesEmit for AppDupesEmitter {
    fn emit_progress(&self, _id: &str, resp: &DuplicateReport) {
        let _ = self.app.emit(EVENT_DUPES_PROGRESS, resp);
    }
    fn emit_done(&self, _id: &str, resp: &DuplicateReport) {
        let _ = self.app.emit(EVENT_DUPES_DONE, resp);
    }
}

/// final report on dupes://done after size + head-hash + full-hash
/// passes.
#[tauri::command]
pub fn start_duplicates(
    app: AppHandle,
    registry: State<'_, DupesRegistry>,
    root: Option<String>,
    min_bytes: Option<u64>,
) -> Result<DupesHandle, String> {
    let root_path: PathBuf = match root.filter(|s| !s.is_empty()) {
        Some(s) => PathBuf::from(s),
        None => home_dir().ok_or_else(|| "could not resolve user home directory".to_string())?,
    };
    // preflight so UI sees NotFound synchronously
    if std::fs::symlink_metadata(&root_path).is_err() {
        return Err(format!(
            "root not found: {}",
            root_path.to_string_lossy()
        ));
    }
    let min = min_bytes.unwrap_or(dupes::DEFAULT_MIN_BYTES);

    let id = next_dupes_handle_id();
    let ctrl = Arc::new(DupesController::new());
    registry.insert(id.clone(), ctrl.clone());

    let emitter = AppDupesEmitter { app: app.clone() };
    let id_thread = id.clone();
    let root_echo = root_path.to_string_lossy().into_owned();
    std::thread::Builder::new()
        .name(format!("safai-dupes-{id}"))
        .spawn(move || {
            run_dupes_stream(id_thread, root_path, min, ctrl, emitter);
        })
        .map_err(|e| format!("failed to spawn dupes thread: {e}"))?;

    Ok(DupesHandle {
        id,
        root: root_echo,
    })
}

#[tauri::command]
pub fn cancel_duplicates(registry: State<'_, DupesRegistry>, handle_id: String) -> bool {
    match registry.get(&handle_id) {
        Some(ctrl) => {
            ctrl.cancel();
            true
        }
        None => false,
    }
}

#[tauri::command]
pub fn forget_duplicates(registry: State<'_, DupesRegistry>, handle_id: String) -> bool {
    registry.remove(&handle_id).is_some()
}

// ---------------- large & old finder ----------------

pub const EVENT_LARGE_OLD_PROGRESS: &str = "large-old://progress";
pub const EVENT_LARGE_OLD_DONE: &str = "large-old://done";

/// sync. UI uses start_large_old.
#[tauri::command]
pub fn find_large_old(
    root: Option<String>,
    min_bytes: Option<u64>,
    min_days_idle: Option<u64>,
    max_results: Option<usize>,
) -> Result<LargeOldReport, String> {
    let root_path: PathBuf = match root.filter(|s| !s.is_empty()) {
        Some(s) => PathBuf::from(s),
        None => home_dir().ok_or_else(|| "could not resolve user home directory".to_string())?,
    };
    let min_bytes = min_bytes.unwrap_or(largeold::DEFAULT_MIN_BYTES);
    let min_days_idle = min_days_idle.unwrap_or(largeold::DEFAULT_MIN_DAYS_IDLE);
    let max_results = max_results.unwrap_or(largeold::DEFAULT_MAX_RESULTS);
    scan_large_old(&root_path, min_bytes, min_days_idle, max_results).map_err(Into::into)
}

struct AppLargeOldEmitter {
    app: AppHandle,
}

impl LargeOldEmit for AppLargeOldEmitter {
    fn emit_progress(&self, _id: &str, resp: &LargeOldReport) {
        let _ = self.app.emit(EVENT_LARGE_OLD_PROGRESS, resp);
    }
    fn emit_done(&self, _id: &str, resp: &LargeOldReport) {
        let _ = self.app.emit(EVENT_LARGE_OLD_DONE, resp);
    }
}

#[tauri::command]
pub fn start_large_old(
    app: AppHandle,
    registry: State<'_, LargeOldRegistry>,
    root: Option<String>,
    min_bytes: Option<u64>,
    min_days_idle: Option<u64>,
    max_results: Option<usize>,
) -> Result<LargeOldHandle, String> {
    let root_path: PathBuf = match root.filter(|s| !s.is_empty()) {
        Some(s) => PathBuf::from(s),
        None => home_dir().ok_or_else(|| "could not resolve user home directory".to_string())?,
    };
    if std::fs::symlink_metadata(&root_path).is_err() {
        return Err(format!("root not found: {}", root_path.to_string_lossy()));
    }
    let min_bytes = min_bytes.unwrap_or(largeold::DEFAULT_MIN_BYTES);
    let min_days_idle = min_days_idle.unwrap_or(largeold::DEFAULT_MIN_DAYS_IDLE);
    let max_results = max_results.unwrap_or(largeold::DEFAULT_MAX_RESULTS);

    let id = next_large_old_handle_id();
    let ctrl = Arc::new(LargeOldController::new());
    registry.insert(id.clone(), ctrl.clone());

    let emitter = AppLargeOldEmitter { app: app.clone() };
    let id_thread = id.clone();
    let root_echo = root_path.to_string_lossy().into_owned();
    std::thread::Builder::new()
        .name(format!("safai-large-old-{id}"))
        .spawn(move || {
            run_large_old_stream(
                id_thread,
                root_path,
                min_bytes,
                min_days_idle,
                max_results,
                ctrl,
                emitter,
            );
        })
        .map_err(|e| format!("failed to spawn large-old thread: {e}"))?;

    Ok(LargeOldHandle {
        id,
        root: root_echo,
    })
}

#[tauri::command]
pub fn cancel_large_old(
    registry: State<'_, LargeOldRegistry>,
    handle_id: String,
) -> bool {
    match registry.get(&handle_id) {
        Some(ctrl) => {
            ctrl.cancel();
            true
        }
        None => false,
    }
}

#[tauri::command]
pub fn forget_large_old(
    registry: State<'_, LargeOldRegistry>,
    handle_id: String,
) -> bool {
    registry.remove(&handle_id).is_some()
}

/// mac: open -R, win: explorer /select,, linux: xdg-open on parent dir
/// (no standard select-in-file-manager intent on linux). fire and
/// forget, don't wait for the reveal tool.
#[tauri::command]
pub fn reveal_in_file_manager(path: String) -> Result<(), String> {
    let p = PathBuf::from(&path);
    if std::fs::symlink_metadata(&p).is_err() {
        return Err(format!("path not found: {path}"));
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(&p)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("open failed: {e}"))
    }
    #[cfg(target_os = "windows")]
    {
        // explorer /select, parses the rest of argv as one path
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", p.display()))
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("explorer failed: {e}"))
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        // no standard select intent, just open the containing dir
        let target = if p.is_dir() {
            p.clone()
        } else {
            p.parent().map(|x| x.to_path_buf()).unwrap_or(p.clone())
        };
        std::process::Command::new("xdg-open")
            .arg(&target)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("xdg-open failed: {e}"))
    }
}

// ---------------- memory + activity monitor ----------------

pub const EVENT_ACTIVITY_SNAPSHOT: &str = "activity://snapshot";

/// one-shot. first-paint rows while the stream warms up. CPU column
/// reports 0% on first call (same as `top -l 1`) because we need two
/// samples for a delta.
#[tauri::command]
pub fn activity_sample(top_n: Option<usize>) -> Result<ActivitySnapshot, String> {
    let mut probe = SysinfoProbe::new();
    // seed sysinfo delta cache, sleep min interval so the second
    // refresh has something to diff
    probe.refresh();
    std::thread::sleep(std::time::Duration::from_millis(
        activity::MIN_INTERVAL_MS,
    ));
    let n = top_n.unwrap_or(activity::DEFAULT_TOP_N);
    Ok(sample_activity(&mut probe, n, 0))
}

struct AppActivityEmitter {
    app: AppHandle,
}

impl ActivityEmit for AppActivityEmitter {
    fn emit_snapshot(&self, _id: &str, snap: &ActivitySnapshot) {
        let _ = self.app.emit(EVENT_ACTIVITY_SNAPSHOT, snap);
    }
}

/// tauri's event bus is broadcast, 2 subscribers just fan-out the
/// same snapshot.
#[tauri::command]
pub fn start_activity(
    app: AppHandle,
    registry: State<'_, ActivityRegistry>,
    interval_ms: Option<u64>,
    top_n: Option<usize>,
) -> Result<ActivityHandle, String> {
    let ctrl = Arc::new(ActivityController::new());
    if let Some(ms) = interval_ms {
        ctrl.set_interval_ms(ms);
    }
    let id = next_activity_handle_id();
    registry.insert(id.clone(), ctrl.clone());
    let n = top_n.unwrap_or(activity::DEFAULT_TOP_N);
    let probe = SysinfoProbe::new();
    let emitter = AppActivityEmitter { app: app.clone() };
    let resolved_interval = ctrl.interval_ms();
    let id_thread = id.clone();
    let ctrl_thread = ctrl.clone();
    std::thread::Builder::new()
        .name(format!("safai-activity-{id}"))
        .spawn(move || {
            run_activity_stream(id_thread, ctrl_thread, probe, n, emitter);
        })
        .map_err(|e| format!("failed to spawn activity thread: {e}"))?;
    Ok(ActivityHandle {
        id,
        interval_ms: resolved_interval,
    })
}

/// nudge to re-tick without waiting for the next interval. noop
/// interval reset so the cancellable-sleep wakes up.
#[tauri::command]
pub fn refresh_activity(
    registry: State<'_, ActivityRegistry>,
    handle_id: String,
) -> bool {
    match registry.get(&handle_id) {
        Some(ctrl) => {
            let cur = ctrl.interval_ms();
            ctrl.set_interval_ms(cur);
            true
        }
        None => false,
    }
}

/// wakes the sleep so the new interval applies now
#[tauri::command]
pub fn set_activity_interval(
    registry: State<'_, ActivityRegistry>,
    handle_id: String,
    interval_ms: u64,
) -> bool {
    match registry.get(&handle_id) {
        Some(ctrl) => {
            ctrl.set_interval_ms(interval_ms);
            true
        }
        None => false,
    }
}

#[tauri::command]
pub fn cancel_activity(
    registry: State<'_, ActivityRegistry>,
    handle_id: String,
) -> bool {
    match registry.get(&handle_id) {
        Some(ctrl) => {
            ctrl.cancel();
            true
        }
        None => false,
    }
}

#[tauri::command]
pub fn forget_activity(
    registry: State<'_, ActivityRegistry>,
    handle_id: String,
) -> bool {
    registry.remove(&handle_id).is_some()
}

/// force=true maps to SIGKILL on unix, TerminateProcess on win. pids
/// 0/1/self refused before sysinfo.
#[tauri::command]
pub fn kill_process(pid: u32, force: Option<bool>) -> Result<(), String> {
    activity_kill_pid(pid, force.unwrap_or(false)).map_err(Into::into)
}

// ---------------- onboarding ----------------

/// never fails. bad state.json falls back to defaults, user re-runs
/// onboarding.
#[tauri::command]
pub fn onboarding_state() -> OnboardingState {
    let data = onboarding_data_dir();
    onboarding::load_or_default(&data)
}

/// full object replace, not a patch
#[tauri::command]
pub fn onboarding_save_prefs(prefs: Preferences) -> Result<OnboardingState, String> {
    let data = onboarding_data_dir();
    let mut state = onboarding::load_or_default(&data);
    state.apply_prefs(prefs);
    onboarding::save(&data, &state).map_err(|e| e.to_string())?;
    Ok(state)
}

/// lets relaunch mid-flow resume. does not advance completed_at.
#[tauri::command]
pub fn onboarding_set_step(step: OnboardingStep) -> Result<OnboardingState, String> {
    let data = onboarding_data_dir();
    let mut state = onboarding::load_or_default(&data);
    state.last_step = step;
    onboarding::save(&data, &state).map_err(|e| e.to_string())?;
    Ok(state)
}

/// overwrites any prior record for the same kind
#[tauri::command]
pub fn onboarding_record_permission(
    kind: PermissionKind,
    status: PermissionStatus,
) -> Result<OnboardingState, String> {
    let data = onboarding_data_dir();
    let mut state = onboarding::load_or_default(&data);
    state.record_permission(kind, status, now_unix());
    onboarding::save(&data, &state).map_err(|e| e.to_string())?;
    Ok(state)
}

/// one-shot so the UI doesn't resend the whole prefs block just to
/// toggle a checkbox
#[tauri::command]
pub fn onboarding_set_telemetry(opt_in: bool) -> Result<OnboardingState, String> {
    let data = onboarding_data_dir();
    let mut state = onboarding::load_or_default(&data);
    state.telemetry_opt_in = opt_in;
    onboarding::save(&data, &state).map_err(|e| e.to_string())?;
    Ok(state)
}

/// idempotent, second call keeps original timestamp
#[tauri::command]
pub fn onboarding_complete() -> Result<OnboardingState, String> {
    let data = onboarding_data_dir();
    let mut state = onboarding::load_or_default(&data);
    state.mark_complete(now_unix());
    onboarding::save(&data, &state).map_err(|e| e.to_string())?;
    Ok(state)
}

/// settings screen "Re-run onboarding" button
#[tauri::command]
pub fn onboarding_reset() -> Result<(), String> {
    let data = onboarding_data_dir();
    onboarding::reset(&data).map_err(|e| e.to_string())
}

/// live permission status per kind. best-effort, non-mac is always
/// Unknown.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionStatusEntry {
    pub kind: PermissionKind,
    pub status: PermissionStatus,
    pub settings_url: Option<String>,
}

#[tauri::command]
pub fn onboarding_permission_status() -> Vec<PermissionStatusEntry> {
    let platform = onboarding::Platform::current();
    let home = home_dir().unwrap_or_else(|| PathBuf::from("/"));
    onboarding::applicable_for(platform)
        .into_iter()
        .map(|kind| PermissionStatusEntry {
            kind,
            status: onboarding::detect_status(kind, &home),
            settings_url: onboarding::settings_url(kind).map(String::from),
        })
        .collect()
}

/// fire and forget, shell returns immediately
#[tauri::command]
pub fn open_permission_settings(kind: PermissionKind) -> Result<(), String> {
    onboarding::open_settings(kind)
}

fn onboarding_data_dir() -> PathBuf {
    let home = home_dir().unwrap_or_else(|| PathBuf::from("/"));
    cleaner::default_data_dir(&home)
}

// ---------------- settings + scheduler ----------------

/// scheduler uses this to tell UI "fire a scan". frontend listener
/// maps it to startScan().
pub const EVENT_SCHEDULER_FIRED: &str = "scheduler://fired";

/// one-trip bundle so settings screen doesn't fan out to 5 commands
/// on mount.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsBundle {
    pub prefs: Preferences,
    pub telemetry_opt_in: bool,
    pub completed_at: Option<u64>,
    pub last_scheduled_at: Option<u64>,
    pub scheduler: SchedulerStatus,
    /// CARGO_PKG_VERSION so About doesn't go stale
    pub app_version: &'static str,
}

#[tauri::command]
pub fn settings_get(state: State<'_, Scheduler>) -> SettingsBundle {
    let data = onboarding_data_dir();
    let s = onboarding::load_or_default(&data);
    let now = now_unix();
    let status = SchedulerStatus::derive(s.prefs.scheduled_scan, s.last_scheduled_at, now);
    // sync in-memory controller with disk in case user hand-edited
    // state.json
    let _ = state.controller.set_cadence(s.prefs.scheduled_scan);
    SettingsBundle {
        prefs: s.prefs,
        telemetry_opt_in: s.telemetry_opt_in,
        completed_at: s.completed_at,
        last_scheduled_at: s.last_scheduled_at,
        scheduler: status,
        app_version: env!("CARGO_PKG_VERSION"),
    }
}

/// scheduler picks up cadence changes here via set_cadence, which
/// wakes the loop via condvar.
#[tauri::command]
pub fn settings_update(
    scheduler: State<'_, Scheduler>,
    prefs: Preferences,
    telemetry_opt_in: bool,
) -> Result<SettingsBundle, String> {
    let data = onboarding_data_dir();
    let mut state = onboarding::load_or_default(&data);
    let old_cadence = state.prefs.scheduled_scan;
    state.apply_prefs(prefs);
    state.telemetry_opt_in = telemetry_opt_in;
    // cadence flip -> clear anchor so new cadence starts fresh,
    // otherwise a stale anchor from the old cadence would fire
    // immediately
    if state.prefs.scheduled_scan != old_cadence {
        state.last_scheduled_at = None;
    }
    onboarding::save(&data, &state).map_err(|e| e.to_string())?;
    scheduler.controller.set_cadence(state.prefs.scheduled_scan);
    let now = now_unix();
    let status =
        SchedulerStatus::derive(state.prefs.scheduled_scan, state.last_scheduled_at, now);
    Ok(SettingsBundle {
        prefs: state.prefs,
        telemetry_opt_in: state.telemetry_opt_in,
        completed_at: state.completed_at,
        last_scheduled_at: state.last_scheduled_at,
        scheduler: status,
        app_version: env!("CARGO_PKG_VERSION"),
    })
}

/// resets prefs + telemetry + last_scheduled_at. keeps completion
/// and permissions, reset those via onboarding_reset.
#[tauri::command]
pub fn settings_reset_prefs(
    scheduler: State<'_, Scheduler>,
) -> Result<SettingsBundle, String> {
    let data = onboarding_data_dir();
    let mut state = onboarding::load_or_default(&data);
    state.prefs = Preferences::default();
    state.telemetry_opt_in = false;
    state.last_scheduled_at = None;
    onboarding::save(&data, &state).map_err(|e| e.to_string())?;
    scheduler.controller.set_cadence(state.prefs.scheduled_scan);
    let now = now_unix();
    let status =
        SchedulerStatus::derive(state.prefs.scheduled_scan, state.last_scheduled_at, now);
    Ok(SettingsBundle {
        prefs: state.prefs,
        telemetry_opt_in: state.telemetry_opt_in,
        completed_at: state.completed_at,
        last_scheduled_at: state.last_scheduled_at,
        scheduler: status,
        app_version: env!("CARGO_PKG_VERSION"),
    })
}

/// readout for Settings "Next scheduled scan", read-only
#[tauri::command]
pub fn scheduler_status() -> SchedulerStatus {
    let data = onboarding_data_dir();
    let s = onboarding::load_or_default(&data);
    SchedulerStatus::derive(s.prefs.scheduled_scan, s.last_scheduled_at, now_unix())
}

/// wake the loop. doesn't itself fire, just gets a cadence change
/// reflected in ms instead of whenever the sleep expires.
#[tauri::command]
pub fn scheduler_nudge(scheduler: State<'_, Scheduler>) -> bool {
    scheduler.controller.notify();
    true
}

/// callback emits scheduler://fired. frontend reacts by starting a
/// scan, scheduler stays decoupled from scanner internals.
pub fn spawn_scheduler(app: AppHandle) -> Scheduler {
    let home = home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let data = cleaner::default_data_dir(&home);
    let state = onboarding::load_or_default(&data);
    let initial_cadence = state.prefs.scheduled_scan;
    let controller = Arc::new(scheduler::SchedulerController::new(initial_cadence));

    let ctrl_thread = controller.clone();
    let data_thread = data.clone();
    let app_emit = app.clone();
    std::thread::Builder::new()
        .name("safai-scheduler".into())
        .spawn(move || {
            scheduler::run_scheduler_loop(
                data_thread,
                ctrl_thread,
                scheduler::SystemClock,
                move || {
                    // fire and forget. UI scans if open. emit failing
                    // (window dropped) just means next launch re-evals.
                    let _ = app_emit.emit(EVENT_SCHEDULER_FIRED, ());
                },
            );
        })
        .expect("failed to spawn scheduler thread");

    Scheduler::new(controller)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// $HOME as the User root, plus per-OS world-readable system paths as
/// System roots. System paths are best-effort (any permission-denied
/// subtree just gets silently skipped by jwalk), count toward
/// bytes_scanned but are not classified.
///
/// intentional gaps:
/// - /System, /private (mac) - SIP-protected and largely unreadable
/// - /proc, /sys, /run, /dev (linux) - pseudo-fs, counted elsewhere
/// - System Volume Information, Recycle Bin (windows) - ACL-locked
/// the cross-device guard in walk_one also drops external drives and
/// network mounts automatically.
fn default_roots() -> Vec<ScanRoot> {
    let mut out: Vec<ScanRoot> = Vec::new();
    if let Some(home) = home_dir() {
        out.push(ScanRoot::user(home));
    }

    #[cfg(target_os = "macos")]
    {
        for p in ["/Applications", "/Library", "/usr/local", "/opt"] {
            let path = PathBuf::from(p);
            if path.exists() {
                out.push(ScanRoot::system(path));
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        for p in ["/usr", "/opt", "/var", "/srv"] {
            let path = PathBuf::from(p);
            if path.exists() {
                out.push(ScanRoot::system(path));
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        for name in ["Program Files", "Program Files (x86)", "ProgramData", "Windows"] {
            let path = PathBuf::from(&drive).join(name);
            if path.exists() {
                out.push(ScanRoot::system(path));
            }
        }
    }

    // last resort, so the walker always has something
    if out.is_empty() {
        #[cfg(windows)]
        {
            out.push(ScanRoot::user(PathBuf::from("C:\\")));
        }
        #[cfg(not(windows))]
        {
            out.push(ScanRoot::user(PathBuf::from("/")));
        }
    }

    out
}

#[cfg(test)]
mod default_roots_tests {
    use super::*;
    use crate::scanner::run::RootKind;

    #[test]
    fn home_is_first_user_root() {
        let roots = default_roots();
        assert!(!roots.is_empty());
        let first = &roots[0];
        assert_eq!(first.kind, RootKind::User);
    }

    #[test]
    fn os_specific_system_roots_are_marked_system() {
        let roots = default_roots();
        let system = roots
            .iter()
            .filter(|r| matches!(r.kind, RootKind::System))
            .count();
        // at least one system root should exist on any of the three
        // supported OSes (they all have readable system dirs). host
        // CI can run on linux, mac, or windows.
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        assert!(system > 0, "expected at least one system root");
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        let _ = system;
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_default_roots_include_usr() {
        let roots = default_roots();
        let paths: Vec<_> = roots
            .iter()
            .map(|r| r.path.to_string_lossy().into_owned())
            .collect();
        assert!(paths.iter().any(|p| p == "/usr"), "got {paths:?}");
    }
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

