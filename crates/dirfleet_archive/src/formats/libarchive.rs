//! The fallback for archives no native backend reads: libarchive, the
//! system's own copy (`/usr/lib/libarchive.2.dylib`, which also serves
//! `bsdtar` and Archive Utility). It adds formats such as LHA and WARC, and
//! variants a native backend reports as unsupported. Only the few functions
//! used are declared, so no headers or bundled library are needed.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

use super::{Backend, Job, Link, Listing, Sink};
use crate::archive::{Kind, Member};
use crate::error::{CoreError, Result};
use crate::names::{self, Encoding};
use crate::volumes::VolumeSet;

#[repr(C)]
struct RawArchive {
    _private: [u8; 0],
}

#[repr(C)]
struct RawEntry {
    _private: [u8; 0],
}

const OK: c_int = 0;
const EOF: c_int = 1;
const WARN: c_int = -20;
const IFMT: u32 = 0o170000;
const IFREG: u32 = 0o100000;
const IFDIR: u32 = 0o040000;
const IFLNK: u32 = 0o120000;

#[link(name = "archive")]
unsafe extern "C" {
    fn archive_read_new() -> *mut RawArchive;
    fn archive_read_support_filter_all(a: *mut RawArchive) -> c_int;
    fn archive_read_support_format_all(a: *mut RawArchive) -> c_int;
    fn archive_read_add_passphrase(a: *mut RawArchive, passphrase: *const c_char) -> c_int;
    fn archive_read_open_filenames(
        a: *mut RawArchive,
        filenames: *const *const c_char,
        block_size: usize,
    ) -> c_int;
    fn archive_read_next_header(a: *mut RawArchive, entry: *mut *mut RawEntry) -> c_int;
    fn archive_read_data(a: *mut RawArchive, buf: *mut c_void, size: usize) -> isize;
    fn archive_read_free(a: *mut RawArchive) -> c_int;
    fn archive_error_string(a: *mut RawArchive) -> *const c_char;
    fn archive_format_name(a: *mut RawArchive) -> *const c_char;
    fn archive_format(a: *mut RawArchive) -> c_int;
    fn archive_entry_pathname(e: *mut RawEntry) -> *const c_char;
    fn archive_entry_size(e: *mut RawEntry) -> i64;
    fn archive_entry_size_is_set(e: *mut RawEntry) -> c_int;
    fn archive_entry_mtime(e: *mut RawEntry) -> libc::time_t;
    fn archive_entry_mtime_is_set(e: *mut RawEntry) -> c_int;
    fn archive_entry_filetype(e: *mut RawEntry) -> libc::mode_t;
    fn archive_entry_perm(e: *mut RawEntry) -> libc::mode_t;
    fn archive_entry_symlink(e: *mut RawEntry) -> *const c_char;
    fn archive_entry_hardlink(e: *mut RawEntry) -> *const c_char;
    fn archive_entry_is_encrypted(e: *mut RawEntry) -> c_int;
}

/// An open libarchive reader, freed when dropped.
struct Reader {
    raw: *mut RawArchive,
}

impl Drop for Reader {
    fn drop(&mut self) {
        // SAFETY: `raw` came from archive_read_new and is freed only here.
        unsafe { archive_read_free(self.raw) };
    }
}

/// The current entry, valid until the next header is read.
struct Entry(*mut RawEntry);

fn string(ptr: *const c_char) -> Option<Vec<u8>> {
    // SAFETY: libarchive returns NUL-terminated strings or null.
    (!ptr.is_null()).then(|| unsafe { CStr::from_ptr(ptr) }.to_bytes().to_vec())
}

