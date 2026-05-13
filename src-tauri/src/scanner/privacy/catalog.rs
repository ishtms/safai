//! browser-data catalog.
//!
//! where each browser stores per-category private data, per-OS. zero IO,
//! entirely table-driven so every platform can be tested from one host.
//!
//! two shapes of browser:
//!
//! - profile-sharded (Chrome/Edge/Firefox): data lives under per-profile subdirs
//!   of one root. scanner enumerates at walk time, catalog just names the root
//!   + discovery strategy.
//! - single-profile (Safari): data at fixed home-rooted paths. modelled as
//!   [`ProfileMode::None`] with [`PrivacyCategorySpec::rel_to_home`] set instead
//!   of [`PrivacyCategorySpec::rel_to_profile`].
//!
//! every category is a whitelist of named files/subdirs inside a profile, never
//! the profile itself. accidentally handing `~/Library/.../Firefox/Profiles/abc.default`
//! to the cleaner would nuke bookmarks + passwords. invariant validated in
//! [`super::scan::resolve_targets`].

use std::path::{Path, PathBuf};

use serde::Serialize;

pub use super::super::junk::catalog::current_os;
pub use super::super::junk::catalog::Os;

/// kebab-case browser id. UI keys icons + copy on this. additive only,
/// removing a variant breaks persisted selection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BrowserId {
    Chrome,
    Chromium,
    Edge,
    Brave,
    Vivaldi,
    Firefox,
    Safari,
}

impl BrowserId {
    /// must match the serde value
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chrome => "chrome",
            Self::Chromium => "chromium",
            Self::Edge => "edge",
            Self::Brave => "brave",
            Self::Vivaldi => "vivaldi",
            Self::Firefox => "firefox",
            Self::Safari => "safari",
        }
    }
}

/// UI groups + colour-codes by these. maps onto real browser artefacts, no
/// invented categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrivacyCategoryId {
    /// HTTP + code + GPU shader caches. always regenerated.
    Cache,
    /// cookies store. clears most sign-ins.
    Cookies,
    /// visited pages, favicons, visited-links bloom filter
    History,
    /// tab restore snapshots, kills "reopen closed tabs"
    Sessions,
    /// localStorage, IndexedDB, sw caches. way more invasive than cookies.
    LocalStorage,
}

impl PrivacyCategoryId {
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cache => "cache",
            Self::Cookies => "cookies",
            Self::History => "history",
            Self::Sessions => "sessions",
            Self::LocalStorage => "local-storage",
        }
    }
}

/// how profiles are arranged on disk.
///
/// - [`ProfileMode::None`]: no enumeration. Safari-style, all home-rooted paths.
/// - [`ProfileMode::ChromeLike`]: under `root`, profiles named `Default`,
///   `Profile 1`, `Profile 2`, etc. `System Profile` exists but we skip it,
///   not a user profile.
/// - [`ProfileMode::FirefoxLike`]: every direct subdir of `root` is a profile.
///   firefox uses random-prefix names like `abc.default-release`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileMode {
    None,
    ChromeLike,
    FirefoxLike,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivacyCategorySpec {
    pub id: PrivacyCategoryId,
    pub label: &'static str,
    pub description: &'static str,
    /// must match a variant of the TS `IconName` union
    pub icon: &'static str,
    /// leaf paths inside each discovered profile. must resolve to a concrete
    /// file/subdir, never the profile itself. empty string is rejected at scan time.
    pub rel_to_profile: Vec<&'static str>,
    /// home-rel paths. used for single-profile browsers (Safari) and for OS-managed
    /// caches outside the profile tree.
    pub rel_to_home: Vec<&'static str>,
}

/// per-browser entry. roots pre-joined with `home` by [`catalog_for`] so the
/// scanner never re-derives platform paths.
///
/// `roots` is a list because some browsers spread data across dirs. firefox on
/// XDG-compliant linux (Arch/Fedora with MOZ_ENABLE_XDG_DIRS=1) splits a single
/// logical profile across `~/.mozilla/firefox/<profile>` (config),
/// `~/.config/mozilla/firefox/<profile>` (bookmarks/cookies/places.sqlite), and
/// `~/.cache/mozilla/firefox/<profile>` (cache2 + startupCache). scanner unions
/// profile names across every root, then probes (root x profile x rel_to_profile).
///
/// browsers with one canonical root (Chrome everywhere, Firefox on mac/win, Safari)
/// get a single-entry vec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserCatalogEntry {
    pub id: BrowserId,
    pub label: &'static str,
    pub icon: &'static str,
    /// dirs whose children are enumerated as profiles (when profile_mode != None).
    /// still set for ProfileMode::None so UI can show the root as a subtitle,
    /// no walking happens.
    pub roots: Vec<PathBuf>,
    pub profile_mode: ProfileMode,
    pub categories: Vec<PrivacyCategorySpec>,
}

