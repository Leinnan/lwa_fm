//! macOS file clipboard: file URLs on the general pasteboard, the way Finder
//! writes them, so files can be copied between `DirFleet` and Finder in both
//! directions.
//!
//! The pasteboard has no notion of a cut. A cut is remembered here and holds
//! only while the pasteboard still has what it wrote; other applications
//! see a copy.

use super::Mode;
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeFileURL, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSString, NSURL};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::{Mutex, PoisonError};

/// The pasteboard change count right after a cut was written.
static CUT: Mutex<Option<isize>> = Mutex::new(None);

pub fn write_files(paths: &[PathBuf], mode: Mode) -> Result<(), String> {
    let urls: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = paths
        .iter()
        .map(|path| ProtocolObject::from_retained(file_url(path)))
        .collect();
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    if !pasteboard.writeObjects(&NSArray::from_retained_slice(&urls)) {
        return Err("Could not write to the clipboard".to_owned());
    }
    *CUT.lock().unwrap_or_else(PoisonError::into_inner) =
        (mode == Mode::Cut).then(|| pasteboard.changeCount());
    Ok(())
}

pub fn read_files() -> Option<(Vec<PathBuf>, Mode)> {
    let pasteboard = NSPasteboard::generalPasteboard();
    // SAFETY: an immutable string constant exported by AppKit.
    #[allow(unsafe_code)]
    let file_type = unsafe { NSPasteboardTypeFileURL };
    let paths: Vec<PathBuf> = pasteboard
        .pasteboardItems()?
        .iter()
        .filter_map(|item| item.stringForType(file_type))
        .filter_map(|text| NSURL::URLWithString(&text))
        .filter_map(|url| url_path(&url))
        .collect();
    if paths.is_empty() {
        return None;
    }
    let cut = *CUT.lock().unwrap_or_else(PoisonError::into_inner) == Some(pasteboard.changeCount());
    Some((paths, if cut { Mode::Cut } else { Mode::Copy }))
}

/// The pasteboard change count, which every write (in any application)
/// increments.
pub fn sequence_number() -> u64 {
    NSPasteboard::generalPasteboard().changeCount() as u64
}

/// A file URL for `path`, keeping its bytes as they are (`fileURLWithPath:`
/// would decompose accented letters).
pub fn file_url(path: &Path) -> Retained<NSURL> {
    let Ok(raw) = CString::new(path.as_os_str().as_bytes()) else {
        return NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    };
    // SAFETY: `raw` is NUL-terminated and outlives the call, which copies it.
    #[allow(unsafe_code)]
    unsafe {
        NSURL::fileURLWithFileSystemRepresentation_isDirectory_relativeToURL(
            NonNull::from(raw.as_c_str()).cast(),
            path.is_dir(),
            None,
        )
    }
}

/// The file system path of a file URL, resolving `file:///.file/id=` references.
/// (`fileSystemRepresentation` would decompose accented letters.)
pub fn url_path(url: &NSURL) -> Option<PathBuf> {
    let path = url.filePathURL()?.path()?;
    Some(PathBuf::from(path.to_string()))
}
