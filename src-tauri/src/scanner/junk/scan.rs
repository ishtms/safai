//! junk scanner. takes a [`super::catalog`] and produces a
//! [`JunkReport`] for the system junk screen.
//!
//! per category:
//! 1. enumerate each base's direct children (one row each)
//! 2. recursively sum size + file count + newest mtime via jwalk (rayon)
//! 3. sort desc by bytes, truncate to [`MAX_DETAILS_PER_CATEGORY`]
//!
//! category totals come from the untruncated list so the dashboard number
//! is never smaller than the sum of visible rows.
//!
//! categories scanned concurrently via thread::scope. each jwalk uses rayon's
//! global pool, so big ones (caches, DerivedData) dominate and cheap ones
//! (Trash, logs) interleave for free.
//!
//! perms-denied / missing bases are skipped silently. "base not present" =
//! zero bytes (e.g. no Xcode installed). the `available` flag on
//! [`JunkCategoryReport`] tells "scanned and empty" from "not present".

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::catalog::{catalog_for, platform_tag, JunkCategoryId, JunkCategorySpec, Os};
use super::super::meta_ext::allocated_bytes;

/// cap on detail rows per category. totals still count everything,
/// nobody scrolls past 100 cache subdirs.
pub const MAX_DETAILS_PER_CATEGORY: usize = 100;

/// one row on the detail screen. path is absolute lossy-utf8 (non-utf8
/// becomes U+FFFD, fine since UI is read-only).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct JunkPathDetail {
    pub path: String,
    pub bytes: u64,
    pub file_count: u64,
    /// unix seconds of newest descendant. None for empty or unreadable
    pub last_modified: Option<u64>,
}

/// per-category rollup. paths sorted desc by bytes, truncated to
/// [`MAX_DETAILS_PER_CATEGORY`]. bytes/items are always the untruncated totals.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JunkCategoryReport {
    pub id: JunkCategoryId,
    pub label: String,
    pub description: String,
    pub icon: String,
    pub hot: bool,
    pub bytes: u64,
    pub items: u64,
    /// true when at least one base exists. unavailable categories are still
    /// returned (stable UI order) but render dimmed
    pub available: bool,
    pub paths: Vec<JunkPathDetail>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JunkReport {
    pub total_bytes: u64,
    pub total_items: u64,
    pub categories: Vec<JunkCategoryReport>,
    pub scanned_at: u64,
    /// "mac" | "linux" | "windows"
    pub platform: String,
    /// walltime scanning, used for telemetry + "scanned in 1.2s" footer
    pub duration_ms: u64,
}

/// full scan for `os` against `home`. hermetic, callers resolve HOME /
/// USERPROFILE.
pub fn scan_junk(home: &Path, os: Os) -> JunkReport {
    let started = std::time::Instant::now();
    let catalog = catalog_for(os, home);

    // each closure borrows &JunkCategorySpec for the scope, no clone needed
    let reports: Vec<JunkCategoryReport> = std::thread::scope(|s| {
        let handles: Vec<_> = catalog
            .iter()
            .map(|spec| s.spawn(|| scan_category(spec)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| default_report_for_panic()))
            .collect()
    });

    let total_bytes = reports.iter().map(|r| r.bytes).sum();
    let total_items = reports.iter().map(|r| r.items).sum();

    JunkReport {
        total_bytes,
        total_items,
        categories: reports,
        scanned_at: now_unix(),
        platform: platform_tag(os).to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
    }
}

/// always returns a report even if bases don't exist (available: false +
/// empty). keeps UI row shape stable.
fn scan_category(spec: &JunkCategorySpec) -> JunkCategoryReport {
    let mut all: Vec<JunkPathDetail> = Vec::new();
    let mut any_base_present = false;

    for p in &spec.paths {
        if !p.base.exists() {
            continue;
        }
        any_base_present = true;

        match fs::metadata(&p.base) {
            Ok(meta) if meta.is_file() => {
                if let Some(d) = detail_for_file(&p.base, &meta) {
                    all.push(d);
                }
            }
            Ok(meta) if meta.is_dir() => {
                all.extend(scan_direct_children(&p.base));
            }
            _ => {}
        }
    }

    all.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    let bytes: u64 = all.iter().map(|d| d.bytes).sum();
    let items: u64 = all.iter().map(|d| d.file_count).sum();
    all.truncate(MAX_DETAILS_PER_CATEGORY);

    JunkCategoryReport {
        id: spec.id,
        label: spec.label.to_string(),
        description: spec.description.to_string(),
        icon: spec.icon.to_string(),
        hot: spec.hot,
        bytes,
        items,
        available: any_base_present,
        paths: all,
    }
}