impl BrowserCatalogEntry {
    /// first root. UI subtitle + stable identity in tests. panics only on
    /// malformed catalog, guarded by the no-empty test.
    pub fn primary_root(&self) -> &Path {
        &self.roots[0]
    }
}

/// hermetic, deterministic. no env, no IO. safe from tests.
pub fn catalog_for(os: Os, home: &Path) -> Vec<BrowserCatalogEntry> {
    match os {
        Os::Mac => mac_catalog(home),
        Os::Linux => linux_catalog(home),
        Os::Windows => windows_catalog(home),
    }
}

fn mac_catalog(home: &Path) -> Vec<BrowserCatalogEntry> {
    vec![
        BrowserCatalogEntry {
            id: BrowserId::Chrome,
            label: "Google Chrome",
            icon: "shield",
            roots: vec![home.join("Library/Application Support/Google/Chrome")],
            profile_mode: ProfileMode::ChromeLike,
            categories: chrome_like_categories(),
        },
        BrowserCatalogEntry {
            id: BrowserId::Chromium,
            label: "Chromium",
            icon: "shield",
            roots: vec![home.join("Library/Application Support/Chromium")],
            profile_mode: ProfileMode::ChromeLike,
            categories: chrome_like_categories(),
        },
        BrowserCatalogEntry {
            id: BrowserId::Edge,
            label: "Microsoft Edge",
            icon: "shield",
            roots: vec![home.join("Library/Application Support/Microsoft Edge")],
            profile_mode: ProfileMode::ChromeLike,
            categories: chrome_like_categories(),
        },
        BrowserCatalogEntry {
            id: BrowserId::Brave,
            label: "Brave",
            icon: "shield",
            roots: vec![home.join("Library/Application Support/BraveSoftware/Brave-Browser")],
            profile_mode: ProfileMode::ChromeLike,
            categories: chrome_like_categories(),
        },
        BrowserCatalogEntry {
            id: BrowserId::Vivaldi,
            label: "Vivaldi",
            icon: "shield",
            roots: vec![home.join("Library/Application Support/Vivaldi")],
            profile_mode: ProfileMode::ChromeLike,
            categories: chrome_like_categories(),
        },
        BrowserCatalogEntry {
            id: BrowserId::Firefox,
            label: "Mozilla Firefox",
            icon: "shield",
            // some newer mac firefox builds split cache to ~/Library/Caches/Firefox/Profiles, probe both
            roots: vec![
                home.join("Library/Application Support/Firefox/Profiles"),
                home.join("Library/Caches/Firefox/Profiles"),
            ],
            profile_mode: ProfileMode::FirefoxLike,
            categories: firefox_categories(),
        },
        BrowserCatalogEntry {
            id: BrowserId::Safari,
            label: "Safari",
            icon: "shield",
            roots: vec![home.join("Library/Safari")],
            profile_mode: ProfileMode::None,
            categories: safari_categories(),
        },
    ]
}

