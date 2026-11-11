//! Runs file operations and reports their progress back to the core; checks
//! names for clashes; and reads and writes the file clipboard.
//!
//! Small operations (a rename, a new folder, an undo) run one after another
//! on a thread of their own, as the egui app's worker runs everything, so
//! two of them never touch the same item at once. Copies, moves and
//! deletions each get a thread: the core decides when each starts, and can
//! hold one where it is.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use crux_core::Request;
use crux_core::effects::ResolveSink;
use dirfleet_core::app::{
    ClipboardDone, ClipboardOp, Conflicts, FileOpUpdate, FindConflicts, RunFileOp,
};
use dirfleet_core::fileops::{FileOp, FileOpEvent};

use crate::clipboard::{self, Mode};
use crate::fileops::OpControl;
use crate::vfs::Vfs;

type Controls = Arc<Mutex<HashMap<u64, Arc<OpControl>>>>;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs `request` to its end on the calling thread, reporting to `sink`.
fn run_to_end<R>(request: Request<RunFileOp>, control: &OpControl, sink: &Weak<R>, vfs: &Vfs)
where
    R: ResolveSink<RunFileOp> + Send + Sync + 'static,
{
    let RunFileOp { id, op, origin, .. } = request.operation.clone();
    let request = Mutex::new(request);
    let report = |event: FileOpEvent| {
        if let Some(sink) = sink.upgrade() {
            let _ = sink.resolve_request(&mut lock(&request), FileOpUpdate(event));
        }
    };
    let outcome = match &op {
        _ if crate::transfer::reaches_server(&op) => {
            crate::transfer::run(vfs, id, &op, origin, control, &report)
        }
        FileOp::Extract { .. } => crate::extracting::run(vfs, id, &op, origin, control, &report),
        FileOp::Compress { .. } => crate::compressing::run(id, &op, origin, control, &report),
        _ => crate::fileops::execute(id, op, origin, control, &report),
    };
    report(FileOpEvent::Finished(outcome));
}

type Job = (Request<RunFileOp>, Arc<OpControl>);

pub struct FileJobs {
    /// The line of small operations.
    jobs: Sender<Job>,
    /// Starts a thread for a copy, move or deletion.
    start: Box<dyn Fn(Job) + Send + Sync>,
    controls: Controls,
}

impl FileJobs {
    pub fn new<R>(sink: Weak<R>, vfs: Arc<Vfs>) -> Self
    where
        R: ResolveSink<RunFileOp> + Send + Sync + 'static,
    {
        let (jobs, queue) = channel::<Job>();
        let serial_vfs = Arc::clone(&vfs);
        let controls: Controls = Arc::default();
        let finished = Arc::clone(&controls);
        let serial_sink = sink.clone();
        let spawned = std::thread::Builder::new()
            .name("dirfleet_file_ops".into())
            .spawn(move || {
                while let Ok((request, control)) = queue.recv() {
                    let id = request.operation.id;
                    run_to_end(request, &control, &serial_sink, &serial_vfs);
                    lock(&finished).remove(&id);
                }
            });
        if let Err(error) = spawned {
            log::error!("could not start the file operation thread: {error}");
        }
        let finished = Arc::clone(&controls);
        let start = move |(request, control): Job| {
            let id = request.operation.id;
            let sink = sink.clone();
            let finished = Arc::clone(&finished);
            let vfs = Arc::clone(&vfs);
            let spawned = std::thread::Builder::new()
                .name("dirfleet_file_op".into())
                .spawn(move || {
                    run_to_end(request, &control, &sink, &vfs);
                    lock(&finished).remove(&id);
                });
            if let Err(error) = spawned {
                log::error!("could not start a file operation's thread: {error}");
            }
        };
        Self {
            jobs,
            start: Box::new(start),
            controls,
        }
    }

    pub fn run(&self, request: Request<RunFileOp>) {
        let control = Arc::new(OpControl::with_verify(request.operation.verify));
        lock(&self.controls).insert(request.operation.id, Arc::clone(&control));
        // What a server is asked may take its time: never in the line of
        // small operations, which it would hold up.
        let op = &request.operation.op;
        if op.is_bulk() || crate::transfer::reaches_server(op) {
            (self.start)((request, control));
        } else if self.jobs.send((request, control)).is_err() {
            log::error!("the file operation thread is not running");
        }
    }

    pub fn cancel(&self, id: u64) {
        let control = lock(&self.controls).get(&id).cloned();
        if let Some(control) = control {
            control.cancel();
        }
    }

    /// Holds the operation `id` where it is, or lets it go on.
    pub fn pause(&self, id: u64, paused: bool) {
        let control = lock(&self.controls).get(&id).cloned();
        if let Some(control) = control {
            control.set_paused(paused);
        }
    }
}

pub fn find_conflicts<R>(mut request: Request<FindConflicts>, sink: Weak<R>, vfs: Arc<Vfs>)
where
    R: ResolveSink<FindConflicts> + Send + Sync + 'static,
{
    std::thread::spawn(move || {
        let found = Conflicts(crate::transfer::conflicts(&vfs, &request.operation));
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
        let check = FindConflicts {
            srcs: vec![from.join("a.txt"), from.join("b.txt")],
            dst: to,
        };
        let found = crate::transfer::conflicts(&Vfs::default(), &check);
        assert_eq!(found, ["b.txt"]);
    }
}
