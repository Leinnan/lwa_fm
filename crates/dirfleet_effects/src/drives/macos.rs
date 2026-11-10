//! macOS drives: the boot volume plus what Finder shows under `/Volumes`;
//! eject through `diskutil`, which also powers the disk down.

use std::ffi::OsStr;

use super::{Drive, DriveKind, unix};

pub fn list() -> Vec<Drive> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .iter()
        .filter(|disk| unix::macos_should_show(disk.mount_point()))
        .map(|disk| {
            // Anything under /Volumes is an extra disk or image Finder would
            // offer to eject; the boot volume never is.
            let removable = disk.is_removable() || disk.mount_point() != std::path::Path::new("/");
            let kind = unix::kind_for(&disk.file_system().to_string_lossy(), removable);
            let mut drive = unix::drive_from_disk(disk, kind, "Macintosh HD");
            let name = disk.name().to_string_lossy();
            if !name.is_empty() && !name.starts_with("/dev/") {
                drive.label = name.into_owned();
            }
            drive
        })
        .collect()
}

pub fn eject(drive: &Drive) -> anyhow::Result<()> {
    let verb = if drive.kind == DriveKind::Network {
        "unmount"
    } else {
        "eject"
    };
    unix::run("diskutil", &[OsStr::new(verb), drive.mount.as_os_str()])
}
