//! Mount-table parsing and the filters that decide which mounts are drives,
//! shared by the Linux and macOS backends.

// The Linux parsers are pure, so they are built (and tested) on macOS too.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::path::{Path, PathBuf};

use super::{Drive, DriveKind};

/// File systems that are kernel or container plumbing, never a drive.
const PSEUDO_FS: &[&str] = &[
    "autofs",
    "binfmt_misc",
    "bpf",
    "cgroup",
    "cgroup2",
    "configfs",
    "debugfs",
    "devpts",
    "devtmpfs",
    "efivarfs",
    "fusectl",
    "hugetlbfs",
    "mqueue",
    "nsfs",
    "overlay",
    "proc",
    "pstore",
    "ramfs",
    "securityfs",
    "squashfs",
    "sysfs",
    "tmpfs",
    "tracefs",
    "fuse.portal",
    "fuse.gvfsd-fuse",
];

const NETWORK_FS: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "smbfs",
    "afpfs",
    "webdav",
    "fuse.sshfs",
    "fuse.rclone",
];

/// System locations a Linux desktop never lists as drives.
const HIDDEN_MOUNTS: &[&str] = &[
    "/boot", "/dev", "/proc", "/sys", "/run", "/snap", "/var", "/tmp", "/usr", "/etc", "/opt",
    "/srv",
];

const OPTICAL_FS: &[&str] = &["iso9660", "udf", "cd9660"];

/// One line of `/proc/self/mountinfo`.
#[derive(Debug, PartialEq, Eq)]
pub struct MountEntry {
    pub mount: PathBuf,
    pub fs_type: String,
    pub source: String,
}

/// Parse `/proc/self/mountinfo`: `id parent maj:min root mount opts
/// [optional fields] - fstype source superopts`.
pub fn parse_mountinfo(text: &str) -> Vec<MountEntry> {
    text.lines()
        .filter_map(|line| {
            let (left, right) = line.split_once(" - ")?;
            let mount = left.split(' ').nth(4)?;
            let mut right = right.split(' ');
            let fs_type = right.next()?;
            let source = right.next()?;
            Some(MountEntry {
                mount: PathBuf::from(unescape(mount)),
                fs_type: fs_type.to_owned(),
                source: unescape(source),
            })
        })
        .collect()
}

/// Undo the octal escapes (`\040` for a space) the kernel uses in mount
/// tables.
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && let Some(code) = bytes.get(i + 1..i + 4)
            && code.iter().all(|b| (b'0'..=b'7').contains(b))
        {
            out.push(
                code.iter()
                    .fold(0u8, |acc, b| acc.wrapping_mul(8).wrapping_add(b - b'0')),
            );
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The block device mounted at `mount` (`/dev/sdb1`), for `udisksctl`.
pub fn device_for_mount(mountinfo: &str, mount: &Path) -> Option<String> {
    parse_mountinfo(mountinfo)
        .into_iter()
        .rev()
        .find(|entry| entry.mount == mount && entry.source.starts_with("/dev/"))
        .map(|entry| entry.source)
}

pub fn is_pseudo_fs(fs_type: &str) -> bool {
    PSEUDO_FS.contains(&fs_type)
}

/// Mounts a Linux desktop would show as drives: the root, and anything
/// that is not plumbing, boot partitions, snaps or containers.
pub fn linux_should_show(mount: &Path, fs_type: &str) -> bool {
    if is_pseudo_fs(fs_type) {
        return false;
    }
    if mount == Path::new("/") {
        return true;
    }
    let user_mount = ["/run/media", "/media", "/mnt"]
        .iter()
        .any(|base| mount.starts_with(base));
    user_mount || !HIDDEN_MOUNTS.iter().any(|hidden| mount.starts_with(hidden))
}

/// Where desktops mount hot-plugged disks.
pub fn linux_is_removable_mount(mount: &Path) -> bool {
    ["/run/media", "/media"]
        .iter()
        .any(|base| mount.starts_with(base))
}

/// Mounts Finder lists: the boot volume and user-visible `/Volumes` entries.
pub fn macos_should_show(mount: &Path) -> bool {
    if mount == Path::new("/") {
        return true;
    }
    mount.parent() == Some(Path::new("/Volumes"))
        && !mount
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("com.apple."))
}

