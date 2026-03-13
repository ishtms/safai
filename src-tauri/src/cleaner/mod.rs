//! safe deletion engine + undo.
//!
//! cleaner never hard-deletes. commit moves eligible files into a
//! graveyard under the user data dir (`~/.local/share/safai/` on linux,
//! `~/Library/Application Support/safai/` on mac, `%APPDATA%\safai\` on
//! win). per-batch manifest.json records origin paths so restore_last can
//! put them back. no dependency on non-portable Recycle Bin / .Trash
//! APIs.
//!
//! flow:
//!
//! 1. preview(paths) stats candidates, classifies safety, sums dir sizes,
//!    returns a DeletePlan with a token. plan is cached in-memory so the
//!    frontend can confirm and commit by token without re-submitting the
//!    paths (would be a TOCTOU if they disagree).
//! 2. commit(token) looks up the plan, moves non-protected items into the
//!    graveyard, writes manifest, appends to audit.log.
//! 3. restore_last reads the audit tail, finds the latest commit batch,
//!    moves items back to orig_path. skips anything the user recreated.
//!
//! safety policy (safety::classify) is paranoid on purpose. rejects
//! system roots (/, /usr, C:\Windows), home itself, primary user folders
//! (Documents, Desktop, Downloads, Pictures, Music, Videos), and any
//! ancestor of the above. scanner catalog never surfaces these but the
//! cleaner revalidates as belt-and-suspenders. callers can't trash an
//! unsafe path even with a hand-crafted list.
//!
//! concurrency: preview + commit don't hold the plan lock across fs I/O.
//! two simultaneous commits of the same token serialise: first wins,
//! second gets UnknownToken because the winner removes it from cache.

pub mod audit;
pub mod graveyard;
pub mod plan;
pub mod safety;
pub mod types;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub use types::{
    CleanerError, DeletePlan, DeleteResult, GraveyardStats, PurgeResult, RestoreResult,
};

/// 14 days. if they haven't come looking by now they won't. empty-trash
/// in UI covers the "reclaim space now" case.
pub const DEFAULT_GRAVEYARD_TTL_SECS: u64 = 14 * 24 * 3600;

/// top-level facade. holds the plan cache and owns the data-dir paths.
/// tauri keeps one for process lifetime, all methods take &self.
pub struct Cleaner {
    data_dir: PathBuf,
    home: PathBuf,
    plans: Mutex<HashMap<String, DeletePlan>>,
}

impl Cleaner {
    /// neither path needs to exist yet, subdirs are created lazily
    pub fn new(data_dir: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            home: home.into(),
            plans: Mutex::new(HashMap::new()),
        }
    }

    /// build a plan, cache it by token. frontend confirms, then commits
    /// by token.
    pub fn preview(&self, paths: Vec<PathBuf>) -> DeletePlan {
        let plan = plan::build_plan(&self.home, paths);
        if let Ok(mut g) = self.plans.lock() {
            // evict stale plans so preview-without-commit doesn't leak
            plan::prune_stale(&mut g, plan.created_at);
            g.insert(plan.token.clone(), plan.clone());
        }
        plan
    }

    /// commit by token. plan is yanked from cache first so a racing
    /// second commit gets UnknownToken.
    pub fn commit(&self, token: &str) -> Result<DeleteResult, CleanerError> {
        let plan = match self.plans.lock() {
            Ok(mut g) => g.remove(token),
            Err(_) => return Err(CleanerError::Io("plan cache poisoned".into())),
        };
        let plan = plan.ok_or(CleanerError::UnknownToken)?;
        let result = graveyard::commit(&self.data_dir, &plan)?;
        // audit append errors surface but don't roll back the move,
        // files are safe in the graveyard and manifest is intact
        audit::append_commit(&self.data_dir, &result)?;
        Ok(result)
    }

    /// used by the UI readout and to enable/disable Empty
    pub fn graveyard_stats(&self) -> Result<GraveyardStats, CleanerError> {
        graveyard::stats(&self.data_dir)
    }

    /// irreversible, caller must confirm first
    pub fn purge_all(&self) -> Result<PurgeResult, CleanerError> {
        graveyard::purge_all(&self.data_dir)
    }

    /// called on startup with DEFAULT_GRAVEYARD_TTL_SECS. separate so
    /// tests can pass a tiny ttl.
    pub fn sweep_stale(&self, ttl_secs: u64) -> Result<PurgeResult, CleanerError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        graveyard::sweep_older_than(&self.data_dir, now, ttl_secs)
    }

    pub fn restore_last(&self) -> Result<RestoreResult, CleanerError> {
        let latest = audit::latest_commit(&self.data_dir)?
            .ok_or(CleanerError::NothingToRestore)?;
        let result = graveyard::restore_batch(&self.data_dir, &latest.batch_id)?;
        audit::append_restore(&self.data_dir, &result)?;
        Ok(result)
    }

    #[cfg(test)]
    pub fn plan_count(&self) -> usize {
        self.plans.lock().map(|g| g.len()).unwrap_or(0)
    }

    #[cfg(test)]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
}

