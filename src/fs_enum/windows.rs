//! `NtQueryDirectoryFile`-based enumeration (see the parent module).
#![allow(unsafe_code)]

use std::{
    cell::RefCell,
    fs::File,
    io,
    mem::offset_of,
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::Path,
    ptr, slice,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use windows::{
    Wdk::Storage::FileSystem::{
        FILE_ID_BOTH_DIR_INFORMATION, FILE_ID_EXTD_DIR_INFORMATION, FILE_INFORMATION_CLASS,
        FileIdBothDirectoryInformation, FileIdExtdDirectoryInformation, NtQueryDirectoryFile,
    },
    Win32::{
        Foundation::{
            HANDLE, NTSTATUS, STATUS_INVALID_INFO_CLASS, STATUS_INVALID_PARAMETER,
            STATUS_NO_MORE_FILES, STATUS_NO_SUCH_FILE, STATUS_NOT_SUPPORTED,
        },
        Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE,
        },
        System::IO::IO_STATUS_BLOCK,
    },
};

use super::{ATTR_REPARSE_POINT, EntryName, RawEntry};

/// Bytes per call; the kernel fills as many whole records as fit.
const BUFFER_BYTES: usize = 64 * 1024;
/// 100 ns ticks between 1601-01-01 and 1970-01-01.
const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;

