//! junk path catalog. pure table, no IO beyond path joins.
//! table-driven so every platform can be unit-tested against a synthetic $HOME.
//!
//! scanning lives in [`super::scan`]. this file just says where to look.
//!
//! categories are conservative: every entry is a dir whose contents regen
//! on their own. ambiguous stuff (~/Downloads, random build outputs) belongs
//! in large & old, not here.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// stable kebab-case category id. new variants are additive, don't rename
/// or renumber without bumping the persistence schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum JunkCategoryId {
    UserCaches,
    SystemLogs,
    XcodeDerivedData,
    NpmCache,
    PnpmStore,
    CargoCache,
    GoModCache,
    Trash,
    TempFiles,
    ChromeCache,
    EdgeCache,
    FirefoxCache,
}

impl JunkCategoryId {
    /// flat id for tests + ts side
    #[allow(dead_code)] // mirrored for tests + future deletion routing
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserCaches => "user-caches",
            Self::SystemLogs => "system-logs",
            Self::XcodeDerivedData => "xcode-derived-data",
            Self::NpmCache => "npm-cache",
            Self::PnpmStore => "pnpm-store",
            Self::CargoCache => "cargo-cache",
            Self::GoModCache => "go-mod-cache",
            Self::Trash => "trash",
            Self::TempFiles => "temp-files",
            Self::ChromeCache => "chrome-cache",
            Self::EdgeCache => "edge-cache",
            Self::FirefoxCache => "firefox-cache",
        }
    }
}

/// concrete enum instead of cfg branches so tests on any host exercise
/// every platform layout. [`current_os`] picks build target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // variants not picked by the current build target are exercised via tests
pub enum Os {
    Mac,
    Linux,
    Windows,
}

/// host OS from build target. picked rust-side so a cross-built binary
/// behaves right without a JS round-trip.
pub fn current_os() -> Os {
    #[cfg(target_os = "macos")]
    {
        return Os::Mac;
    }
    #[cfg(target_os = "linux")]
    {
        return Os::Linux;
    }
    #[cfg(target_os = "windows")]
    {
        return Os::Windows;
    }
    #[allow(unreachable_code)]
    Os::Linux
}

/// one fs location under a category. base is absolute after resolution,
/// home arg to [`catalog_for`] is baked in. scan = enumerate direct
/// children + sum each subtree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JunkPathSpec {
    pub base: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JunkCategorySpec {
    pub id: JunkCategoryId,
    pub label: &'static str,
    pub description: &'static str,
    /// matches `IconName` on the TS side
    pub icon: &'static str,
    /// UI badges "hot" categories (Xcode DerivedData, Windows Temp) so
    /// users see the big wins
    pub hot: bool,
    pub paths: Vec<JunkPathSpec>,
}

