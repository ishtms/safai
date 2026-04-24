//! privacy scanner.
//!
//! walks the catalog from [`super::catalog::catalog_for`] against a real fs and
//! rolls up per-category byte totals. deletion runs through the cleaner,
//! we just emit absolute paths for the frontend to hand back.
//!
//! # profile discovery
//!
//! chrome/edge/firefox keep per-profile data in sibling subdirs of the browser
//! root. `$HOME/.mozilla/firefox` usually has `abc.default-release` +
//! `xyz.dev-edition` + bookkeeping (`profiles.ini`, `installs.ini`, `Crash Reports`).
//! we walk every direct child, classify via [`is_profile_dir`], keep the matches.
//! handles firefox's random-prefix names + chrome multi-profile setups.
//!
//! # concurrency
//!
//! browsers scan concurrently via `std::thread::scope`, same pattern as //! per-browser category work is sequential, but each `sum_target` uses jwalk
//! which pulls from rayon's global pool. so 4 browsers x 5 categories = 20
//! sequential sums interleaved across threads, each fanned through rayon.
//!
//! # errors
//!
//! missing paths = silent zero. catalog names lots of optional files (firefox
//! doesn't ship `OfflineCache` on every version, chrome's `Network/` only
//! post-M96). permission errors during a walk skip per-entry. UI only needs
//! "installed?" so any path existing inside a browser tree flips
//! [`BrowserReport::available`] to true.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::catalog::{
    catalog_for, platform_tag, BrowserCatalogEntry, BrowserId, Os, PrivacyCategoryId,
    PrivacyCategorySpec, ProfileMode,
};

/// hard cap on detail rows per category. chrome averages ~10, even heavy
/// firefox with nested `storage/` is well under 100. 200 is safe headroom.
pub const MAX_TARGETS_PER_CATEGORY: usize = 200;

/// one absolute path the cleaner can act on. `path` is lossy UTF-8,
/// non-UTF-8 bytes become U+FFFD (UI is read-only, fine).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyTarget {
    pub path: String,
    pub bytes: u64,
    pub file_count: u64,
    /// unix seconds of newest descendant mtime. None if missing/unreadable.
    pub last_modified: Option<u64>,
    /// profile this target belongs to, for UI grouping. empty for ProfileMode::None.
    pub profile: String,
}

/// per-category roll-up. targets sorted desc by bytes, truncated to
/// [`MAX_TARGETS_PER_CATEGORY`]. bytes/items totals always count every resolved
/// target, even truncated ones.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyCategoryReport {
    pub id: PrivacyCategoryId,
    pub label: String,
    pub description: String,
    pub icon: String,
    pub bytes: u64,
    pub items: u64,
    pub targets: Vec<PrivacyTarget>,
}

/// per-browser roll-up. categories emit in catalog order for stable UI layout.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserReport {
    pub id: BrowserId,
    pub label: String,
    pub icon: String,
    /// root dir the catalog found this browser under. UI subtitle + debug hint.
    pub root: String,
    /// true if any catalog-resolved path exists on disk. unavailable browsers
    /// still show up zeroed so UI layout doesn't reshuffle between machines.
    pub available: bool,
    /// discovered profile dir names, enum order. empty for ProfileMode::None.
    pub profiles: Vec<String>,
    pub bytes: u64,
    pub items: u64,
    pub categories: Vec<PrivacyCategoryReport>,
}

/// top-level response from `privacy_scan`
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyReport {
    pub total_bytes: u64,
    pub total_items: u64,
    pub browsers: Vec<BrowserReport>,
    pub scanned_at: u64,
    /// "mac" | "linux" | "windows"
    pub platform: String,
    pub duration_ms: u64,
}

/// hermetic, no env consulted.
pub fn scan_privacy(home: &Path, os: Os) -> PrivacyReport {
    let started = std::time::Instant::now();
    let catalog = catalog_for(os, home);

    let reports: Vec<BrowserReport> = std::thread::scope(|s| {
        let handles: Vec<_> = catalog
            .iter()
            .map(|spec| s.spawn(|| scan_browser(home, spec)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| panic_placeholder()))
            .collect()
    });

    let total_bytes = reports.iter().map(|r| r.bytes).sum();
    let total_items = reports.iter().map(|r| r.items).sum();

    PrivacyReport {
        total_bytes,
        total_items,
        browsers: reports,
        scanned_at: now_unix(),
        platform: platform_tag(os).to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
    }
}