pub fn kind_for(fs_type: &str, removable: bool) -> DriveKind {
    let fs_type = fs_type.to_ascii_lowercase();
    if NETWORK_FS.contains(&fs_type.as_str()) {
        DriveKind::Network
    } else if OPTICAL_FS.contains(&fs_type.as_str()) {
        DriveKind::Optical
    } else if removable {
        DriveKind::Removable
    } else {
        DriveKind::Fixed
    }
}

/// A sysinfo disk as a [`Drive`]; `name` is the label to fall back on when
/// the mount point has no last component of its own.
pub fn drive_from_disk(disk: &sysinfo::Disk, kind: DriveKind, root_name: &str) -> Drive {
    let mount = disk.mount_point().to_path_buf();
    let name = disk.name().to_string_lossy();
    let label = if mount == Path::new("/") {
        if name.is_empty() || name.starts_with("/dev/") {
            root_name.to_owned()
        } else {
            name.into_owned()
        }
    } else {
        mount.file_name().map_or_else(
            || kind.generic_name().to_owned(),
            |n| n.to_string_lossy().into_owned(),
        )
    };
    Drive {
        mount,
        label,
        kind,
        total: disk.total_space(),
        available: disk.available_space(),
        file_system: disk.file_system().to_string_lossy().into_owned(),
    }
}

/// Run an eject helper, turning a failure into its own error text.
pub fn run(program: &str, args: &[&std::ffi::OsStr]) -> anyhow::Result<()> {
    let output = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|err| anyhow::anyhow!("Could not run {program}: {err}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let message = stderr.trim();
    if message.is_empty() {
        anyhow::bail!("{program} failed ({})", output.status)
    }
    anyhow::bail!("{message}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
22 1 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw
25 22 0:22 / /proc rw,nosuid shared:5 - proc proc rw
40 22 8:1 / /boot/efi rw shared:30 - vfat /dev/sda1 rw
88 22 8:17 / /run/media/me/My\\040Stick rw,nosuid shared:50 - exfat /dev/sdb1 rw
90 22 0:50 / /mnt/share rw shared:52 - cifs //server/share rw
91 22 7:3 / /snap/core/1 ro shared:53 - squashfs /dev/loop3 ro";

    #[test]
    fn mountinfo_unescapes_spaces() {
        let entries = parse_mountinfo(SAMPLE);
        assert_eq!(entries.len(), 6);
        assert_eq!(entries[3].mount, Path::new("/run/media/me/My Stick"));
        assert_eq!(entries[3].fs_type, "exfat");
        assert_eq!(entries[3].source, "/dev/sdb1");
    }

    #[test]
    fn finds_device_of_a_mount() {
        assert_eq!(
            device_for_mount(SAMPLE, Path::new("/run/media/me/My Stick")).as_deref(),
            Some("/dev/sdb1")
        );
        assert_eq!(device_for_mount(SAMPLE, Path::new("/mnt/share")), None);
    }

    #[test]
    fn linux_filter_hides_plumbing() {
        let shown: Vec<PathBuf> = parse_mountinfo(SAMPLE)
            .into_iter()
            .filter(|e| linux_should_show(&e.mount, &e.fs_type))
            .map(|e| e.mount)
            .collect();
        assert_eq!(
            shown,
            vec![
                PathBuf::from("/"),
                PathBuf::from("/run/media/me/My Stick"),
                PathBuf::from("/mnt/share"),
            ]
        );
    }

    #[test]
    fn removable_mounts_and_kinds() {
        assert!(linux_is_removable_mount(Path::new("/run/media/me/X")));
        assert!(linux_is_removable_mount(Path::new("/media/X")));
        assert!(!linux_is_removable_mount(Path::new("/home")));
        assert_eq!(kind_for("cifs", false), DriveKind::Network);
        assert_eq!(kind_for("iso9660", true), DriveKind::Optical);
        assert_eq!(kind_for("exfat", true), DriveKind::Removable);
        assert_eq!(kind_for("ext4", false), DriveKind::Fixed);
    }

    #[test]
    fn macos_filter_shows_volumes_only() {
        assert!(macos_should_show(Path::new("/")));
        assert!(macos_should_show(Path::new("/Volumes/USB")));
        assert!(!macos_should_show(Path::new("/System/Volumes/Data")));
        assert!(!macos_should_show(Path::new(
            "/Volumes/com.apple.TimeMachine.localsnapshots"
        )));
        assert!(!macos_should_show(Path::new("/Volumes/USB/nested")));
    }
}