#[cfg(test)]
mod tests {
    //! e2e tests for the Cleaner facade. unit-level is per-submodule,
    //! these run preview -> commit -> restore through the public API so
    //! wiring regressions (token round-trip, audit log, data-dir layout)
    //! get caught here.

    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use std::sync::Arc;

    fn write_file(path: &Path, size: usize) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        let mut f = File::create(path).unwrap();
        f.write_all(&vec![0u8; size]).unwrap();
    }

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let data = tmp.path().join("data");
        fs::create_dir_all(&home).unwrap();
        write_file(&home.join(".cache/spotify/blob.bin"), 4096);
        write_file(&home.join(".cache/slack/session"), 2048);
        (tmp, home, data)
    }

    #[test]
    fn preview_stores_plan_in_cache() {
        let (_tmp, home, data) = fixture();
        let c = Cleaner::new(&data, &home);
        let plan = c.preview(vec![home.join(".cache/spotify")]);
        assert_eq!(c.plan_count(), 1);
        assert!(plan.total_bytes > 0);
    }

    #[test]
    fn commit_consumes_plan_token() {
        let (_tmp, home, data) = fixture();
        let c = Cleaner::new(&data, &home);
        let plan = c.preview(vec![home.join(".cache/slack/session")]);
        c.commit(&plan.token).unwrap();
        assert_eq!(c.plan_count(), 0, "plan was not removed from cache");
        // second commit with same token must fail
        let err = c.commit(&plan.token).unwrap_err();
        assert!(matches!(err, CleanerError::UnknownToken));
    }

    #[test]
    fn commit_then_restore_round_trip() {
        let (_tmp, home, data) = fixture();
        let c = Cleaner::new(&data, &home);

        let plan = c.preview(vec![
            home.join(".cache/spotify"),
            home.join(".cache/slack"),
        ]);
        let want_bytes = plan.total_bytes;
        let commit = c.commit(&plan.token).unwrap();
        assert_eq!(commit.bytes_trashed, want_bytes);
        assert!(!home.join(".cache/spotify").exists());
        assert!(!home.join(".cache/slack").exists());

        let restore = c.restore_last().unwrap();
        assert_eq!(restore.restored.len(), 2);
        assert_eq!(restore.bytes_restored, want_bytes);
        assert!(home.join(".cache/spotify/blob.bin").exists());
        assert!(home.join(".cache/slack/session").exists());
    }

    #[test]
    fn restore_last_errors_when_nothing_committed() {
        let (_tmp, home, data) = fixture();
        let c = Cleaner::new(&data, &home);
        let err = c.restore_last().unwrap_err();
        assert!(matches!(err, CleanerError::NothingToRestore));
    }

    #[test]
    fn restore_last_returns_to_none_after_undoing_everything() {
        let (_tmp, home, data) = fixture();
        let c = Cleaner::new(&data, &home);
        let plan = c.preview(vec![home.join(".cache/slack/session")]);
        c.commit(&plan.token).unwrap();
        c.restore_last().unwrap();
        // already restored, second restore_last should surface empty
        // state, not re-restore
        let err = c.restore_last().unwrap_err();
        assert!(matches!(err, CleanerError::NothingToRestore));
    }

    #[test]
    fn stacked_commits_restore_in_lifo_order() {
        let (_tmp, home, data) = fixture();
        let c = Cleaner::new(&data, &home);

        // commit A: slack
        let a = c.preview(vec![home.join(".cache/slack/session")]);
        c.commit(&a.token).unwrap();
        // commit B: spotify
        let b = c.preview(vec![home.join(".cache/spotify")]);
        c.commit(&b.token).unwrap();

        // first restore should bring back spotify (newer)
        let r1 = c.restore_last().unwrap();
        assert_eq!(r1.restored.len(), 1);
        assert!(r1.restored[0].ends_with("spotify"));
        assert!(home.join(".cache/spotify/blob.bin").exists());
        assert!(!home.join(".cache/slack/session").exists());

        // second restore brings back slack
        let r2 = c.restore_last().unwrap();
        assert_eq!(r2.restored.len(), 1);
        assert!(r2.restored[0].ends_with("session"));
    }

    #[test]
    fn protected_home_dir_cannot_be_committed_even_if_user_submits_it() {
        let (_tmp, home, data) = fixture();
        let c = Cleaner::new(&data, &home);
        // user tries to trash home itself + a real cache dir
        let plan = c.preview(vec![home.clone(), home.join(".cache/spotify")]);
        // home stays in the plan but flagged protected
        let home_item = plan
            .items
            .iter()
            .find(|i| PathBuf::from(&i.path) == home)
            .expect("home included in plan");
        assert!(home_item.protected);
        assert!(home_item.bytes == 0);

        let result = c.commit(&plan.token).unwrap();
        // only the cache entry moved
        assert_eq!(result.committed.len(), 1);
        assert!(home.exists(), "home must not be trashed");
    }

    #[test]
    fn concurrent_commits_with_same_token_serialise_safely() {
        // two threads racing the same plan: exactly one success + one
        // UnknownToken. no leak, no double-move, no panic.
        let (_tmp, home, data) = fixture();
        let c = Arc::new(Cleaner::new(&data, &home));
        let plan = c.preview(vec![home.join(".cache/slack/session")]);
        let token = plan.token.clone();

        let c1 = Arc::clone(&c);
        let t1 = token.clone();
        let h1 = std::thread::spawn(move || c1.commit(&t1));
        let c2 = Arc::clone(&c);
        let t2 = token;
        let h2 = std::thread::spawn(move || c2.commit(&t2));

        let r1 = h1.join().unwrap();
        let r2 = h2.join().unwrap();
        let successes = [&r1, &r2].iter().filter(|r| r.is_ok()).count();
        assert_eq!(successes, 1, "exactly one concurrent commit must win");
        let failures = [&r1, &r2]
            .iter()
            .filter(|r| matches!(r, Err(CleanerError::UnknownToken)))
            .count();
        assert_eq!(failures, 1);
        assert_eq!(c.plan_count(), 0);
    }

    #[test]
    fn data_dir_is_created_lazily_on_first_commit() {
        let (_tmp, home, data) = fixture();
        assert!(!data.exists());
        let c = Cleaner::new(&data, &home);
        let plan = c.preview(vec![home.join(".cache/slack/session")]);
        c.commit(&plan.token).unwrap();
        assert!(data.join("audit.log").exists());
        assert!(data.join("graveyard").exists());
    }

    #[test]
    fn graveyard_stats_tracks_committed_batches() {
        let (_tmp, home, data) = fixture();
        let c = Cleaner::new(&data, &home);
        assert_eq!(c.graveyard_stats().unwrap().batch_count, 0);
        let plan = c.preview(vec![home.join(".cache/slack/session")]);
        c.commit(&plan.token).unwrap();
        let s = c.graveyard_stats().unwrap();
        assert_eq!(s.batch_count, 1);
        assert!(s.total_bytes > 0);
    }

    #[test]
    fn purge_all_clears_graveyard_and_stops_restore() {
        let (_tmp, home, data) = fixture();
        let c = Cleaner::new(&data, &home);
        let plan = c.preview(vec![home.join(".cache/slack/session")]);
        c.commit(&plan.token).unwrap();
        let r = c.purge_all().unwrap();
        assert_eq!(r.purged.len(), 1);
        assert_eq!(c.graveyard_stats().unwrap().batch_count, 0);
        // undo is dead, purge record masks the commit
        assert!(matches!(
            c.restore_last().unwrap_err(),
            CleanerError::NothingToRestore
        ));
    }

    #[test]
    fn sweep_stale_skips_fresh_batches() {
        let (_tmp, home, data) = fixture();
        let c = Cleaner::new(&data, &home);
        let plan = c.preview(vec![home.join(".cache/slack/session")]);
        c.commit(&plan.token).unwrap();
        let r = c.sweep_stale(DEFAULT_GRAVEYARD_TTL_SECS).unwrap();
        assert_eq!(r.purged.len(), 0);
        assert_eq!(c.graveyard_stats().unwrap().batch_count, 1);
    }

    #[test]
    fn default_data_dir_points_under_home_on_unix() {
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let home = PathBuf::from("/home/someone");
            // clear XDG_DATA_HOME for determinism
            let prev = std::env::var_os("XDG_DATA_HOME");
            std::env::remove_var("XDG_DATA_HOME");
            let d = default_data_dir(&home);
            assert_eq!(d, home.join(".local/share/safai"));
            if let Some(p) = prev {
                std::env::set_var("XDG_DATA_HOME", p);
            }
        }
    }
}

/// where graveyard + audit log go. platform-conventional dirs, falls
/// back to `<home>/.safai` if we can't resolve.
pub fn default_data_dir(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        return home
            .join("Library")
            .join("Application Support")
            .join("safai");
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("safai");
        }
        return home.join("AppData").join("Roaming").join("safai");
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            return PathBuf::from(xdg).join("safai");
        }
        home.join(".local").join("share").join("safai")
    }
}
