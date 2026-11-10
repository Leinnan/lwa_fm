//! Mounted drives as the core knows them; the effects crate lists and
//! ejects them.
use std::path::PathBuf;

use facet::Facet;
use serde::{Deserialize, Serialize};

/// One mounted volume as shown in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drive {
    /// Where the volume is mounted: `E:\` on Windows, `/Volumes/USB` on macOS.
    pub mount: PathBuf,
    /// Volume label, or a generic name ("USB Drive") when it has none.
    pub label: String,
    pub kind: DriveKind,
    /// Capacity in bytes; 0 when unknown (offline network drives).
    pub total: u64,
    pub available: u64,
    pub file_system: String,
}

#[derive(Facet, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub enum DriveKind {
    /// The system disk and other internal disks.
    Fixed,
    /// USB sticks, SD cards and external disks.
    Removable,
    Network,
    Optical,
    /// RAM disks and similar.
    #[cfg_attr(
        not(windows),
        allow(dead_code, reason = "only Windows reports RAM disks")
    )]
    Virtual,
}

impl DriveKind {
    /// Name for a volume that has no label, as Explorer and Finder show it.
    pub const fn generic_name(self) -> &'static str {
        match self {
            Self::Fixed => "Local Disk",
            Self::Removable => "USB Drive",
            Self::Network => "Network Drive",
            Self::Optical => "CD Drive",
            Self::Virtual => "RAM Disk",
        }
    }
}

impl Drive {
    /// "Backup (E:)" on Windows, where the letter is how people know a
    /// drive; the label alone elsewhere.
    pub fn display_name(&self) -> String {
        if cfg!(windows) {
            let letter = self.mount.to_string_lossy();
            format!("{} ({})", self.label, letter.trim_end_matches('\\'))
        } else {
            self.label.clone()
        }
    }

    /// Whether the drive can be ejected (or, for network drives,
    /// disconnected) from the app.
    pub const fn ejectable(&self) -> bool {
        matches!(
            self.kind,
            DriveKind::Removable | DriveKind::Network | DriveKind::Optical
        )
    }

    pub const fn eject_label(&self) -> &'static str {
        match self.kind {
            DriveKind::Network => "Disconnect",
            _ => "Eject",
        }
    }

    /// Share of the drive in use, 0.0..=1.0, when the capacity is known.
    pub fn used_fraction(&self) -> Option<f32> {
        (self.total > 0)
            .then(|| (self.total.saturating_sub(self.available)) as f32 / self.total as f32)
    }
}

/// A drive for tests: 64 GB with 16 GB free.
#[doc(hidden)]
pub fn test_drive(mount: &str, label: &str, kind: DriveKind) -> Drive {
    Drive {
        mount: PathBuf::from(mount),
        label: label.into(),
        kind,
        total: 64_000_000_000,
        available: 16_000_000_000,
        file_system: "exFAT".into(),
    }
}
