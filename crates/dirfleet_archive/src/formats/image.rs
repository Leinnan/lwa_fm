//! Disk images (`.dmg`, `.iso`, `.cdr`, `.sparseimage`), attached read-only
//! with `hdiutil`, which reads every image format and file system macOS
//! does: UDIF with any compression, APFS, HFS+, ISO 9660, UDF, encrypted
//! images. The mounted volume is listed like a folder and detached again
//! when the archive is closed. Images stay attached if the process dies, so
//! each process mounts under a folder of its own, and the first image a
//! process opens detaches those of processes that are gone.

use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rayon::prelude::*;

use super::{Backend, Job, Link, Listing, Sink, copy};
use crate::archive::{Kind, Member};
use crate::error::{CoreError, Result};
use crate::platform;
use crate::volumes::VolumeSet;

const HDIUTIL: &str = "/usr/bin/hdiutil";

/// File system bookkeeping at a volume's root, never part of its contents.
const SYSTEM_ITEMS: &[&str] = &[
    ".Trashes",
    ".fseventsd",
    ".Spotlight-V100",
    ".DocumentRevisions-V100",
    ".TemporaryItems",
    ".HFS+ Private Directory Data\r",
    "\0\0\0\0HFS+ Private Data",
];

pub(crate) struct Image {
    /// The whole disk, which detaching takes down with its volumes.
    device: String,
    /// Each member's path on the mounted volumes.
    paths: Vec<PathBuf>,
}

fn run(args: &[&str], input: Option<&[u8]>) -> std::io::Result<std::process::Output> {
    let mut child = Command::new(HDIUTIL)
        .args(args)
        // A license agreement is paged to stdout; answer it without a pager.
        .env("PAGER", "/bin/cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.unwrap_or(b""));
    }
    child.wait_with_output()
}

/// Values of `key` in hdiutil's plist output, in order.
fn plist_strings(plist: &str, key: &str) -> Vec<String> {
    let tag = format!("<key>{key}</key>");
    plist
        .match_indices(&tag)
        .filter_map(|(at, _)| {
            let rest = plist[at + tag.len()..].trim_start();
            let value = rest.strip_prefix("<string>")?;
            Some(value[..value.find("</string>")?].to_string())
        })
        .collect()
}

/// Where this process mounts images: a folder per process ID.
fn mount_root(pid: u32) -> PathBuf {
    std::env::temp_dir()
        .join("uunpack-images")
        .join(pid.to_string())
}

/// Detaches images that processes no longer running left attached.
fn detach_stale() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let Ok(output) = run(&["info", "-plist"], None) else {
            return;
        };
        let plist = String::from_utf8_lossy(&output.stdout);
        let parent = std::env::temp_dir().join("uunpack-images");
        for mount in plist_strings(&plist, "mount-point") {
            let Ok(rel) = Path::new(&mount).strip_prefix(&parent) else {
                continue;
            };
            let pid = rel
                .components()
                .next()
                .and_then(|c| c.as_os_str().to_str()?.parse::<i32>().ok());
            if let Some(pid) = pid
                && pid != std::process::id() as i32
                && !platform::process_alive(pid)
            {
                let _ = run(&["detach", &mount, "-force", "-quiet"], None);
            }
        }
        if let Ok(entries) = fs::read_dir(&parent) {
            for entry in entries.flatten() {
                let _ = fs::remove_dir(entry.path());
            }
        }
    });
}

