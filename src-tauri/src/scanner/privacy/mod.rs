//! privacy cleaner.
//!
//! scans browsers for per-profile private data (cache/cookies/history/sessions/
//! local storage) and reports per-category sizes. deletion goes through the
//! cleaner, nothing is hard-deleted, Restore last works same as System Junk.
//!
//! split:
//! - [`catalog`] - pure per-OS per-browser path desc. hermetic, takes a synthetic
//!   $HOME so tests hit every platform on one host.
//! - [`scan`] - walks catalog against real fs, discovers Chrome/Firefox profile dirs.

pub mod catalog;
pub mod scan;

pub use catalog::current_os;
pub use scan::{scan_privacy, PrivacyReport};
