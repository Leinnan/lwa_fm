//! Folder sizes the core asks for: each folder is added up on a thread of
//! its own until it is done or stopped. The core asks for a few at a time.
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use crux_core::Request;
use crux_core::effects::ResolveSink;
use dirfleet_core::app::FolderSize;

use crate::usage;

/// The folders being added up, by path, each with what stops it.
type Running = Mutex<HashMap<String, Arc<AtomicBool>>>;
type Start = dyn Fn(Request<FolderSize>, Arc<AtomicBool>) + Send + Sync;

pub struct SizeHandler {
    running: Arc<Running>,
    start: Box<Start>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The disk space under `path`; `None` when it can't be read or `cancel`
/// was set meanwhile.
fn total(path: &Path, cancel: &AtomicBool) -> Option<u64> {
    usage::measure(path, &usage::Options::default(), cancel)
        .ok()
        .flatten()
        .map(|found| found.bytes)
}

impl SizeHandler {
    pub fn new<R>(sink: Weak<R>) -> Self
    where
        R: ResolveSink<FolderSize> + Send + Sync + 'static,
    {
        let running: Arc<Running> = Arc::default();
        let finished = Arc::downgrade(&running);
        Self {
            running,
            start: Box::new(move |request, cancel| {
                spawn(request, cancel, sink.clone(), finished.clone());
            }),
        }
    }

    pub fn start(&self, request: Request<FolderSize>) {
        let cancel = Arc::new(AtomicBool::new(false));
        // The same folder asked for again: the earlier walk gives way.
        let earlier =
            lock(&self.running).insert(request.operation.path.clone(), Arc::clone(&cancel));
        if let Some(earlier) = earlier {
            earlier.store(true, Ordering::Release);
        }
        (self.start)(request, cancel);
    }

    pub fn stop(&self, path: &str) {
        let cancel = lock(&self.running).remove(path);
        if let Some(cancel) = cancel {
            cancel.store(true, Ordering::Release);
        }
    }

    /// How many folders are being added up.
    pub fn count(&self) -> usize {
        lock(&self.running).len()
    }
}

fn spawn<R>(
    mut request: Request<FolderSize>,
    cancel: Arc<AtomicBool>,
    sink: Weak<R>,
    running: Weak<Running>,
) where
    R: ResolveSink<FolderSize> + Send + Sync + 'static,
{
    let spawned = std::thread::Builder::new()
        .name("dirfleet_size".into())
        .spawn(move || {
            let path = request.operation.path.clone();
            // A stopped walk answers too, with nothing: the core has
            // forgotten the asking, and the request is not left waiting.
            let total = total(Path::new(&path), &cancel);
            if let Some(running) = running.upgrade() {
                let mut running = lock(&running);
                if running
                    .get(&path)
                    .is_some_and(|current| Arc::ptr_eq(current, &cancel))
                {
                    running.remove(&path);
                }
            }
            if let Some(sink) = sink.upgrade() {
                let _ = sink.resolve_request(&mut request, total);
            }
        });
    if let Err(error) = spawned {
        log::warn!("could not start adding up a folder: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_is_added_up_through_its_subfolders_unless_stopped() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("sub/deeper")).expect("creates");
        std::fs::write(dir.path().join("a.bin"), vec![1u8; 3000]).expect("writes");
        std::fs::write(dir.path().join("sub/deeper/b.bin"), vec![1u8; 5000]).expect("writes");
        let found = total(dir.path(), &AtomicBool::new(false)).expect("added up");
        assert!(found >= 8000, "{found}");
        assert_eq!(total(dir.path(), &AtomicBool::new(true)), None, "stopped");
        assert_eq!(
            total(&dir.path().join("missing"), &AtomicBool::new(false)),
            None
        );
    }
}