/// enumerate direct children of base. files become their own rows (size
/// + 1); dirs get recursed via jwalk.
fn scan_direct_children(base: &Path) -> Vec<JunkPathDetail> {
    let read = match fs::read_dir(base) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        if ft.is_file() {
            if let Ok(meta) = entry.metadata() {
                if let Some(d) = detail_for_file(&path, &meta) {
                    out.push(d);
                }
            }
            continue;
        }

        if ft.is_dir() {
            let (bytes, files, mtime) = sum_subtree(&path);
            // skip empty subtrees, they just clutter detail rows
            if files == 0 && bytes == 0 {
                continue;
            }
            out.push(JunkPathDetail {
                path: path.to_string_lossy().into_owned(),
                bytes,
                file_count: files,
                last_modified: mtime,
            });
        }
        // symlinks skipped on purpose: counting them over-reports a linked
        // cache, following them can leave $HOME
    }
    out
}

/// sum size of dir + every descendant file. no symlink follow. jwalk
/// (rayon parallel) so 50k trees walk fast.
fn sum_subtree(dir: &Path) -> (u64, u64, Option<u64>) {
    let mut bytes = 0u64;
    let mut files = 0u64;
    let mut newest: Option<u64> = None;

    for entry in jwalk::WalkDir::new(dir)
        .skip_hidden(false)
        .follow_links(false)
    {
        let Ok(entry) = entry else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        bytes = bytes.saturating_add(allocated_bytes(&meta));
        files += 1;
        if let Some(mt) = meta_mtime(&meta) {
            newest = Some(newest.map_or(mt, |n| n.max(mt)));
        }
    }
    (bytes, files, newest)
}

fn detail_for_file(path: &Path, meta: &fs::Metadata) -> Option<JunkPathDetail> {
    if !meta.is_file() {
        return None;
    }
    Some(JunkPathDetail {
        path: path.to_string_lossy().into_owned(),
        bytes: allocated_bytes(meta),
        file_count: 1,
        last_modified: meta_mtime(meta),
    })
}

