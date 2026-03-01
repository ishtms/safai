//! wire types for the volumes api.
//!
//! Volume = UI-facing (camelCase, enums kebab). RawVolume = internal,
//! what the platform reports before process cleans it.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VolumeKind {
    Ssd,
    Hdd,
    Removable,
    // SMB/NFS detection on mac/linux
    #[allow(dead_code)]
    Network,
    Unknown,
}

/// pre-interpretation. bytes can be 0 for pseudo fs, mount_point may
/// repeat on linux (bind mounts). process() cleans it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawVolume {
    pub name: String,
    pub mount_point: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub file_system: String,
    pub kind: VolumeKind,
    pub is_removable: bool,
}

/// UI-facing. field order matches the sidebar footer. used_bytes is a
/// saturating convenience so the frontend never sees underflow when a
/// disk briefly reports free > total during fsck.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Volume {
    pub name: String,
    pub mount_point: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub used_bytes: u64,
    pub file_system: String,
    pub kind: VolumeKind,
    pub is_removable: bool,
    /// exactly one volume has this true when any disk is non-empty.
    /// highlighted by sidebar footer + smart scan hero.
    pub is_primary: bool,
}