/// always returns a populated report even for uninstalled browsers (zeroed,
/// `available: false`). keeps UI row shape stable.
fn scan_browser(home: &Path, spec: &BrowserCatalogEntry) -> BrowserReport {
    let profiles = discover_profiles(spec);

    // "available" if:
    //   profile-sharded: any profile dir found OR any root exists
    //   single-profile:  any home-relative path exists
    let mut available = !profiles.is_empty() || spec.roots.iter().any(|r| r.exists());

    let mut categories: Vec<PrivacyCategoryReport> =
        Vec::with_capacity(spec.categories.len());
    let mut browser_bytes = 0u64;
    let mut browser_items = 0u64;
    let mut any_category_had_files = false;

    for cat in &spec.categories {
        let targets = resolve_targets(home, spec, cat, &profiles);
        let mut details: Vec<PrivacyTarget> = targets
            .into_iter()
            .filter_map(|(abs, profile)| stat_target(&abs, &profile))
            .collect();

        details.sort_by(|a, b| b.bytes.cmp(&a.bytes));
        let bytes: u64 = details.iter().map(|d| d.bytes).sum();
        let items: u64 = details.iter().map(|d| d.file_count).sum();
        if items > 0 {
            any_category_had_files = true;
        }
        details.truncate(MAX_TARGETS_PER_CATEGORY);

        browser_bytes = browser_bytes.saturating_add(bytes);
        browser_items = browser_items.saturating_add(items);

        categories.push(PrivacyCategoryReport {
            id: cat.id,
            label: cat.label.to_string(),
            description: cat.description.to_string(),
            icon: cat.icon.to_string(),
            bytes,
            items,
            targets: details,
        });
    }

    // Safari's ~/Library/Safari exists on every mac even if Safari never opened.
    // fall back to "had any files" so a pristine home doesn't falsely claim install.
    if spec.profile_mode == ProfileMode::None {
        available = any_category_had_files;
    }

    // UI subtitle: first root that exists, else first declared (empty-home fallback)
    let display_root = spec
        .roots
        .iter()
        .find(|r| r.exists())
        .map(|p| p.as_path())
        .unwrap_or_else(|| spec.primary_root())
        .to_string_lossy()
        .into_owned();

    BrowserReport {
        id: spec.id,
        label: spec.label.to_string(),
        icon: spec.icon.to_string(),
        root: display_root,
        available,
        profiles,
        bytes: browser_bytes,
        items: browser_items,
        categories,
    }
}

/// enumerate profile dirs under every declared root, union + dedup + sort.
/// stable output means UI checkbox state survives rescans.
///
/// unioning is load-bearing for firefox on XDG-split linux: same logical profile
/// `abc.default-release` lives in 3 dirs, scanner must see one profile not three.
///
/// skips non-dir entries, hidden files (.DS_Store, profiles.ini), and symlinks.
/// symlinks skipped on purpose, they could point anywhere and we don't want to
/// leak outside the browser tree.
fn discover_profiles(spec: &BrowserCatalogEntry) -> Vec<String> {
    if spec.profile_mode == ProfileMode::None {
        return Vec::new();
    }
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for root in &spec.roots {
        let Ok(read) = fs::read_dir(root) else {
            continue;
        };
        for entry in read.flatten() {
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            // real dirs only, no symlinks, no files
            if !ft.is_dir() || ft.is_symlink() {
                continue;
            }
            let name = match entry.file_name().into_string() {
                Ok(s) => s,
                Err(_) => continue,
            };
            if is_profile_dir(spec.profile_mode, &name) {
                seen.insert(name);
            }
        }
    }
    seen.into_iter().collect()
}

/// pure, exhaustively unit-testable without touching disk.
pub fn is_profile_dir(mode: ProfileMode, name: &str) -> bool {
    match mode {
        ProfileMode::None => false,
        ProfileMode::ChromeLike => {
            // chromium: Default, Profile 1, Profile 2, ...
            // System Profile = sign-in state, not user data. skip.
            // Guest Profile = ephemeral, skip.
            name == "Default"
                || (name.starts_with("Profile ")
                    && name[8..].chars().all(|c| c.is_ascii_digit()))
        }
        ProfileMode::FirefoxLike => {
            // firefox: <random>.<channel> like abc.default-release.
            // the `.` is the reliable marker. everything else under Profiles/ is
            // housekeeping (Crash Reports, Pending Pings, profiles.ini).
            let skip = [
                "Crash Reports",
                "Pending Pings",
                "Telemetry",
                "Install",
                "Installs",
            ];
            if skip.contains(&name) {
                return false;
            }
            // profile name matches <letters/digits>.<anything>
            match name.split_once('.') {
                Some((pre, _)) => !pre.is_empty() && pre.chars().all(is_profile_prefix_char),
                None => false,
            }
        }
    }
}

fn is_profile_prefix_char(c: char) -> bool {
    // upstream firefox uses [a-z0-9]{8}, we're permissive to cover Nightly (mixed case)
    c.is_ascii_alphanumeric()
}

/// absolute target paths this category resolves to, paired with profile name.
/// rel_to_profile: one target per discovered profile.
/// rel_to_home: one target each.
/// deduped by path to guard against unlikely overlap.
fn resolve_targets(
    home: &Path,
    spec: &BrowserCatalogEntry,
    category: &PrivacyCategorySpec,
    profiles: &[String],
) -> Vec<(PathBuf, String)> {
    let mut out: Vec<(PathBuf, String)> = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    // probe every root x profile x rel. single-root = just root.join(profile).join(rel).
    // XDG-split linux firefox: profile `abc.default-release` has cache2 under
    // ~/.cache/mozilla/firefox/<profile>/ and cookies.sqlite under
    // ~/.config/mozilla/firefox/<profile>/. probe both, stat_target filters missing.
    if !category.rel_to_profile.is_empty() {
        for profile in profiles {
            for root in &spec.roots {
                let profile_root = root.join(profile);
                for rel in &category.rel_to_profile {
                    // load-bearing: every resolved target must be strictly inside
                    // the profile dir. empty rel would match profile root itself.
                    if rel.is_empty() || rel.contains("..") {
                        continue;
                    }
                    let abs = profile_root.join(rel);
                    if !abs.starts_with(&profile_root) || abs == profile_root {
                        continue;
                    }
                    if seen.insert(abs.clone()) {
                        out.push((abs, profile.clone()));
                    }
                }
            }
        }
    }

    // home-relative
    for rel in &category.rel_to_home {
        if rel.is_empty() || rel.starts_with('/') || rel.contains("..") {
            continue;
        }
        let abs = home.join(rel);
        if !abs.starts_with(home) || abs == home {
            continue;
        }
        if seen.insert(abs.clone()) {
            out.push((abs, String::new()));
        }
    }

    out
}

