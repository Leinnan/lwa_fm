//! The standard Windows dialogs for choosing a folder or a program.
#![allow(unsafe_code)]

use crate::{
    com::Com,
    raw::{optional_hwnd, wide},
};
use std::path::{Path, PathBuf};
use windows::{
    Win32::{
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree},
        UI::Shell::{
            Common::COMDLG_FILTERSPEC, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog,
            IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
        },
    },
    core::PCWSTR,
};

/// Asks for a folder with the standard Windows folder picker, owned by
/// `owner` and starting in `start`. Blocks until it closes; `None` when
/// cancelled.
#[must_use]
pub fn pick_folder(owner: isize, start: Option<&Path>) -> Option<PathBuf> {
    pick(owner, start, None)
}

/// Asks for a program (an .exe) with the standard Windows open dialog, owned
/// by `owner` and starting in `start`. Blocks until it closes; `None` when
/// cancelled.
#[must_use]
pub fn pick_program(owner: isize, start: Option<&Path>) -> Option<PathBuf> {
    pick(owner, start, Some(("Programs", "*.exe")))
}

/// The Windows open dialog: a folder picker without `filter`, otherwise a
/// file picker showing files matching `filter` (name, pattern).
fn pick(owner: isize, start: Option<&Path>, filter: Option<(&str, &str)>) -> Option<PathBuf> {
    let _com = Com::enter();
    // SAFETY: plain COM calls on the caller's thread, with COM entered above.
    // Every string passed in is NUL-terminated and outlives its call, and the
    // returned path is freed with `CoTaskMemFree` as the API requires.
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let mut options = dialog.GetOptions().ok()? | FOS_FORCEFILESYSTEM;
        // Kept alive until the dialog closes: the filter points into them.
        let filter_text = filter.map(|(name, pattern)| (wide(name), wide(pattern)));
        match &filter_text {
            Some((name, pattern)) => {
                let spec = [COMDLG_FILTERSPEC {
                    pszName: PCWSTR(name.as_ptr()),
                    pszSpec: PCWSTR(pattern.as_ptr()),
                }];
                let _ = dialog.SetFileTypes(&spec);
            }
            None => options |= FOS_PICKFOLDERS,
        }
        dialog.SetOptions(options).ok()?;
        if let Some(start) = start {
            let path = wide(start);
            if let Ok(folder) =
                SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(path.as_ptr()), None)
            {
                let _ = dialog.SetFolder(&folder);
            }
        }
        dialog.Show(optional_hwnd(owner)).ok()?;
        let name = dialog
            .GetResult()
            .ok()?
            .GetDisplayName(SIGDN_FILESYSPATH)
            .ok()?;
        let path = name.to_string().ok();
        CoTaskMemFree(Some(name.0.cast()));
        path.map(PathBuf::from)
    }
}