/// build catalog for `os` with `home` as the user home dir. deterministic.
/// never reads env in here, call sites pass HOME / USERPROFILE. that's
/// what keeps this hermetic.
pub fn catalog_for(os: Os, home: &Path) -> Vec<JunkCategorySpec> {
    let h = |rel: &str| JunkPathSpec {
        base: home.join(rel),
    };
    let abs = |s: &str| JunkPathSpec {
        base: PathBuf::from(s),
    };

    match os {
        Os::Mac => vec![
            JunkCategorySpec {
                id: JunkCategoryId::UserCaches,
                label: "User caches",
                description: "Per-app caches under ~/Library/Caches. Apps regenerate these on next launch.",
                icon: "broom",
                hot: false,
                paths: vec![h("Library/Caches")],
            },
            JunkCategorySpec {
                id: JunkCategoryId::SystemLogs,
                label: "System logs",
                description: "User-scope app logs. Rotate on their own; safe to clear between sessions.",
                icon: "file",
                hot: false,
                paths: vec![h("Library/Logs")],
            },
            JunkCategorySpec {
                id: JunkCategoryId::XcodeDerivedData,
                label: "Xcode derived data",
                description: "Build products Xcode regenerates on the next build.",
                icon: "archive",
                hot: true,
                paths: vec![h("Library/Developer/Xcode/DerivedData")],
            },
            JunkCategorySpec {
                id: JunkCategoryId::NpmCache,
                label: "npm cache",
                description: "Package tarballs npm can re-download.",
                icon: "archive",
                hot: false,
                paths: vec![h(".npm/_cacache")],
            },
            JunkCategorySpec {
                id: JunkCategoryId::PnpmStore,
                label: "pnpm store",
                description: "Content-addressed package store; rebuilt from npm on demand.",
                icon: "archive",
                hot: false,
                paths: vec![h(".pnpm-store")],
            },
            JunkCategorySpec {
                id: JunkCategoryId::CargoCache,
                label: "Cargo cache",
                description: "Downloaded crate sources. Cargo re-fetches as needed.",
                icon: "archive",
                hot: false,
                paths: vec![
                    h(".cargo/registry/cache"),
                    h(".cargo/registry/src"),
                ],
            },
            JunkCategorySpec {
                id: JunkCategoryId::GoModCache,
                label: "Go module cache",
                description: "`go mod` downloaded modules.",
                icon: "archive",
                hot: false,
                paths: vec![h("go/pkg/mod/cache")],
            },
            JunkCategorySpec {
                id: JunkCategoryId::Trash,
                label: "Trash",
                description: "Already in the Trash, ready to empty.",
                icon: "trash",
                hot: false,
                paths: vec![h(".Trash")],
            },
        ],
        Os::Linux => vec![
            JunkCategorySpec {
                id: JunkCategoryId::UserCaches,
                label: "User caches",
                description: "~/.cache - the XDG cache root. Apps regenerate these.",
                icon: "broom",
                hot: false,
                paths: vec![h(".cache")],
            },
            JunkCategorySpec {
                id: JunkCategoryId::TempFiles,
                label: "Temp files",
                description: "/tmp contents. Cleared on reboot anyway; reclaiming now is free.",
                icon: "file",
                hot: false,
                paths: vec![abs("/tmp")],
            },
            JunkCategorySpec {
                id: JunkCategoryId::Trash,
                label: "Trash",
                description: "XDG Trash - ~/.local/share/Trash.",
                icon: "trash",
                hot: false,
                paths: vec![h(".local/share/Trash")],
            },
            JunkCategorySpec {
                id: JunkCategoryId::NpmCache,
                label: "npm cache",
                description: "Package tarballs npm can re-download.",
                icon: "archive",
                hot: false,
                paths: vec![h(".npm/_cacache")],
            },
            JunkCategorySpec {
                id: JunkCategoryId::PnpmStore,
                label: "pnpm store",
                description: "Content-addressed package store; rebuilt from npm on demand.",
                icon: "archive",
                hot: false,
                paths: vec![h(".pnpm-store")],
            },
            JunkCategorySpec {
                id: JunkCategoryId::CargoCache,
                label: "Cargo cache",
                description: "Downloaded crate sources. Cargo re-fetches as needed.",
                icon: "archive",
                hot: false,
                paths: vec![
                    h(".cargo/registry/cache"),
                    h(".cargo/registry/src"),
                ],
            },
            JunkCategorySpec {
                id: JunkCategoryId::GoModCache,
                label: "Go module cache",
                description: "`go mod` downloaded modules.",
                icon: "archive",
                hot: false,
                paths: vec![h("go/pkg/mod/cache")],
            },
        ],
        Os::Windows => vec![
            JunkCategorySpec {
                id: JunkCategoryId::TempFiles,
                label: "Temp files",
                description: "%LOCALAPPDATA%\\Temp - per-user scratch.",
                icon: "file",
                hot: true,
                paths: vec![h("AppData/Local/Temp")],
            },
            JunkCategorySpec {
                id: JunkCategoryId::ChromeCache,
                label: "Chrome cache",
                description: "Google Chrome's HTTP cache.",
                icon: "archive",
                hot: false,
                paths: vec![h("AppData/Local/Google/Chrome/User Data/Default/Cache")],
            },
            JunkCategorySpec {
                id: JunkCategoryId::EdgeCache,
                label: "Edge cache",
                description: "Microsoft Edge's HTTP cache.",
                icon: "archive",
                hot: false,
                paths: vec![h("AppData/Local/Microsoft/Edge/User Data/Default/Cache")],
            },
            JunkCategorySpec {
                id: JunkCategoryId::FirefoxCache,
                label: "Firefox cache",
                description: "Firefox profile caches (all profiles).",
                icon: "archive",
                hot: false,
                paths: vec![h("AppData/Local/Mozilla/Firefox/Profiles")],
            },
            JunkCategorySpec {
                id: JunkCategoryId::NpmCache,
                label: "npm cache",
                description: "Package tarballs npm can re-download.",
                icon: "archive",
                hot: false,
                paths: vec![h("AppData/Roaming/npm-cache")],
            },
            JunkCategorySpec {
                id: JunkCategoryId::CargoCache,
                label: "Cargo cache",
                description: "Downloaded crate sources. Cargo re-fetches as needed.",
                icon: "archive",
                hot: false,
                paths: vec![
                    h(".cargo/registry/cache"),
                    h(".cargo/registry/src"),
                ],
            },
        ],
    }
}