impl Reader {
    fn open(paths: &[PathBuf], password: Option<&[u8]>) -> Result<Self> {
        // SAFETY: archive_read_new has no preconditions; a null result is
        // an allocation failure.
        let raw = unsafe { archive_read_new() };
        if raw.is_null() {
            return Err(CoreError::Unsupported {
                message: "libarchive is unavailable".to_string(),
            });
        }
        let reader = Self { raw };
        let names: Vec<CString> = paths
            .iter()
            .filter_map(|p| CString::new(p.as_os_str().as_bytes()).ok())
            .collect();
        let mut pointers: Vec<*const c_char> = names.iter().map(|n| n.as_ptr()).collect();
        pointers.push(std::ptr::null());
        // SAFETY: `raw` is a fresh reader; the strings outlive the calls.
        unsafe {
            archive_read_support_filter_all(raw);
            archive_read_support_format_all(raw);
            if let Some(password) = password.and_then(|p| CString::new(p).ok()) {
                archive_read_add_passphrase(raw, password.as_ptr());
            }
            if archive_read_open_filenames(raw, pointers.as_ptr(), 64 << 10) != OK {
                return Err(reader.error());
            }
        }
        Ok(reader)
    }

    fn error(&self) -> CoreError {
        // SAFETY: `raw` is a live reader.
        let message = string(unsafe { archive_error_string(self.raw) })
            .map(|m| String::from_utf8_lossy(&m).into_owned())
            .unwrap_or_else(|| "unknown error".to_string());
        let lower = message.to_lowercase();
        if lower.contains("passphrase") && (lower.contains("incorrect") || lower.contains("wrong"))
        {
            CoreError::BadPassword
        } else if lower.contains("passphrase") || lower.contains("encrypted") {
            CoreError::PasswordRequired
        } else if lower.contains("unrecognized archive format") || lower.contains("not supported") {
            CoreError::Unsupported { message }
        } else {
            CoreError::Corrupt { message }
        }
    }

    fn next(&mut self) -> Result<Option<Entry>> {
        let mut entry = std::ptr::null_mut();
        // SAFETY: `raw` is a live reader and `entry` a valid out pointer.
        match unsafe { archive_read_next_header(self.raw, &mut entry) } {
            EOF => Ok(None),
            OK | WARN => Ok(Some(Entry(entry))),
            _ => Err(self.error()),
        }
    }

    /// The format's name; some readers leave it unset, so the format
    /// family's code names it then.
    fn format(&self) -> String {
        // SAFETY: `raw` is a live reader.
        if let Some(name) = string(unsafe { archive_format_name(self.raw) }) {
            return String::from_utf8_lossy(&name).into_owned();
        }
        // SAFETY: as above.
        let family = unsafe { archive_format(self.raw) } & 0xff_0000;
        match family {
            0x01_0000 => "cpio",
            0x03_0000 => "tar",
            0x04_0000 => "ISO 9660",
            0x05_0000 => "ZIP",
            0x07_0000 => "ar",
            0x08_0000 => "mtree",
            0x0a_0000 => "XAR",
            0x0b_0000 => "LHA",
            0x0c_0000 => "Cabinet",
            0x0d_0000 | 0x10_0000 => "RAR",
            0x0e_0000 => "7z",
            0x0f_0000 => "WARC",
            _ => "Archive",
        }
        .to_string()
    }

    /// Copies the current entry's data into `writer`, up to `size` bytes
    /// when the entry records its size: some readers (WARC) hand out the
    /// record's padding too. The rest is still read, which keeps those
    /// readers in step for the next header.
    fn copy(
        &mut self,
        writer: &mut dyn std::io::Write,
        size: Option<u64>,
        job: &Job<'_>,
    ) -> Result<()> {
        let mut buf = vec![0u8; 64 << 10];
        let mut left = size.unwrap_or(u64::MAX);
        loop {
            job.check_cancelled()?;
            // SAFETY: `buf` is valid for its length.
            let n = unsafe { archive_read_data(self.raw, buf.as_mut_ptr().cast(), buf.len()) };
            if n == 0 {
                return Ok(());
            }
            if n < 0 {
                return Err(self.error());
            }
            let keep = (n as u64).min(left) as usize;
            left -= keep as u64;
            writer.write_all(&buf[..keep]).map_err(|e| CoreError::Io {
                path: String::new(),
                message: e.to_string(),
            })?;
        }
    }
}

