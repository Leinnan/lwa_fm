//! macOS counterparts of `windows_tools`: Finder integration and the
//! standard open panel. `AppKit` panels must run on the main thread, which is
//! where the UI calls these from.

use crate::helper::DetachedSpawn;
use anyhow::Context;
use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::{NSModalResponseOK, NSOpenPanel};
use objc2_foundation::{NSArray, NSString, NSURL};
use std::{
    ffi::{CString, OsStr},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Command,
    ptr::NonNull,
};

/// Selects `path` in a Finder window.
pub fn display_in_explorer(path: impl AsRef<OsStr>) -> anyhow::Result<()> {
    Command::new("open")
        .arg("-R")
        .arg(path.as_ref())
        .spawn_detached()
        .context("Failed to open Finder")?;
    Ok(())
}

/// Opens Finder's Get Info window for a given path.
pub fn open_properties(path: impl AsRef<OsStr>) {
    // The path is passed as an argument, so it needs no quoting.
    const SCRIPT: &str = "on run argv
        tell application \"Finder\"
            activate
            open information window of (POSIX file (item 1 of argv) as alias)
        end tell
    end run";
    if let Err(error) = Command::new("osascript")
        .args(["-e", SCRIPT])
        .arg(path.as_ref())
        .spawn_detached()
    {
        log::warn!("Failed to open Get Info: {error}");
    }
}

/// Asks for an application and opens the file with it, like Finder's
/// "Open With > Other…".
pub fn open_with(path: impl AsRef<OsStr>) {
    let Some(app) = pick_program(Some(Path::new("/Applications"))) else {
        return;
    };
    if let Err(error) = Command::new("open")
        .arg("-a")
        .arg(app)
        .arg(path.as_ref())
        .spawn_detached()
    {
        log::warn!("Open with failed: {error}");
    }
}

/// Opens `paths` with the application bundle `app` (a drop onto an app).
pub fn open_files_with(app: &Path, paths: &[PathBuf]) {
    if let Err(error) = Command::new("open")
        .arg("-a")
        .arg(app)
        .args(paths)
        .spawn_detached()
    {
        log::warn!("Opening with {} failed: {error}", app.display());
    }
}

/// Asks for a folder with the standard open panel, starting in `start`.
/// Blocks until it closes; `None` when cancelled.
pub fn pick_folder(start: Option<&Path>) -> Option<PathBuf> {
    pick(start, false)
}

/// Asks for an application with the standard open panel, starting in
/// `start`. Blocks until it closes; `None` when cancelled.
pub fn pick_program(start: Option<&Path>) -> Option<PathBuf> {
    pick(start, true)
}

/// The open panel: a folder picker, or an application picker with
/// `applications`.
fn pick(start: Option<&Path>, applications: bool) -> Option<PathBuf> {
    let mtm = MainThreadMarker::new()?;
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setAllowsMultipleSelection(false);
    panel.setCanChooseFiles(applications);
    panel.setCanChooseDirectories(!applications);
    panel.setCanCreateDirectories(!applications);
    if applications {
        // Deprecated in favour of content types, which need another framework.
        #[allow(deprecated)]
        panel.setAllowedFileTypes(Some(&NSArray::from_retained_slice(&[NSString::from_str(
            "app",
        )])));
    }
    if let Some(start) = start {
        panel.setDirectoryURL(Some(&file_url(start)));
    }
    if panel.runModal() != NSModalResponseOK {
        return None;
    }
    url_path(&*panel.URL()?)
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

/// The `.app` bundle this process runs from, `None` for a bare binary (such
/// as `cargo run`, whose file access macOS charges to the terminal).
pub fn app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let is_bundle = macos.file_name() == Some(OsStr::new("MacOS"))
        && contents.file_name() == Some(OsStr::new("Contents"))
        && bundle.extension() == Some(OsStr::new("app"));
    is_bundle.then(|| bundle.to_path_buf())
}

/// Whether `DirFleet` has Full Disk Access, so no folder asks for permission.
/// Probes the privacy database itself, which only that grant can read; the
/// probe never shows a prompt.
pub fn has_full_disk_access() -> bool {
    let Some(dirs) = directories::BaseDirs::new() else {
        return true;
    };
    let database = dirs
        .home_dir()
        .join("Library/Application Support/com.apple.TCC/TCC.db");
    // Only a refusal counts: anything else can't be told apart, and nagging
    // about it would not help.
    !matches!(
        std::fs::File::open(database),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied
    )
}

/// Opens System Settings at Privacy & Security > Full Disk Access.
pub fn open_full_disk_access_settings() {
    const URL: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";
    if let Err(error) = Command::new("open").arg(URL).spawn_detached() {
        log::warn!("Failed to open System Settings: {error}");
    }
}

/// Starts this app again once the current process has exited, so a new
/// Full Disk Access grant takes effect. The caller then closes the window.
pub fn relaunch_after_exit() {
    // The shell waits for this process to quit, then reopens the bundle.
    const SCRIPT: &str = "while kill -0 \"$1\" 2>/dev/null; do sleep 0.2; done; open \"$2\"";
    let Some(bundle) = app_bundle() else {
        return;
    };
    if let Err(error) = Command::new("/bin/sh")
        .args(["-c", SCRIPT, "sh"])
        .arg(std::process::id().to_string())
        .arg(bundle)
        .spawn_detached()
    {
        log::warn!("Failed to relaunch: {error}");
    }
}
