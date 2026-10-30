//! macOS file clipboard: file URLs on the general pasteboard, the way Finder
//! writes them, so files can be copied between `DirFleet` and Finder in both
//! directions.
//!
//! The pasteboard has no notion of a cut. A cut is remembered here and holds
//! only while the pasteboard still has what it wrote; other applications
//! see a copy.

use super::Mode;
use crate::macos_tools::{file_url, url_path};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeFileURL, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSURL};
use std::path::PathBuf;
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
