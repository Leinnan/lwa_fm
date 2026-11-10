//! The user's own folders, mounted drives (polled every two seconds, as egui
//! does) and ejecting them, each off the caller's thread.
use std::path::Path;
use std::sync::Weak;
use std::time::Duration;

use crux_core::Request;
use crux_core::effects::ResolveSink;
use dirfleet_core::app::{Bookmark, DriveList, Eject, EjectDone, UserFolders, WatchDrives};

use crate::drives;

const DRIVE_POLL: Duration = Duration::from_secs(2);
/// How often the drives' free space is read again while nothing is mounted
/// or ejected: it is shown in the sidebar and the status bar, where half a
/// minute old is new enough, and every reading that differs is drawn.
const SPACE_POLL: Duration = Duration::from_secs(30);

/// What is mounted, read cheaply: when it differs, the drives are listed
/// in full. `None` where there is no cheap way, and they are listed each
/// time.
fn mounted() -> Option<Vec<std::ffi::OsString>> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let mut names: Vec<_> = std::fs::read_dir("/Volumes")
        .ok()?
        .flatten()
        .map(|entry| entry.file_name())
        .collect();
    names.sort();
    Some(names)
}

/// Home, Documents, Desktop, Downloads, Pictures and Music, where they exist.
pub fn user_folders() -> Vec<Bookmark> {
    let Some(dirs) = directories::UserDirs::new() else {
        return Vec::new();
    };
    let bookmark = |name: &str, path: &Path| Bookmark {
        name: name.to_owned(),
        path: path.to_string_lossy().into_owned(),
    };
    let mut folders = vec![bookmark("User", dirs.home_dir())];
    for (name, dir) in [
        ("Documents", dirs.document_dir()),
        ("Desktop", dirs.desktop_dir()),
        ("Downloads", dirs.download_dir()),
        ("Pictures", dirs.picture_dir()),
        ("Music", dirs.audio_dir()),
    ] {
        if let Some(dir) = dir {
            folders.push(bookmark(name, dir));
        }
    }
    folders
}

pub fn answer_user_folders<R>(mut request: Request<UserFolders>, sink: &Weak<R>)
where
    R: ResolveSink<UserFolders>,
{
    if let Some(sink) = sink.upgrade() {
        let _ = sink.resolve_request(&mut request, user_folders());
    }
}

/// Reports the drives at once, then on every change, until the core stops
/// listening.
pub fn watch_drives<R>(mut request: Request<WatchDrives>, sink: Weak<R>)
where
    R: ResolveSink<WatchDrives> + Send + Sync + 'static,
{
    let spawned = std::thread::Builder::new()
        .name("dirfleet_drives".into())
        .spawn(move || {
            let mut last = None;
            let mut seen = None;
            let mut listed = std::time::Instant::now();
            loop {
                // A drive coming or going shows within a poll; free space
                // alone is read every half minute.
                let mounts = mounted();
                let due = last.is_none()
                    || mounts.is_none()
                    || mounts != seen
                    || listed.elapsed() >= SPACE_POLL;
                if due {
                    seen = mounts;
                    listed = std::time::Instant::now();
                    let now = drives::list();
                    if last.as_ref() != Some(&now) {
                        let delivered = sink.upgrade().is_some_and(|sink| {
                            sink.resolve_request(&mut request, DriveList(now.clone()))
                                .is_ok()
                        });
                        if !delivered {
                            return;
                        }
                        last = Some(now);
                    }
                } else if sink.strong_count() == 0 {
                    return;
                }
                std::thread::sleep(DRIVE_POLL);
            }
        });
    if let Err(error) = spawned {
        log::warn!("could not watch drives: {error}");
    }
}

pub fn eject<R>(mut request: Request<Eject>, sink: Weak<R>)
where
    R: ResolveSink<Eject> + Send + Sync + 'static,
{
    let spawned = std::thread::Builder::new()
        .name("dirfleet_eject".into())
        .spawn(move || {
            let mount = Path::new(&request.operation.mount);
            let done = drives::list()
                .into_iter()
                .find(|drive| drive.mount == mount)
                .map_or_else(
                    || Err(format!("{} is no longer mounted", mount.display())),
                    |drive| drives::eject(&drive).map_err(|error| format!("{error:#}")),
                );
            if let Some(sink) = sink.upgrade() {
                let _ = sink.resolve_request(&mut request, EjectDone(done));
            }
        });
    if let Err(error) = spawned {
        log::warn!("could not eject: {error}");
    }
}