fn linux_catalog(home: &Path) -> Vec<BrowserCatalogEntry> {
    // linux install variants:
    //   - classic distro pkg:  ~/.config/<app>  (+ ~/.cache/<app>)
    //   - snap (Ubuntu):       ~/snap/<app>/common/...
    //   - flatpak (Fedora):    ~/.var/app/<id>/...
    //   - XDG-split (Arch/Fedora with MOZ_ENABLE_XDG_DIRS=1) - firefox only
    //
    // we list every variant's root, missing roots cost one read_dir.
    // UI shows one row per browser, first-existing root as subtitle.
    vec![
        BrowserCatalogEntry {
            id: BrowserId::Chrome,
            label: "Google Chrome",
            icon: "shield",
            roots: vec![
                home.join(".config/google-chrome"),
                home.join(".var/app/com.google.Chrome/config/google-chrome"),
            ],
            profile_mode: ProfileMode::ChromeLike,
            categories: chrome_like_categories(),
        },
        BrowserCatalogEntry {
            id: BrowserId::Chromium,
            label: "Chromium",
            icon: "shield",
            roots: vec![
                home.join(".config/chromium"),
                home.join("snap/chromium/common/chromium"),
                home.join(".var/app/org.chromium.Chromium/config/chromium"),
            ],
            profile_mode: ProfileMode::ChromeLike,
            categories: chrome_like_categories(),
        },
        BrowserCatalogEntry {
            id: BrowserId::Edge,
            label: "Microsoft Edge",
            icon: "shield",
            roots: vec![
                home.join(".config/microsoft-edge"),
                home.join(".config/microsoft-edge-dev"),
                home.join(".config/microsoft-edge-beta"),
                home.join(".var/app/com.microsoft.Edge/config/microsoft-edge"),
            ],
            profile_mode: ProfileMode::ChromeLike,
            categories: chrome_like_categories(),
        },
        BrowserCatalogEntry {
            id: BrowserId::Brave,
            label: "Brave",
            icon: "shield",
            roots: vec![
                home.join(".config/BraveSoftware/Brave-Browser"),
                home.join(".var/app/com.brave.Browser/config/BraveSoftware/Brave-Browser"),
            ],
            profile_mode: ProfileMode::ChromeLike,
            categories: chrome_like_categories(),
        },
        BrowserCatalogEntry {
            id: BrowserId::Vivaldi,
            label: "Vivaldi",
            icon: "shield",
            roots: vec![
                home.join(".config/vivaldi"),
                home.join(".var/app/com.vivaldi.Vivaldi/config/vivaldi"),
            ],
            profile_mode: ProfileMode::ChromeLike,
            categories: chrome_like_categories(),
        },
        BrowserCatalogEntry {
            id: BrowserId::Firefox,
            label: "Mozilla Firefox",
            icon: "shield",
            // classic distro pkg: everything under ~/.mozilla/firefox.
            // XDG-split (Arch / Fedora w/ MOZ_ENABLE_XDG_DIRS=1):
            //   ~/.mozilla/firefox         -> profile metadata + extensions
            //   ~/.config/mozilla/firefox  -> cookies, places.sqlite, sessions, storage
            //   ~/.cache/mozilla/firefox   -> cache2, startupCache
            // snap (Ubuntu 22+): ~/snap/firefox/common/... mirrors the XDG split in sandbox.
            // flatpak: ~/.var/app/org.mozilla.firefox/... same deal.
            // Debian ESR: ~/.mozilla/firefox-esr (separate profile tree).
            roots: vec![
                home.join(".mozilla/firefox"),
                home.join(".config/mozilla/firefox"),
                home.join(".cache/mozilla/firefox"),
                home.join(".mozilla/firefox-esr"),
                home.join("snap/firefox/common/.mozilla/firefox"),
                home.join("snap/firefox/common/.cache/mozilla/firefox"),
                home.join(".var/app/org.mozilla.firefox/.mozilla/firefox"),
                home.join(".var/app/org.mozilla.firefox/.config/mozilla/firefox"),
                home.join(".var/app/org.mozilla.firefox/.cache/mozilla/firefox"),
            ],
            profile_mode: ProfileMode::FirefoxLike,
            categories: firefox_categories(),
        },
    ]
}

fn windows_catalog(home: &Path) -> Vec<BrowserCatalogEntry> {
    // windows browsers put config + cache in the same `User Data` tree under
    // LOCALAPPDATA. firefox is the exception: profile config under APPDATA (roaming),
    // cache under LOCALAPPDATA. probe both.
    vec![
        BrowserCatalogEntry {
            id: BrowserId::Chrome,
            label: "Google Chrome",
            icon: "shield",
            roots: vec![home.join("AppData/Local/Google/Chrome/User Data")],
            profile_mode: ProfileMode::ChromeLike,
            categories: chrome_like_categories(),
        },
        BrowserCatalogEntry {
            id: BrowserId::Chromium,
            label: "Chromium",
            icon: "shield",
            roots: vec![home.join("AppData/Local/Chromium/User Data")],
            profile_mode: ProfileMode::ChromeLike,
            categories: chrome_like_categories(),
        },
        BrowserCatalogEntry {
            id: BrowserId::Edge,
            label: "Microsoft Edge",
            icon: "shield",
            roots: vec![
                home.join("AppData/Local/Microsoft/Edge/User Data"),
                home.join("AppData/Local/Microsoft/Edge Dev/User Data"),
                home.join("AppData/Local/Microsoft/Edge Beta/User Data"),
            ],
            profile_mode: ProfileMode::ChromeLike,
            categories: chrome_like_categories(),
        },
        BrowserCatalogEntry {
            id: BrowserId::Brave,
            label: "Brave",
            icon: "shield",
            roots: vec![home.join("AppData/Local/BraveSoftware/Brave-Browser/User Data")],
            profile_mode: ProfileMode::ChromeLike,
            categories: chrome_like_categories(),
        },
        BrowserCatalogEntry {
            id: BrowserId::Vivaldi,
            label: "Vivaldi",
            icon: "shield",
            roots: vec![home.join("AppData/Local/Vivaldi/User Data")],
            profile_mode: ProfileMode::ChromeLike,
            categories: chrome_like_categories(),
        },
        BrowserCatalogEntry {
            id: BrowserId::Firefox,
            label: "Mozilla Firefox",
            icon: "shield",
            roots: vec![
                home.join("AppData/Roaming/Mozilla/Firefox/Profiles"),
                home.join("AppData/Local/Mozilla/Firefox/Profiles"),
            ],
            profile_mode: ProfileMode::FirefoxLike,
            categories: firefox_categories(),
        },
    ]
}