/// None for missing. catalog is fuzzy on purpose, non-existent entries are normal.
fn stat_target(abs: &Path, profile: &str) -> Option<PrivacyTarget> {
    // symlink_metadata so symlinks are classified, not followed. a symlinked
    // Cache/ could point outside the browser tree.
    let meta = fs::symlink_metadata(abs).ok()?;
    if meta.file_type().is_symlink() {
        // skip entirely, don't want the symlink's own byte count and definitely
        // don't want to follow it
        return None;
    }
    if meta.is_file() {
        return Some(PrivacyTarget {
            path: abs.to_string_lossy().into_owned(),
            bytes: super::super::meta_ext::allocated_bytes(&meta),
            file_count: 1,
            last_modified: meta_mtime(&meta),
            profile: profile.to_string(),
        });
    }
    if meta.is_dir() {
        let (bytes, files, mtime) = sum_subtree(abs);
        if files == 0 && bytes == 0 {
            return None;
        }
        return Some(PrivacyTarget {
            path: abs.to_string_lossy().into_owned(),
            bytes,
            file_count: files,
            last_modified: mtime,
            profile: profile.to_string(),
        });
    }
    None
}

/// recursive sum via jwalk. no symlink follow, a profile's service-worker dir
/// routinely symlinks into the system WebKit cache on mac, which would over-
/// report bytes and leak out of the browser tree.
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
        bytes = bytes.saturating_add(super::super::meta_ext::allocated_bytes(&meta));
        files += 1;
        if let Some(mt) = meta_mtime(&meta) {
            newest = Some(newest.map_or(mt, |n| n.max(mt)));
        }
    }
    (bytes, files, newest)
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

