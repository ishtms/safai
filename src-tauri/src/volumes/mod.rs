//! cross-platform disk telemetry.
//!
//! two layers so the interesting logic is testable without the OS:
//!
//! * source: adapter over sysinfo::Disks. returns RawVolume exactly as
//!   reported, no interpretation.
//! * process: pure fn. RawVolume + platform -> Volume. dedup, pseudo-fs
//!   filter, primary-disk pick, ordering.
//!
//! commands.rs just glues them together.

mod process;
mod source;
mod types;

pub use source::list_volumes;
pub use types::Volume;
