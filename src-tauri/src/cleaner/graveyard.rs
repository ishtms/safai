//! our own cross-platform trash. committed batch layout:
//!
//! ```text
//! <data_dir>/graveyard/<batch_id>/
//!     manifest.json        record of every moved item + origin
//!     items/
//!         000000/<name>    the moved data
//!         000001/<name>
//!         ...
//! ```
//!
//! why not OS trash? restore isn't portable. trash crate's
//! os_limited::restore_all is linux+windows only, macOS Undo is
//! AppKit-specific. owning the graveyard = restore is just rename in
//! reverse. no SDK, no Finder IPC, no platform surprises.
//!
//! atomicity: items move via fs::rename (same-device = atomic everywhere).
//! cross-device fallback is allowed only after a destination free-space
//! preflight succeeds. if copy succeeds but delete fails, original stays
//! put and we don't add it to committed, so the user's view stays
//! consistent.
//!
//! durability: a staging manifest is created before moves begin, each item
//! is recorded before it is touched, and successful moves are marked as
//! moved. the batch is atomically marked committed at the end. a crash
//! mid-batch leaves enough information for restore/purge to find moved
//! items without hand-recovering numbered slots.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::audit;
use super::types::{
    CleanerError, DeleteFailure, DeletePlan, DeleteResult, GraveyardStats, ItemKind, PurgeResult,
    RestoreResult,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestEntry {
    pub orig_path: String,
    pub moved_path: String,
    pub bytes: u64,
    pub file_count: u64,
    pub kind: ItemKind,
    #[serde(default = "default_entry_state")]
    pub state: ManifestEntryState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ManifestEntryState {
    Pending,
    Moved,
}

fn default_entry_state() -> ManifestEntryState {
    ManifestEntryState::Moved
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BatchStatus {
    Staging,
    Committed,
}

fn default_batch_status() -> BatchStatus {
    BatchStatus::Committed
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchManifest {
    pub batch_id: String,
    pub created_at: u64,
    #[serde(default = "default_batch_status")]
    pub status: BatchStatus,
    pub items: Vec<ManifestEntry>,
}

/// move everything non-protected into the graveyard and write the
/// manifest. never hard-deletes. failed move = original stays put.
pub fn commit(data_dir: &Path, plan: &DeletePlan) -> Result<DeleteResult, CleanerError> {
    let batch_id = new_batch_id();
    let batch_dir = data_dir.join("graveyard").join(&batch_id);
    let items_dir = batch_dir.join("items");
    fs::create_dir_all(&items_dir)
        .map_err(|e| CleanerError::Io(format!("create graveyard: {e}")))?;

    let mut manifest = BatchManifest {
        batch_id: batch_id.clone(),
        created_at: now_unix(),
        status: BatchStatus::Staging,
        items: Vec::new(),
    };
    let manifest_path = batch_dir.join("manifest.json");
    write_manifest_atomic(&manifest_path, &manifest)?;

    let mut committed: Vec<String> = Vec::new();
    let mut failed: Vec<DeleteFailure> = Vec::new();
    let mut bytes_trashed: u64 = 0;

    for (i, item) in plan.items.iter().enumerate() {
        if item.protected {
            continue;
        }
        let orig = PathBuf::from(&item.path);

        let slot = items_dir.join(format!("{i:06}"));
        if let Err(e) = fs::create_dir_all(&slot) {
            failed.push(DeleteFailure {
                path: item.path.clone(),
                error: format!("create slot: {e}"),
            });
            continue;
        }

        // keep original name in the numbered slot so the graveyard is
        // human-readable. fallback "item" shouldn't fire for abs paths but
        // belt and suspenders
        let file_name = orig
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| std::ffi::OsString::from("item"));
        let moved = slot.join(&file_name);

        let entry_index = manifest.items.len();
        manifest.items.push(ManifestEntry {
            orig_path: item.path.clone(),
            moved_path: moved.to_string_lossy().into_owned(),
            bytes: item.bytes,
            file_count: item.file_count,
            kind: item.kind,
            state: ManifestEntryState::Pending,
        });
        if let Err(e) = write_manifest_atomic(&manifest_path, &manifest) {
            manifest.items.pop();
            failed.push(DeleteFailure {
                path: item.path.clone(),
                error: format!("stage manifest entry: {e}"),
            });
            let _ = fs::remove_dir_all(&slot);
            continue;
        }

        match safe_move(&orig, &moved, item.bytes) {
            Ok(()) => {
                bytes_trashed = bytes_trashed.saturating_add(item.bytes);
                committed.push(item.path.clone());
                if let Some(entry) = manifest.items.get_mut(entry_index) {
                    entry.state = ManifestEntryState::Moved;
                }
                if let Err(e) = write_manifest_atomic(&manifest_path, &manifest) {
                    eprintln!(
                        "[safai] graveyard manifest update failed after moving {}: {e}",
                        item.path
                    );
                }
            }
            Err(e) => {
                manifest.items.remove(entry_index);
                if let Err(write_err) = write_manifest_atomic(&manifest_path, &manifest) {
                    eprintln!(
                        "[safai] graveyard manifest cleanup failed for {}: {write_err}",
                        item.path
                    );
                }
                failed.push(DeleteFailure {
                    path: item.path.clone(),
                    error: e,
                });
                // nuke the empty slot so the graveyard stays tidy
                let _ = fs::remove_dir_all(&slot);
            }
        }
    }

    manifest.status = BatchStatus::Committed;
    write_manifest_atomic(&manifest_path, &manifest)?;

    Ok(DeleteResult {
        token: plan.token.clone(),
        batch_id,
        committed_at: manifest.created_at,
        committed,
        failed,
        bytes_trashed,
    })
}

/// move manifest entries back to orig_path. skips entries where the
/// original path now exists (never overwrites live data). full success
/// removes the batch dir, partial leaves it so leftovers stay recoverable.
pub fn restore_batch(data_dir: &Path, batch_id: &str) -> Result<RestoreResult, CleanerError> {
    let batch_dir = data_dir.join("graveyard").join(batch_id);
    let manifest_path = batch_dir.join("manifest.json");
    let encoded =
        fs::read(&manifest_path).map_err(|e| CleanerError::Io(format!("read manifest: {e}")))?;
    let manifest: BatchManifest = serde_json::from_slice(&encoded)
        .map_err(|e| CleanerError::Audit(format!("parse manifest: {e}")))?;

    let mut restored: Vec<String> = Vec::new();
    let mut failed: Vec<DeleteFailure> = Vec::new();
    let mut bytes_restored: u64 = 0;

    for entry in &manifest.items {
        let moved = PathBuf::from(&entry.moved_path);
        let orig = PathBuf::from(&entry.orig_path);

        if fs::symlink_metadata(&moved).is_err() {
            if entry.state == ManifestEntryState::Pending && orig.exists() {
                continue;
            }
            failed.push(DeleteFailure {
                path: entry.orig_path.clone(),
                error: "graveyard entry missing".into(),
            });
            continue;
        }
        if orig.exists() {
            failed.push(DeleteFailure {
                path: entry.orig_path.clone(),
                error: "original path now exists, refusing to overwrite".into(),
            });
            continue;
        }
        if let Some(parent) = orig.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                failed.push(DeleteFailure {
                    path: entry.orig_path.clone(),
                    error: format!("create parent: {e}"),
                });
                continue;
            }
        }

        match safe_move(&moved, &orig, entry.bytes) {
            Ok(()) => {
                restored.push(entry.orig_path.clone());
                bytes_restored = bytes_restored.saturating_add(entry.bytes);
            }
            Err(e) => failed.push(DeleteFailure {
                path: entry.orig_path.clone(),
                error: e,
            }),
        }
    }

    // all restored = nuke the batch dir, otherwise leave leftovers
    if failed.is_empty() {
        let _ = fs::remove_dir_all(&batch_dir);
    }

    Ok(RestoreResult {
        batch_id: manifest.batch_id,
        restored_at: now_unix(),
        restored,
        failed,
        bytes_restored,
    })
}

/// current graveyard summary. cheap, reads one small JSON per batch,
/// no subtree walks. safe to call on every Junk-screen mount.
pub fn stats(data_dir: &Path) -> Result<GraveyardStats, CleanerError> {
    let grave = data_dir.join("graveyard");
    if !grave.exists() {
        return Ok(GraveyardStats::default());
    }
    let mut stats = GraveyardStats::default();
    for manifest in read_manifests(&grave)? {
        let sum = manifest_bytes_on_disk(&manifest);
        if manifest.status == BatchStatus::Staging && sum == 0 {
            continue;
        }
        stats.batch_count += 1;
        stats.total_bytes = stats.total_bytes.saturating_add(sum);
        stats.oldest_at = Some(match stats.oldest_at {
            None => manifest.created_at,
            Some(o) => o.min(manifest.created_at),
        });
        stats.newest_at = Some(match stats.newest_at {
            None => manifest.created_at,
            Some(n) => n.max(manifest.created_at),
        });
    }
    Ok(stats)
}

/// drop batches older than ttl_secs. called on startup so old undo
/// history doesn't pile up forever. each purge writes an audit record so
/// latest_commit stops returning the dead batch.
pub fn sweep_older_than(
    data_dir: &Path,
    now: u64,
    ttl_secs: u64,
) -> Result<PurgeResult, CleanerError> {
    sweep(data_dir, now, |m| {
        now.saturating_sub(m.created_at) > ttl_secs
    })
}

/// empty the graveyard. irreversible, UI must confirm first.
pub fn purge_all(data_dir: &Path) -> Result<PurgeResult, CleanerError> {
    sweep(data_dir, now_unix(), |_| true)
}

fn sweep<F>(data_dir: &Path, now: u64, should_purge: F) -> Result<PurgeResult, CleanerError>
where
    F: Fn(&BatchManifest) -> bool,
{
    let grave = data_dir.join("graveyard");
    let mut result = PurgeResult {
        purged_at: now,
        ..Default::default()
    };
    if !grave.exists() {
        return Ok(result);
    }
    for manifest in read_manifests(&grave)? {
        if !should_purge(&manifest) {
            continue;
        }
        let batch_dir = grave.join(&manifest.batch_id);
        let batch_bytes = manifest_bytes_on_disk(&manifest);
        match fs::remove_dir_all(&batch_dir) {
            Ok(()) => {
                result.purged.push(manifest.batch_id.clone());
                result.bytes_freed = result.bytes_freed.saturating_add(batch_bytes);
                // audit write failures don't abort, batch is gone anyway
                let _ = audit::append_purge(data_dir, &manifest.batch_id, now, batch_bytes);
            }
            Err(e) => result.failed.push(DeleteFailure {
                path: batch_dir.to_string_lossy().into_owned(),
                error: format!("remove: {e}"),
            }),
        }
    }
    Ok(result)
}

/// readable batch manifests under `grave`. missing/bad manifests are
/// skipped silently, they'll sit there until purge_all wipes them.
fn read_manifests(grave: &Path) -> Result<Vec<BatchManifest>, CleanerError> {
    let dir = match fs::read_dir(grave) {
        Ok(d) => d,
        Err(e) => return Err(CleanerError::Io(format!("read graveyard: {e}"))),
    };
    let mut out = Vec::new();
    for entry in dir.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("manifest.json");
        let Ok(bytes) = fs::read(&manifest_path) else {
            continue;
        };
        if let Ok(m) = serde_json::from_slice::<BatchManifest>(&bytes) {
            out.push(m);
        }
    }
    Ok(out)
}

