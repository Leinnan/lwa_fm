//! Thumbnails and animated previews the core asks for, made a few at a time
//! on worker threads and answered with the cached image file, and the media
//! service's notices.
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, PoisonError, Weak};

use crossbeam::channel::{self, Sender};
use crux_core::Request;
use crux_core::effects::ResolveSink;
use dirfleet_core::app::{MediaNotice, MediaNotices, Thumbnail, ToastKind};

use crate::media::{self, Notice};

/// Thumbnails made at once: half the cores, leaving the rest to the
/// interface and to what the thumbnails are drawn for.
fn workers() -> usize {
    std::thread::available_parallelism().map_or(4, |cores| (cores.get() / 2).clamp(2, 8))
}

pub struct ThumbnailHandler {
    jobs: Sender<Request<Thumbnail>>,
}

impl ThumbnailHandler {
    pub fn new<R>(sink: &Weak<R>) -> Self
    where
        R: ResolveSink<Thumbnail> + Send + Sync + 'static,
    {
        let (jobs, queue) = channel::unbounded::<Request<Thumbnail>>();
        for worker in 0..workers() {
            let (queue, sink) = (queue.clone(), sink.clone());
            let spawned = std::thread::Builder::new()
                .name(format!("dirfleet_thumbnail_{worker}"))
                .spawn(move || {
                    let cancel = AtomicBool::new(false);
                    for mut request in queue {
                        let Thumbnail {
                            path,
                            edge,
                            revision,
                            size,
                            animated,
                        } = &request.operation;
                        let make = if *animated {
                            media::animated_preview_file
                        } else {
                            media::thumbnail_file
                        };
                        let file = make(Path::new(path), *edge, *revision, *size, &cancel)
                            .map(|file| file.to_string_lossy().into_owned());
                        let Some(sink) = sink.upgrade() else {
                            return;
                        };
                        let _ = sink.resolve_request(&mut request, file);
                    }
                });
            if let Err(error) = spawned {
                log::warn!("could not start a thumbnail worker: {error}");
            }
        }
        Self { jobs }
    }

    pub fn make(&self, request: Request<Thumbnail>) {
        let _ = self.jobs.send(request);
    }
}

/// Sends the media service's notices to the core from now on, and fetches
/// `FFmpeg` when it is missing, unless `downloads` is off.
pub fn notices<R>(request: Request<MediaNotices>, sink: &Weak<R>, downloads: bool)
where
    R: ResolveSink<MediaNotices> + Send + Sync + 'static,
{
    let request = Mutex::new(request);
    let sink = sink.clone();
    media::set_notifier(move |level, text| {
        let kind = match level {
            Notice::Info => ToastKind::Info,
            Notice::Success => ToastKind::Success,
            Notice::Warning => ToastKind::Warning,
        };
        if let Some(sink) = sink.upgrade() {
            let mut request = request.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = sink.resolve_request(
                &mut request,
                MediaNotice {
                    kind,
                    text: text.to_owned(),
                },
            );
        }
    });
    // Tests never download. On macOS the system's frameworks make the
    // posters, so FFmpeg is fetched when something first needs it (a file
    // they can't read, a chosen frame, an animated preview), not at launch.
    #[cfg(not(any(test, feature = "test-support")))]
    if downloads && !cfg!(target_os = "macos") {
        media::video::prepare_backend_async();
    }
    #[cfg(any(test, feature = "test-support"))]
    let _ = downloads;
}