pub(crate) struct LibArchive {
    paths: Vec<PathBuf>,
}

pub(crate) fn open(
    volumes: VolumeSet,
    password: Option<&[u8]>,
    encoding: Encoding,
) -> Result<(LibArchive, Listing)> {
    let mut reader = Reader::open(&volumes.paths, password)?;
    let mut members = Vec::new();
    // libarchive names the format once it has read a header.
    let mut format = None;
    while let Some(Entry(entry)) = reader.next()? {
        format.get_or_insert_with(|| reader.format());
        // SAFETY: `entry` is the current entry of a live reader.
        let (raw, size, mtime, filetype, perm, encrypted) = unsafe {
            (
                string(archive_entry_pathname(entry)).unwrap_or_default(),
                archive_entry_size(entry).max(0) as u64,
                (archive_entry_mtime_is_set(entry) != 0).then(|| archive_entry_mtime(entry)),
                u32::from(archive_entry_filetype(entry)),
                u32::from(archive_entry_perm(entry)),
                archive_entry_is_encrypted(entry) > 0,
            )
        };
        let name = names::decode(&raw, encoding);
        // SAFETY: as above.
        let hardlink = unsafe { !archive_entry_hardlink(entry).is_null() };
        members.push(Member {
            rel_path: names::safe_relative_path(&name, false),
            display: String::from_utf8_lossy(&name).into_owned(),
            entry_id: None,
            kind: match filetype & IFMT {
                _ if hardlink => Kind::Link,
                IFDIR => Kind::Dir,
                IFREG => Kind::File,
                _ => Kind::Link,
            },
            size,
            packed_size: 0,
            mtime: mtime
                .filter(|&t| t >= 0)
                .map(|t| UNIX_EPOCH + Duration::from_secs(t as u64)),
            mode: Some(perm & 0o7777),
            encrypted,
            hidden: false,
        });
    }
    let packed = volumes
        .paths
        .iter()
        .filter_map(|p| p.metadata().ok())
        .map(|m| m.len())
        .sum();
    let listing = Listing {
        members,
        format: format.unwrap_or_else(|| reader.format()),
        volumes: volumes.paths.clone(),
        solid: true,
        comment: None,
        sizes_exact: true,
        packed_size: Some(packed),
    };
    Ok((
        LibArchive {
            paths: volumes.paths,
        },
        listing,
    ))
}

impl Backend for LibArchive {
    fn extract(&self, job: &Job<'_>, sink: &dyn Sink) -> Result<()> {
        let Some(last) = job.last_wanted() else {
            return Ok(());
        };
        let mut reader = Reader::open(&self.paths, job.password)?;
        let mut index = 0;
        while let Some(Entry(entry)) = reader.next()? {
            let current = index;
            index += 1;
            if current > last {
                break;
            }
            if !job.wanted[current] {
                continue;
            }
            job.check_cancelled()?;
            match job.members[current].kind {
                Kind::Dir => drop(sink.open(current)?),
                Kind::File => {
                    // SAFETY: `entry` is the current entry of a live reader.
                    let size = unsafe {
                        (archive_entry_size_is_set(entry) != 0)
                            .then(|| archive_entry_size(entry).max(0) as u64)
                    };
                    reader.copy(&mut sink.open(current)?, size, job)?;
                }
                Kind::Link => {
                    // SAFETY: `entry` is the current entry of a live reader.
                    let (symlink, hardlink) = unsafe {
                        (
                            string(archive_entry_symlink(entry)),
                            string(archive_entry_hardlink(entry)),
                        )
                    };
                    // SAFETY: as above.
                    let filetype = u32::from(unsafe { archive_entry_filetype(entry) });
                    let link = match (&hardlink, &symlink) {
                        (Some(target), _) => Link::Copy(target),
                        (None, Some(target)) if filetype & IFMT == IFLNK => Link::Symlink(target),
                        _ => Link::Unsupported,
                    };
                    sink.link(current, link)?;
                }
            }
        }
        Ok(())
    }
}
