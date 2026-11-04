//! `getattrlistbulk`: a folder's file details in batches, for the walk in
//! [`super::usage`]. One folder of a Cargo `target` can hold 800,000 files, so
//! a system call per file is most of a scan's time; this takes 2 to 4 times
//! less on that folder. Only folders still get an `lstat`: they are few, and
//! it keeps their sizes and filesystem check the same as the plain walk.

use super::usage::{LinkedFiles, Options, Usage, first_link, freed};
use std::ffi::OsStr;
use std::os::fd::AsRawFd as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const ATTR_CMN_NAME: u32 = 0x0000_0001;
const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
const ATTR_CMN_MODTIME: u32 = 0x0000_0400;
const ATTR_CMN_FILEID: u32 = 0x0200_0000;
const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
const ATTR_FILE_LINKCOUNT: u32 = 0x0000_0001;
const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;
const COMMON: u32 =
    ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME | ATTR_CMN_OBJTYPE | ATTR_CMN_MODTIME | ATTR_CMN_FILEID;
const FILE: u32 = ATTR_FILE_LINKCOUNT | ATTR_FILE_ALLOCSIZE;
const VREG: u32 = 1;
const VDIR: u32 = 2;
const BUFFER_BYTES: usize = 256 * 1024;

/// One entry of a batch.
#[derive(Debug, PartialEq, Eq)]
struct Entry<'a> {
    name: &'a [u8],
    kind: u32,
    modified: i64,
    inode: u64,
    /// Absent for folders.
    links: u64,
    bytes: u64,
}

fn u32_at(record: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(record.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(record: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_ne_bytes(record.get(at..at + 8)?.try_into().ok()?))
}

/// Reads a record: its length, which attributes it has (five `u32`s), then
/// those attributes packed in a fixed order. `None` when it lacks any we need.
fn parse(record: &[u8]) -> Option<Entry<'_>> {
    let (common, file) = (u32_at(record, 4)?, u32_at(record, 16)?);
    if common & COMMON != COMMON {
        return None;
    }
    // The name is stored as an offset from this very field, and a length
    // that counts the final NUL.
    let name_field = 24;
    let offset = usize::try_from(i32::from_ne_bytes(
        record.get(name_field..name_field + 4)?.try_into().ok()?,
    ))
    .ok()?;
    let length = u32_at(record, name_field + 4)? as usize;
    let start = name_field + offset;
    let name = record.get(start..start + length.checked_sub(1)?)?;
    let kind = u32_at(record, 32)?;
    let modified = u64_at(record, 36)?.cast_signed();
    let inode = u64_at(record, 52)?;
    let (links, bytes) = if file & FILE == FILE {
        (u64::from(u32_at(record, 60)?), u64_at(record, 64)?)
    } else if kind == VREG {
        return None;
    } else {
        (1, 0)
    };
    Some(Entry {
        name,
        kind,
        modified,
        inode,
        links,
        bytes,
    })
}

/// Why a folder gave no batch.
enum ReadError {
    /// This filesystem doesn't do batches: the plain walk has to.
    Unsupported,
    Other,
}

/// Fills `buffer` with the next records of `folder`: how many came.
fn next_batch(folder: &std::fs::File, buffer: &mut [u8]) -> Result<usize, ReadError> {
    let mut wanted = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: COMMON,
        volattr: 0,
        dirattr: 0,
        fileattr: FILE,
        forkattr: 0,
    };
    // SAFETY: the descriptor is open for the call, `wanted` lives through it,
    // and the kernel writes at most `buffer.len()` bytes into `buffer`.
    #[allow(unsafe_code)]
    let count = unsafe {
        libc::getattrlistbulk(
            folder.as_raw_fd(),
            (&raw mut wanted).cast(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            0,
        )
    };
    usize::try_from(count).map_err(|_| match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ENOTSUP | libc::EINVAL) => ReadError::Unsupported,
        _ => ReadError::Other,
    })
}

/// A folder waiting to be read.
struct Pending {
    path: PathBuf,
    /// Inside a folder whose files don't count towards `newest`.
    skipping: bool,
    /// Inside this one of the root's subtotals.
    part: Option<String>,
}