/// newest batch that still has payloads on disk. used as a recovery path
/// when a crash happened before the audit log received its commit record.
pub fn latest_restorable_batch(data_dir: &Path) -> Result<Option<String>, CleanerError> {
    let grave = data_dir.join("graveyard");
    if !grave.exists() {
        return Ok(None);
    }
    let mut best: Option<BatchManifest> = None;
    for manifest in read_manifests(&grave)? {
        if !manifest.items.iter().any(entry_payload_exists) {
            continue;
        }
        best = match best {
            None => Some(manifest),
            Some(cur)
                if (manifest.created_at, manifest.batch_id.as_str())
                    > (cur.created_at, cur.batch_id.as_str()) =>
            {
                Some(manifest)
            }
            Some(cur) => Some(cur),
        };
    }
    Ok(best.map(|m| m.batch_id))
}

fn manifest_bytes_on_disk(manifest: &BatchManifest) -> u64 {
    manifest
        .items
        .iter()
        .filter(|entry| entry_payload_exists(entry))
        .map(|entry| entry.bytes)
        .sum()
}

fn entry_payload_exists(entry: &ManifestEntry) -> bool {
    fs::symlink_metadata(Path::new(&entry.moved_path)).is_ok()
}

fn write_manifest_atomic(
    manifest_path: &Path,
    manifest: &BatchManifest,
) -> Result<(), CleanerError> {
    let dir = manifest_path
        .parent()
        .ok_or_else(|| CleanerError::Io("manifest path has no parent".into()))?;
    fs::create_dir_all(dir).map_err(|e| CleanerError::Io(format!("create manifest dir: {e}")))?;
    let encoded = serde_json::to_vec_pretty(manifest)
        .map_err(|e| CleanerError::Io(format!("encode manifest: {e}")))?;
    let seq = MANIFEST_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_path = dir.join(format!(
        ".manifest.json.tmp.{}.{}.{}",
        std::process::id(),
        now_nanos(),
        seq
    ));

    {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|e| CleanerError::Io(format!("open manifest tmp: {e}")))?;
        f.write_all(&encoded)
            .map_err(|e| CleanerError::Io(format!("write manifest tmp: {e}")))?;
        f.sync_all()
            .map_err(|e| CleanerError::Io(format!("sync manifest tmp: {e}")))?;
    }

    fs::rename(&tmp_path, manifest_path).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        CleanerError::Io(format!("rename manifest: {e}"))
    })?;
    sync_parent_dir(manifest_path)
        .map_err(|e| CleanerError::Io(format!("sync manifest dir: {e}")))?;
    Ok(())
}

