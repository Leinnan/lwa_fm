//! Hands things to the operating system: opening files, showing them in the
//! file manager, a terminal, Get Info and copying text. Each runs on its own
//! thread, since launching apps can wait on the system.
use std::io;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Weak;

use crux_core::Request;
use crux_core::effects::ResolveSink;
use dirfleet_core::app::{SystemDone, SystemOp};

/// Starts `command` with no terminal attached, reaping it when it exits.
pub(crate) fn spawn_detached(command: &mut Command) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
    }
    let mut child: Child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn failed(what: &str, error: impl std::fmt::Display) -> String {
    format!("{what}: {error}")
}

fn open(paths: &[String]) -> Result<(), String> {
    for path in paths {
        open::that_detached(path)
            .map_err(|error| failed(&format!("Could not open {path}"), error))?;
    }
    Ok(())
}

fn open_with(app: &str, paths: &[String]) -> Result<(), String> {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("open");
        command.arg("-a").arg(app);
        command
    } else {
        Command::new(app)
    };
    spawn_detached(command.args(paths))
        .map_err(|error| failed(&format!("Could not start {app}"), error))
}

/// As egui: opening a package would launch it, so it is shown instead.
fn open_folder(path: &str) -> Result<(), String> {
    if crate::fs_enum::is_package(Path::new(path)) {
        reveal(&[path.to_owned()])
    } else {
        open(&[path.to_owned()])
    }
}

fn reveal(paths: &[String]) -> Result<(), String> {
    let Some(first) = paths.first() else {
        return Ok(());
    };
    let result = if cfg!(target_os = "macos") {
        spawn_detached(Command::new("open").arg("-R").args(paths))
    } else if cfg!(windows) {
        spawn_detached(Command::new("explorer").arg(format!("/select,{first}")))
    } else {
        // Linux file managers have no common "select this" call.
        let folder = Path::new(first)
            .parent()
            .unwrap_or_else(|| Path::new(first));
        open::that_detached(folder)
    };
    result.map_err(|error| failed("Could not show it in the file manager", error))
}

fn terminal(folder: &str, app: &str) -> Result<(), String> {
    let result = if cfg!(target_os = "macos") {
        spawn_detached(Command::new("open").arg("-a").arg(app).arg(folder))
    } else if cfg!(windows) && app.eq_ignore_ascii_case("cmd") {
        spawn_detached(
            Command::new("cmd")
                .args(["/c", "start", "cmd"])
                .current_dir(folder),
        )
    } else {
        spawn_detached(Command::new(app).current_dir(folder))
    };
    result.map_err(|error| failed("Could not open a terminal", error))
}

fn get_info(paths: &[String]) -> Result<(), String> {
    // The paths are passed as arguments, so they need no quoting.
    const SCRIPT: &str = "on run argv
        tell application \"Finder\"
            activate
            repeat with item_path in argv
                open information window of (POSIX file item_path as alias)
            end repeat
        end tell
    end run";
    if !cfg!(target_os = "macos") {
        return Err("Get Info is not available here yet".into());
    }
    spawn_detached(Command::new("osascript").args(["-e", SCRIPT]).args(paths))
        .map_err(|error| failed("Could not open Get Info", error))
}

#[cfg(target_os = "macos")]
fn copy_text(text: &str) -> Result<(), String> {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    use objc2_foundation::NSString;
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    // SAFETY: a constant pasteboard type the system provides.
    #[allow(unsafe_code)]
    let kind = unsafe { NSPasteboardTypeString };
    if pasteboard.setString_forType(&NSString::from_str(text), kind) {
        Ok(())
    } else {
        Err("Could not copy to the clipboard".into())
    }
}

#[cfg(not(target_os = "macos"))]
fn copy_text(text: &str) -> Result<(), String> {
    arboard::Clipboard::new()
        .and_then(|mut clipboard| clipboard.set_text(text.to_owned()))
        .map_err(|error| failed("Could not copy to the clipboard", error))
}

/// Does `op`.
///
/// # Errors
/// Why the system could not do it.
pub fn run(op: &SystemOp) -> Result<(), String> {
    match op {
        SystemOp::Open(paths) => open(paths),
        SystemOp::OpenWith { app, paths } => open_with(app, paths),
        SystemOp::Reveal(paths) => reveal(paths),
        SystemOp::OpenFolder(path) => open_folder(path),
        SystemOp::Terminal { folder, app } => terminal(folder, app),
        SystemOp::GetInfo(paths) => get_info(paths),
        SystemOp::CopyText(text) => copy_text(text),
        SystemOp::OpenPrivacySettings => open_privacy_settings(),
        SystemOp::Relaunch => crate::access::relaunch_after_exit(),
    }
}

/// System Settings at Privacy & Security > Full Disk Access, as egui opens it.
fn open_privacy_settings() -> Result<(), String> {
    const URL: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";
    if !cfg!(target_os = "macos") {
        return Err("Privacy settings are only on macOS".into());
    }
    open::that_detached(URL).map_err(|error| failed("Could not open System Settings", error))
}

pub fn handle<R>(mut request: Request<SystemOp>, sink: Weak<R>)
where
    R: ResolveSink<SystemOp> + Send + Sync + 'static,
{
    let spawned = std::thread::Builder::new()
        .name("dirfleet_system".into())
        .spawn(move || {
            let done = SystemDone(run(&request.operation));
            if let Some(sink) = sink.upgrade() {
                let _ = sink.resolve_request(&mut request, done);
            }
        });
    if let Err(error) = spawned {
        log::warn!("could not hand it to the system: {error}");
    }
}
