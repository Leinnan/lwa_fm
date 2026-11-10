//! Windows file clipboard: `CF_HDROP` with the `Preferred DropEffect` flag
//! that tells Explorer (and us) whether the files were cut or copied.
#![allow(unsafe_code)]

use super::Mode;
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::UI::Shell::{DROPFILES, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
};
use windows::core::w;

/// `CF_HDROP` from `Win32::System::Ole`, which is otherwise not needed.
const CF_HDROP: u32 = 15;
const DROPEFFECT_COPY: u32 = 1;
const DROPEFFECT_MOVE: u32 = 2;

fn drop_effect_format() -> u32 {
    // SAFETY: registering a format name has no preconditions.
    unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) }
}

thread_local! {
    /// A hidden message-only window that owns the clipboard while we write.
    /// With no owner, `EmptyClipboard` leaves the clipboard ownerless and
    /// later formats can be dropped.
    static OWNER: Option<HWND> = {
        // SAFETY: creates a message-only window of the built-in STATIC class;
        // it lives as long as the thread.
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("DirFleet clipboard"),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            )
            .ok()
        }
    };
}

/// Holds the clipboard open; closes it on drop.
struct OpenedClipboard;

impl OpenedClipboard {
    fn open() -> Option<Self> {
        let owner = OWNER.with(|owner| *owner);
        // Another application may hold the clipboard for a moment.
        for _ in 0..10 {
            // SAFETY: `owner` is a window of this thread (or none).
            if unsafe { OpenClipboard(owner) }.is_ok() {
                return Some(Self);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        None
    }
}

impl Drop for OpenedClipboard {
    fn drop(&mut self) {
        // SAFETY: the clipboard was opened by `open`.
        let _ = unsafe { CloseClipboard() };
    }
}

/// Copies `bytes` into a movable global memory block for `SetClipboardData`.
fn global_from_bytes(bytes: &[u8]) -> Option<HGLOBAL> {
    // SAFETY: the block is allocated with the requested size, locked, filled
    // with exactly `bytes.len()` bytes and unlocked again.
    unsafe {
        let global = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).ok()?;
        let target = GlobalLock(global).cast::<u8>();
        if target.is_null() {
            let _ = GlobalFree(Some(global));
            return None;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
        let _ = GlobalUnlock(global);
        Some(global)
    }
}

/// Hands `bytes` to the clipboard, which then owns the memory.
fn set_data(format: u32, bytes: &[u8]) -> Result<(), String> {
    let global = global_from_bytes(bytes).ok_or("Out of memory")?;
    // SAFETY: the clipboard is open and `global` is a valid movable block.
    if let Err(error) = unsafe { SetClipboardData(format, Some(HANDLE(global.0))) } {
        // On failure the memory still belongs to us.
        // SAFETY: `global` was allocated above and not handed over.
        let _ = unsafe { GlobalFree(Some(global)) };
        return Err(error.message());
    }
    Ok(())
}

pub fn write_files(paths: &[PathBuf], mode: Mode) -> Result<(), String> {
    let header_size = std::mem::size_of::<DROPFILES>();
    let header = DROPFILES {
        pFiles: u32::try_from(header_size).map_err(|e| e.to_string())?,
        fWide: true.into(),
        ..Default::default()
    };
    // Each path ends with NUL; the list ends with an extra NUL.
    let wide: Vec<u16> = paths
        .iter()
        .flat_map(|path| path.as_os_str().encode_wide().chain(std::iter::once(0)))
        .chain(std::iter::once(0))
        .collect();
    let mut bytes = Vec::with_capacity(header_size + wide.len() * 2);
    // SAFETY: DROPFILES is a plain `repr(C)` struct; viewing it as bytes is sound.
    bytes.extend_from_slice(unsafe {
        std::slice::from_raw_parts(std::ptr::from_ref(&header).cast::<u8>(), header_size)
    });
    bytes.extend(wide.iter().flat_map(|unit| unit.to_le_bytes()));

    let effect = match mode {
        Mode::Copy => DROPEFFECT_COPY,
        Mode::Cut => DROPEFFECT_MOVE,
    };
    let _clipboard = OpenedClipboard::open().ok_or("The clipboard is in use")?;
    // SAFETY: the clipboard is open.
    unsafe { EmptyClipboard() }.map_err(|e| e.message())?;
    set_data(CF_HDROP, &bytes)?;
    set_data(drop_effect_format(), &effect.to_le_bytes())
}

pub fn read_files() -> Option<(Vec<PathBuf>, Mode)> {
    // SAFETY: querying a format has no preconditions.
    unsafe { IsClipboardFormatAvailable(CF_HDROP) }.ok()?;
    let _clipboard = OpenedClipboard::open()?;
    // SAFETY: the clipboard is open; the handle stays valid until it closes,
    // and an HDROP clipboard handle is what DragQueryFileW expects.
    let paths = unsafe {
        let handle = GetClipboardData(CF_HDROP).ok()?;
        let drop = HDROP(handle.0);
        let count = DragQueryFileW(drop, u32::MAX, None);
        (0..count)
            .filter_map(|index| {
                let len = DragQueryFileW(drop, index, None) as usize;
                let mut buffer = vec![0_u16; len + 1];
                let copied = DragQueryFileW(drop, index, Some(&mut buffer)) as usize;
                (copied > 0).then(|| PathBuf::from(OsString::from_wide(&buffer[..copied])))
            })
            .collect::<Vec<_>>()
    };
    let mode = read_drop_effect().map_or(Mode::Copy, |effect| {
        if effect & DROPEFFECT_MOVE != 0 && effect & DROPEFFECT_COPY == 0 {
            Mode::Cut
        } else {
            Mode::Copy
        }
    });
    (!paths.is_empty()).then_some((paths, mode))
}

/// Reads the `Preferred DropEffect` DWORD. The clipboard must be open.
fn read_drop_effect() -> Option<u32> {
    // SAFETY: the clipboard is open (see `read_files`); the block is locked
    // only while its first four bytes are read, after checking its size.
    unsafe {
        let handle = GetClipboardData(drop_effect_format()).ok()?;
        let global = HGLOBAL(handle.0);
        if windows::Win32::System::Memory::GlobalSize(global) < 4 {
            return None;
        }
        let data = GlobalLock(global).cast::<u8>();
        if data.is_null() {
            return None;
        }
        let mut bytes = [0_u8; 4];
        std::ptr::copy_nonoverlapping(data, bytes.as_mut_ptr(), 4);
        let _ = GlobalUnlock(global);
        Some(u32::from_le_bytes(bytes))
    }
}

pub fn sequence_number() -> u64 {
    // SAFETY: no preconditions.
    u64::from(unsafe { GetClipboardSequenceNumber() })
}