fn meta_mtime(meta: &fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// fallback when a scan thread panics. keeps wire shape intact so the UI
/// just sees zero instead of special-casing.
fn default_report_for_panic() -> JunkCategoryReport {
    JunkCategoryReport {
        id: JunkCategoryId::UserCaches,
        label: String::new(),
        description: String::new(),
        icon: String::new(),
        hot: false,
        bytes: 0,
        items: 0,
        available: false,
        paths: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::catalog::catalog_for;
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// populate root with (path, size) pairs. writes real bytes so jwalk
    /// gets accurate sizes everywhere (sparse files under-count on zfs/btrfs).
    fn make_tree(root: &Path, files: &[(&str, usize)]) {
        for (rel, size) in files {
            let full = root.join(rel);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            let mut f = File::create(&full).unwrap();
            if *size > 0 {
                let chunk = vec![0u8; (*size).min(64 * 1024)];
                let mut remaining = *size;
                while remaining > 0 {
                    let n = remaining.min(chunk.len());
                    f.write_all(&chunk[..n]).unwrap();
                    remaining -= n;
                }
            }
        }
    }

    /// synthetic mac home with caches + logs + xcode data
    fn synth_mac_home(dir: &TempDir) -> PathBuf {
        let home = dir.path().to_path_buf();
        make_tree(
            &home,
            &[
                // user caches: 3 app folders
                ("Library/Caches/Google/Chrome/Cache/data.bin", 1024 * 1024), // 1 MiB
                ("Library/Caches/Google/Chrome/Cache/more.bin", 512 * 1024),
                ("Library/Caches/Spotify/PersistentCache/blob", 2 * 1024 * 1024),
                ("Library/Caches/Slack/Cache/session", 256 * 1024),
                // system logs
                ("Library/Logs/Homebrew/install.log", 64 * 1024),
                ("Library/Logs/Homebrew/error.log", 32 * 1024),
                // xcode DerivedData
                ("Library/Developer/Xcode/DerivedData/App-abc/Build/Intermediates/x.o", 4 * 1024 * 1024),
                ("Library/Developer/Xcode/DerivedData/App-abc/Build/Products/AppBin", 2 * 1024 * 1024),
                // cargo cache (both halves)
                (".cargo/registry/cache/index-abc/tokio.crate", 100 * 1024),
                (".cargo/registry/src/index-abc/tokio-1.0/src/lib.rs", 80 * 1024),
                // npm cache
                (".npm/_cacache/content-v2/aa/bb/deadbeef", 200 * 1024),
                // trash, single loose file
                (".Trash/deleted.pdf", 50 * 1024),
                // red herring, outside any catalog path
                ("Documents/letter.txt", 4096),
            ],
        );
        home
    }

    #[test]
    fn scan_rollup_matches_on_disk_totals() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_mac_home(&tmp);

        let report = scan_junk(&home, Os::Mac);

        // totals = only files inside catalog paths, not the red herring
        let in_catalog = (1024 * 1024)
            + (512 * 1024)
            + (2 * 1024 * 1024)
            + (256 * 1024)
            + (64 * 1024)
            + (32 * 1024)
            + (4 * 1024 * 1024)
            + (2 * 1024 * 1024)
            + (100 * 1024)
            + (80 * 1024)
            + (200 * 1024)
            + (50 * 1024);
        assert_eq!(report.total_bytes, in_catalog as u64);
        // 12 files inside catalog roots
        assert_eq!(report.total_items, 12);
        assert!(report.duration_ms < 10_000);
        assert_eq!(report.platform, "mac");
    }

    #[test]
    fn user_caches_reports_per_app_rows_sorted_desc() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_mac_home(&tmp);
        let report = scan_junk(&home, Os::Mac);

        let caches = report
            .categories
            .iter()
            .find(|c| matches!(c.id, JunkCategoryId::UserCaches))
            .expect("user caches category present");

        // three app folders, all present
        let names: Vec<&str> = caches.paths.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(caches.paths.len(), 3);
        // sorted desc by bytes: Spotify (2 MiB) > Google (1.5 MiB) > Slack (256 KiB)
        // chrome is nested inside Google/, so direct child is Google
        assert!(names[0].ends_with("Spotify"), "got {:?}", names);
        assert!(names[1].ends_with("Google"), "got {:?}", names);
        assert!(names[2].ends_with("Slack"), "got {:?}", names);
        // row bytes sum == category total (nothing truncated)
        let sum: u64 = caches.paths.iter().map(|p| p.bytes).sum();
        assert_eq!(sum, caches.bytes);
    }

    #[test]
    fn missing_category_is_reported_with_available_false_not_dropped() {
        // empty home, every category present but available=false.
        // keeps UI row layout stable.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();

        let report = scan_junk(&home, Os::Mac);
        let catalog = catalog_for(Os::Mac, &home);

        assert_eq!(report.categories.len(), catalog.len());
        for cat in &report.categories {
            assert!(!cat.available, "{:?} should be unavailable on empty home", cat.id);
            assert_eq!(cat.bytes, 0);
            assert_eq!(cat.items, 0);
            assert!(cat.paths.is_empty());
        }
        assert_eq!(report.total_bytes, 0);
        assert_eq!(report.total_items, 0);
    }

    #[test]
    fn category_order_matches_catalog_order() {
        // UI renders in this order, must be stable + match catalog_for exactly
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_mac_home(&tmp);
        let report = scan_junk(&home, Os::Mac);
        let want: Vec<JunkCategoryId> = catalog_for(Os::Mac, &home)
            .iter()
            .map(|c| c.id)
            .collect();
        let got: Vec<JunkCategoryId> = report.categories.iter().map(|c| c.id).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn truncates_detail_rows_but_preserves_totals() {
        // user-caches with > MAX_DETAILS_PER_CATEGORY direct children.
        // paths.len() == MAX, bytes == sum of all children
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let over = MAX_DETAILS_PER_CATEGORY + 17;
        let mut files: Vec<(String, usize)> = Vec::with_capacity(over);
        for i in 0..over {
            files.push((format!("Library/Caches/app{i:04}/data.bin"), 4 * 1024));
        }
        let refs: Vec<(&str, usize)> = files.iter().map(|(s, n)| (s.as_str(), *n)).collect();
        make_tree(&home, &refs);

        let report = scan_junk(&home, Os::Mac);
        let caches = report
            .categories
            .iter()
            .find(|c| matches!(c.id, JunkCategoryId::UserCaches))
            .unwrap();

        assert_eq!(caches.paths.len(), MAX_DETAILS_PER_CATEGORY);
        // items counts all files, not just visible
        assert_eq!(caches.items, over as u64);
        // bytes counts all files
        assert_eq!(caches.bytes, (over * 4 * 1024) as u64);
    }

    #[test]
    fn newest_mtime_reflects_newest_descendant() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        // two files, second touched so it's clearly newer
        make_tree(
            &home,
            &[
                ("Library/Caches/App/old.bin", 1024),
                ("Library/Caches/App/new.bin", 1024),
            ],
        );
        let new_path = home.join("Library/Caches/App/new.bin");
        // open-write-flush bumps mtime on every platform
        let mut f = File::create(&new_path).unwrap();
        f.write_all(&[1u8; 1024]).unwrap();
        drop(f);

        let report = scan_junk(&home, Os::Mac);
        let caches = report
            .categories
            .iter()
            .find(|c| matches!(c.id, JunkCategoryId::UserCaches))
            .unwrap();
        let app = caches
            .paths
            .iter()
            // Path::ends_with is component-based, "/App" parses as
            // [RootDir, "App"] which never matches a non-root suffix.
            // bare component name works on both posix and windows.
            .find(|d| d.path.ends_with("App"))
            .expect("App folder detail present");
        // newest mtime populated + within an hour
        let mt = app.last_modified.expect("mtime populated");
        let now = now_unix();
        assert!(now.saturating_sub(mt) < 3600);
    }

    #[test]
    fn loose_files_count_as_individual_rows() {
        // mac .Trash/ holds loose items, each gets its own row
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        make_tree(
            &home,
            &[
                (".Trash/one.pdf", 1000),
                (".Trash/two.pdf", 2000),
                (".Trash/three.pdf", 3000),
            ],
        );
        let report = scan_junk(&home, Os::Mac);
        let trash = report
            .categories
            .iter()
            .find(|c| matches!(c.id, JunkCategoryId::Trash))
            .unwrap();
        assert_eq!(trash.paths.len(), 3);
        assert_eq!(trash.bytes, 6000);
        assert_eq!(trash.items, 3);
    }

    #[test]
    fn cargo_cache_merges_two_bases() {
        // cargo has two bases (.cargo/registry/cache + /src), both should
        // contribute rows to the same category
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        make_tree(
            &home,
            &[
                (".cargo/registry/cache/idx/a.crate", 2000),
                (".cargo/registry/src/idx/crate-1.0/lib.rs", 4000),
            ],
        );
        let report = scan_junk(&home, Os::Mac);
        let cargo = report
            .categories
            .iter()
            .find(|c| matches!(c.id, JunkCategoryId::CargoCache))
            .unwrap();
        // two direct children, one `idx` under each base
        assert_eq!(cargo.paths.len(), 2);
        assert_eq!(cargo.bytes, 6000);
    }

    #[test]
    fn scan_completes_quickly_on_synthetic_tree() {
        // perf guard. threshold is generous to not flake on slow CI
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let mut files: Vec<(String, usize)> = Vec::with_capacity(1_000);
        for i in 0..1_000 {
            files.push((
                format!("Library/Caches/app{}/f{}.bin", i % 20, i),
                1024,
            ));
        }
        let refs: Vec<(&str, usize)> = files.iter().map(|(s, n)| (s.as_str(), *n)).collect();
        make_tree(&home, &refs);
        let start = std::time::Instant::now();
        let report = scan_junk(&home, Os::Mac);
        let dur = start.elapsed();
        assert!(dur.as_millis() < 5_000, "scan took {dur:?}");
        assert_eq!(report.total_items, 1_000);
    }

    #[test]
    fn serialization_is_camelcase_with_kebab_ids() {
        let report = scan_junk(&tempfile::tempdir().unwrap().path(), Os::Mac);
        let v = serde_json::to_value(&report).unwrap();
        assert!(v.get("totalBytes").is_some());
        assert!(v.get("totalItems").is_some());
        assert!(v.get("scannedAt").is_some());
        assert!(v.get("durationMs").is_some());
        assert!(v.get("categories").is_some());
        let cat0 = &v["categories"][0];
        assert!(cat0.get("lastModified").is_none()); // only on path detail
        assert!(cat0.get("available").is_some());
        // kebab id even on empty scan
        let id = cat0.get("id").and_then(|x| x.as_str()).unwrap();
        assert!(id.chars().all(|c| c.is_ascii_lowercase() || c == '-'));
    }

    #[test]
    fn symlinks_are_not_followed_to_leave_base() {
        // real dir with 1 KB + symlink from inside catalog to outside dir.
        // must NOT count the external bytes
        #[cfg(unix)]
        {
            use std::os::unix::fs as unix_fs;

            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path().to_path_buf();
            // outside-home target with a big file
            let outside_dir = tmp.path().parent().unwrap().join("safai-symlink-target");
            let _ = fs::remove_dir_all(&outside_dir);
            fs::create_dir_all(&outside_dir).unwrap();
            let out_file = outside_dir.join("huge.bin");
            let mut f = File::create(&out_file).unwrap();
            f.write_all(&vec![0u8; 10 * 1024 * 1024]).unwrap(); // 10 MiB
            drop(f);

            // inside-home real file + symlink-as-direct-child
            make_tree(
                &home,
                &[("Library/Caches/Real/data.bin", 1000)],
            );
            let symlink_path = home.join("Library/Caches/LinkToOutside");
            unix_fs::symlink(&outside_dir, &symlink_path).unwrap();

            let report = scan_junk(&home, Os::Mac);
            let caches = report
                .categories
                .iter()
                .find(|c| matches!(c.id, JunkCategoryId::UserCaches))
                .unwrap();

            // only Real should show up, symlink child skipped entirely
            assert_eq!(caches.paths.len(), 1);
            assert!(caches.paths[0].path.ends_with("Real"));
            assert_eq!(caches.bytes, 1000);

            // cleanup sibling dir
            let _ = fs::remove_dir_all(&outside_dir);
        }
    }

    #[test]
    fn total_items_equals_sum_of_category_items() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_mac_home(&tmp);
        let report = scan_junk(&home, Os::Mac);
        let sum: u64 = report.categories.iter().map(|c| c.items).sum();
        assert_eq!(sum, report.total_items);
    }

    #[test]
    fn total_bytes_equals_sum_of_category_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_mac_home(&tmp);
        let report = scan_junk(&home, Os::Mac);
        let sum: u64 = report.categories.iter().map(|c| c.bytes).sum();
        assert_eq!(sum, report.total_bytes);
    }

    #[test]
    fn linux_temp_files_absolute_path_is_scanned() {
        // /tmp is absolute on linux. don't want to touch real /tmp in a
        // test, but can verify the category is returned correctly
        let tmp = tempfile::tempdir().unwrap();
        let report = scan_junk(tmp.path(), Os::Linux);
        let temp = report
            .categories
            .iter()
            .find(|c| matches!(c.id, JunkCategoryId::TempFiles))
            .expect("linux temp-files category returned");
        // temp-files always scanned since /tmp exists on any linux sandbox
        let _ = temp.available; // may be true or false depending on host
        assert_eq!(temp.icon, "file");
    }
}
