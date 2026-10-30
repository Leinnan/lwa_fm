//! Mounted drives: listing them, noticing when they come and go, and
//! ejecting them. The platform modules do the OS work; everything here is
//! shared so the app sees one model on every system.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod monitor;
// Mount-table parsing and filters are plain functions, compiled everywhere
// so their tests run on every platform.
#[cfg_attr(not(unix), allow(dead_code))]
mod unix;
#[cfg(windows)]
mod windows;

use std::path::{Path, PathBuf};

pub use monitor::Monitor;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    pub const fn icon(self) -> lucide_icons::Icon {
        match self {
            Self::Fixed => lucide_icons::Icon::HardDrive,
            Self::Removable => lucide_icons::Icon::Usb,
            Self::Network => lucide_icons::Icon::Server,
            Self::Optical => lucide_icons::Icon::Disc,
            Self::Virtual => lucide_icons::Icon::MemoryStick,
        }
    }

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

    /// Whether `path` is on this drive.
    pub fn contains(&self, path: &Path) -> bool {
        is_under(path, &self.mount)
    }

    /// Share of the drive in use, 0.0..=1.0, when the capacity is known.
    pub fn used_fraction(&self) -> Option<f32> {
        (self.total > 0)
            .then(|| (self.total.saturating_sub(self.available)) as f32 / self.total as f32)
    }
}

/// Every mounted drive worth showing, sorted by mount point.
pub fn list() -> Vec<Drive> {
    #[cfg(windows)]
    let mut drives = windows::list();
    #[cfg(target_os = "macos")]
    let mut drives = macos::list();
    #[cfg(target_os = "linux")]
    let mut drives = linux::list();
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    let mut drives: Vec<Drive> = Vec::new();
    drives.sort_by(|a, b| a.mount.cmp(&b.mount));
    drives
}

/// Eject a drive (disconnect, for network drives). Blocks while the OS
/// flushes and releases the device, so call it off the UI thread.
pub fn eject(drive: &Drive) -> anyhow::Result<()> {
    #[cfg(windows)]
    return windows::eject(drive);
    #[cfg(target_os = "macos")]
    return macos::eject(drive);
    #[cfg(target_os = "linux")]
    return linux::eject(drive);
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    anyhow::bail!("Ejecting {} is not supported here", drive.label)
}

/// The drive `path` is on: the one with the longest matching mount point,
/// so `/Volumes/USB/x` belongs to `/Volumes/USB`, not `/`.
pub fn drive_for_path<'a>(drives: &'a [Drive], path: &Path) -> Option<&'a Drive> {
    drives
        .iter()
        .filter(|drive| drive.contains(path))
        .max_by_key(|drive| drive.mount.as_os_str().len())
}

/// Whether `path` is `mount` or below it, compared component by component
/// (so `/Volumes/USB2` is not under `/Volumes/USB`), ignoring case on
/// Windows and macOS, whose disks usually do.
pub fn is_under(path: &Path, mount: &Path) -> bool {
    let path = crate::helper::canonical_form(path);
    let mount = crate::helper::canonical_form(mount);
    if cfg!(any(windows, target_os = "macos")) {
        let lower = |p: &Path| PathBuf::from(p.to_string_lossy().to_lowercase());
        lower(&path).starts_with(lower(&mount))
    } else {
        path.starts_with(&mount)
    }
}

/// Drives in `new` that were not in `old`, and drives in `old` that are gone
/// from `new`, matched by mount point.
pub fn diff(old: &[Drive], new: &[Drive]) -> (Vec<Drive>, Vec<Drive>) {
    let added = new
        .iter()
        .filter(|drive| !old.iter().any(|o| o.mount == drive.mount))
        .cloned()
        .collect();
    let removed = old
        .iter()
        .filter(|drive| !new.iter().any(|n| n.mount == drive.mount))
        .cloned()
        .collect();
    (added, removed)
}

#[cfg(test)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_mount_wins() {
        let root = if cfg!(windows) { r"C:\" } else { "/" };
        let usb = if cfg!(windows) {
            r"C:\mnt\usb"
        } else {
            "/media/usb"
        };
        let drives = [
            test_drive(root, "System", DriveKind::Fixed),
            test_drive(usb, "USB", DriveKind::Removable),
        ];
        let inside = Path::new(usb).join("photos");
        assert_eq!(
            drive_for_path(&drives, &inside).map(|d| d.label.as_str()),
            Some("USB")
        );
        let elsewhere = Path::new(root).join("home");
        assert_eq!(
            drive_for_path(&drives, &elsewhere).map(|d| d.label.as_str()),
            Some("System")
        );
    }

    #[test]
    fn sibling_with_common_prefix_is_not_under() {
        let (mount, sibling) = if cfg!(windows) {
            (r"C:\Volumes\USB", r"C:\Volumes\USB2\file")
        } else {
            ("/Volumes/USB", "/Volumes/USB2/file")
        };
        assert!(!is_under(Path::new(sibling), Path::new(mount)));
        assert!(is_under(Path::new(mount), Path::new(mount)));
    }

    #[cfg(windows)]
    #[test]
    fn drive_letters_ignore_case() {
        assert!(is_under(Path::new(r"e:\Photos"), Path::new(r"E:\")));
    }

    #[test]
    fn diff_reports_added_and_removed() {
        let a = test_drive("/a", "A", DriveKind::Fixed);
        let b = test_drive("/b", "B", DriveKind::Removable);
        let c = test_drive("/c", "C", DriveKind::Removable);
        let (added, removed) = diff(&[a.clone(), b.clone()], &[a, c.clone()]);
        assert_eq!(added, vec![c]);
        assert_eq!(removed, vec![b]);
    }

    #[test]
    fn used_fraction_needs_a_capacity() {
        let mut drive = test_drive("/a", "A", DriveKind::Fixed);
        assert!((drive.used_fraction().unwrap_or_default() - 0.75).abs() < 1e-6);
        drive.total = 0;
        assert_eq!(drive.used_fraction(), None);
    }
}
