//! Runs file operations one after another on a thread of their own, as the
//! egui app's worker does, reporting progress back to the core; checks names
//! for clashes; and reads and writes the file clipboard.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use crux_core::Request;
use crux_core::effects::ResolveSink;
use dirfleet_core::app::{
    ClipboardDone, ClipboardOp, Conflicts, FileOpUpdate, FindConflicts, RunFileOp,
};
use dirfleet_core::fileops::FileOpEvent;

use crate::clipboard::{self, Mode};

type Cancels = Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct FileJobs {
    jobs: Sender<(Request<RunFileOp>, Arc<AtomicBool>)>,
    cancels: Cancels,
}

impl FileJobs {
    pub fn new<R>(sink: Weak<R>) -> Self
    where
        R: ResolveSink<RunFileOp> + Send + Sync + 'static,
    {
        let (jobs, queue) = channel::<(Request<RunFileOp>, Arc<AtomicBool>)>();
        let cancels: Cancels = Arc::default();
        let finished = Arc::clone(&cancels);
        let spawned = std::thread::Builder::new()
            .name("dirfleet_file_ops".into())
            .spawn(move || {
                while let Ok((request, cancel)) = queue.recv() {
                    let id = request.operation.id;
                    let RunFileOp { op, origin, .. } = request.operation.clone();
                    let request = Mutex::new(request);
                    let report = |event: FileOpEvent| {
                        if let Some(sink) = sink.upgrade() {
                            let _ = sink.resolve_request(&mut lock(&request), FileOpUpdate(event));
                        }
                    };
                    let outcome = crate::fileops::execute(id, op, origin, &cancel, &report);
                    report(FileOpEvent::Finished(outcome));
                    lock(&finished).remove(&id);
                }
            });
        if let Err(error) = spawned {
            log::error!("could not start the file operation thread: {error}");
        }
        Self { jobs, cancels }
    }

    pub fn run(&self, request: Request<RunFileOp>) {
        let cancel = Arc::new(AtomicBool::new(false));
        lock(&self.cancels).insert(request.operation.id, Arc::clone(&cancel));
        if self.jobs.send((request, cancel)).is_err() {
            log::error!("the file operation thread is not running");
        }
    }

    pub fn cancel(&self, id: u64) {
        let cancel = lock(&self.cancels).get(&id).cloned();
        if let Some(cancel) = cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// The names among the sources that already exist in the destination.
pub fn conflicts(check: &FindConflicts) -> Vec<String> {
    check
        .srcs
        .iter()
        .filter_map(|src| src.file_name())
        .filter(|name| check.dst.join(name).symlink_metadata().is_ok())
        .map(|name| name.to_string_lossy().into_owned())
        .collect()
}

pub fn find_conflicts<R>(mut request: Request<FindConflicts>, sink: Weak<R>)
where
    R: ResolveSink<FindConflicts> + Send + Sync + 'static,
{
    std::thread::spawn(move || {
        let found = Conflicts(conflicts(&request.operation));
        if let Some(sink) = sink.upgrade() {
            let _ = sink.resolve_request(&mut request, found);
        }
    });
}

fn clipboard_op(op: &ClipboardOp) -> Result<Option<(Vec<PathBuf>, bool)>, String> {
    match op {
        ClipboardOp::Write { paths, cut } => {
            let mode = if *cut { Mode::Cut } else { Mode::Copy };
            clipboard::write_files(paths, mode).map(|()| None)
        }
        ClipboardOp::Read => clipboard::read_files()
            .map(|(paths, mode)| Some((paths, mode == Mode::Cut)))
            .ok_or_else(|| "The clipboard holds no files".to_owned()),
    }
}

pub fn clipboard<R>(mut request: Request<ClipboardOp>, sink: Weak<R>)
where
    R: ResolveSink<ClipboardOp> + Send + Sync + 'static,
{
    std::thread::spawn(move || {
        let done = ClipboardDone(clipboard_op(&request.operation));
        if let Some(sink) = sink.upgrade() {
            let _ = sink.resolve_request(&mut request, done);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clashing_names_are_found() {
        let dir = tempfile::tempdir().expect("temp dir");
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        std::fs::create_dir_all(&from).expect("from");
        std::fs::create_dir_all(&to).expect("to");
        for name in ["a.txt", "b.txt"] {
            std::fs::write(from.join(name), b"x").expect("write");
        }
        std::fs::write(to.join("b.txt"), b"y").expect("write");
        let found = conflicts(&FindConflicts {
            srcs: vec![from.join("a.txt"), from.join("b.txt")],
            dst: to,
        });
        assert_eq!(found, ["b.txt"]);
    }
}