thread_local! {
    /// Reused across calls on the same thread; `u64` keeps it 8-byte aligned
    /// as the `FILE_*_DIR_INFORMATION` records require.
    static BUFFER: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn enumerate(dir: &Path, sink: &mut dyn FnMut(&RawEntry<'_>)) -> io::Result<()> {
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_LIST_DIRECTORY.0)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(dir)?;
    BUFFER.with(|cell| match cell.try_borrow_mut() {
        Ok(mut buffer) => read_all(&file, &mut buffer, sink),
        // Re-entered from inside a sink callback: use a private buffer.
        Err(_) => read_all(&file, &mut Vec::new(), sink),
    })
}

fn read_all(
    file: &File,
    buffer: &mut Vec<u64>,
    sink: &mut dyn FnMut(&RawEntry<'_>),
) -> io::Result<()> {
    buffer.resize(BUFFER_BYTES / 8, 0);
    let handle = HANDLE(file.as_raw_handle());
    let mut class = FileIdExtdDirectoryInformation;
    let mut restart = true;
    loop {
        let mut iosb = IO_STATUS_BLOCK::default();
        // SAFETY: `handle` is an open directory handle owned by `file`, which
        // outlives the call; `buffer` holds `BUFFER_BYTES` writable bytes with
        // 8-byte alignment; the call is synchronous (no event or APC), so the
        // status block and buffer are not touched after it returns.
        let status = unsafe {
            NtQueryDirectoryFile(
                handle,
                None,
                None,
                None,
                &raw mut iosb,
                buffer.as_mut_ptr().cast(),
                BUFFER_BYTES as u32,
                class,
                false,
                None,
                restart,
            )
        };
        if status == STATUS_NO_MORE_FILES || (restart && status == STATUS_NO_SUCH_FILE) {
            return Ok(());
        }
        if restart
            && class == FileIdExtdDirectoryInformation
            && (status == STATUS_INVALID_INFO_CLASS
                || status == STATUS_NOT_SUPPORTED
                || status == STATUS_INVALID_PARAMETER)
        {
            // Older redirectors and some filter drivers only know the classic
            // class; it lacks the reparse tag field but carries it in `EaSize`.
            class = FileIdBothDirectoryInformation;
            continue;
        }
        if status.0 < 0 {
            return Err(nt_error(status));
        }
        restart = false;
        let written = iosb.Information.min(BUFFER_BYTES);
        // SAFETY: the kernel wrote `written` bytes of `class` records into
        // `buffer` (`BUFFER_BYTES` long, zero-initialised), which stays alive
        // and untouched while they are walked.
        unsafe { walk_records(buffer.as_ptr().cast::<u8>(), written, class, sink) };
    }
}

fn nt_error(status: NTSTATUS) -> io::Error {
    io::Error::other(format!(
        "NtQueryDirectoryFile failed with NTSTATUS {:#010x}",
        status.0 as u32
    ))
}

/// # Safety
/// `base` must point at `BUFFER_BYTES` initialised bytes whose first `len`
/// bytes are records of `class`.
unsafe fn walk_records(
    base: *const u8,
    len: usize,
    class: FILE_INFORMATION_CLASS,
    sink: &mut dyn FnMut(&RawEntry<'_>),
) {
    let mut offset = 0usize;
    loop {
        // SAFETY: `offset` stays within `len` (checked per record) and every
        // record starts 8-byte aligned, as the kernel lays them out.
        let next = unsafe {
            if class == FileIdExtdDirectoryInformation {
                record::<FILE_ID_EXTD_DIR_INFORMATION>(base, offset, len, sink, extd_fields)
            } else {
                record::<FILE_ID_BOTH_DIR_INFORMATION>(base, offset, len, sink, both_fields)
            }
        };
        match next {
            Some(next) if next > offset => offset = next,
            _ => return,
        }
    }
}

/// Fields shared by both record layouts.
struct Fields {
    next_entry_offset: u32,
    attributes: u32,
    reparse_tag: u32,
    size: u64,
    created: i64,
    modified: i64,
    name_len_bytes: usize,
    file_id: u64,
    name_offset: usize,
}

fn extd_fields(record: &FILE_ID_EXTD_DIR_INFORMATION) -> Fields {
    let mut id = [0u8; 8];
    id.copy_from_slice(&record.FileId.Identifier[..8]);
    Fields {
        next_entry_offset: record.NextEntryOffset,
        attributes: record.FileAttributes,
        reparse_tag: if record.FileAttributes & ATTR_REPARSE_POINT == 0 {
            0
        } else {
            record.ReparsePointTag
        },
        size: record.EndOfFile.max(0) as u64,
        created: record.CreationTime,
        modified: record.LastWriteTime,
        name_len_bytes: record.FileNameLength as usize,
        file_id: u64::from_le_bytes(id),
        name_offset: offset_of!(FILE_ID_EXTD_DIR_INFORMATION, FileName),
    }
}

fn both_fields(record: &FILE_ID_BOTH_DIR_INFORMATION) -> Fields {
    Fields {
        next_entry_offset: record.NextEntryOffset,
        attributes: record.FileAttributes,
        // `FindFirstFile` reports the tag from this field (`dwReserved0`).
        reparse_tag: if record.FileAttributes & ATTR_REPARSE_POINT == 0 {
            0
        } else {
            record.EaSize
        },
        size: record.EndOfFile.max(0) as u64,
        created: record.CreationTime,
        modified: record.LastWriteTime,
        name_len_bytes: record.FileNameLength as usize,
        file_id: record.FileId as u64,
        name_offset: offset_of!(FILE_ID_BOTH_DIR_INFORMATION, FileName),
    }
}

/// Reads one record at `offset`, feeds it to `sink`, and returns the offset of
/// the next record (or `None` at the end of the buffer).
///
/// # Safety
/// `base..base + BUFFER_BYTES` must be initialised, of which `..len` is
/// record data, and `offset` must be 8-aligned.
unsafe fn record<T: Copy>(
    base: *const u8,
    offset: usize,
    len: usize,
    sink: &mut dyn FnMut(&RawEntry<'_>),
    fields: fn(&T) -> Fields,
) -> Option<usize> {
    // A record ends right after its name, so the last one in a buffer can be
    // shorter than `size_of::<T>()` (whose trailing `[u16; 1]` and padding
    // overlap the name). Its fixed fields must lie within the data, and the
    // `size_of::<T>()` read must stay inside the (initialised) allocation.
    if offset >= len || offset.checked_add(size_of::<T>())? > BUFFER_BYTES {
        return None;
    }
    // SAFETY: `offset + size_of::<T>()` is within the initialised buffer
    // (checked above); `read_unaligned` makes no alignment assumption. Bytes
    // past `len` are stale or zero and only ever land in the name, which is
    // bounds-checked against `len` below before use.
    let header: T = unsafe { ptr::read_unaligned(base.add(offset).cast::<T>()) };
    let fields = fields(&header);
    if offset + fields.name_offset > len {
        return None;
    }
    let name_units = fields.name_len_bytes / 2;
    let name_end = offset + fields.name_offset + name_units * 2;
    let next = (fields.next_entry_offset != 0).then(|| offset + fields.next_entry_offset as usize);
    if name_end > len {
        return next;
    }
    // SAFETY: `name_end <= len` and the name starts at an even offset of an
    // 8-aligned record, so the `u16` slice is aligned and in bounds.
    #[allow(clippy::cast_ptr_alignment)]
    let name = unsafe {
        slice::from_raw_parts(
            base.add(offset + fields.name_offset).cast::<u16>(),
            name_units,
        )
    };
    let dot = u16::from(b'.');
    let is_dot = name == [dot] || name == [dot, dot];
    if !is_dot && !name.is_empty() {
        let entry = RawEntry {
            name: EntryName::Utf16(name),
            attributes: fields.attributes,
            reparse_tag: fields.reparse_tag,
            size: fields.size,
            created: filetime(fields.created),
            modified: filetime(fields.modified),
            file_id: fields.file_id,
        };
        sink(&entry);
    }
    next
}

fn filetime(ticks: i64) -> Option<SystemTime> {
    let since_epoch = ticks.checked_sub(FILETIME_UNIX_EPOCH)?;
    if since_epoch < 0 {
        return None;
    }
    UNIX_EPOCH.checked_add(Duration::from_nanos(since_epoch as u64 * 100))
}
