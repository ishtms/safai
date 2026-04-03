//! startup items manager.
//!
//! cross-platform enumerate + toggle for login auto-start:
//!
//! - [`linux`] - XDG autostart (~/.config/autostart/*.desktop) + user systemd
//!   (~/.config/systemd/user/*.service)
//! - [`mac`] - launch agents under ~/Library/LaunchAgents (rw) + /Library/
//!   LaunchAgents + /Library/LaunchDaemons (ro)
//! - [`windows`] - Startup folder shortcuts (rw). registry Run keys empty on
//!   non-windows builds, full support later.
//!
//! [`scan`] orchestrator fans the enumerators under std::thread::scope and
//! merges into one sorted [`types::StartupReport`]. toggle = UI hands back
//! id+source+path, [`scan::toggle_startup`] dispatches to the per-source impl.
//!
//! no cleaner here, "disabled" = flipped flag or renamed file, not
//! moved-to-graveyard. user expects file still there, just inert.

pub mod linux;
pub mod mac;
pub mod scan;
pub mod types;
pub mod windows;

pub use scan::{current_os, scan_startup, toggle_startup};
pub use types::{StartupReport, StartupSource, ToggleResult};
