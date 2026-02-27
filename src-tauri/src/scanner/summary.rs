//! smart scan summary for the home screen dashboard.
//!
//! contract (field shapes, kebab-case ids, camelCase wire) preserved from
//! what changed: summary now derived from the *last completed scan*
//! not a hardcoded mock. [`LastScanStore`] holds run_scan's emit_done totals,
//! command surface reads from it at paint time.
//!
//! if no scan ever completed in this process, return empty (zeros +
//! scanned_at: None). frontend renders as "Last scan - Never" with a prompt
//! to run a scan, no stale demo numbers.
//!
//! category breakdowns aren't produced by the generic walker (only labels
//! files Found/Safe via classify). category totals ship empty here, the
//! per-category screens (junk, duplicates, large/old, privacy) remain the
//! source of truth for their own views

use std::sync::Mutex;

use serde::Serialize;

/// serialized as kebab-case so TypeScript can switch on it without a mapping
/// table. kept for when category-level roll-ups ship
#[allow(dead_code)] // variants pinned for future category aggregation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CategoryId {
    SystemJunk,
    Duplicates,
    LargeOld,
    Privacy,
    AppLeftovers,
    Trash,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategorySummary {
    pub id: CategoryId,
    pub label: &'static str,
    /// matches `IconName` on TS side
    pub icon: &'static str,
    /// CSS custom property for the swatch
    pub color_var: &'static str,
    pub bytes: u64,
    pub items: u64,
    /// short phrase shown under the number, e.g. "All safe to remove"
    pub safe_note: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SmartScanSummary {
    pub total_bytes: u64,
    pub total_items: u64,
    /// unix seconds. None means never scanned, UI shows "Never"
    pub scanned_at: Option<u64>,
    pub categories: Vec<CategorySummary>,
    /// kept in wire format for backward-compat w/ the TS type. always false
    /// now that the mock path is gone
    pub mocked: bool,
}

impl SmartScanSummary {
    /// kept for callers that mutate categories in place. pure over the
    /// category list so totals can't drift
    #[allow(dead_code)] // kept on API surface for future category aggregation
    pub fn recompute_totals(&mut self) {
        self.total_bytes = self.categories.iter().map(|c| c.bytes).sum();
        self.total_items = self.categories.iter().map(|c| c.items).sum();
    }
}

/// subset of [`crate::scanner::run::ScanProgress`] the dashboard cares about
#[derive(Debug, Clone, Copy)]
pub struct LastScanFacts {
    /// bytes labelled Found or Safe by classify
    pub flagged_bytes: u64,
    /// count of files that produced a verdict
    pub flagged_items: u64,
    /// unix seconds when the scan completed
    pub scanned_at: u64,
}

/// process-wide holder for the most recent completed scan. written by
/// run_scan's emit_done hook (via AppEmitter adapter), read by the
/// `smart_scan_summary` tauri command.
///
/// simple `Mutex<Option<...>>`, written once per scan, read on mount,
/// no contention worth an RwLock
#[derive(Default)]
pub struct LastScanStore {
    inner: Mutex<Option<LastScanFacts>>,
}

impl LastScanStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, facts: LastScanFacts) {
        if let Ok(mut g) = self.inner.lock() {
            *g = Some(facts);
        }
    }

    pub fn get(&self) -> Option<LastScanFacts> {
        self.inner.lock().ok().and_then(|g| *g)
    }
}

/// shown when no scan has completed yet. hero renders "0 B", "Last scan - Never"
pub fn empty_summary() -> SmartScanSummary {
    SmartScanSummary {
        total_bytes: 0,
        total_items: 0,
        scanned_at: None,
        categories: Vec::new(),
        mocked: false,
    }
}

/// category breakdown empty (generic walker doesn't bucket into
/// Junk/Duplicates/etc), per-category screens remain authoritative for detail
pub fn summary_from_scan(facts: LastScanFacts) -> SmartScanSummary {
    SmartScanSummary {
        total_bytes: facts.flagged_bytes,
        total_items: facts.flagged_items,
        scanned_at: Some(facts.scanned_at),
        categories: Vec::new(),
        mocked: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_summary_is_never_scanned() {
        let s = empty_summary();
        assert_eq!(s.total_bytes, 0);
        assert_eq!(s.total_items, 0);
        assert!(s.scanned_at.is_none());
        assert!(s.categories.is_empty());
        assert!(!s.mocked);
    }

    #[test]
    fn summary_from_scan_reflects_facts() {
        let facts = LastScanFacts {
            flagged_bytes: 7_600_000_000,
            flagged_items: 12_345,
            scanned_at: 1_700_000_000,
        };
        let s = summary_from_scan(facts);
        assert_eq!(s.total_bytes, 7_600_000_000);
        assert_eq!(s.total_items, 12_345);
        assert_eq!(s.scanned_at, Some(1_700_000_000));
        assert!(s.categories.is_empty());
    }

    #[test]
    fn last_scan_store_round_trip() {
        let store = LastScanStore::new();
        assert!(store.get().is_none());
        let facts = LastScanFacts {
            flagged_bytes: 1,
            flagged_items: 2,
            scanned_at: 3,
        };
        store.set(facts);
        let got = store.get().expect("set then get");
        assert_eq!(got.flagged_bytes, 1);
        assert_eq!(got.flagged_items, 2);
        assert_eq!(got.scanned_at, 3);
    }

    #[test]
    fn last_scan_store_overwrites_on_set() {
        let store = LastScanStore::new();
        store.set(LastScanFacts {
            flagged_bytes: 100,
            flagged_items: 1,
            scanned_at: 10,
        });
        store.set(LastScanFacts {
            flagged_bytes: 200,
            flagged_items: 2,
            scanned_at: 20,
        });
        let got = store.get().unwrap();
        assert_eq!(got.flagged_bytes, 200);
        assert_eq!(got.scanned_at, 20);
    }

    #[test]
    fn recompute_totals_replaces_drift() {
        let mut s = SmartScanSummary {
            total_bytes: 99,
            total_items: 99,
            scanned_at: None,
            categories: vec![CategorySummary {
                id: CategoryId::SystemJunk,
                label: "x",
                icon: "y",
                color_var: "--z",
                bytes: 5,
                items: 3,
                safe_note: "n",
            }],
            mocked: false,
        };
        s.recompute_totals();
        assert_eq!(s.total_bytes, 5);
        assert_eq!(s.total_items, 3);
    }

    #[test]
    fn serializes_ids_as_kebab_case() {
        // TS consumers switch on the kebab string, pin the wire format
        let json = serde_json::to_string(&CategoryId::AppLeftovers).unwrap();
        assert_eq!(json, "\"app-leftovers\"");
        let junk = serde_json::to_string(&CategoryId::SystemJunk).unwrap();
        assert_eq!(junk, "\"system-junk\"");
    }

    #[test]
    fn summary_is_serializable_as_camelcase() {
        let s = summary_from_scan(LastScanFacts {
            flagged_bytes: 10,
            flagged_items: 1,
            scanned_at: 42,
        });
        let v = serde_json::to_value(&s).unwrap();
        assert!(v.get("totalBytes").is_some());
        assert!(v.get("totalItems").is_some());
        assert!(v.get("scannedAt").is_some());
        assert!(v.get("mocked").is_some());
    }
}
