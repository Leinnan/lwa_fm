//! Writing staged files. Creating a file, and the folder it goes in, costs
//! far more than writing a few kilobytes, so small files are buffered and
//! handed to a few writer threads; larger ones are written as they are
//! decoded. Either way times, mode and quarantine are set on the open handle
//! rather than by path later.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::SystemTime;

use crate::error::CoreError;
use crate::extract::Progress;
use crate::platform;

/// Files up to this size are written by the pool.
const POOLED_MAX: usize = 256 << 10;
const WRITERS: usize = 4;
/// Queued files per writer; bounds memory to WRITERS × QUEUE × POOLED_MAX.
const QUEUE: usize = 64;

/// Metadata applied to a file once its contents are written.
#[derive(Clone, Default)]
pub(crate) struct FileMeta {
    pub mtime: Option<SystemTime>,
    pub mode: Option<u32>,
    pub quarantine: Option<Arc<[u8]>>,
}

impl FileMeta {
    /// Best effort: an archive's timestamps and modes never fail an extraction.
    /// `path` is where `file` is.
    pub fn apply(&self, file: &fs::File, path: &Path) {
        // The Windows mark is a second stream of the file, and writing it
        // changes the modification time, so it goes first there.
        #[cfg(windows)]
        if let Some(value) = &self.quarantine {
            platform::set_file_quarantine(file, path, value);
        }
        if let Some(mtime) = self.mtime {
            let _ = file.set_times(fs::FileTimes::new().set_modified(mtime));
        }
        if let Some(mode) = self.mode {
            platform::set_file_mode(file, mode);
        }
        #[cfg(not(windows))]
        if let Some(value) = &self.quarantine {
            platform::set_file_quarantine(file, path, value);
        }
    }
}

struct Job {
    path: PathBuf,
    data: Vec<u8>,
    meta: FileMeta,
}

#[derive(Default)]
struct Shared {
    pending: AtomicUsize,
    idle: (Mutex<()>, Condvar),
    failure: Mutex<Option<CoreError>>,
}

impl Shared {
    fn done(&self) {
        if self.pending.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _guard = self.idle.0.lock();
            self.idle.1.notify_all();
        }
    }
}

/// Writer threads for one extraction. Dropping the pool waits for every
/// queued file, so nothing writes into staging after it is gone.
pub(crate) struct WriterPool {
    senders: Vec<SyncSender<Job>>,
    threads: Vec<JoinHandle<()>>,
    shared: Arc<Shared>,
}

impl WriterPool {
    pub fn new() -> Arc<Self> {
        let shared = Arc::new(Shared::default());
        let (senders, threads) = (0..WRITERS)
            .map(|i| {
                let (sender, receiver) = sync_channel::<Job>(QUEUE);
                let shared = shared.clone();
                let thread = std::thread::Builder::new()
                    .name(format!("uunpack-writer-{i}"))
                    .spawn(move || {
                        for job in receiver {
                            if let Err(e) = write_file(&job) {
                                lock(&shared.failure).get_or_insert(CoreError::io(&job.path, e));
                            }
                            shared.done();
                        }
                    })
                    .expect("spawn writer thread");
                (sender, thread)
            })
            .unzip();
        Arc::new(Self {
            senders,
            threads,
            shared,
        })
    }

    /// Queues a file. Files in the same folder go to the same writer: the
    /// writers don't contend for a folder, and a later copy of a name still
    /// replaces an earlier one.
    fn submit(&self, job: Job) {
        let mut hasher = DefaultHasher::new();
        job.path.parent().hash(&mut hasher);
        let writer = hasher.finish() as usize % self.senders.len();
        self.shared.pending.fetch_add(1, Ordering::AcqRel);
        if self.senders[writer].send(job).is_err() {
            self.shared.done();
        }
    }

    /// Waits until every queued file is on disk.
    pub fn wait_idle(&self) {
        let mut guard = lock(&self.shared.idle.0);
        while self.shared.pending.load(Ordering::Acquire) > 0 {
            guard = self
                .shared
                .idle
                .1
                .wait(guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// The first error a writer ran into.
    pub fn take_failure(&self) -> Option<CoreError> {
        lock(&self.shared.failure).take()
    }

    /// A writer for the file at `path`.
    pub fn file(
        self: &Arc<Self>,
        path: PathBuf,
        size_hint: u64,
        meta: FileMeta,
        progress: Arc<Progress>,
    ) -> io::Result<StagedFile> {
        let state = if size_hint as usize <= POOLED_MAX {
            State::Buffer(Vec::with_capacity(size_hint as usize))
        } else {
            State::File(create(&path)?)
        };
        Ok(StagedFile {
            state,
            path,
            meta,
            progress,
            pool: self.clone(),
        })
    }
}

impl Drop for WriterPool {
    fn drop(&mut self) {
        self.senders.clear();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// Creates the file at `path`, and its folder when that doesn't exist yet.
fn create(path: &Path) -> io::Result<fs::File> {
    match fs::File::create(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::File::create(path)
        }
        other => other,
    }
}

fn write_file(job: &Job) -> io::Result<()> {
    let mut file = create(&job.path)?;
    file.write_all(&job.data)?;
    job.meta.apply(&file, &job.path);
    Ok(())
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

enum State {
    Buffer(Vec<u8>),
    File(fs::File),
    Done,
}

/// A file being staged: buffered for the pool while small, written directly
/// once it outgrows the buffer.
pub(crate) struct StagedFile {
    state: State,
    path: PathBuf,
    meta: FileMeta,
    progress: Arc<Progress>,
    pool: Arc<WriterPool>,
}

impl Write for StagedFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let State::Buffer(data) = &mut self.state
            && data.len() + buf.len() > POOLED_MAX
        {
            let mut file = create(&self.path)?;
            file.write_all(data)?;
            self.state = State::File(file);
        }
        match &mut self.state {
            State::Buffer(data) => data.extend_from_slice(buf),
            State::File(file) => file.write_all(buf)?,
            State::Done => return Err(io::Error::other("file already finished")),
        }
        self.progress.add(buf.len() as u64);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        match std::mem::replace(&mut self.state, State::Done) {
            State::Buffer(data) => self.pool.submit(Job {
                path: std::mem::take(&mut self.path),
                data,
                meta: std::mem::take(&mut self.meta),
            }),
            State::File(file) => self.meta.apply(&file, &self.path),
            State::Done => {}
        }
    }
}