static MANIFEST_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// rename if same fs, copy+delete across devices only after a destination
/// free-space preflight. callers pass the previewed payload bytes so a
/// cross-volume cleanup cannot blindly duplicate a large tree.
fn safe_move(src: &Path, dst: &Path, bytes: u64) -> Result<(), String> {
    match fs::rename(src, dst) {
        Ok(()) => return Ok(()),
        Err(e) => {
            // ErrorKind::CrossesDevices is unstable, check the raw code
            if !is_rename_cross_device_error(&e) {
                // real error (perm denied, ro fs, etc), don't paper it over
                return Err(format!("rename: {e}"));
            }
        }
    }
    preflight_cross_device_copy(dst, bytes)?;
    copy_recursive(src, dst).map_err(|e| format!("copy fallback: {e}"))?;
    remove_recursive(src).map_err(|e| format!("remove after copy: {e}"))?;
    Ok(())
}

fn preflight_cross_device_copy(dst: &Path, bytes: u64) -> Result<(), String> {
    if bytes == 0 {
        return Ok(());
    }
    let available = available_space_for_path(dst).ok_or_else(|| {
        format!(
            "cross-device move needs {bytes} bytes, but destination volume could not be resolved"
        )
    })?;
    let required = bytes.saturating_add(cross_device_copy_margin(bytes));
    if available < required {
        return Err(format!(
            "cross-device move needs {required} bytes available, destination has {available}"
        ));
    }
    Ok(())
}