/// categories for every chromium fork. layout is stable across versions and forks.
fn chrome_like_categories() -> Vec<PrivacyCategorySpec> {
    vec![
        PrivacyCategorySpec {
            id: PrivacyCategoryId::Cache,
            label: "Cache",
            description: "HTTP, GPU, and code caches. Regenerated on next page load.",
            icon: "broom",
            rel_to_profile: vec![
                "Cache",
                "Code Cache",
                "GPUCache",
                "Media Cache",
                "Service Worker/CacheStorage",
                "Service Worker/ScriptCache",
            ],
            rel_to_home: vec![],
        },
        PrivacyCategorySpec {
            id: PrivacyCategoryId::Cookies,
            label: "Cookies",
            description: "Site cookies. Clears most sign-ins.",
            icon: "file",
            rel_to_profile: vec![
                "Cookies",
                "Cookies-journal",
                "Network/Cookies",
                "Network/Cookies-journal",
            ],
            rel_to_home: vec![],
        },
        PrivacyCategorySpec {
            id: PrivacyCategoryId::History,
            label: "Browsing history",
            description: "Visited pages, downloads list, favicons, top-sites cache.",
            icon: "archive",
            rel_to_profile: vec![
                "History",
                "History-journal",
                "History Provider Cache",
                "Visited Links",
                "Top Sites",
                "Top Sites-journal",
                "Favicons",
                "Favicons-journal",
            ],
            rel_to_home: vec![],
        },
        PrivacyCategorySpec {
            id: PrivacyCategoryId::Sessions,
            label: "Sessions",
            description: "Tab-restore snapshots. Clears \"reopen closed tabs\".",
            icon: "file",
            rel_to_profile: vec![
                "Sessions",
                "Session Storage",
                "Current Session",
                "Current Tabs",
                "Last Session",
                "Last Tabs",
            ],
            rel_to_home: vec![],
        },
        PrivacyCategorySpec {
            id: PrivacyCategoryId::LocalStorage,
            label: "Local storage",
            description: "localStorage, IndexedDB, service worker caches.",
            icon: "archive",
            rel_to_profile: vec![
                "Local Storage",
                "IndexedDB",
                "Service Worker/Database",
                "File System",
            ],
            rel_to_home: vec![],
        },
    ]
}

