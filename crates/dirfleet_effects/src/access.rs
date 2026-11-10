//! Full Disk Access on macOS, the app bundle the process runs from, and
//! whether a terminal app exists, for the core's access checks; moved from
//! the egui app, which uses them too.
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use dirfleet_core::app::DiskAccess;

/// The `.app` bundle this process runs from, `None` for a bare binary (such
/// as `cargo run`, whose file access macOS charges to the terminal) and for
/// test runners.
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

/// What the core's Full Disk Access check finds: it matters only for a
/// built app on macOS.
pub fn check() -> DiskAccess {
    let applies = cfg!(target_os = "macos") && app_bundle().is_some();
    DiskAccess {
        applies,
        granted: !applies || has_full_disk_access(),
    }
}

/// Starts this app again once the current process has exited, so a new
/// Full Disk Access grant takes effect. The caller then quits.
///
/// # Errors
/// When there is no app bundle to start, or the waiting shell can't start.
pub fn relaunch_after_exit() -> Result<(), String> {
    // The shell waits for this process to quit, then reopens the bundle.
    const SCRIPT: &str = "while kill -0 \"$1\" 2>/dev/null; do sleep 0.2; done; open \"$2\"";
    let bundle = app_bundle().ok_or("Only the app can restart itself")?;
    crate::system::spawn_detached(
        Command::new("/bin/sh")
            .args(["-c", SCRIPT, "sh"])
            .arg(std::process::id().to_string())
            .arg(bundle),
    )
    .map_err(|error| format!("Could not restart: {error}"))
}

/// Whether the terminal app `app` can be found: on macOS an app name (or
/// path) among the usual Applications folders, elsewhere a program path or
/// a name on `PATH`.
pub fn terminal_found(app: &str) -> bool {
    let app = app.trim();
    if app.is_empty() {
        return false;
    }
    let path = Path::new(app);
    if path.is_absolute() {
        return path.exists();
    }
    if cfg!(target_os = "macos") {
        let bundle = if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
        {
            app.to_owned()
        } else {
            format!("{app}.app")
        };
        let home = directories::BaseDirs::new().map(|dirs| dirs.home_dir().join("Applications"));
        return [
            Some(PathBuf::from("/Applications")),
            Some(PathBuf::from("/Applications/Utilities")),
            Some(PathBuf::from("/System/Applications")),
            Some(PathBuf::from("/System/Applications/Utilities")),
            home,
        ]
        .into_iter()
        .flatten()
        .any(|folder| folder.join(&bundle).exists());
    }
    let program = if cfg!(windows) && path.extension().is_none() {
        format!("{app}.exe")
    } else {
        app.to_owned()
    };
    std::env::var_os("PATH").is_some_and(|search| {
        std::env::split_paths(&search).any(|folder| folder.join(&program).is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runners_are_not_the_app_and_need_no_access() {
        assert_eq!(app_bundle(), None);
        assert_eq!(
            check(),
            DiskAccess {
                applies: false,
                granted: true
            }
        );
        assert!(relaunch_after_exit().is_err());
    }

    #[test]
    fn terminals_are_found_by_name_or_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(terminal_found(&dir.path().to_string_lossy()));
        assert!(!terminal_found("Definitely Not A Terminal 42"));
        assert!(!terminal_found(""));
        if cfg!(target_os = "macos") {
            assert!(terminal_found("Terminal"), "macOS ships Terminal");
        }
    }
}
