//! macOS specifics: the quarantine attribute, case sensitivity and process liveness.

use std::ffi::{CString, OsStr};
use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) use std::fs::{remove_file, rename};

const QUARANTINE: &std::ffi::CStr = c"com.apple.quarantine";

fn c_path(path: &Path) -> Option<CString> {
    CString::new(path.as_os_str().as_bytes()).ok()
}

/// The `com.apple.quarantine` value of `path`, set on files downloaded by
/// browsers and mail clients.
pub(crate) fn quarantine(path: &Path) -> Option<Vec<u8>> {
    let path = c_path(path)?;
    // SAFETY: `path` and `QUARANTINE` are NUL-terminated; a null buffer asks for the size.
    let size = unsafe {
        libc::getxattr(
            path.as_ptr(),
            QUARANTINE.as_ptr(),
            std::ptr::null_mut(),
            0,
            0,
            0,
        )
    };
    if size <= 0 {
        return None;
    }
    let mut value = vec![0u8; size as usize];
    // SAFETY: `value` has room for `size` bytes.
    let read = unsafe {
        libc::getxattr(
            path.as_ptr(),
            QUARANTINE.as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
            0,
            0,
        )
    };
    (read > 0).then(|| {
        value.truncate(read as usize);
        value
    })
}

/// Sets the quarantine attribute on `path` itself, not on a link's target.
/// Best effort: an item that can't take the attribute is still extracted.
pub(crate) fn set_quarantine(path: &Path, value: &[u8]) {
    if let Some(c) = c_path(path) {
        // SAFETY: `c` and `QUARANTINE` are NUL-terminated and `value` is valid for its length.
        unsafe {
            libc::setxattr(
                c.as_ptr(),
                QUARANTINE.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
                libc::XATTR_NOFOLLOW,
            );
        }
    }
}

/// Sets extended attribute `name` on `path` itself. Best effort, like the
/// rest of an archive's metadata.
pub(crate) fn set_xattr(path: &Path, name: &str, value: &[u8]) {
    let (Some(path), Ok(name)) = (c_path(path), CString::new(name)) else {
        return;
    };
    // SAFETY: both strings are NUL-terminated and `value` is valid for its length.
    unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            libc::XATTR_NOFOLLOW,
        );
    }
}

/// The extended attributes of `path` itself, except the quarantine and
/// provenance markers, which the extraction sets on its own.
pub(crate) fn xattrs(path: &Path) -> Vec<(String, Vec<u8>)> {
    let Some(c) = c_path(path) else {
        return Vec::new();
    };
    // SAFETY: `c` is NUL-terminated; a null buffer asks for the size.
    let size =
        unsafe { libc::listxattr(c.as_ptr(), std::ptr::null_mut(), 0, libc::XATTR_NOFOLLOW) };
    if size <= 0 {
        return Vec::new();
    }
    let mut names = vec![0u8; size as usize];
    // SAFETY: `names` has room for `size` bytes.
    let size = unsafe {
        libc::listxattr(
            c.as_ptr(),
            names.as_mut_ptr().cast(),
            names.len(),
            libc::XATTR_NOFOLLOW,
        )
    };
    if size <= 0 {
        return Vec::new();
    }
    names.truncate(size as usize);
    names
        .split(|&b| b == 0)
        .filter(|name| !name.is_empty())
        .filter(|name| *name != b"com.apple.quarantine" && *name != b"com.apple.provenance")
        .filter_map(|name| {
            let name_c = CString::new(name).ok()?;
            // SAFETY: both strings are NUL-terminated; a null buffer asks for the size.
            let len = unsafe {
                libc::getxattr(
                    c.as_ptr(),
                    name_c.as_ptr(),
                    std::ptr::null_mut(),
                    0,
                    0,
                    libc::XATTR_NOFOLLOW,
                )
            };
            if len < 0 {
                return None;
            }
            let mut value = vec![0u8; len as usize];
            // SAFETY: `value` has room for `len` bytes.
            let read = unsafe {
                libc::getxattr(
                    c.as_ptr(),
                    name_c.as_ptr(),
                    value.as_mut_ptr().cast(),
                    value.len(),
                    0,
                    libc::XATTR_NOFOLLOW,
                )
            };
            (read >= 0).then(|| {
                value.truncate(read as usize);
                (String::from_utf8_lossy(name).into_owned(), value)
            })
        })
        .collect()
}