pub(crate) fn open(volumes: VolumeSet, password: Option<&[u8]>) -> Result<(Image, Listing)> {
    detach_stale();
    let path = volumes.paths[0].to_string_lossy().into_owned();
    let image_error = |message: String| CoreError::Io {
        path: path.clone(),
        message,
    };
    let encrypted = run(&["isencrypted", &path], None)
        .map(|out| String::from_utf8_lossy(&out.stdout).contains("encrypted: YES"))
        .unwrap_or(false);
    if encrypted && password.is_none() {
        return Err(CoreError::PasswordRequired);
    }
    let mount_root = mount_root(std::process::id());
    fs::create_dir_all(&mount_root).map_err(|e| CoreError::io(&mount_root, e))?;
    let mount_root = mount_root.to_string_lossy();
    let mut args = vec![
        "attach",
        "-readonly",
        "-nobrowse",
        "-noautoopen",
        "-noverify",
        "-mountrandom",
        &mount_root,
        "-plist",
    ];
    let mut input = b"Y\n".to_vec();
    if encrypted {
        args.push("-stdinpass");
        input = password.unwrap_or_default().to_vec();
    }
    args.push(&path);
    let output = run(&args, Some(&input)).map_err(|e| image_error(e.to_string()))?;
    let plist = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.status.success() {
        // hdiutil's messages are localized, so they can't be told apart;
        // with the password given, a wrong one is by far the likeliest cause.
        if encrypted {
            return Err(CoreError::BadPassword);
        }
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(image_error(message));
    }
    let devices = plist_strings(&plist, "dev-entry");
    let device = devices.first().cloned().unwrap_or_default();
    let mounts: Vec<PathBuf> = plist_strings(&plist, "mount-point")
        .into_iter()
        .map(PathBuf::from)
        .collect();
    let mut image = Image {
        device,
        paths: Vec::new(),
    };
    if mounts.is_empty() {
        return Err(CoreError::Unsupported {
            message: "this disk image has no volume macOS can read".to_string(),
        });
    }

    let mut members = Vec::new();
    for (number, mount) in mounts.iter().enumerate() {
        // Several volumes each get a folder of their own.
        let prefix = if mounts.len() > 1 {
            PathBuf::from(format!("Volume {}", number + 1))
        } else {
            PathBuf::new()
        };
        walk(mount, mount, &prefix, &mut members, &mut image.paths)?;
    }
    let format = match volumes.paths[0]
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("iso") | Some("cdr") | Some("toast") => "Disc image",
        _ => "Disk image",
    };
    let packed = fs::metadata(&volumes.paths[0]).map(|m| m.len()).ok();
    let listing = Listing {
        members,
        volumes: volumes.paths,
        format: format.to_string(),
        solid: false,
        comment: None,
        sizes_exact: true,
        packed_size: packed,
    };
    Ok((image, listing))
}

fn walk(
    root: &Path,
    dir: &Path,
    prefix: &Path,
    members: &mut Vec<Member>,
    paths: &mut Vec<PathBuf>,
) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .map_err(|e| CoreError::io(dir, e))?
        .flatten()
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if dir == root && SYSTEM_ITEMS.iter().any(|item| entry.file_name() == *item) {
            continue;
        }
        let Ok(meta) = path.symlink_metadata() else {
            continue;
        };
        let rel = prefix.join(path.strip_prefix(root).unwrap_or(&path));
        let kind = if meta.is_dir() {
            Kind::Dir
        } else if meta.is_file() {
            Kind::File
        } else {
            Kind::Link
        };
        members.push(Member {
            display: rel.to_string_lossy().into_owned(),
            rel_path: Some(rel),
            entry_id: None,
            kind,
            size: if kind == Kind::File { meta.len() } else { 0 },
            packed_size: 0,
            mtime: meta.modified().ok(),
            mode: Some(meta.permissions().mode() & 0o7777),
            encrypted: false,
            hidden: false,
        });
        paths.push(path.clone());
        if kind == Kind::Dir {
            walk(root, &path, prefix, members, paths)?;
        }
    }
    Ok(())
}

impl Backend for Image {
    fn extract(&self, job: &Job<'_>, sink: &dyn Sink) -> Result<()> {
        let wanted: Vec<usize> = (0..self.paths.len()).filter(|&i| job.wanted[i]).collect();
        wanted.par_iter().try_for_each(|&index| {
            job.check_cancelled()?;
            let path = &self.paths[index];
            match job.members[index].kind {
                Kind::Dir => drop(sink.open(index)?),
                Kind::Link => match fs::read_link(path) {
                    Ok(target) => sink.link(index, Link::Symlink(target.as_os_str().as_bytes()))?,
                    Err(_) => sink.link(index, Link::Unsupported)?,
                },
                Kind::File => {
                    let mut file = fs::File::open(path).map_err(|e| CoreError::io(path, e))?;
                    copy(&mut file, &mut sink.open(index)?, job, false)?;
                }
            }
            let attributes = platform::xattrs(path);
            if !attributes.is_empty() {
                sink.xattrs(index, attributes);
            }
            Ok(())
        })
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        if self.device.is_empty() {
            return;
        }
        let detached =
            run(&["detach", &self.device, "-quiet"], None).is_ok_and(|out| out.status.success());
        if !detached {
            let _ = run(&["detach", &self.device, "-force", "-quiet"], None);
        }
    }
}