/// The walk, or `None` when the plain one should do it: `root` isn't a folder
/// we can read in batches. A missing root is also left to it, for its error.
pub fn measure(
    root: &Path,
    options: &Options,
    cancel: &AtomicBool,
) -> Option<std::io::Result<Option<Usage>>> {
    let metadata = std::fs::symlink_metadata(root).ok()?;
    if !metadata.is_dir() {
        return None;
    }
    let mut usage = Usage {
        bytes: metadata.blocks() * 512,
        ..Default::default()
    };
    let device = metadata.dev();
    let mut linked = LinkedFiles::new();
    let mut buffer = vec![0u8; BUFFER_BYTES];
    let mut pending = vec![Pending {
        path: root.to_path_buf(),
        skipping: false,
        part: None,
    }];
    while let Some(folder) = pending.pop() {
        if cancel.load(Ordering::Acquire) {
            return Some(Ok(None));
        }
        let Ok(handle) = std::fs::File::open(&folder.path) else {
            usage.errors += 1;
            continue;
        };
        let depth_one = folder.path == root;
        loop {
            if cancel.load(Ordering::Acquire) {
                return Some(Ok(None));
            }
            let count = match next_batch(&handle, &mut buffer) {
                Ok(0) => break,
                Ok(count) => count,
                Err(ReadError::Unsupported) => return None,
                Err(ReadError::Other) => {
                    usage.errors += 1;
                    break;
                }
            };
            let mut at = 0;
            for _ in 0..count {
                let Some(length) = u32_at(&buffer, at).map(|length| length as usize) else {
                    usage.errors += 1;
                    break;
                };
                let Some(entry) = buffer.get(at..at + length).and_then(parse) else {
                    usage.errors += 1;
                    at += length.max(4);
                    continue;
                };
                at += length;
                add_entry(
                    &entry,
                    &folder,
                    depth_one,
                    device,
                    options,
                    &mut usage,
                    &mut linked,
                    &mut pending,
                );
            }
        }
    }
    usage.freed = freed(usage.bytes, &linked);
    Some(Ok(Some(usage)))
}

/// Counts an entry, and queues it if it is a folder to go into.
#[allow(clippy::too_many_arguments)]
fn add_entry(
    entry: &Entry<'_>,
    folder: &Pending,
    depth_one: bool,
    device: u64,
    options: &Options,
    usage: &mut Usage,
    linked: &mut LinkedFiles,
    pending: &mut Vec<Pending>,
) {
    let name = || OsStr::from_bytes(entry.name).to_str();
    let part = if depth_one {
        options
            .subtotals
            .iter()
            .find(|subtotal| entry.kind == VDIR && Some(subtotal.as_str()) == name())
            .cloned()
    } else {
        folder.part.clone()
    };
    if entry.kind == VDIR {
        let path = folder.path.join(OsStr::from_bytes(entry.name));
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            usage.errors += 1;
            return;
        };
        let bytes = metadata.blocks() * 512;
        add_bytes(usage, part.as_deref(), bytes);
        if metadata.dev() == device {
            let skipped = options
                .newest_skip
                .iter()
                .any(|skipped| Some(skipped.as_str()) == name());
            pending.push(Pending {
                path,
                skipping: folder.skipping || skipped,
                part,
            });
        }
        return;
    }
    if entry.kind == VREG
        && entry.links > 1
        && !first_link(linked, (device, entry.inode), entry.links, entry.bytes)
    {
        return;
    }
    add_bytes(usage, part.as_deref(), entry.bytes);
    usage.files += 1;
    if entry.kind == VREG && !folder.skipping {
        usage.newest = Some(
            usage
                .newest
                .map_or(entry.modified, |newest| newest.max(entry.modified)),
        );
    }
}

fn add_bytes(usage: &mut Usage, part: Option<&str>, bytes: u64) {
    usage.bytes += bytes;
    if let Some(part) = part {
        *usage.parts.entry(part.to_owned()).or_default() += bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_or_incomplete_records_are_refused() {
        assert_eq!(parse(&[]), None);
        assert_eq!(parse(&[0; 12]), None);
        // Claims every attribute, but stops before them.
        let mut record = vec![0u8; 40];
        record[4..8].copy_from_slice(&COMMON.to_ne_bytes());
        assert_eq!(parse(&record), None);
    }
}