/// firefox categories. upstream profile layout: https://wiki.mozilla.org/Profile
fn firefox_categories() -> Vec<PrivacyCategorySpec> {
    vec![
        PrivacyCategorySpec {
            id: PrivacyCategoryId::Cache,
            label: "Cache",
            description: "HTTP cache + startup cache. Regenerated on launch.",
            icon: "broom",
            rel_to_profile: vec!["cache2", "startupCache", "OfflineCache", "jumpListCache"],
            rel_to_home: vec![],
        },
        PrivacyCategorySpec {
            id: PrivacyCategoryId::Cookies,
            label: "Cookies",
            description: "Site cookies.",
            icon: "file",
            rel_to_profile: vec!["cookies.sqlite", "cookies.sqlite-shm", "cookies.sqlite-wal"],
            rel_to_home: vec![],
        },
        PrivacyCategorySpec {
            id: PrivacyCategoryId::History,
            label: "Browsing history",
            description: "Places DB + favicons.",
            icon: "archive",
            rel_to_profile: vec![
                "places.sqlite",
                "places.sqlite-shm",
                "places.sqlite-wal",
                "favicons.sqlite",
                "favicons.sqlite-shm",
                "favicons.sqlite-wal",
            ],
            rel_to_home: vec![],
        },
        PrivacyCategorySpec {
            id: PrivacyCategoryId::Sessions,
            label: "Sessions",
            description: "Session-restore snapshots.",
            icon: "file",
            rel_to_profile: vec![
                "sessionstore.jsonlz4",
                "sessionstore.js",
                "sessionstore-backups",
                "recovery.jsonlz4",
                "recovery.baklz4",
            ],
            rel_to_home: vec![],
        },
        PrivacyCategorySpec {
            id: PrivacyCategoryId::LocalStorage,
            label: "Local storage",
            description: "IndexedDB + site storage + WebAppsStore.",
            icon: "archive",
            rel_to_profile: vec![
                "storage",
                "webappsstore.sqlite",
                "webappsstore.sqlite-shm",
                "webappsstore.sqlite-wal",
            ],
            rel_to_home: vec![],
        },
    ]
}

/// safari (mac only). single profile, home-rooted. data split across 3 top-level dirs:
///  - `~/Library/Safari`                     - history, sessions, downloads manifest
///  - `~/Library/Caches/com.apple.Safari`    - HTTP + webkit caches
///  - `~/Library/Cookies`                    - cookies binary plist
///
/// sandboxed safari (post-macOS 14) mirrors everything into
/// `~/Library/Containers/com.apple.Safari/Data/Library/...`, list both so we
/// catch whichever the running OS uses.
fn safari_categories() -> Vec<PrivacyCategorySpec> {
    vec![
        PrivacyCategorySpec {
            id: PrivacyCategoryId::Cache,
            label: "Cache",
            description: "HTTP, webkit, and CloudKit caches.",
            icon: "broom",
            rel_to_profile: vec![],
            rel_to_home: vec![
                "Library/Caches/com.apple.Safari",
                "Library/Caches/com.apple.WebKit.PluginProcess",
                "Library/Containers/com.apple.Safari/Data/Library/Caches/com.apple.Safari",
            ],
        },
        PrivacyCategorySpec {
            id: PrivacyCategoryId::Cookies,
            label: "Cookies",
            description: "Cookies binary-plist store.",
            icon: "file",
            rel_to_profile: vec![],
            rel_to_home: vec![
                "Library/Cookies/Cookies.binarycookies",
                "Library/Containers/com.apple.Safari/Data/Library/Cookies/Cookies.binarycookies",
            ],
        },
        PrivacyCategorySpec {
            id: PrivacyCategoryId::History,
            label: "Browsing history",
            description: "History + TopSites + downloads list.",
            icon: "archive",
            rel_to_profile: vec![],
            rel_to_home: vec![
                "Library/Safari/History.db",
                "Library/Safari/History.db-shm",
                "Library/Safari/History.db-wal",
                "Library/Safari/Downloads.plist",
                "Library/Safari/TopSites.plist",
                "Library/Containers/com.apple.Safari/Data/Library/Safari/History.db",
                "Library/Containers/com.apple.Safari/Data/Library/Safari/Downloads.plist",
            ],
        },
        PrivacyCategorySpec {
            id: PrivacyCategoryId::Sessions,
            label: "Sessions",
            description: "Last-session state.",
            icon: "file",
            rel_to_profile: vec![],
            rel_to_home: vec![
                "Library/Safari/LastSession.plist",
                "Library/Saved Application State/com.apple.Safari.savedState",
                "Library/Containers/com.apple.Safari/Data/Library/Safari/LastSession.plist",
            ],
        },
        PrivacyCategorySpec {
            id: PrivacyCategoryId::LocalStorage,
            label: "Local storage",
            description: "WebKit localStorage + IndexedDB.",
            icon: "archive",
            rel_to_profile: vec![],
            rel_to_home: vec![
                "Library/Safari/LocalStorage",
                "Library/Safari/Databases",
                "Library/WebKit/WebsiteData",
                "Library/Containers/com.apple.Safari/Data/Library/WebKit/WebsiteData",
            ],
        },
    ]
}

