// scanner module. shipped the summary surface with mocked data,
// adds the real streaming walker (`run`) + its path-verdict
// classifier. scanner::junk replaces this with per-platform catalogs

pub mod activity;
pub mod classify;
pub mod dupes;
pub mod fs_guard;
pub mod junk;
pub mod largeold;
pub mod malware;
pub mod meta_ext;
pub mod privacy;
pub mod run;
pub mod startup;
pub mod summary;
pub mod treemap;
pub mod work_budget;

pub use summary::{
    empty_summary, summary_from_scan, LastScanFacts, LastScanStore, SmartScanSummary,
};
