pub mod context_menu;

use crate::helper::DetachedSpawn;
use anyhow::Context;
use std::{ffi::OsStr, iter::once, os::windows::ffi::OsStrExt};
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND},
        UI::Shell::{SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW, ShellExecuteExW},
    },
    core::PCWSTR,
};

pub fn display_in_explorer(path: impl AsRef<OsStr>) -> anyhow::Result<()> {
    std::process::Command::new("explorer.exe")
        .arg("/select,")
        .arg(&path)
        .spawn_detached()
        .context("Failed to open explorer")?;
    Ok(())
}

/// Opens the explorer properties for a given path
pub fn open_properties(path: impl AsRef<OsStr>) {
    // Convert the path to a wide string
    let wide_path: Vec<u16> = OsStr::new(&path).encode_wide().chain(once(0)).collect();
    // Convert the verb "properties" to a wide string
    let wide_verb: Vec<u16> = OsStr::new("properties")
        .encode_wide()
        .chain(once(0))
        .collect();
    // Set up the SHELLEXECUTEINFOW structure
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST,
        hwnd: HWND(std::ptr::null_mut()),
        lpVerb: PCWSTR::from_raw(wide_verb.as_ptr()),
        lpFile: PCWSTR::from_raw(wide_path.as_ptr()),
        lpParameters: PCWSTR::null(),
        lpDirectory: PCWSTR::null(),
        nShow: 0,
        hInstApp: HINSTANCE(std::ptr::null_mut()),
        ..Default::default()
    };

    // Open the properties window
    #[allow(unsafe_code)]
    unsafe {
        #[allow(clippy::borrow_as_ptr)]
        match ShellExecuteExW(&mut sei) {
            Ok(()) => {}
            Err(e) => eprintln!("Failed to open properties window: {e:?}"),
        }
    }
}

/// Shows the Windows "Open with" dialog for a file.
pub fn open_with(path: impl AsRef<OsStr>) {
    use windows::Win32::UI::Shell::{
        OAIF_ALLOW_REGISTRATION, OAIF_EXEC, OAIF_REGISTER_EXT, OPENASINFO, SHOpenWithDialog,
    };
    let wide: Vec<u16> = OsStr::new(&path).encode_wide().chain(once(0)).collect();
    let info = OPENASINFO {
        pcszFile: windows::core::PCWSTR(wide.as_ptr()),
        pcszClass: windows::core::PCWSTR::null(),
        oaifInFlags: OAIF_ALLOW_REGISTRATION | OAIF_REGISTER_EXT | OAIF_EXEC,
    };
    // SAFETY: `info` points at a NUL-terminated path that outlives the call.
    #[allow(unsafe_code)]
    let result = unsafe { SHOpenWithDialog(None, &raw const info) };
    if let Err(error) = result {
        log::warn!("Open with failed: {error:?}");
    }
}

/// Asks for a folder with the standard Windows folder picker, starting in
/// `start`. Blocks until it closes; `None` when cancelled.
pub fn pick_folder(start: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    pick(start, None)
}

/// Asks for a program (an .exe) with the standard Windows open dialog,
/// starting in `start`. Blocks until it closes; `None` when cancelled.
pub fn pick_program(start: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    pick(start, Some(("Programs", "*.exe")))
}

/// The Windows open dialog: a folder picker without `filter`, otherwise a
/// file picker showing files matching `filter` (name, pattern).
fn pick(
    start: Option<&std::path::Path>,
    filter: Option<(&str, &str)>,
) -> Option<std::path::PathBuf> {
    use windows::Win32::{
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoTaskMemFree,
        },
        UI::Shell::{
            Common::COMDLG_FILTERSPEC, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog,
            IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
        },
    };
    let wide = |text: &OsStr| -> Vec<u16> { text.encode_wide().chain(once(0)).collect() };
    #[allow(unsafe_code)]
    // SAFETY: plain COM calls on the UI thread. Initialising COM again is
    // harmless (the window already did); every string passed in is
    // NUL-terminated and outlives its call, and the returned path is freed
    // with `CoTaskMemFree` as the API requires.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let mut options = dialog.GetOptions().ok()? | FOS_FORCEFILESYSTEM;
        // Kept alive until the dialog closes: the filter points into them.
        let filter_text =
            filter.map(|(name, pattern)| (wide(OsStr::new(name)), wide(OsStr::new(pattern))));
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
            let path = wide(start.as_os_str());
            if let Ok(folder) =
                SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(path.as_ptr()), None)
            {
                let _ = dialog.SetFolder(&folder);
            }
        }
        dialog.Show(None).ok()?;
        let name = dialog
            .GetResult()
            .ok()?
            .GetDisplayName(SIGDN_FILESYSPATH)
            .ok()?;
        let path = name.to_string().ok();
        CoTaskMemFree(Some(name.0.cast()));
        path.map(std::path::PathBuf::from)
    }
}
