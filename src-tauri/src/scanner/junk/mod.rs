//! junk catalog + scanner. see [`catalog`] for the path table and
//! [`scan`] for the walker. use the re-exports here, don't reach in.

pub mod catalog;
pub mod scan;

pub use catalog::current_os;
pub use scan::{scan_junk, JunkReport};