/// platform tag for the wire response. UI pivots copy off the same string as
/// [`super::super::junk::catalog::platform_tag`].
pub fn platform_tag(os: Os) -> &'static str {
    match os {
        Os::Mac => "mac",
        Os::Linux => "linux",
        Os::Windows => "windows",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn home() -> PathBuf {
        PathBuf::from("/fake/home/user")
    }

    #[test]
    fn mac_catalog_lists_browsers_in_stable_order() {
        // UI row order. Safari last (apple-last), Chrome first (most used), Firefox between.
        let cat = catalog_for(Os::Mac, &home());
        let got: Vec<BrowserId> = cat.iter().map(|b| b.id).collect();
        assert_eq!(
            got,
            vec![
                BrowserId::Chrome,
                BrowserId::Chromium,
                BrowserId::Edge,
                BrowserId::Brave,
                BrowserId::Vivaldi,
                BrowserId::Firefox,
                BrowserId::Safari,
            ],
        );
    }

    #[test]
    fn linux_catalog_has_no_safari() {
        let cat = catalog_for(Os::Linux, &home());
        assert!(cat.iter().all(|b| b.id != BrowserId::Safari));
        // chromium family + firefox
        let ids: HashSet<BrowserId> = cat.iter().map(|b| b.id).collect();
        for want in [
            BrowserId::Chrome,
            BrowserId::Chromium,
            BrowserId::Edge,
            BrowserId::Brave,
            BrowserId::Vivaldi,
            BrowserId::Firefox,
        ] {
            assert!(ids.contains(&want), "linux catalog missing {want:?}");
        }
    }

    #[test]
    fn windows_catalog_has_no_safari() {
        let cat = catalog_for(Os::Windows, &home());
        assert!(cat.iter().all(|b| b.id != BrowserId::Safari));
        let ids: HashSet<BrowserId> = cat.iter().map(|b| b.id).collect();
        for want in [
            BrowserId::Chrome,
            BrowserId::Chromium,
            BrowserId::Edge,
            BrowserId::Brave,
            BrowserId::Vivaldi,
            BrowserId::Firefox,
        ] {
            assert!(ids.contains(&want), "windows catalog missing {want:?}");
        }
    }

    #[test]
    fn every_browser_root_is_under_home() {
        for os in [Os::Mac, Os::Linux, Os::Windows] {
            for b in catalog_for(os, &home()) {
                assert!(!b.roots.is_empty(), "{os:?} {:?} has no roots", b.id);
                for r in &b.roots {
                    assert!(
                        r.starts_with(home()),
                        "{os:?} {:?} root {:?} leaks outside home",
                        b.id,
                        r,
                    );
                }
            }
        }
    }

    #[test]
    fn linux_firefox_has_xdg_split_roots() {
        // Arch/Fedora w/ MOZ_ENABLE_XDG_DIRS=1 splits firefox across 3 roots.
        // missing any one silently zeros a category. guard against that regression.
        let cat = catalog_for(Os::Linux, &home());
        let ff = cat.iter().find(|b| b.id == BrowserId::Firefox).unwrap();
        let roots: Vec<&std::path::Path> = ff.roots.iter().map(|p| p.as_path()).collect();
        assert!(roots.iter().any(|p| p.ends_with(".mozilla/firefox")));
        assert!(roots.iter().any(|p| p.ends_with(".config/mozilla/firefox")));
        assert!(roots.iter().any(|p| p.ends_with(".cache/mozilla/firefox")));
    }

    #[test]
    fn firefox_has_multiple_roots_on_every_platform() {
        // firefox splits data on every platform. guards the regression where
        // an Arch user had no ~/.mozilla/firefox but profile was under ~/.config/mozilla/firefox.
        for os in [Os::Mac, Os::Linux, Os::Windows] {
            let cat = catalog_for(os, &home());
            let ff = cat.iter().find(|b| b.id == BrowserId::Firefox).unwrap();
            assert!(
                ff.roots.len() >= 2,
                "{os:?} Firefox should probe multiple roots, got {}",
                ff.roots.len(),
            );
        }
    }

    #[test]
    fn linux_firefox_covers_snap_flatpak_and_esr() {
        // Ubuntu 22+ = snap, Fedora Silverblue = flatpak, Debian = firefox-esr.
        // missing any root silently zeros firefox on that distro.
        let cat = catalog_for(Os::Linux, &home());
        let ff = cat.iter().find(|b| b.id == BrowserId::Firefox).unwrap();
        let roots: Vec<String> = ff
            .roots
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert!(
            roots.iter().any(|p| p.contains("snap/firefox/")),
            "no snap root: {roots:?}"
        );
        assert!(
            roots
                .iter()
                .any(|p| p.contains(".var/app/org.mozilla.firefox")),
            "no flatpak root: {roots:?}",
        );
        assert!(
            roots.iter().any(|p| p.contains("firefox-esr")),
            "no Debian ESR root: {roots:?}",
        );
    }

    #[test]
    fn linux_chromium_family_covers_snap_and_flatpak() {
        let cat = catalog_for(Os::Linux, &home());
        let chromium = cat.iter().find(|b| b.id == BrowserId::Chromium).unwrap();
        let roots: Vec<String> = chromium
            .roots
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert!(roots.iter().any(|p| p.contains("snap/chromium")));
        assert!(roots
            .iter()
            .any(|p| p.contains(".var/app/org.chromium.Chromium")));

        let brave = cat.iter().find(|b| b.id == BrowserId::Brave).unwrap();
        assert!(
            brave
                .roots
                .iter()
                .any(|p| p.to_string_lossy().contains(".var/app/com.brave.Browser")),
            "brave missing flatpak root",
        );
    }

    #[test]
    fn windows_edge_covers_dev_and_beta_channels() {
        let cat = catalog_for(Os::Windows, &home());
        let edge = cat.iter().find(|b| b.id == BrowserId::Edge).unwrap();
        let roots: Vec<String> = edge
            .roots
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert!(roots.iter().any(|p| p.contains("Edge Dev/")));
        assert!(roots.iter().any(|p| p.contains("Edge Beta/")));
    }

    #[test]
    fn mac_safari_cookies_covers_sandboxed_container() {
        // sandboxed safari mirrors data to `~/Library/Containers/com.apple.Safari/Data/Library/...`
        let mac = catalog_for(Os::Mac, &home());
        let safari = mac.iter().find(|b| b.id == BrowserId::Safari).unwrap();
        let cookies = safari
            .categories
            .iter()
            .find(|c| c.id == PrivacyCategoryId::Cookies)
            .unwrap();
        assert!(cookies
            .rel_to_home
            .iter()
            .any(|p| p.contains("Containers/com.apple.Safari/Data/Library/Cookies")));
    }

    #[test]
    fn profile_modes_match_upstream_layout() {
        // chrome+edge = ChromeLike, firefox = FirefoxLike, safari = None
        let mac = catalog_for(Os::Mac, &home());
        let find = |id: BrowserId| mac.iter().find(|b| b.id == id).unwrap().profile_mode;
        assert_eq!(find(BrowserId::Chrome), ProfileMode::ChromeLike);
        assert_eq!(find(BrowserId::Edge), ProfileMode::ChromeLike);
        assert_eq!(find(BrowserId::Firefox), ProfileMode::FirefoxLike);
        assert_eq!(find(BrowserId::Safari), ProfileMode::None);
    }

    #[test]
    fn every_browser_has_all_five_categories() {
        for os in [Os::Mac, Os::Linux, Os::Windows] {
            for b in catalog_for(os, &home()) {
                let ids: HashSet<PrivacyCategoryId> = b.categories.iter().map(|c| c.id).collect();
                for want in [
                    PrivacyCategoryId::Cache,
                    PrivacyCategoryId::Cookies,
                    PrivacyCategoryId::History,
                    PrivacyCategoryId::Sessions,
                    PrivacyCategoryId::LocalStorage,
                ] {
                    assert!(ids.contains(&want), "{os:?} {:?} missing {want:?}", b.id,);
                }
            }
        }
    }

    #[test]
    fn category_ids_are_unique_within_a_browser() {
        for os in [Os::Mac, Os::Linux, Os::Windows] {
            for b in catalog_for(os, &home()) {
                let set: HashSet<PrivacyCategoryId> = b.categories.iter().map(|c| c.id).collect();
                assert_eq!(set.len(), b.categories.len(), "{os:?} {:?} dup", b.id);
            }
        }
    }

    #[test]
    fn chrome_like_categories_have_only_rel_to_profile() {
        // chromium keeps everything inside the profile dir, rel_to_home must be
        // empty. load-bearing: a stray absolute path would bypass profile enum
        // and silently scan every user's chrome data.
        for browser in [
            BrowserId::Chrome,
            BrowserId::Chromium,
            BrowserId::Edge,
            BrowserId::Brave,
            BrowserId::Vivaldi,
        ] {
            for os in [Os::Mac, Os::Linux, Os::Windows] {
                let cat = catalog_for(os, &home());
                let b = cat.iter().find(|x| x.id == browser).unwrap();
                for c in &b.categories {
                    assert!(
                        c.rel_to_home.is_empty(),
                        "{os:?} {browser:?} {:?} has home-relative paths",
                        c.id,
                    );
                    assert!(
                        !c.rel_to_profile.is_empty(),
                        "{os:?} {browser:?} {:?} has no profile-relative paths",
                        c.id,
                    );
                }
            }
        }
    }

    #[test]
    fn safari_categories_have_only_rel_to_home() {
        // inverse of the chromium check. safari has no profile concept so every
        // path must be home-rooted. non-empty rel_to_profile would orphan paths.
        let mac = catalog_for(Os::Mac, &home());
        let safari = mac.iter().find(|b| b.id == BrowserId::Safari).unwrap();
        for c in &safari.categories {
            assert!(c.rel_to_profile.is_empty(), "{:?} has profile paths", c.id);
            assert!(!c.rel_to_home.is_empty(), "{:?} has no home paths", c.id);
        }
    }

    #[test]
    fn no_rel_path_is_empty_or_escapes() {
        // empty rel-path + join(profile) = profile, would delete the whole thing.
        // "../" escapes. both disallowed by construction, locks it in.
        for os in [Os::Mac, Os::Linux, Os::Windows] {
            for b in catalog_for(os, &home()) {
                for c in &b.categories {
                    for p in c.rel_to_profile.iter().chain(c.rel_to_home.iter()) {
                        assert!(!p.is_empty(), "{os:?} {:?} {:?} empty", b.id, c.id);
                        assert!(
                            !p.starts_with("..") && !p.contains("/.."),
                            "{os:?} {:?} {:?} escapes: {p}",
                            b.id,
                            c.id,
                        );
                        assert!(
                            !p.starts_with('/'),
                            "{os:?} {:?} {:?} absolute: {p}",
                            b.id,
                            c.id,
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn id_strings_round_trip_to_serde() {
        for (id, want) in [
            (BrowserId::Chrome, "chrome"),
            (BrowserId::Edge, "edge"),
            (BrowserId::Firefox, "firefox"),
            (BrowserId::Safari, "safari"),
        ] {
            assert_eq!(id.as_str(), want);
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(json, format!("\"{want}\""));
        }
        for (id, want) in [
            (PrivacyCategoryId::Cache, "cache"),
            (PrivacyCategoryId::Cookies, "cookies"),
            (PrivacyCategoryId::History, "history"),
            (PrivacyCategoryId::Sessions, "sessions"),
            (PrivacyCategoryId::LocalStorage, "local-storage"),
        ] {
            assert_eq!(id.as_str(), want);
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(json, format!("\"{want}\""));
        }
    }

    #[test]
    fn catalog_is_deterministic_across_calls() {
        let a = catalog_for(Os::Mac, &home());
        let b = catalog_for(Os::Mac, &home());
        assert_eq!(a, b);
    }

    #[test]
    fn every_spec_has_non_empty_label_icon_description() {
        for os in [Os::Mac, Os::Linux, Os::Windows] {
            for b in catalog_for(os, &home()) {
                assert!(!b.label.is_empty());
                assert!(!b.icon.is_empty());
                for c in &b.categories {
                    assert!(!c.label.is_empty());
                    assert!(!c.description.is_empty());
                    assert!(!c.icon.is_empty());
                }
            }
        }
    }

    #[test]
    fn mac_safari_categories_reference_library_subpaths() {
        let mac = catalog_for(Os::Mac, &home());
        let safari = mac.iter().find(|b| b.id == BrowserId::Safari).unwrap();
        let cache = safari
            .categories
            .iter()
            .find(|c| c.id == PrivacyCategoryId::Cache)
            .unwrap();
        assert!(cache
            .rel_to_home
            .iter()
            .any(|p| p.contains("com.apple.Safari")));
        let history = safari
            .categories
            .iter()
            .find(|c| c.id == PrivacyCategoryId::History)
            .unwrap();
        assert!(history.rel_to_home.iter().any(|p| p.contains("History.db")));
    }

    #[test]
    fn platform_tags_are_stable() {
        assert_eq!(platform_tag(Os::Mac), "mac");
        assert_eq!(platform_tag(Os::Linux), "linux");
        assert_eq!(platform_tag(Os::Windows), "windows");
    }
}