fn cross_device_copy_margin(bytes: u64) -> u64 {
    const MIN_MARGIN: u64 = 16 * 1024 * 1024;
    (bytes / 100).max(MIN_MARGIN)
}

fn available_space_for_path(path: &Path) -> Option<u64> {
    use sysinfo::Disks;

    let anchor = existing_ancestor(path)?;
    let anchor = normalize_for_mount_match(&anchor);
    let disks = Disks::new_with_refreshed_list();
    let mut best: Option<(usize, u64)> = None;
    for disk in disks.iter() {
        let mount = normalize_for_mount_match(disk.mount_point());
        if !anchor.starts_with(&mount) {
            continue;
        }
        let score = mount.components().count() * 1024 + mount.to_string_lossy().len();
        if best
            .map(|(best_score, _)| score > best_score)
            .unwrap_or(true)
        {
            best = Some((score, disk.available_space()));
        }
    }
    best.map(|(_, available)| available)
}

fn existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut cur = if path.exists() {
        path.to_path_buf()
    } else {
        path.parent()?.to_path_buf()
    };
    loop {
        if cur.exists() {
            return fs::canonicalize(&cur).ok().or(Some(cur));
        }
        if !cur.pop() {
            return None;
        }
    }
}

#[cfg(windows)]
fn normalize_for_mount_match(path: &Path) -> PathBuf {
    PathBuf::from(
        path.to_string_lossy()
            .replace('/', "\\")
            .to_ascii_lowercase(),
    )
}

#[cfg(not(windows))]
fn normalize_for_mount_match(path: &Path) -> PathBuf {
    path.to_path_buf()
}

/// EXDEV=18 on linux/mac, ERROR_NOT_SAME_DEVICE=17 on windows
fn is_rename_cross_device_error(e: &io::Error) -> bool {
    if e.raw_os_error() == Some(18) {
        return true;
    }
    #[cfg(windows)]
    {
        if e.raw_os_error() == Some(17) {
            return true;
        }
    }
    false
}

