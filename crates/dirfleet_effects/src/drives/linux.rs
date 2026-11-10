//! Linux drives from the mount table; eject through udisks, like the file
//! managers of the common desktops.

use std::{ffi::OsStr, path::Path};

use super::{Drive, DriveKind, unix};

pub fn list() -> Vec<Drive> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .iter()
        .filter(|disk| {
            unix::linux_should_show(disk.mount_point(), &disk.file_system().to_string_lossy())
        })
        .map(|disk| {
            let removable =
                disk.is_removable() || unix::linux_is_removable_mount(disk.mount_point());
            let kind = unix::kind_for(&disk.file_system().to_string_lossy(), removable);
            unix::drive_from_disk(disk, kind, "File System")
        })
        .collect()
}

pub fn eject(drive: &Drive) -> anyhow::Result<()> {
    let mount = drive.mount.as_os_str();
    if drive.kind == DriveKind::Network {
        return unix::run("gio", &[OsStr::new("mount"), OsStr::new("-u"), mount])
            .or_else(|_| unix::run("umount", &[mount]));
    }
    let device = std::fs::read_to_string("/proc/self/mountinfo")
        .ok()
        .and_then(|info| unix::device_for_mount(&info, &drive.mount));
    if let Some(device) = device {
        let device = OsStr::new(&device);
        let unmounted = unix::run(
            "udisksctl",
            &[OsStr::new("unmount"), OsStr::new("-b"), device],
        );
        if unmounted.is_ok() {
            // Unmounted is already safe to unplug; powering off just spins
            // the disk down, and fails harmlessly for card readers.
            if let Err(err) = unix::run(
                "udisksctl",
                &[OsStr::new("power-off"), OsStr::new("-b"), device],
            ) {
                log::info!("Power-off of {} skipped: {err:#}", drive.mount.display());
            }
            return Ok(());
        }
        if mounted(&drive.mount) {
            log::info!("udisksctl unmount failed, trying gio: {unmounted:?}");
        } else {
            return Ok(());
        }
    }
    unix::run("gio", &[OsStr::new("mount"), OsStr::new("-e"), mount])
        .or_else(|_| unix::run("umount", &[mount]))
}

fn mounted(mount: &Path) -> bool {
    std::fs::read_to_string("/proc/self/mountinfo").is_ok_and(|info| {
        unix::parse_mountinfo(&info)
            .iter()
            .any(|e| e.mount == mount)
    })
}
