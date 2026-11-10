//! Mounted drives: listing them, noticing when they come and go, and
//! ejecting them. The platform modules do the OS work; everything here is
//! shared so the app sees one model on every system.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
// Mount-table parsing and filters are plain functions, compiled everywhere
// so their tests run on every platform.
#[cfg_attr(not(unix), allow(dead_code))]
mod unix;
#[cfg(windows)]
mod windows;

use std::path::{Path, PathBuf};

pub use dirfleet_core::drives::{Drive, DriveKind, test_drive};

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
///
/// # Errors
/// When the OS refuses, for instance because files on it are open.
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
        .filter(|drive| is_under(path, &drive.mount))
        .max_by_key(|drive| drive.mount.as_os_str().len())
}

/// Whether `path` is `mount` or below it, compared component by component
/// (so `/Volumes/USB2` is not under `/Volumes/USB`), ignoring case on
/// Windows and macOS, whose disks usually do.
pub fn is_under(path: &Path, mount: &Path) -> bool {
    let path = crate::paths::canonical_form(path);
    let mount = crate::paths::canonical_form(mount);
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