/// used when a browser scan thread panics. keeps wire shape intact so UI sees
/// a zeroed entry instead of a missing one.
fn panic_placeholder() -> BrowserReport {
    BrowserReport {
        id: BrowserId::Chrome,
        label: String::new(),
        icon: String::new(),
        root: String::new(),
        available: false,
        profiles: Vec::new(),
        bytes: 0,
        items: 0,
        categories: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::catalog::catalog_for;
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use tempfile::TempDir;

    /// real bytes so jwalk sizes are accurate on every platform
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

    /// 2-profile chrome + 1 firefox profile + safari install
    fn synth_mac_home(dir: &TempDir) -> PathBuf {
        let home = dir.path().to_path_buf();
        let chrome =
            "Library/Application Support/Google/Chrome";
        let firefox = "Library/Application Support/Firefox/Profiles";
        make_tree(
            &home,
            &[
                // chrome Default
                (&format!("{chrome}/Default/Cache/data_0"), 1024 * 1024),
                (&format!("{chrome}/Default/Cache/data_1"), 512 * 1024),
                (&format!("{chrome}/Default/Code Cache/js/index"), 256 * 1024),
                (&format!("{chrome}/Default/Cookies"), 64 * 1024),
                (&format!("{chrome}/Default/History"), 320 * 1024),
                (&format!("{chrome}/Default/Local Storage/leveldb/000003.log"), 8 * 1024),
                // chrome Profile 1
                (&format!("{chrome}/Profile 1/Cache/data_0"), 128 * 1024),
                (&format!("{chrome}/Profile 1/Cookies"), 16 * 1024),
                // chrome System Profile, must be skipped
                (&format!("{chrome}/System Profile/Cache/data_0"), 999 * 1024),
                // firefox
                (&format!("{firefox}/abc.default-release/cache2/entries/data"), 4 * 1024 * 1024),
                (&format!("{firefox}/abc.default-release/cookies.sqlite"), 64 * 1024),
                (&format!("{firefox}/abc.default-release/places.sqlite"), 512 * 1024),
                // firefox housekeeping, must not count as profile
                (&format!("{firefox}/Crash Reports/events/abc"), 1024),
                (&format!("{firefox}/profiles.ini"), 512),
                // safari
                ("Library/Caches/com.apple.Safari/Cache.db", 2 * 1024 * 1024),
                ("Library/Cookies/Cookies.binarycookies", 128 * 1024),
                ("Library/Safari/History.db", 700 * 1024),
                // red herring, must not count
                ("Documents/resume.pdf", 16 * 1024),
            ],
        );
        home
    }

    #[test]
    fn empty_home_reports_all_browsers_unavailable() {
        // empty disk = every browser unavailable, UI layout stays stable
        let tmp = tempfile::tempdir().unwrap();
        let report = scan_privacy(tmp.path(), Os::Mac);
        let catalog = catalog_for(Os::Mac, tmp.path());
        assert_eq!(report.browsers.len(), catalog.len());
        for b in &report.browsers {
            assert!(!b.available, "{:?} should be unavailable", b.id);
            assert_eq!(b.bytes, 0);
            assert_eq!(b.items, 0);
            assert!(b.profiles.is_empty());
        }
        assert_eq!(report.total_bytes, 0);
        assert_eq!(report.total_items, 0);
        assert_eq!(report.platform, "mac");
    }

    #[test]
    fn chrome_profile_discovery_keeps_default_and_numeric_profiles() {
        // Default + Profile 1 count, System Profile (installer always creates it) does not
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_mac_home(&tmp);
        let report = scan_privacy(&home, Os::Mac);

        let chrome = report
            .browsers
            .iter()
            .find(|b| b.id == BrowserId::Chrome)
            .unwrap();
        assert!(chrome.available);
        // alpha sorted: Default, Profile 1
        assert_eq!(chrome.profiles, vec!["Default", "Profile 1"]);
        // System Profile + any non-profile child skipped
        assert!(!chrome.profiles.iter().any(|p| p == "System Profile"));
    }

    #[test]
    fn chrome_cache_bytes_sum_across_profiles() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_mac_home(&tmp);
        let report = scan_privacy(&home, Os::Mac);
        let chrome = report
            .browsers
            .iter()
            .find(|b| b.id == BrowserId::Chrome)
            .unwrap();
        let cache = chrome
            .categories
            .iter()
            .find(|c| c.id == PrivacyCategoryId::Cache)
            .unwrap();
        // Default: 1MiB + 512KiB + 256KiB, Profile 1: 128KiB
        let want = (1024 * 1024) + (512 * 1024) + (256 * 1024) + (128 * 1024);
        assert_eq!(cache.bytes, want as u64);
        // System Profile's 999KiB must not count
        assert!(
            !cache
                .targets
                .iter()
                .any(|t| t.path.contains("System Profile"))
        );
    }

    #[test]
    fn chrome_targets_carry_profile_name_for_ui_grouping() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_mac_home(&tmp);
        let report = scan_privacy(&home, Os::Mac);
        let chrome = report
            .browsers
            .iter()
            .find(|b| b.id == BrowserId::Chrome)
            .unwrap();
        let cookies = chrome
            .categories
            .iter()
            .find(|c| c.id == PrivacyCategoryId::Cookies)
            .unwrap();
        // 2 profiles x 1 Cookies entry = 2 targets
        let profiles: std::collections::HashSet<&str> =
            cookies.targets.iter().map(|t| t.profile.as_str()).collect();
        assert!(profiles.contains("Default"));
        assert!(profiles.contains("Profile 1"));
    }

    #[test]
    fn firefox_discovers_only_dot_named_profiles() {
        // abc.default-release = profile. `Crash Reports` + `profiles.ini` are not.
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_mac_home(&tmp);
        let report = scan_privacy(&home, Os::Mac);
        let firefox = report
            .browsers
            .iter()
            .find(|b| b.id == BrowserId::Firefox)
            .unwrap();
        assert_eq!(firefox.profiles, vec!["abc.default-release"]);
    }

    #[test]
    fn safari_uses_home_paths_without_profile_enumeration() {
        // no profiles, targets live at fixed ~/Library paths. available via
        // any-files-found in scan_browser.
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_mac_home(&tmp);
        let report = scan_privacy(&home, Os::Mac);
        let safari = report
            .browsers
            .iter()
            .find(|b| b.id == BrowserId::Safari)
            .unwrap();
        assert!(safari.available);
        assert!(safari.profiles.is_empty());

        let history = safari
            .categories
            .iter()
            .find(|c| c.id == PrivacyCategoryId::History)
            .unwrap();
        assert_eq!(history.bytes, 700 * 1024);
        assert_eq!(history.targets.len(), 1);
        assert!(history.targets[0].profile.is_empty());
    }

    #[test]
    fn empty_category_has_no_rows_not_a_placeholder() {
        // safari LocalStorage has no data in synth home. row should be zero/zero,
        // not a bogus placeholder.
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_mac_home(&tmp);
        let report = scan_privacy(&home, Os::Mac);
        let safari = report
            .browsers
            .iter()
            .find(|b| b.id == BrowserId::Safari)
            .unwrap();
        let local = safari
            .categories
            .iter()
            .find(|c| c.id == PrivacyCategoryId::LocalStorage)
            .unwrap();
        assert_eq!(local.bytes, 0);
        assert_eq!(local.items, 0);
        assert!(local.targets.is_empty());
    }

    #[test]
    fn total_bytes_equals_sum_of_browser_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_mac_home(&tmp);
        let report = scan_privacy(&home, Os::Mac);
        let sum: u64 = report.browsers.iter().map(|b| b.bytes).sum();
        assert_eq!(sum, report.total_bytes);
        let sum: u64 = report.browsers.iter().map(|b| b.items).sum();
        assert_eq!(sum, report.total_items);
    }

    #[test]
    fn browser_bytes_equals_sum_of_category_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_mac_home(&tmp);
        let report = scan_privacy(&home, Os::Mac);
        for b in &report.browsers {
            let cat_bytes: u64 = b.categories.iter().map(|c| c.bytes).sum();
            let cat_items: u64 = b.categories.iter().map(|c| c.items).sum();
            assert_eq!(cat_bytes, b.bytes, "{:?} byte mismatch", b.id);
            assert_eq!(cat_items, b.items, "{:?} item mismatch", b.id);
        }
    }

    #[test]
    fn documents_folder_is_never_touched() {
        // ~/Documents/resume.pdf red herring. privacy screen is browser-data only.
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_mac_home(&tmp);
        let report = scan_privacy(&home, Os::Mac);
        for b in &report.browsers {
            for c in &b.categories {
                for t in &c.targets {
                    assert!(
                        !t.path.contains("Documents/resume.pdf"),
                        "Documents leaked into {:?}: {}",
                        b.id,
                        t.path,
                    );
                }
            }
        }
    }

    #[test]
    fn targets_sorted_desc_by_bytes_within_category() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_mac_home(&tmp);
        let report = scan_privacy(&home, Os::Mac);
        for b in &report.browsers {
            for c in &b.categories {
                let sizes: Vec<u64> = c.targets.iter().map(|t| t.bytes).collect();
                let mut sorted = sizes.clone();
                sorted.sort_by(|a, b| b.cmp(a));
                assert_eq!(sizes, sorted, "{:?} {:?} not sorted desc", b.id, c.id);
            }
        }
    }

    #[test]
    fn scan_is_deterministic_across_runs() {
        // identical output across runs = UI checkbox state has stable identity
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_mac_home(&tmp);
        let a = scan_privacy(&home, Os::Mac);
        let b = scan_privacy(&home, Os::Mac);
        assert_eq!(a.total_bytes, b.total_bytes);
        for (x, y) in a.browsers.iter().zip(b.browsers.iter()) {
            assert_eq!(x.profiles, y.profiles);
            assert_eq!(x.bytes, y.bytes);
            for (cx, cy) in x.categories.iter().zip(y.categories.iter()) {
                let xp: Vec<&str> = cx.targets.iter().map(|t| t.path.as_str()).collect();
                let yp: Vec<&str> = cy.targets.iter().map(|t| t.path.as_str()).collect();
                assert_eq!(xp, yp);
            }
        }
    }

    #[test]
    fn serialization_is_camel_case() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_mac_home(&tmp);
        let report = scan_privacy(&home, Os::Mac);
        let v = serde_json::to_value(&report).unwrap();
        assert!(v.get("totalBytes").is_some());
        assert!(v.get("totalItems").is_some());
        assert!(v.get("scannedAt").is_some());
        assert!(v.get("durationMs").is_some());
        let b0 = &v["browsers"][0];
        assert!(b0.get("available").is_some());
        let c0 = &b0["categories"][0];
        assert!(c0.get("targets").is_some());
        let id = c0.get("id").and_then(|x| x.as_str()).unwrap();
        assert!(id.chars().all(|c| c.is_ascii_lowercase() || c == '-'));
    }

    #[test]
    fn symlink_profiles_are_skipped() {
        // symlinked "profile" could point anywhere. never enumerate or scan it.
        #[cfg(unix)]
        {
            use std::os::unix::fs as unix_fs;
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path().to_path_buf();
            let chrome = home.join("Library/Application Support/Google/Chrome");
            fs::create_dir_all(&chrome).unwrap();
            // real Default profile
            make_tree(
                &home,
                &[("Library/Application Support/Google/Chrome/Default/Cache/x", 1024)],
            );
            // symlinked 'Profile 1' pointing somewhere huge outside
            let outside = tmp.path().parent().unwrap().join("safai-privacy-evil");
            let _ = fs::remove_dir_all(&outside);
            fs::create_dir_all(&outside).unwrap();
            let huge = outside.join("Cache/data");
            fs::create_dir_all(huge.parent().unwrap()).unwrap();
            let mut f = File::create(&huge).unwrap();
            f.write_all(&vec![0u8; 10 * 1024 * 1024]).unwrap();
            drop(f);
            unix_fs::symlink(&outside, chrome.join("Profile 1")).unwrap();

            let report = scan_privacy(&home, Os::Mac);
            let chrome_r = report
                .browsers
                .iter()
                .find(|b| b.id == BrowserId::Chrome)
                .unwrap();
            // only Default enumerated
            assert_eq!(chrome_r.profiles, vec!["Default"]);
            // cache bytes from Default only, never the symlinked target
            let cache = chrome_r
                .categories
                .iter()
                .find(|c| c.id == PrivacyCategoryId::Cache)
                .unwrap();
            assert_eq!(cache.bytes, 1024);
            let _ = fs::remove_dir_all(&outside);
        }
    }

    #[test]
    fn no_target_is_ever_a_profile_root() {
        // cleaner would refuse these anyway, but double-guard: every resolved
        // target is a strict descendant or a home-rooted leaf, never the profile root.
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_mac_home(&tmp);
        let report = scan_privacy(&home, Os::Mac);
        let mac = catalog_for(Os::Mac, &home);
        let chrome_spec = mac.iter().find(|b| b.id == BrowserId::Chrome).unwrap();
        let chrome = report
            .browsers
            .iter()
            .find(|b| b.id == BrowserId::Chrome)
            .unwrap();
        for c in &chrome.categories {
            for t in &c.targets {
                for profile in &chrome.profiles {
                    for root in &chrome_spec.roots {
                        let profile_root = root.join(profile);
                        assert_ne!(
                            PathBuf::from(&t.path),
                            profile_root,
                            "{:?} target equals profile root",
                            c.id,
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn truncation_caps_targets_but_preserves_totals() {
        // firefox profile with more targets than cap. use `storage/`, it's the
        // LocalStorage target and we can nest lots of files.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        // one FirefoxLike profile
        let prof =
            "Library/Application Support/Firefox/Profiles/abc.default-release";
        let mut files: Vec<(String, usize)> = Vec::new();
        // Sessions has up to 5 rel_to_profile entries, naturally bounded.
        // can't exceed 200 without lots of categories, and sum_subtree folds
        // nested files into one target. so sanity check the non-exceeded path:
        // cap doesn't trim below expected count on normal data.
        for i in 0..5 {
            files.push((format!("{prof}/storage/default/https+443+a{i}/x"), 64));
        }
        let refs: Vec<(&str, usize)> = files.iter().map(|(s, n)| (s.as_str(), *n)).collect();
        make_tree(&home, &refs);
        let report = scan_privacy(&home, Os::Mac);
        let firefox = report
            .browsers
            .iter()
            .find(|b| b.id == BrowserId::Firefox)
            .unwrap();
        let local = firefox
            .categories
            .iter()
            .find(|c| c.id == PrivacyCategoryId::LocalStorage)
            .unwrap();
        // `storage` dir is one target, 5 descendants
        assert!(local.targets.len() <= MAX_TARGETS_PER_CATEGORY);
        assert!(local.items >= 5);
    }

    #[test]
    fn large_synthetic_tree_scans_quickly() {
        // ~2k files across 50 cache dirs in one profile
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let prof = "Library/Application Support/Google/Chrome/Default/Cache";
        let mut files: Vec<(String, usize)> = Vec::new();
        for i in 0..2_000 {
            files.push((format!("{prof}/shard{}/data_{}", i % 50, i), 1024));
        }
        let refs: Vec<(&str, usize)> = files.iter().map(|(s, n)| (s.as_str(), *n)).collect();
        make_tree(&home, &refs);
        let start = std::time::Instant::now();
        let report = scan_privacy(&home, Os::Mac);
        assert!(start.elapsed().as_millis() < 10_000);
        // byte count reflects every shard
        assert!(report.total_bytes >= (2_000 * 1024) as u64);
    }

    #[test]
    fn browser_order_matches_catalog_order() {
        let tmp = tempfile::tempdir().unwrap();
        let home = synth_mac_home(&tmp);
        let report = scan_privacy(&home, Os::Mac);
        let want: Vec<BrowserId> = catalog_for(Os::Mac, &home)
            .iter()
            .map(|b| b.id)
            .collect();
        let got: Vec<BrowserId> = report.browsers.iter().map(|b| b.id).collect();
        assert_eq!(got, want);
    }

    // ---- is_profile_dir pure tests ----

    #[test]
    fn is_profile_dir_chrome_accepts_default_and_numbered() {
        use ProfileMode::ChromeLike;
        assert!(is_profile_dir(ChromeLike, "Default"));
        assert!(is_profile_dir(ChromeLike, "Profile 1"));
        assert!(is_profile_dir(ChromeLike, "Profile 42"));
        assert!(!is_profile_dir(ChromeLike, "Profile Adrian"));
        assert!(!is_profile_dir(ChromeLike, "System Profile"));
        assert!(!is_profile_dir(ChromeLike, "Guest Profile"));
        assert!(!is_profile_dir(ChromeLike, "profiles.ini"));
        assert!(!is_profile_dir(ChromeLike, ".DS_Store"));
        assert!(!is_profile_dir(ChromeLike, ""));
    }

    #[test]
    fn is_profile_dir_firefox_accepts_dot_suffix_names() {
        use ProfileMode::FirefoxLike;
        assert!(is_profile_dir(FirefoxLike, "abc.default-release"));
        assert!(is_profile_dir(FirefoxLike, "8char123.dev-edition"));
        assert!(is_profile_dir(FirefoxLike, "MixedCase99.nightly"));
        assert!(!is_profile_dir(FirefoxLike, "Crash Reports"));
        assert!(!is_profile_dir(FirefoxLike, "Pending Pings"));
        // profiles.ini is a file, filtered by is_dir check before is_profile_dir
        // runs. covered e2e in firefox_discovers_only_dot_named_profiles.
        assert!(!is_profile_dir(FirefoxLike, "bare-name-no-dot"));
        assert!(!is_profile_dir(FirefoxLike, ".dotfile"));
        assert!(!is_profile_dir(FirefoxLike, ""));
    }

    #[test]
    fn is_profile_dir_none_mode_always_false() {
        assert!(!is_profile_dir(ProfileMode::None, "anything"));
    }

    // ---- resolve_targets pure tests ----

    #[test]
    fn resolve_targets_rejects_empty_and_escaping_rels() {
        // catalog test already guards empty rels, but the scanner must refuse
        // them defensively. lock it in with a hand-built category.
        let home = PathBuf::from("/fake/home");
        let spec = BrowserCatalogEntry {
            id: BrowserId::Chrome,
            label: "c",
            icon: "i",
            roots: vec![home.join("Chrome")],
            profile_mode: ProfileMode::ChromeLike,
            categories: vec![],
        };
        let cat = PrivacyCategorySpec {
            id: PrivacyCategoryId::Cache,
            label: "c",
            description: "d",
            icon: "i",
            rel_to_profile: vec!["", "../evil", "Cache"],
            rel_to_home: vec!["../outside", "/abs/path", "Library/OK"],
        };
        let out = resolve_targets(&home, &spec, &cat, &["Default".to_string()]);
        // only Cache + Library/OK survive
        let paths: Vec<String> = out
            .iter()
            .map(|(p, _)| p.to_string_lossy().into_owned())
            .collect();
        assert!(paths.iter().any(|p| p.ends_with("Chrome/Default/Cache")));
        assert!(paths.iter().any(|p| p.ends_with("Library/OK")));
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn firefox_xdg_split_layout_unions_profile_across_three_roots() {
        // regression guard: Arch user had firefox under ~/.config/mozilla/firefox/
        // with no ~/.mozilla/firefox/. scanner must (a) discover profile once not
        // 3x and (b) attribute cache2 bytes (.cache root) + cookies.sqlite bytes
        // (.config root) to the same profile.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let profile = "3cy8zo8x.default-release";

        // config root = cookies + places + sessions. no .mozilla/firefox.
        make_tree(
            &home,
            &[
                // config root -> cookies + places
                (&format!(".config/mozilla/firefox/{profile}/cookies.sqlite"), 64 * 1024),
                (&format!(".config/mozilla/firefox/{profile}/places.sqlite"), 512 * 1024),
                (
                    &format!(".config/mozilla/firefox/{profile}/sessionstore.jsonlz4"),
                    4 * 1024,
                ),
                // cache root -> cache2
                (&format!(".cache/mozilla/firefox/{profile}/cache2/entries/data"), 4 * 1024 * 1024),
                // housekeeping, must not be enumerated
                (".config/mozilla/firefox/profiles.ini", 256),
                (".cache/mozilla/firefox/Crash Reports/events/x", 128),
            ],
        );

        let report = scan_privacy(&home, Os::Linux);
        let firefox = report
            .browsers
            .iter()
            .find(|b| b.id == BrowserId::Firefox)
            .expect("firefox browser entry present");

        assert!(firefox.available, "firefox should be detected on XDG-split host");
        // profile seen in multiple roots, counted once
        assert_eq!(firefox.profiles, vec![profile.to_string()]);

        let cache = firefox
            .categories
            .iter()
            .find(|c| c.id == PrivacyCategoryId::Cache)
            .unwrap();
        let cookies = firefox
            .categories
            .iter()
            .find(|c| c.id == PrivacyCategoryId::Cookies)
            .unwrap();
        let history = firefox
            .categories
            .iter()
            .find(|c| c.id == PrivacyCategoryId::History)
            .unwrap();
        assert_eq!(cache.bytes, 4 * 1024 * 1024);
        assert_eq!(cookies.bytes, 64 * 1024);
        assert_eq!(history.bytes, 512 * 1024);

        // targets from different roots still land in same category.
        // cache target from .cache, cookies from .config.
        assert!(cache.targets[0].path.contains(".cache/mozilla/firefox"));
        assert!(cookies.targets[0].path.contains(".config/mozilla/firefox"));
    }

    #[test]
    fn firefox_snap_layout_detects_installation() {
        // Ubuntu 22+ default: firefox under ~/snap/firefox/common/..., nothing
        // at ~/.mozilla/firefox.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let profile = "xyz.default-release";
        make_tree(
            &home,
            &[
                (
                    &format!("snap/firefox/common/.mozilla/firefox/{profile}/cookies.sqlite"),
                    32 * 1024,
                ),
                (
                    &format!("snap/firefox/common/.cache/mozilla/firefox/{profile}/cache2/data"),
                    1024 * 1024,
                ),
            ],
        );
        let report = scan_privacy(&home, Os::Linux);
        let ff = report
            .browsers
            .iter()
            .find(|b| b.id == BrowserId::Firefox)
            .unwrap();
        assert!(ff.available, "snap Firefox should be detected");
        assert_eq!(ff.profiles, vec![profile.to_string()]);
        let cookies = ff
            .categories
            .iter()
            .find(|c| c.id == PrivacyCategoryId::Cookies)
            .unwrap();
        let cache = ff
            .categories
            .iter()
            .find(|c| c.id == PrivacyCategoryId::Cache)
            .unwrap();
        assert_eq!(cookies.bytes, 32 * 1024);
        assert_eq!(cache.bytes, 1024 * 1024);
    }

    #[test]
    fn firefox_flatpak_layout_detects_installation() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let profile = "abc.default";
        make_tree(
            &home,
            &[
                (
                    &format!(
                        ".var/app/org.mozilla.firefox/.mozilla/firefox/{profile}/cookies.sqlite"
                    ),
                    24 * 1024,
                ),
                (
                    &format!(
                        ".var/app/org.mozilla.firefox/.cache/mozilla/firefox/{profile}/cache2/data"
                    ),
                    2 * 1024 * 1024,
                ),
            ],
        );
        let report = scan_privacy(&home, Os::Linux);
        let ff = report
            .browsers
            .iter()
            .find(|b| b.id == BrowserId::Firefox)
            .unwrap();
        assert!(ff.available);
        assert_eq!(ff.profiles, vec![profile.to_string()]);
    }

    #[test]
    fn chromium_snap_layout_detects_installation() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        make_tree(
            &home,
            &[
                ("snap/chromium/common/chromium/Default/Cookies", 4 * 1024),
                ("snap/chromium/common/chromium/Default/Cache/data_0", 512 * 1024),
            ],
        );
        let report = scan_privacy(&home, Os::Linux);
        let chromium = report
            .browsers
            .iter()
            .find(|b| b.id == BrowserId::Chromium)
            .unwrap();
        assert!(chromium.available);
        assert_eq!(chromium.profiles, vec!["Default".to_string()]);
    }

    #[test]
    fn brave_flatpak_layout_detects_installation() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        make_tree(
            &home,
            &[
                (
                    ".var/app/com.brave.Browser/config/BraveSoftware/Brave-Browser/Default/Cookies",
                    2 * 1024,
                ),
                (
                    ".var/app/com.brave.Browser/config/BraveSoftware/Brave-Browser/Default/Cache/data_0",
                    256 * 1024,
                ),
            ],
        );
        let report = scan_privacy(&home, Os::Linux);
        let brave = report
            .browsers
            .iter()
            .find(|b| b.id == BrowserId::Brave)
            .unwrap();
        assert!(brave.available);
    }

    #[test]
    fn windows_firefox_appdata_split_works() {
        // windows firefox: profiles under APPDATA (roaming), cache under
        // LOCALAPPDATA. catalog probes both.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let profile = "abcd1234.default-release";
        make_tree(
            &home,
            &[
                (
                    &format!("AppData/Roaming/Mozilla/Firefox/Profiles/{profile}/cookies.sqlite"),
                    8 * 1024,
                ),
                (
                    &format!(
                        "AppData/Local/Mozilla/Firefox/Profiles/{profile}/cache2/entries/data"
                    ),
                    3 * 1024 * 1024,
                ),
            ],
        );
        let report = scan_privacy(&home, Os::Windows);
        let ff = report
            .browsers
            .iter()
            .find(|b| b.id == BrowserId::Firefox)
            .unwrap();
        assert!(ff.available);
        assert_eq!(ff.profiles, vec![profile.to_string()]);
        let cookies = ff
            .categories
            .iter()
            .find(|c| c.id == PrivacyCategoryId::Cookies)
            .unwrap();
        let cache = ff
            .categories
            .iter()
            .find(|c| c.id == PrivacyCategoryId::Cache)
            .unwrap();
        assert_eq!(cookies.bytes, 8 * 1024);
        assert_eq!(cache.bytes, 3 * 1024 * 1024);
    }

    #[test]
    fn mac_firefox_split_cache_root_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let profile = "abc.default-release";
        make_tree(
            &home,
            &[
                (
                    &format!(
                        "Library/Application Support/Firefox/Profiles/{profile}/places.sqlite"
                    ),
                    64 * 1024,
                ),
                (
                    &format!("Library/Caches/Firefox/Profiles/{profile}/cache2/data"),
                    2 * 1024 * 1024,
                ),
            ],
        );
        let report = scan_privacy(&home, Os::Mac);
        let ff = report
            .browsers
            .iter()
            .find(|b| b.id == BrowserId::Firefox)
            .unwrap();
        assert_eq!(ff.profiles, vec![profile.to_string()]);
        let cache = ff
            .categories
            .iter()
            .find(|c| c.id == PrivacyCategoryId::Cache)
            .unwrap();
        assert_eq!(cache.bytes, 2 * 1024 * 1024);
    }

    #[test]
    fn multiple_browsers_detected_simultaneously() {
        // Arch box: classic firefox + flatpak brave + distro chromium. all 3 available.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        make_tree(
            &home,
            &[
                (".mozilla/firefox/a.default/cookies.sqlite", 1024),
                (".config/chromium/Default/Cookies", 1024),
                (
                    ".var/app/com.brave.Browser/config/BraveSoftware/Brave-Browser/Default/Cookies",
                    1024,
                ),
            ],
        );
        let report = scan_privacy(&home, Os::Linux);
        let available: std::collections::HashSet<BrowserId> = report
            .browsers
            .iter()
            .filter(|b| b.available)
            .map(|b| b.id)
            .collect();
        assert!(available.contains(&BrowserId::Firefox));
        assert!(available.contains(&BrowserId::Chromium));
        assert!(available.contains(&BrowserId::Brave));
    }

    #[test]
    fn display_root_prefers_first_existing_root() {
        // subtitle shows root that actually contains data, not the first declared one
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        // firefox on linux has .mozilla/firefox declared first, but this user
        // only has .config/mozilla/firefox
        make_tree(
            &home,
            &[(".config/mozilla/firefox/abc.default/cookies.sqlite", 1024)],
        );
        let report = scan_privacy(&home, Os::Linux);
        let ff = report
            .browsers
            .iter()
            .find(|b| b.id == BrowserId::Firefox)
            .unwrap();
        assert!(ff.root.contains(".config/mozilla/firefox"), "got root={}", ff.root);
    }

    #[test]
    fn resolve_targets_dedupes_overlapping_home_entries() {
        // two rel_to_home resolving to same abs path = one target
        let home = PathBuf::from("/fake/home");
        let spec = BrowserCatalogEntry {
            id: BrowserId::Safari,
            label: "s",
            icon: "i",
            roots: vec![home.clone()],
            profile_mode: ProfileMode::None,
            categories: vec![],
        };
        let cat = PrivacyCategorySpec {
            id: PrivacyCategoryId::Cache,
            label: "c",
            description: "d",
            icon: "i",
            rel_to_profile: vec![],
            rel_to_home: vec![
                "Library/Caches/com.apple.Safari",
                "Library/Caches/com.apple.Safari",
            ],
        };
        let out = resolve_targets(&home, &spec, &cat, &[]);
        assert_eq!(out.len(), 1);
    }
}
