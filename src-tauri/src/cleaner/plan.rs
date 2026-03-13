//! preview: raw path list -> validated DeletePlan + token.
//!
//! - stat each path once (size + file count)
//! - classify via safety::classify, surface reason when protected
//! - dedupe exact-equal paths
//! - shadow-suppress descendants so total_bytes doesn't double-count
//! - token so commit is authenticated against preview

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::safety;
use super::types::{DeletePlan, ItemKind, PendingDelete};

/// matches the frontend modal timeout. if user wanders off for 5 min
/// their plan expires before they confirm stale data.
pub const PLAN_TTL_SECS: u64 = 300;

/// build a plan. deterministic for the input modulo token + timestamp.
pub fn build_plan(home: &Path, raw_paths: Vec<PathBuf>) -> DeletePlan {
    // 1. normalise + dedupe. hashset on the normalised form, keeps
    //    original string for display but drops trailing-slash / ".."
    //    duplicates
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut normed: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(raw_paths.len());
    for raw in raw_paths {
        let norm = safety::normalize(&raw);
        if !seen.insert(norm.clone()) {
            continue;
        }
        normed.push((raw, norm));
    }

    // 2. stat + classify BEFORE shadow-suppression. protected ancestor
    //    must not eat its descendants. user could submit $HOME
    //    (protected) and $HOME/.cache/slack, cache entry is still valid
    //    on its own.
    normed.sort_by_key(|(_, n)| n.components().count());
    let staged: Vec<(PathBuf, PendingDelete)> = normed
        .into_iter()
        .map(|(raw, norm)| (norm, stat_and_classify(home, &raw)))
        .collect();

    // 3. shadow-suppress. drop a descendant only when an already-kept
    //    path is a non-protected ancestor. that ancestor moves whole
    //    which covers the descendant.
    let mut items: Vec<PendingDelete> = Vec::with_capacity(staged.len());
    let mut kept_norms: Vec<PathBuf> = Vec::new();
    for (norm, pending) in staged {
        let shadowed_by_kept = kept_norms
            .iter()
            .zip(items.iter())
            .any(|(kn, ki)| !ki.protected && safety::is_strict_ancestor(kn, &norm));
        if shadowed_by_kept {
            continue;
        }
        kept_norms.push(norm);
        items.push(pending);
    }

    // 4. sort by bytes desc so the modal shows biggest wins first.
    //    protected items to the end, sorted by path for stability
    items.sort_by(|a, b| match (a.protected, b.protected) {
        (false, false) => b.bytes.cmp(&a.bytes),
        (true, true) => a.path.cmp(&b.path),
        (false, true) => std::cmp::Ordering::Less,
        (true, false) => std::cmp::Ordering::Greater,
    });

    let mut total_bytes = 0u64;
    let mut total_count = 0u64;
    let mut protected_count = 0u64;
    for i in &items {
        if i.protected {
            protected_count += 1;
        } else {
            total_bytes = total_bytes.saturating_add(i.bytes);
            total_count += 1;
        }
    }

    DeletePlan {
        token: new_token(),
        created_at: now_unix(),
        items,
        total_bytes,
        total_count,
        protected_count,
    }
}

/// drop plans older than PLAN_TTL_SECS. called from preview so the
/// cache doesn't grow unbounded.
pub fn prune_stale(cache: &mut HashMap<String, DeletePlan>, now: u64) {
    cache.retain(|_, v| now.saturating_sub(v.created_at) <= PLAN_TTL_SECS);
}

fn stat_and_classify(home: &Path, raw: &Path) -> PendingDelete {
    let path_str = raw.to_string_lossy().into_owned();

    // safety first. protected path = don't count bytes, otherwise the
    // "total cleaned" number is a lie
    if let Some(reason) = safety::classify(home, raw) {
        return PendingDelete {
            path: path_str,
            bytes: 0,
            file_count: 0,
            kind: ItemKind::File, // placeholder, UI only reads kind when !protected
            protected: true,
            protected_reason: Some(reason.into()),
        };
    }

    // symlink_metadata doesn't follow links. a symlink in a cache is
    // the link itself, not its target
    match std::fs::symlink_metadata(raw) {
        Err(_) => PendingDelete {
            path: path_str,
            bytes: 0,
            file_count: 0,
            kind: ItemKind::Missing,
            protected: true,
            protected_reason: Some("path does not exist".into()),
        },
        Ok(meta) => {
            let ft = meta.file_type();
            if ft.is_symlink() {
                return PendingDelete {
                    path: path_str,
                    bytes: 0,
                    file_count: 1,
                    kind: ItemKind::Symlink,
                    protected: false,
                    protected_reason: None,
                };
            }
            if ft.is_file() {
                return PendingDelete {
                    path: path_str,
                    bytes: meta.len(),
                    file_count: 1,
                    kind: ItemKind::File,
                    protected: false,
                    protected_reason: None,
                };
            }
            if ft.is_dir() {
                let (bytes, file_count) = sum_subtree(raw);
                return PendingDelete {
                    path: path_str,
                    bytes,
                    file_count,
                    kind: ItemKind::Directory,
                    protected: false,
                    protected_reason: None,
                };
            }
            PendingDelete {
                path: path_str,
                bytes: 0,
                file_count: 0,
                kind: ItemKind::Missing,
                protected: true,
                protected_reason: Some("unsupported file type".into()),
            }
        }
    }
}

