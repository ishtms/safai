//! memory + activity monitor.
//!
//! streams live cpu/mem/process telemetry for the Memory and Activity Monitor
//! screens. one streaming command fans out an [`ActivitySnapshot`] per tick
//! on `activity://snapshot`, UI picks what it needs.
//!
//! # why push not poll
//!
//! cadence driven by a single Rust timer instead of the frontend calling a
//! snapshot command every tick:
//!
//! 1. sysinfo's CPU reading is delta-based, needs two samples across time.
//!    if the UI polls, every "first call after nav" reports 0% because the
//!    delta window just reset. long-lived pusher keeps the delta warm
//! 2. back-pressure. command-per-tick pays tauri dispatch + serde every
//!    1s per window. one pusher pays it once
//!
//! # killing processes
//!
//! [`kill::kill_pid`] is the activity monitor kill button entry. refuses
//! pid 0 / pid 1 / our own pid before reaching sysinfo, so a compromised
//! renderer can't ask the backend to SIGKILL launchd
//!
//! # pieces
//!
//! * [`types`] wire shapes
//! * [`sample`] pure sampling pipeline + [`sample::SystemProbe`] trait
//! * [`stream`] [`stream::ActivityController`] / registry / interruptible sleep
//! * [`kill`] cross-platform kill_pid

pub mod kill;
pub mod sample;
pub mod stream;
pub mod types;

pub use kill::kill_pid;
#[allow(unused_imports)]
pub use kill::{is_protected_pid, KillError};
pub use sample::{retop_snapshot, sample as sample_activity, SysinfoProbe};
pub use stream::{
    next_activity_handle_id, run_activity_stream, ActivityController, ActivityEmit, ActivityHandle,
    ActivityInsert, ActivityRegistry, DEFAULT_TOP_N,
};
#[allow(unused_imports)]
pub use stream::{DEFAULT_INTERVAL_MS, MAX_INTERVAL_MS};
pub use types::ActivitySnapshot;
// re-exported for lib/activity.ts mirror consumers + tests in siblings
#[allow(unused_imports)]
pub use types::{CpuSnapshot, MemorySnapshot, ProcessRow};