fn copy_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    let ft = meta.file_type();
    if ft.is_file() {
        fs::copy(src, dst)?;
        return Ok(());
    }
    if ft.is_symlink() {
        // preserve the link, not the target
        #[cfg(unix)]
        {
            let target = fs::read_link(src)?;
            std::os::unix::fs::symlink(target, dst)?;
            return Ok(());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::{symlink_dir, symlink_file, FileTypeExt};

            let target = fs::read_link(src)?;
            if ft.is_symlink_dir() {
                symlink_dir(target, dst)?;
            } else if ft.is_symlink_file() {
                symlink_file(target, dst)?;
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("unsupported windows symlink type: {}", src.display()),
                ));
            }
            return Ok(());
        }
    }
    #[cfg(windows)]
    if is_windows_reparse_point(&meta) {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("unsupported windows reparse point: {}", src.display()),
        ));
    }
    if ft.is_dir() {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let child_src = entry.path();
            let child_dst = dst.join(entry.file_name());
            copy_recursive(&child_src, &child_dst)?;
        }
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        format!("unsupported file type: {}", src.display()),
    ))
}

#[cfg(windows)]
fn is_windows_reparse_point(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

fn remove_recursive(p: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(p)?;
    if meta.is_dir() && !meta.file_type().is_symlink() {
        fs::remove_dir_all(p)
    } else {
        fs::remove_file(p)
    }
}

fn new_batch_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("b-{now_ms:x}-{n:x}")
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(unix)]
fn sync_parent_dir(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent_dir(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cleaner::plan::build_plan;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_file(path: &Path, size: usize) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        let mut f = File::create(path).unwrap();
        f.write_all(&vec![0u8; size]).unwrap();
    }

    /// home dir with known junk + scratch data dir. both under one
    /// tempdir so rename stays in-device.
    fn fixture() -> (TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let data = tmp.path().join("data");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&data).unwrap();
        write_file(&home.join(".cache/spotify/blob.bin"), 2048);
        write_file(&home.join(".cache/spotify/sub/other.bin"), 1024);
        write_file(&home.join(".cache/slack/session"), 4096);
        (tmp, home, data)
    }

    // ---------- commit ----------

    #[test]
    fn commit_moves_files_to_graveyard() {
        let (_tmp, home, data) = fixture();
        let plan = build_plan(
            &home,
            vec![
                home.join(".cache/spotify"),
                home.join(".cache/slack/session"),
            ],
        );
        let bytes_before = plan.total_bytes;
        let result = commit(&data, &plan).unwrap();

        assert_eq!(result.failed.len(), 0);
        assert_eq!(result.committed.len(), 2);
        assert_eq!(result.bytes_trashed, bytes_before);

        // originals gone
        assert!(!home.join(".cache/spotify").exists());
        assert!(!home.join(".cache/slack/session").exists());

        // batch + manifest written
        let batch_dir = data.join("graveyard").join(&result.batch_id);
        assert!(batch_dir.join("manifest.json").exists());
        let m: BatchManifest =
            serde_json::from_slice(&fs::read(batch_dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(m.status, BatchStatus::Committed);
        assert_eq!(m.items.len(), 2);
        assert!(m
            .items
            .iter()
            .all(|entry| entry.state == ManifestEntryState::Moved));
    }

    #[test]
    fn commit_preserves_original_file_name_inside_slot() {
        let (_tmp, home, data) = fixture();
        let plan = build_plan(&home, vec![home.join(".cache/slack/session")]);
        let result = commit(&data, &plan).unwrap();
        let batch_dir = data.join("graveyard").join(&result.batch_id);
        // items/000000/session should exist
        assert!(batch_dir.join("items/000000/session").exists());
    }

    #[test]
    fn commit_skips_protected_items() {
        let (_tmp, home, data) = fixture();
        // home is protected, only the real path should move
        let plan = build_plan(&home, vec![home.clone(), home.join(".cache/slack/session")]);
        let result = commit(&data, &plan).unwrap();
        assert_eq!(result.committed.len(), 1);
        assert!(result.committed[0].ends_with("session"));
        assert!(home.exists());
    }

    #[test]
    fn commit_without_items_still_writes_empty_manifest() {
        let (_tmp, home, data) = fixture();
        let plan = build_plan(&home, vec![home.clone()]); // protected only
        let result = commit(&data, &plan).unwrap();
        assert_eq!(result.committed.len(), 0);
        let batch_dir = data.join("graveyard").join(&result.batch_id);
        let m: BatchManifest =
            serde_json::from_slice(&fs::read(batch_dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(m.items.len(), 0);
    }

    #[test]
    fn commit_batch_ids_are_unique() {
        let (_tmp, home, data) = fixture();
        let p1 = build_plan(&home, vec![home.join(".cache/slack/session")]);
        let r1 = commit(&data, &p1).unwrap();
        write_file(&home.join(".cache/slack/session"), 1024);
        let p2 = build_plan(&home, vec![home.join(".cache/slack/session")]);
        let r2 = commit(&data, &p2).unwrap();
        assert_ne!(r1.batch_id, r2.batch_id);
    }

    #[test]
    fn commit_handles_directory_tree_move() {
        let (_tmp, home, data) = fixture();
        let plan = build_plan(&home, vec![home.join(".cache/spotify")]);
        let result = commit(&data, &plan).unwrap();
        assert_eq!(result.committed.len(), 1);
        // tree preserved inside the slot
        let slot = data
            .join("graveyard")
            .join(&result.batch_id)
            .join("items/000000");
        assert!(slot.join("spotify/blob.bin").exists());
        assert!(slot.join("spotify/sub/other.bin").exists());
    }

    // ---------- restore ----------

    #[test]
    fn restore_puts_files_back() {
        let (_tmp, home, data) = fixture();
        let plan = build_plan(&home, vec![home.join(".cache/spotify")]);
        let bytes = plan.total_bytes;
        let result = commit(&data, &plan).unwrap();
        assert!(!home.join(".cache/spotify").exists());

        let restored = restore_batch(&data, &result.batch_id).unwrap();
        assert_eq!(restored.failed.len(), 0);
        assert_eq!(restored.restored.len(), 1);
        assert_eq!(restored.bytes_restored, bytes);

        // files back where they were
        assert!(home.join(".cache/spotify/blob.bin").exists());
        assert!(home.join(".cache/spotify/sub/other.bin").exists());

        // full success wipes the batch dir
        let batch_dir = data.join("graveyard").join(&result.batch_id);
        assert!(!batch_dir.exists());
    }

    #[test]
    fn restore_refuses_to_overwrite_recreated_path() {
        let (_tmp, home, data) = fixture();
        let plan = build_plan(&home, vec![home.join(".cache/slack/session")]);
        let result = commit(&data, &plan).unwrap();
        // user re-created the file at the same path
        write_file(&home.join(".cache/slack/session"), 16);

        let restored = restore_batch(&data, &result.batch_id).unwrap();
        assert_eq!(restored.restored.len(), 0);
        assert_eq!(restored.failed.len(), 1);
        assert!(restored.failed[0].error.contains("now exists"));
        // partial-success contract, batch dir sticks around
        assert!(data.join("graveyard").join(&result.batch_id).exists());
    }

    #[test]
    fn restore_recovers_staged_pending_payload_after_crash() {
        let (_tmp, home, data) = fixture();
        let batch_id = "b-crash";
        let orig = home.join(".cache/slack/session");
        let moved = data
            .join("graveyard")
            .join(batch_id)
            .join("items/000000/session");
        fs::create_dir_all(moved.parent().unwrap()).unwrap();
        fs::rename(&orig, &moved).unwrap();

        let manifest = BatchManifest {
            batch_id: batch_id.into(),
            created_at: now_unix(),
            status: BatchStatus::Staging,
            items: vec![ManifestEntry {
                orig_path: orig.to_string_lossy().into_owned(),
                moved_path: moved.to_string_lossy().into_owned(),
                bytes: 4096,
                file_count: 1,
                kind: ItemKind::File,
                state: ManifestEntryState::Pending,
            }],
        };
        write_manifest_atomic(
            &data.join("graveyard").join(batch_id).join("manifest.json"),
            &manifest,
        )
        .unwrap();

        assert_eq!(
            latest_restorable_batch(&data).unwrap(),
            Some(batch_id.into())
        );
        let restored = restore_batch(&data, batch_id).unwrap();
        assert_eq!(restored.failed.len(), 0);
        assert_eq!(restored.restored.len(), 1);
        assert!(orig.exists());
    }

    #[test]
    fn restore_creates_missing_parent_directory() {
        let (_tmp, home, data) = fixture();
        // move the whole slack dir so "slack" parent disappears
        let plan = build_plan(&home, vec![home.join(".cache/slack")]);
        let result = commit(&data, &plan).unwrap();
        assert!(!home.join(".cache/slack").exists());
        let restored = restore_batch(&data, &result.batch_id).unwrap();
        assert_eq!(restored.failed.len(), 0);
        assert!(home.join(".cache/slack/session").exists());
    }

    #[test]
    fn restore_of_unknown_batch_is_an_error() {
        let (_tmp, _home, data) = fixture();
        let e = restore_batch(&data, "b-does-not-exist").err().unwrap();
        assert!(matches!(e, CleanerError::Io(_)));
    }

    // ---------- paths within graveyard manifest ----------

    #[test]
    fn manifest_records_are_ordered_as_committed() {
        let (_tmp, home, data) = fixture();
        let plan = build_plan(
            &home,
            vec![
                home.join(".cache/slack/session"),
                home.join(".cache/spotify/blob.bin"),
            ],
        );
        let result = commit(&data, &plan).unwrap();
        let batch_dir = data.join("graveyard").join(&result.batch_id);
        let m: BatchManifest =
            serde_json::from_slice(&fs::read(batch_dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(m.items.len(), 2);
        // same order as plan.items post-protect filter. plan sorts by bytes
        // desc, session (4096) > blob (2048)
        assert!(m.items[0].orig_path.ends_with("session"));
        assert!(m.items[1].orig_path.ends_with("blob.bin"));
    }

    // ---------- symlinks ----------

    #[test]
    #[cfg(unix)]
    fn commit_moves_symlinks_without_following() {
        use std::os::unix::fs as unix_fs;
        let (_tmp, home, data) = fixture();
        // outside-home file we must not touch
        let outside = home.parent().unwrap().join("untouchable.bin");
        write_file(&outside, 8 * 1024);
        let link = home.join(".cache/outside-link");
        unix_fs::symlink(&outside, &link).unwrap();

        let plan = build_plan(&home, vec![link.clone()]);
        let result = commit(&data, &plan).unwrap();

        assert_eq!(result.committed.len(), 1);
        // symlink gone, target intact
        assert!(!link.exists());
        assert!(outside.exists());
        // slot holds a symlink, not a copy of the target
        let slot = data
            .join("graveyard")
            .join(&result.batch_id)
            .join("items/000000/outside-link");
        let m = fs::symlink_metadata(&slot).unwrap();
        assert!(m.file_type().is_symlink());
    }

    // ---------- cross-device fallback ----------
    //
    // forcing a cross-device rename portably needs a 2nd mount. not worth
    // it. cover the path indirectly via copy_recursive.

    #[test]
    fn copy_recursive_preserves_file_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src/a.bin");
        let dst = tmp.path().join("dst/a.bin");
        write_file(&src, 2048);
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        copy_recursive(&src, &dst).unwrap();
        assert_eq!(fs::metadata(&dst).unwrap().len(), 2048);
    }

    #[test]
    fn copy_recursive_copies_directory_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        write_file(&src.join("a.bin"), 100);
        write_file(&src.join("nested/b.bin"), 50);
        let dst = tmp.path().join("dst");
        copy_recursive(&src, &dst).unwrap();
        assert_eq!(fs::metadata(dst.join("a.bin")).unwrap().len(), 100);
        assert_eq!(fs::metadata(dst.join("nested/b.bin")).unwrap().len(), 50);
    }

    // ---------- perf guard ----------

    // ---------- stats + sweeps ----------

    #[test]
    fn stats_on_empty_graveyard_is_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let s = stats(tmp.path()).unwrap();
        assert_eq!(s.batch_count, 0);
        assert_eq!(s.total_bytes, 0);
        assert!(s.oldest_at.is_none());
        assert!(s.newest_at.is_none());
    }

    #[test]
    fn stats_sums_across_all_batches() {
        let (_tmp, home, data) = fixture();
        let p1 = build_plan(&home, vec![home.join(".cache/slack/session")]);
        let b1 = p1.total_bytes;
        commit(&data, &p1).unwrap();
        let p2 = build_plan(&home, vec![home.join(".cache/spotify")]);
        let b2 = p2.total_bytes;
        commit(&data, &p2).unwrap();

        let s = stats(&data).unwrap();
        assert_eq!(s.batch_count, 2);
        assert_eq!(s.total_bytes, b1 + b2);
        assert!(s.oldest_at.unwrap() <= s.newest_at.unwrap());
    }

    #[test]
    fn purge_all_empties_the_graveyard() {
        let (_tmp, home, data) = fixture();
        commit(
            &data,
            &build_plan(&home, vec![home.join(".cache/slack/session")]),
        )
        .unwrap();
        commit(&data, &build_plan(&home, vec![home.join(".cache/spotify")])).unwrap();

        let r = purge_all(&data).unwrap();
        assert_eq!(r.purged.len(), 2);
        assert_eq!(r.failed.len(), 0);
        assert!(r.bytes_freed > 0);

        let s = stats(&data).unwrap();
        assert_eq!(s.batch_count, 0);
    }

    #[test]
    fn purge_all_writes_audit_records_that_mask_latest_commit() {
        let (_tmp, home, data) = fixture();
        // graveyard::commit doesn't touch the audit log, that's on the
        // Cleaner facade, so stage a commit record before purge
        let r = commit(
            &data,
            &build_plan(&home, vec![home.join(".cache/slack/session")]),
        )
        .unwrap();
        audit::append_commit(&data, &r).unwrap();
        assert!(audit::latest_commit(&data).unwrap().is_some());
        purge_all(&data).unwrap();
        // purge wrote an audit record that masks the commit, so
        // restore_last sees nothing and won't poke a gone graveyard dir
        assert!(audit::latest_commit(&data).unwrap().is_none());
    }

    #[test]
    fn sweep_older_than_only_purges_stale_batches() {
        let (_tmp, home, data) = fixture();
        let plan = build_plan(&home, vec![home.join(".cache/slack/session")]);
        let result = commit(&data, &plan).unwrap();

        // rewrite created_at to fake an old batch
        let manifest_path = data
            .join("graveyard")
            .join(&result.batch_id)
            .join("manifest.json");
        let mut m: BatchManifest =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        m.created_at = 100;
        fs::write(&manifest_path, serde_json::to_vec_pretty(&m).unwrap()).unwrap();

        // now=1000 ttl=100, batch is 900s old, past ttl, purged
        let r = sweep_older_than(&data, 1000, 100).unwrap();
        assert_eq!(r.purged.len(), 1);
        assert!(!data.join("graveyard").join(&result.batch_id).exists());
    }

    #[test]
    fn sweep_older_than_keeps_fresh_batches() {
        let (_tmp, home, data) = fixture();
        let plan = build_plan(&home, vec![home.join(".cache/slack/session")]);
        let result = commit(&data, &plan).unwrap();
        // huge ttl, batch stays
        let r = sweep_older_than(&data, now_unix(), 10_000_000).unwrap();
        assert_eq!(r.purged.len(), 0);
        assert!(data.join("graveyard").join(&result.batch_id).exists());
    }

    #[test]
    fn sweep_is_a_noop_on_missing_graveyard() {
        let tmp = tempfile::tempdir().unwrap();
        let r = sweep_older_than(tmp.path(), 1000, 10).unwrap();
        assert_eq!(r.purged.len(), 0);
        assert_eq!(r.failed.len(), 0);
    }

    #[test]
    fn stats_skips_corrupt_manifests() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().to_path_buf();
        let grave = data.join("graveyard");
        fs::create_dir_all(grave.join("bogus")).unwrap();
        fs::write(grave.join("bogus/manifest.json"), b"{ not json").unwrap();
        let s = stats(&data).unwrap();
        assert_eq!(s.batch_count, 0);
    }

    #[test]
    fn purge_all_leaves_audit_log_readable() {
        let (_tmp, home, data) = fixture();
        let r = commit(
            &data,
            &build_plan(&home, vec![home.join(".cache/slack/session")]),
        )
        .unwrap();
        audit::append_commit(&data, &r).unwrap();
        purge_all(&data).unwrap();
        let all = audit::read_all(&data).unwrap();
        // one commit + one purge
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].op, "commit");
        assert_eq!(all[1].op, "purge");
    }

    #[test]
    fn commit_completes_quickly_on_moderate_tree() {
        // sanity, 200 small files should move sub-second
        let (_tmp, home, data) = fixture();
        for i in 0..200 {
            write_file(&home.join(format!(".cache/big/f{i:04}.bin")), 256);
        }
        let plan = build_plan(&home, vec![home.join(".cache/big")]);
        let start = std::time::Instant::now();
        let _ = commit(&data, &plan).unwrap();
        assert!(start.elapsed().as_secs() < 5);
    }
}