/// bytes + file count via jwalk (rayon parallel). symlinks = 1 file, 0
/// bytes, not followed. matches scanner convention.
fn sum_subtree(dir: &Path) -> (u64, u64) {
    let mut bytes = 0u64;
    let mut files = 0u64;
    for entry in jwalk::WalkDir::new(dir)
        .skip_hidden(false)
        .follow_links(false)
    {
        let Ok(e) = entry else { continue };
        let Ok(meta) = e.metadata() else { continue };
        let ft = meta.file_type();
        if ft.is_file() {
            bytes = bytes.saturating_add(meta.len());
            files += 1;
        } else if ft.is_symlink() {
            files += 1;
        }
    }
    (bytes, files)
}

/// non-crypto but unguessable within a session. pid + monotonic counter
/// + wallclock, unique across threads in the same ms.
pub fn new_token() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("plan-{pid:x}-{now_ms:x}-{n:x}")
}

fn now_unix() -> u64 {
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
    use tempfile::TempDir;

    fn write_file(path: &Path, size: usize) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        let mut f = File::create(path).unwrap();
        f.write_all(&vec![0u8; size]).unwrap();
    }

    /// tempdir shaped like a home dir with a cache subtree for tests
    fn synth_home(dir: &TempDir) -> PathBuf {
        let home = dir.path().to_path_buf();
        write_file(&home.join(".cache/spotify/blob.bin"), 2048);
        write_file(&home.join(".cache/spotify/sub/another.bin"), 1024);
        write_file(&home.join(".cache/slack/session"), 4096);
        home
    }

    // ---------- basic stat + classify ----------

    #[test]
    fn plan_counts_dir_bytes_and_files() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_home(&tmp);
        let plan = build_plan(&home, vec![home.join(".cache/spotify")]);
        assert_eq!(plan.items.len(), 1);
        let it = &plan.items[0];
        assert!(!it.protected);
        assert_eq!(it.kind, ItemKind::Directory);
        assert_eq!(it.bytes, 2048 + 1024);
        assert_eq!(it.file_count, 2);
        assert_eq!(plan.total_bytes, 2048 + 1024);
        assert_eq!(plan.total_count, 1);
        assert_eq!(plan.protected_count, 0);
    }

    #[test]
    fn plan_marks_missing_as_protected() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_home(&tmp);
        let plan = build_plan(&home, vec![home.join(".cache/not-there")]);
        assert_eq!(plan.items.len(), 1);
        assert!(plan.items[0].protected);
        assert_eq!(plan.items[0].kind, ItemKind::Missing);
        assert_eq!(plan.total_bytes, 0);
        assert_eq!(plan.total_count, 0);
        assert_eq!(plan.protected_count, 1);
    }

    #[test]
    fn plan_marks_home_root_as_protected() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_home(&tmp);
        let plan = build_plan(&home, vec![home.clone()]);
        assert_eq!(plan.items.len(), 1);
        assert!(plan.items[0].protected);
        assert!(plan.items[0]
            .protected_reason
            .as_deref()
            .unwrap()
            .contains("home"));
    }

    #[test]
    fn plan_marks_root_as_protected() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_home(&tmp);
        let plan = build_plan(&home, vec![PathBuf::from("/")]);
        assert_eq!(plan.items.len(), 1);
        assert!(plan.items[0].protected);
    }

    // ---------- dedup + shadow ----------

    #[test]
    fn plan_dedupes_identical_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_home(&tmp);
        let p = home.join(".cache/spotify/blob.bin");
        let plan = build_plan(&home, vec![p.clone(), p.clone(), p.clone()]);
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.total_count, 1);
    }

    #[test]
    fn plan_drops_descendant_when_ancestor_is_submitted() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_home(&tmp);
        // parent + one descendant, only the parent survives
        let plan = build_plan(
            &home,
            vec![
                home.join(".cache/spotify"),
                home.join(".cache/spotify/blob.bin"),
                home.join(".cache/spotify/sub"),
            ],
        );
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].bytes, 2048 + 1024);
        assert_eq!(plan.total_count, 1);
    }

    #[test]
    fn plan_keeps_siblings_even_when_one_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_home(&tmp);
        // spotify/ and slack/ are siblings, both survive
        let plan = build_plan(
            &home,
            vec![home.join(".cache/spotify"), home.join(".cache/slack")],
        );
        assert_eq!(plan.items.len(), 2);
        let total_fs = 2048 + 1024 + 4096;
        assert_eq!(plan.total_bytes, total_fs);
    }

    // ---------- sort order ----------

    #[test]
    fn plan_sorts_non_protected_by_bytes_desc() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_home(&tmp);
        let plan = build_plan(
            &home,
            vec![home.join(".cache/slack"), home.join(".cache/spotify")],
        );
        assert_eq!(plan.items.len(), 2);
        // slack is 4096 > spotify 3072
        assert!(plan.items[0].path.ends_with("slack"));
        assert!(plan.items[1].path.ends_with("spotify"));
    }

    #[test]
    fn plan_pushes_protected_items_to_end() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_home(&tmp);
        let plan = build_plan(
            &home,
            vec![
                home.clone(), // home root, protected
                home.join(".cache/slack"),
            ],
        );
        assert_eq!(plan.items.len(), 2);
        assert!(!plan.items[0].protected);
        assert!(plan.items[1].protected);
    }

    // ---------- tokens ----------

    #[test]
    fn tokens_are_unique_across_previews() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_home(&tmp);
        let a = build_plan(&home, vec![home.join(".cache/slack")]);
        let b = build_plan(&home, vec![home.join(".cache/slack")]);
        assert_ne!(a.token, b.token);
    }

    // ---------- stale plan pruning ----------

    #[test]
    fn prune_stale_drops_old_plans() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_home(&tmp);
        let plan = build_plan(&home, vec![home.join(".cache/slack")]);
        let mut cache = HashMap::new();
        cache.insert(plan.token.clone(), plan.clone());
        // now = 10 min after the plan was created
        prune_stale(&mut cache, plan.created_at + PLAN_TTL_SECS + 60);
        assert!(cache.is_empty());
    }

    #[test]
    fn prune_stale_keeps_fresh_plans() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_home(&tmp);
        let plan = build_plan(&home, vec![home.join(".cache/slack")]);
        let mut cache = HashMap::new();
        cache.insert(plan.token.clone(), plan.clone());
        prune_stale(&mut cache, plan.created_at + 10);
        assert_eq!(cache.len(), 1);
    }

    // ---------- symlinks ----------

    #[test]
    #[cfg(unix)]
    fn symlinks_are_counted_as_the_link_itself() {
        use std::os::unix::fs as unix_fs;
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_home(&tmp);
        // dangling symlink inside home
        let link = home.join(".cache/dangling");
        unix_fs::symlink("/nowhere/real", &link).unwrap();
        let plan = build_plan(&home, vec![link]);
        assert_eq!(plan.items.len(), 1);
        assert!(!plan.items[0].protected);
        assert_eq!(plan.items[0].kind, ItemKind::Symlink);
        assert_eq!(plan.items[0].bytes, 0);
        assert_eq!(plan.items[0].file_count, 1);
    }

    #[test]
    #[cfg(unix)]
    fn directory_subtree_size_excludes_symlink_target() {
        use std::os::unix::fs as unix_fs;
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_home(&tmp);
        // huge file outside home, symlink to it from inside cache.
        // subtree bytes must NOT include the target
        let outside = tmp.path().parent().unwrap().join("safai-plan-outside.bin");
        let _ = fs::remove_file(&outside);
        write_file(&outside, 8 * 1024 * 1024);
        let link = home.join(".cache/spotify/to-outside");
        unix_fs::symlink(&outside, &link).unwrap();

        let plan = build_plan(&home, vec![home.join(".cache/spotify")]);
        let _ = fs::remove_file(&outside);

        let it = &plan.items[0];
        // real files: blob.bin (2048) + sub/another.bin (1024) + symlink
        // (1 file, 0 bytes). bytes=3072, files=3
        assert_eq!(it.bytes, 3072);
        assert_eq!(it.file_count, 3);
    }

    // ---------- primary-folder block ----------

    #[test]
    fn documents_directory_is_protected_even_when_inside_tempdir_home() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        fs::create_dir_all(home.join("Documents")).unwrap();
        let plan = build_plan(&home, vec![home.join("Documents")]);
        assert_eq!(plan.items.len(), 1);
        assert!(plan.items[0].protected);
    }

    // ---------- tokens survive JSON round-trip ----------

    #[test]
    fn plan_survives_json_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_home(&tmp);
        let plan = build_plan(&home, vec![home.join(".cache/slack")]);
        let s = serde_json::to_string(&plan).unwrap();
        let back: DeletePlan = serde_json::from_str(&s).unwrap();
        assert_eq!(back.token, plan.token);
        assert_eq!(back.total_bytes, plan.total_bytes);
        assert_eq!(back.items.len(), plan.items.len());
    }
}
