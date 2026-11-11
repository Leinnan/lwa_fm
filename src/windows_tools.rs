//! Windows counterparts of `macos_tools`: Explorer integration. The shell
//! calls themselves are in `dirfleet_winshell`, shared with the native
//! Windows shell; this keeps the signatures both systems' menus call.
pub use dirfleet_winshell::context_menu;

use anyhow::Context;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

/// No owner window: these dialogs have always opened on their own.
const NO_OWNER: isize = 0;

/// The window of this thread that has focus, as the handle
/// `dirfleet_winshell` takes; 0 when there is none. eframe only exposes a
/// native handle for the main viewport, and menus open from any of them.
pub fn active_window() -> isize {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
    // SAFETY: no preconditions; null when this thread has no active window.
    #[allow(unsafe_code)]
    let window = unsafe { GetActiveWindow() };
    window.0 as isize
}

pub fn display_in_explorer(path: impl AsRef<OsStr>) -> anyhow::Result<()> {
    dirfleet_winshell::display_in_explorer(Path::new(&path)).context("Failed to open explorer")
}

/// Opens the explorer properties for a given path
pub fn open_properties(path: impl AsRef<OsStr>) {
    if let Err(error) = dirfleet_winshell::open_properties(NO_OWNER, Path::new(&path)) {
        eprintln!("Failed to open properties window: {error}");
    }
}

/// Shows the Windows "Open with" dialog for a file.
pub fn open_with(path: impl AsRef<OsStr>) {
    if let Err(error) = dirfleet_winshell::open_with(NO_OWNER, Path::new(&path)) {
        log::warn!("Open with failed: {error}");
    }
}

/// Asks for a folder with the standard Windows folder picker, starting in
/// `start`. Blocks until it closes; `None` when cancelled.
pub fn pick_folder(start: Option<&Path>) -> Option<PathBuf> {
    dirfleet_winshell::pick_folder(NO_OWNER, start)
}

/// Asks for a program (an .exe) with the standard Windows open dialog,
/// starting in `start`. Blocks until it closes; `None` when cancelled.
pub fn pick_program(start: Option<&Path>) -> Option<PathBuf> {
    dirfleet_winshell::pick_program(NO_OWNER, start)
}