/// platform tag in the final report. UI pivots copy off this.
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
    fn mac_catalog_is_non_empty_and_rooted_in_home() {
        let cat = catalog_for(Os::Mac, &home());
        assert!(!cat.is_empty());
        for spec in &cat {
            assert!(!spec.paths.is_empty(), "{:?} had no paths", spec.id);
            for p in &spec.paths {
                // mac paths all rooted in $HOME, don't let them leak
                // into system dirs
                assert!(
                    p.base.starts_with(home()),
                    "{:?} base {:?} is not under home",
                    spec.id,
                    p.base,
                );
            }
        }
    }

    #[test]
    fn linux_catalog_includes_tmp_and_xdg_trash() {
        let cat = catalog_for(Os::Linux, &home());
        let has_tmp = cat.iter().any(|c| {
            matches!(c.id, JunkCategoryId::TempFiles)
                && c.paths.iter().any(|p| p.base == PathBuf::from("/tmp"))
        });
        let has_xdg_trash = cat.iter().any(|c| {
            matches!(c.id, JunkCategoryId::Trash)
                && c.paths
                    .iter()
                    .any(|p| p.base == home().join(".local/share/Trash"))
        });
        assert!(has_tmp, "linux catalog missing /tmp");
        assert!(has_xdg_trash, "linux catalog missing XDG Trash");
    }

    #[test]
    fn windows_catalog_uses_appdata_local() {
        let cat = catalog_for(Os::Windows, &home());
        for spec in &cat {
            for p in &spec.paths {
                // windows catalog must sit under home (we only use home here)
                assert!(
                    p.base.starts_with(home()),
                    "{:?} base {:?} not under home",
                    spec.id,
                    p.base,
                );
            }
        }
        // need at least one temp-files entry
        assert!(
            cat.iter().any(|c| matches!(c.id, JunkCategoryId::TempFiles)),
            "windows catalog missing Temp files",
        );
    }

    #[test]
    fn category_ids_are_unique_per_os() {
        for os in [Os::Mac, Os::Linux, Os::Windows] {
            let cat = catalog_for(os, &home());
            let set: HashSet<JunkCategoryId> = cat.iter().map(|c| c.id).collect();
            assert_eq!(set.len(), cat.len(), "duplicate id in {os:?} catalog");
        }
    }

    #[test]
    fn every_spec_has_label_description_icon() {
        for os in [Os::Mac, Os::Linux, Os::Windows] {
            for spec in catalog_for(os, &home()) {
                assert!(!spec.label.is_empty());
                assert!(!spec.description.is_empty());
                assert!(!spec.icon.is_empty());
            }
        }
    }

    #[test]
    fn catalog_is_deterministic_across_calls() {
        let a = catalog_for(Os::Mac, &home());
        let b = catalog_for(Os::Mac, &home());
        assert_eq!(a, b);
    }

    #[test]
    fn id_str_round_trip_is_kebab() {
        // as_str must match the serde kebab form, either can be used on
        // the wire
        let pairs = [
            (JunkCategoryId::UserCaches, "user-caches"),
            (JunkCategoryId::XcodeDerivedData, "xcode-derived-data"),
            (JunkCategoryId::GoModCache, "go-mod-cache"),
            (JunkCategoryId::ChromeCache, "chrome-cache"),
        ];
        for (id, expect) in pairs {
            assert_eq!(id.as_str(), expect);
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(json, format!("\"{expect}\""));
        }
    }

    #[test]
    fn platform_tags_are_stable() {
        assert_eq!(platform_tag(Os::Mac), "mac");
        assert_eq!(platform_tag(Os::Linux), "linux");
        assert_eq!(platform_tag(Os::Windows), "windows");
    }

    #[test]
    fn hot_flag_is_set_only_where_expected() {
        // per design: Xcode DerivedData on mac, Temp on windows. rest stay calm
        let mac = catalog_for(Os::Mac, &home());
        let hot_mac: Vec<JunkCategoryId> = mac.iter().filter(|c| c.hot).map(|c| c.id).collect();
        assert_eq!(hot_mac, vec![JunkCategoryId::XcodeDerivedData]);

        let win = catalog_for(Os::Windows, &home());
        let hot_win: Vec<JunkCategoryId> = win.iter().filter(|c| c.hot).map(|c| c.id).collect();
        assert_eq!(hot_win, vec![JunkCategoryId::TempFiles]);
    }

    #[test]
    fn cargo_registry_covers_cache_and_src() {
        // cargo stores .crate files under /cache and extracts under /src,
        // need both or we leave ~30% of reclaimable bytes
        for os in [Os::Mac, Os::Linux, Os::Windows] {
            let cat = catalog_for(os, &home());
            let cargo = cat
                .iter()
                .find(|c| matches!(c.id, JunkCategoryId::CargoCache))
                .expect("Cargo cache category should exist");
            let bases: HashSet<PathBuf> = cargo.paths.iter().map(|p| p.base.clone()).collect();
            assert!(bases.contains(&home().join(".cargo/registry/cache")));
            assert!(bases.contains(&home().join(".cargo/registry/src")));
        }
    }
}
