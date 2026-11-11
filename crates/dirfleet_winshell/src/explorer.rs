//! Handing a file to Explorer: showing it in a folder window, its Properties
//! and the "Open with" dialog.
#![allow(unsafe_code)]

use crate::{
    com::Com,
    error::{Error, ShellResult as _},
    raw::{hwnd, optional_hwnd, wide},
};
use std::{
    io,
    os::windows::process::CommandExt as _,
    path::Path,
    process::{Command, Stdio},
};
use windows::{
    Win32::UI::Shell::{
        OAIF_ALLOW_REGISTRATION, OAIF_EXEC, OAIF_REGISTER_EXT, OPENASINFO, SEE_MASK_INVOKEIDLIST,
        SHELLEXECUTEINFOW, SHOpenWithDialog, ShellExecuteExW,
    },
    core::PCWSTR,
};

/// Opens an Explorer window on the folder of `path`, with `path` selected.
/// Needs no COM and no window; any thread will do.
///
/// # Errors
/// Explorer could not be started.
pub fn display_in_explorer(path: &Path) -> io::Result<()> {
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    Command::new("explorer.exe")
        .arg("/select,")
        .arg(path)
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

/// Opens the explorer properties for a given path, owned by `owner`.
///
/// Returns as soon as the shell has taken the request: the dialog runs on a
/// thread of the shell's own, and closes with the process.
///
/// # Errors
/// The shell refused, for example because nothing is at `path`. It says so in
/// a message box of its own first, and this only returns once that is closed.
pub fn open_properties(owner: isize, path: &Path) -> Result<(), Error> {
    let wide_path = wide(path);
    let wide_verb = wide("properties");
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST,
        hwnd: hwnd(owner),
        lpVerb: PCWSTR(wide_verb.as_ptr()),
        lpFile: PCWSTR(wide_path.as_ptr()),
        nShow: 0,
        ..Default::default()
    };
    // The verb is run by a shell extension, which needs COM.
    let _com = Com::enter();
    // SAFETY: `sei` is fully set up and sized by `cbSize`; its strings are
    // NUL-terminated and outlive the call.
    unsafe { ShellExecuteExW(&raw mut sei) }.shell()
}

/// Shows the Windows "Open with" dialog for a file, owned by `owner`. Blocks
/// until it closes.
///
/// # Errors
/// The dialog could not be shown, or it was cancelled.
pub fn open_with(owner: isize, path: &Path) -> Result<(), Error> {
    let wide = wide(path);
    let info = OPENASINFO {
        pcszFile: PCWSTR(wide.as_ptr()),
        pcszClass: PCWSTR::null(),
        oaifInFlags: OAIF_ALLOW_REGISTRATION | OAIF_REGISTER_EXT | OAIF_EXEC,
    };
    let _com = Com::enter();
    // SAFETY: `info` points at a NUL-terminated path that outlives the call.
    unsafe { SHOpenWithDialog(optional_hwnd(owner), &raw const info) }.shell()
}

#[cfg(test)]
mod tests {
    use super::open_properties;
    use std::time::{Duration, Instant};
    use windows::{
        Win32::{
            Foundation::{HWND, LPARAM, WPARAM},
            UI::WindowsAndMessaging::{
                FindWindowExW, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
            },
        },
        core::w,
    };

    /// A visible dialog of this process, which is what Properties is.
    fn own_dialog() -> Option<HWND> {
        let mut after = None;
        loop {
            // SAFETY: plain window lookups; `after` is null or a window the
            // previous round returned.
            let window = unsafe { FindWindowExW(None, after, w!("#32770"), None) }.ok()?;
            let mut process = 0;
            // SAFETY: `window` was just returned, and `process` is a valid local.
            let visible = unsafe {
                GetWindowThreadProcessId(window, Some(&raw mut process));
                IsWindowVisible(window).as_bool()
            };
            if visible && process == std::process::id() {
                return Some(window);
            }
            after = Some(window);
        }
    }

    /// How the effect handlers call it: from a thread without COM that ends
    /// right after. Ignored because it puts a real window on the desktop.
    #[test]
    #[ignore = "shows the Properties window for a moment"]
    fn properties_outlive_the_thread_that_asked() {
        let path = std::env::temp_dir().join(format!(
            "dirfleet_winshell_properties_{}.txt",
            std::process::id()
        ));
        std::fs::write(&path, "x").expect("write fixture");
        let asked = path.clone();
        std::thread::spawn(move || open_properties(0, &asked))
            .join()
            .expect("the asking thread")
            .expect("the shell took the request");
        let deadline = Instant::now() + Duration::from_secs(10);
        let window = loop {
            if let Some(window) = own_dialog() {
                break window;
            }
            assert!(Instant::now() < deadline, "no Properties window came up");
            std::thread::sleep(Duration::from_millis(50));
        };
        // SAFETY: posting a plain close to a window of this process.
        let _ = unsafe { PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        let _ = std::fs::remove_file(path);
    }
}