/// Sets the quarantine attribute on an open file, without a path lookup.
pub(crate) fn set_file_quarantine(file: &fs::File, _path: &Path, value: &[u8]) {
    // SAFETY: the descriptor is open for the call and `value` is valid for its length.
    unsafe {
        libc::fsetxattr(
            file.as_raw_fd(),
            QUARANTINE.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            0,
        );
    }
}

/// Whether names that differ only in case are different files in `dir`.
pub(crate) fn case_sensitive(dir: &Path) -> bool {
    let Some(c) = c_path(dir) else {
        return false;
    };
    // SAFETY: `c` is NUL-terminated.
    unsafe { libc::pathconf(c.as_ptr(), libc::_PC_CASE_SENSITIVE) == 1 }
}

/// Whether a process with `pid` exists.
pub(crate) fn process_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks for existence and permission.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Reads from `file` at `offset` without moving its position.
pub(crate) fn read_at(file: &fs::File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    file.read_at(buf, offset)
}

pub(crate) fn set_file_mode(file: &fs::File, mode: u32) {
    let _ = file.set_permissions(fs::Permissions::from_mode(mode));
}

pub(crate) fn set_dir_mode(path: &Path, mode: u32) {
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

/// Folders restored with read-only modes can't be emptied; open them back up.
pub(crate) fn make_writable(dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o700))?;
            make_writable(&entry.path())?;
        }
    }
    Ok(())
}

pub(crate) fn set_dir_mtime(path: &Path, mtime: SystemTime) -> io::Result<()> {
    fs::File::open(path)?.set_times(fs::FileTimes::new().set_modified(mtime))
}

/// A stored link target or member name as a path.
pub(crate) fn raw_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsStr::from_bytes(bytes))
}

/// Creates a symbolic link at `dest` to `target`, as stored.
pub(crate) fn symlink(target: &[u8], dest: &Path) -> bool {
    std::os::unix::fs::symlink(OsStr::from_bytes(target), dest).is_ok()
}

/// A local date and time, as archives from DOS and Windows store them, as
/// an instant.
pub(crate) fn local_time(
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
) -> Option<SystemTime> {
    // SAFETY: an all-zero `tm` is a valid value to fill in.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = i32::from(month) - 1;
    tm.tm_mday = i32::from(day);
    tm.tm_hour = i32::from(hour);
    tm.tm_min = i32::from(minute);
    tm.tm_sec = i32::from(second);
    tm.tm_isdst = -1;
    // SAFETY: `tm` is initialized; mktime only reads and normalizes it.
    let seconds = unsafe { libc::mktime(&mut tm) };
    (seconds >= 0).then(|| UNIX_EPOCH + Duration::from_secs(seconds as u64))
}

/// A UTC date and time as an instant.
pub(crate) fn utc_time(
    year: i32,
    month: i32,
    day: i32,
    hour: i32,
    minute: i32,
    second: i32,
) -> Option<SystemTime> {
    // SAFETY: an all-zero `tm` is a valid value to fill in.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = month - 1;
    tm.tm_mday = day;
    tm.tm_hour = hour;
    tm.tm_min = minute;
    tm.tm_sec = second;
    // SAFETY: `tm` is initialized; timegm only reads and normalizes it.
    let seconds = unsafe { libc::timegm(&mut tm) };
    (seconds >= 0).then(|| UNIX_EPOCH + Duration::from_secs(seconds as u64))
}
