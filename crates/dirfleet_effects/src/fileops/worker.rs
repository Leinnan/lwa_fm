//! Background execution of [`FileOp`]s.

use super::{
    ConflictPolicy, FileOp, FileOpEvent, Origin, Outcome, Undo, copy_name, display_name,
    unique_name,
};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Copies are done in chunks of this size so progress and cancel stay live.
const CHUNK: usize = 1024 * 1024;
const REPORT_INTERVAL: Duration = Duration::from_millis(60);

/// How long a copy's speed is measured over before it is told again.
const RATE_INTERVAL: Duration = Duration::from_millis(500);

/// What whoever started an operation can tell it while it runs: to stop, or
/// to hold where it is until told to go on.
#[derive(Debug, Default)]
pub struct OpControl {
    cancel: AtomicBool,
    paused: Mutex<bool>,
    changed: Condvar,
    /// Read each copied file back and compare it with its original.
    verify: bool,
}

impl OpControl {
    /// A control for an operation that checks its copies, or doesn't.
    pub fn with_verify(verify: bool) -> Self {
        Self {
            verify,
            ..Self::default()
        }
    }

    /// Whether copies are read back and compared with their originals.
    pub const fn verifies(&self) -> bool {
        self.verify
    }

    /// Asks the operation to stop; a paused one wakes to do so.
    pub fn cancel(&self) {
        // Taken so a paused operation can't miss the wake-up between its
        // check and its wait.
        let _paused = self.paused.lock().unwrap_or_else(PoisonError::into_inner);
        self.cancel.store(true, Ordering::Relaxed);
        self.changed.notify_all();
    }

    /// Holds the operation at its next step, or lets it go on.
    pub fn set_paused(&self, paused: bool) {
        *self.paused.lock().unwrap_or_else(PoisonError::into_inner) = paused;
        self.changed.notify_all();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Waits here while the operation is paused; says whether it is to stop.
    pub fn wait_if_paused(&self) -> bool {
        self.checkpoint().0
    }

    /// Waits here while the operation is paused. Says whether it is to
    /// stop, and whether it waited.
    fn checkpoint(&self) -> (bool, bool) {
        let mut waited = false;
        let mut paused = self.paused.lock().unwrap_or_else(PoisonError::into_inner);
        while *paused && !self.is_cancelled() {
            waited = true;
            paused = self
                .changed
                .wait(paused)
                .unwrap_or_else(PoisonError::into_inner);
        }
        drop(paused);
        (self.is_cancelled(), waited)
    }
}

struct Job {
    id: u64,
    op: FileOp,
    origin: Origin,
    control: Arc<OpControl>,
}

/// Runs operations one after another on a dedicated thread.
pub struct Worker {
    sender: crossbeam::channel::Sender<Job>,
    next_id: AtomicU64,
    running: Arc<Mutex<HashMap<u64, Arc<OpControl>>>>,
}

impl std::fmt::Debug for Worker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Worker").finish_non_exhaustive()
    }
}

impl Worker {
    /// Starts the worker thread. `report` is called from that thread.
    pub fn new(report: impl Fn(FileOpEvent) + Send + Sync + 'static) -> Self {
        let (sender, receiver) = crossbeam::channel::unbounded::<Job>();
        let running: Arc<Mutex<HashMap<u64, Arc<OpControl>>>> = Arc::default();
        let thread_running = Arc::clone(&running);
        let spawned = std::thread::Builder::new()
            .name("file-ops".into())
            .spawn(move || {
                for job in receiver {
                    let outcome = execute(job.id, job.op, job.origin, &job.control, &report);
                    thread_running
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(&job.id);
                    report(FileOpEvent::Finished(outcome));
                }
            });
        if let Err(error) = spawned {
            log::error!("Could not start the file operation thread: {error}");
        }
        Self {
            sender,
            next_id: AtomicU64::new(1),
            running,
        }
    }

    /// Queues an operation and returns its id.
    pub fn submit(&self, op: FileOp, origin: Origin) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let control = Arc::new(OpControl::default());
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, Arc::clone(&control));
        if self
            .sender
            .send(Job {
                id,
                op,
                origin,
                control,
            })
            .is_err()
        {
            log::error!("The file operation thread is not running");
        }
        id
    }

    /// Asks a queued or running operation to stop.
    pub fn cancel(&self, id: u64) {
        if let Some(control) = self
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
        {
            control.cancel();
        }
    }
}

/// Runs one operation to completion on the calling thread. `control` stops
/// it or holds it between items and between the chunks of a file.
pub fn execute(
    id: u64,
    op: FileOp,
    origin: Origin,
    control: &OpControl,
    report: &dyn Fn(FileOpEvent),
) -> Outcome {
    let description = op.describe();
    report(FileOpEvent::Started {
        id,
        description: description.clone(),
    });
    let mut run = Run {
        id,
        control,
        report,
        done: 0,
        total: 0,
        in_bytes: false,
        verifying: false,
        verified: 0,
        unverified: 0,
        rate: 0,
        rate_from: (Instant::now(), 0),
        // Report the first chunk right away so short operations show progress.
        last_report: Instant::now()
            .checked_sub(REPORT_INTERVAL)
            .unwrap_or_else(Instant::now),
        errors: Vec::new(),
        affected: BTreeSet::new(),
        created: Vec::new(),
    };
    let trash_changed = op.changes_trash();
    let undo = run.op(op);
    Outcome {
        id,
        origin,
        description,
        cancelled: control.is_cancelled(),
        verified: run.verified > 0 && run.unverified == 0 && run.errors.is_empty(),
        needs_password: None,
        errors: run.errors,
        undo,
        affected_dirs: run.affected.into_iter().collect(),
        created: run.created,
        trash_changed,
    }
}

struct Run<'a> {
    id: u64,
    control: &'a OpControl,
    report: &'a dyn Fn(FileOpEvent),
    /// Progress in bytes for copies, in items otherwise.
    done: u64,
    total: u64,
    /// `done` and `total` count bytes.
    in_bytes: bool,
    /// A copy is being read back: progress says so.
    verifying: bool,
    /// Files read back and found the same as their originals, and files
    /// copied without that check (an APFS clone, a link).
    verified: u64,
    unverified: u64,
    /// Bytes a second, as last measured.
    rate: u64,
    /// When the speed was last measured, and `done` then.
    rate_from: (Instant, u64),
    last_report: Instant,
    errors: Vec<String>,
    affected: BTreeSet<PathBuf>,
    created: Vec<PathBuf>,
}

fn interrupted() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "cancelled")
}

fn is_dir(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_dir())
}

/// A directory the user browses into; packages name like files
/// ("Tool copy.app", not "Tool.app copy").
fn is_folder(path: &Path) -> bool {
    is_dir(path) && !crate::fs_enum::is_package(path)
}

fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

pub(super) use crate::fs_ops::remove_any;

/// Best effort: some volumes can't hold every attribute, and the contents
/// are what matters most.
#[cfg(target_os = "macos")]
fn copy_metadata(src: &Path, target: &Path) {
    if let Err(error) = super::clone::copy_metadata(src, target) {
        log::warn!("Could not copy attributes of {}: {error}", src.display());
    }
}

/// Bytes a copy of `path` writes. Links count as nothing, since they are
/// copied as links.
fn tree_size(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.path().symlink_metadata().ok())
        .filter(std::fs::Metadata::is_file)
        .map(|meta| meta.len())
        .sum()
}

/// A path as disks that take one spelling of a name for another tell
/// paths apart: its folder, and its name without regard to case.
fn place(path: &Path) -> (Option<&Path>, String) {
    use unicode_normalization::UnicodeNormalization;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    (
        path.parent(),
        name.nfc().flat_map(char::to_lowercase).collect(),
    )
}

/// A name beside `path` that nothing has, for it to wait under while its
/// own new name is still taken.
fn aside_name(path: &Path, index: usize) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    (0..1000)
        .map(|attempt| path.with_file_name(format!(".{name}.renaming-{index}-{attempt}")))
        .find(|aside| !exists(aside))
        .unwrap_or_else(|| path.to_path_buf())
}

/// Moves aside, under a temporary name, each of `pairs`' first paths that
/// is where another pair's second path is to be: so names can be swapped,
/// or handed down a line. Returns where each first path is now, and tells
/// `moved` of every one that stepped aside (where it is, where it was).
fn step_aside(
    pairs: &[(PathBuf, PathBuf)],
    mut moved: impl FnMut(&Path, &Path),
) -> io::Result<Vec<PathBuf>> {
    let wanted: HashSet<(Option<&Path>, String)> = pairs.iter().map(|(_, to)| place(to)).collect();
    let mut at = Vec::with_capacity(pairs.len());
    for (index, (from, to)) in pairs.iter().enumerate() {
        // A name written another way is in nobody's way but its own.
        if wanted.contains(&place(from)) && place(from) != place(to) {
            let aside = aside_name(from, index);
            fs::rename(from, &aside)?;
            moved(&aside, from);
            at.push(aside);
        } else {
            at.push(from.clone());
        }
    }
    Ok(at)
}

/// Whether `inner` is `outer` or inside it.
fn is_within(inner: &Path, outer: &Path) -> bool {
    match (fs::canonicalize(inner), fs::canonicalize(outer)) {
        (Ok(inner), Ok(outer)) => inner.starts_with(outer),
        _ => inner.starts_with(outer),
    }
}

impl Run<'_> {
    /// Whether to stop. Waits first while the operation is paused.
    fn cancelled(&mut self) -> bool {
        let (cancelled, waited) = self.control.checkpoint();
        if waited {
            // The time spent paused says nothing about the speed.
            self.rate_from = (Instant::now(), self.done);
        }
        cancelled
    }

    /// Measures the copy's speed over the last while.
    fn measure_rate(&mut self) {
        let (since, done_then) = self.rate_from;
        let elapsed = since.elapsed();
        if elapsed < RATE_INTERVAL {
            return;
        }
        let copied = self.done.saturating_sub(done_then);
        self.rate = (copied as f64 / elapsed.as_secs_f64()) as u64;
        self.rate_from = (Instant::now(), self.done);
    }

    fn progress(&mut self, current: &Path, force: bool) {
        if !force && self.last_report.elapsed() < REPORT_INTERVAL {
            return;
        }
        self.last_report = Instant::now();
        let fraction = if self.total == 0 {
            0.0
        } else {
            (self.done as f64 / self.total as f64).min(1.0) as f32
        };
        let (bytes_done, bytes_total, bytes_per_second) = if self.in_bytes {
            self.measure_rate();
            (self.done, self.total, self.rate)
        } else {
            (0, 0, 0)
        };
        let name = display_name(current);
        (self.report)(FileOpEvent::Progress {
            id: self.id,
            fraction,
            current: if self.verifying {
                format!("Verifying {name}")
            } else {
                name
            },
            bytes_done,
            bytes_total,
            bytes_per_second,
        });
    }

    fn fail(&mut self, path: &Path, error: &io::Error) {
        if error.kind() != io::ErrorKind::Interrupted {
            self.errors.push(format!("{}: {error}", display_name(path)));
        }
    }

    fn touch_parent(&mut self, path: &Path) {
        if let Some(parent) = path.parent() {
            self.affected.insert(parent.to_path_buf());
        }
    }

    fn op(&mut self, op: FileOp) -> Option<Undo> {
        match op {
            FileOp::Copy { srcs, dst, policy } => self.copy_or_move(&srcs, &dst, policy, false),
            FileOp::Move { srcs, dst, policy } => self.copy_or_move(&srcs, &dst, policy, true),
            FileOp::Duplicate(srcs) => self.duplicate(&srcs),
            FileOp::CopyPairs { pairs, .. } => self.copy_pairs(&pairs),
            // Run by `extracting::run` and `compressing::run`.
            FileOp::Extract { .. } | FileOp::Compress { .. } => {
                self.errors
                    .push("Archives are made and extracted elsewhere".to_owned());
                None
            }
            FileOp::Trash(paths) => self.trash(&paths),
            FileOp::Delete(paths) => {
                self.delete(&paths);
                None
            }
            FileOp::RestoreTrashed(keys) => self.restore_trashed(&keys),
            FileOp::PurgeTrashed(keys) => {
                self.bin_result(super::trash_bin::purge(&keys));
                None
            }
            FileOp::EmptyTrash => {
                self.bin_result(super::trash_bin::empty());
                None
            }
            FileOp::Rename { from, to } => self.rename(&from, &to),
            FileOp::RenameMany(pairs) => self.rename_many(&pairs),
            FileOp::NewFolder { parent } => self.create(&parent, true),
            FileOp::NewFile { parent } => self.create(&parent, false),
            FileOp::Undo(undo) => self.undo(undo),
        }
    }

    /// Where `src` should go inside `dst`, or `None` to skip it.
    fn target_for(src: &Path, dst: &Path, policy: ConflictPolicy, moving: bool) -> Option<PathBuf> {
        let name = src.file_name()?.to_string_lossy().into_owned();
        if src.parent() == Some(dst) {
            // Moving into its own folder does nothing; copying makes "- Copy".
            return (!moving).then(|| copy_name(dst, &name, is_folder(src)));
        }
        let target = dst.join(&name);
        if !exists(&target) {
            return Some(target);
        }
        match policy {
            ConflictPolicy::Replace => Some(target),
            ConflictPolicy::Skip => None,
            ConflictPolicy::KeepBoth => Some(unique_name(dst, &name, is_folder(src))),
        }
    }

    fn copy_or_move(
        &mut self,
        srcs: &[PathBuf],
        dst: &Path,
        policy: ConflictPolicy,
        moving: bool,
    ) -> Option<Undo> {
        self.affected.insert(dst.to_path_buf());
        self.total = if moving {
            srcs.len() as u64
        } else {
            srcs.iter().map(|src| tree_size(src)).sum()
        };
        self.in_bytes = !moving;
        let mut moved = Vec::new();
        for src in srcs {
            if self.cancelled() {
                break;
            }
            if is_dir(src) && is_within(dst, src) {
                self.errors.push(format!(
                    "{}: a folder can't be {} into itself",
                    display_name(src),
                    if moving { "moved" } else { "copied" }
                ));
                continue;
            }
            let Some(target) = Self::target_for(src, dst, policy, moving) else {
                continue;
            };
            let replacing = exists(&target);
            let result = if moving {
                self.touch_parent(src);
                self.move_item(src, &target, replacing)
            } else {
                self.copy_item(src, &target)
            };
            match result {
                Ok(()) if moving => {
                    self.done += 1;
                    // A merge into an existing item can't be moved back cleanly.
                    if !replacing {
                        moved.push((target.clone(), src.clone()));
                    }
                }
                Ok(()) if !replacing => self.created.push(target.clone()),
                Ok(()) => {}
                Err(error) => self.fail(src, &error),
            }
            self.progress(&target, true);
        }
        if moving {
            self.created = moved.iter().map(|(now, _)| now.clone()).collect();
            (!moved.is_empty()).then_some(Undo::Moves(moved))
        } else {
            (!self.created.is_empty()).then(|| Undo::Created(self.created.clone()))
        }
    }

    /// Copies each file to the place named for it, replacing a file already
    /// there and making the folders it needs on the way. What replaced
    /// nothing is undone with the rest; a folder made here is undone whole.
    fn copy_pairs(&mut self, pairs: &[(PathBuf, PathBuf)]) -> Option<Undo> {
        self.total = pairs
            .iter()
            .map(|(src, _)| src.symlink_metadata().map_or(0, |meta| meta.len()))
            .sum();
        self.in_bytes = true;
        // Folders made here: what is copied into them goes when they do.
        let mut made: HashSet<PathBuf> = HashSet::new();
        for (src, target) in pairs {
            if self.cancelled() {
                break;
            }
            let Some(parent) = target.parent() else {
                continue;
            };
            // The topmost folder that has to be made for this copy.
            let top = parent
                .ancestors()
                .take_while(|dir| !exists(dir))
                .last()
                .map(Path::to_path_buf);
            if let Some(top) = top {
                if let Err(error) = fs::create_dir_all(parent) {
                    self.fail(src, &error);
                    continue;
                }
                self.touch_parent(&top);
                // A folder made inside one made here goes with that one.
                let in_new_folder = top.ancestors().skip(1).any(|dir| made.contains(dir));
                if !in_new_folder && made.insert(top.clone()) {
                    self.created.push(top);
                }
            }
            let replacing = exists(target);
            match self.copy_item(src, target) {
                Ok(()) => {
                    let in_new_folder = target.ancestors().skip(1).any(|dir| made.contains(dir));
                    if !replacing && !in_new_folder {
                        self.created.push(target.clone());
                    }
                }
                Err(error) => self.fail(src, &error),
            }
            self.touch_parent(target);
            self.progress(target, true);
        }
        (!self.created.is_empty()).then(|| Undo::Created(self.created.clone()))
    }

    fn move_item(&mut self, src: &Path, target: &Path, replacing: bool) -> io::Result<()> {
        if !replacing {
            match fs::rename(src, target) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() != io::ErrorKind::CrossesDevices => return Err(error),
                Err(_) => {}
            }
        }
        // Another volume, or merging into an existing item: copy, then remove.
        self.copy_item(src, target)?;
        remove_any(src)
    }

    fn copy_item(&mut self, src: &Path, target: &Path) -> io::Result<()> {
        if self.cancelled() {
            return Err(interrupted());
        }
        // An APFS clone copies a whole tree, metadata included, at once.
        #[cfg(target_os = "macos")]
        if !exists(target) && super::clone::clone(src, target).is_ok() {
            self.done += tree_size(src);
            self.unverified += 1;
            self.progress(src, false);
            return Ok(());
        }
        // Links are copied as links, so bundles keep `Versions/Current` and
        // the like instead of growing duplicate trees.
        #[cfg(unix)]
        if fs::symlink_metadata(src)?.file_type().is_symlink() {
            if exists(target) {
                remove_any(target)?;
            }
            return std::os::unix::fs::symlink(fs::read_link(src)?, target);
        }
        let meta = fs::metadata(src)?;
        if meta.is_dir() {
            if exists(target) && !is_dir(target) {
                fs::remove_file(target)?;
            }
            fs::create_dir_all(target)?;
            for entry in fs::read_dir(src)? {
                let entry = entry?;
                self.copy_item(&entry.path(), &target.join(entry.file_name()))?;
            }
            // Last, so a read-only folder still takes its children first.
            #[cfg(target_os = "macos")]
            copy_metadata(src, target);
            fs::set_permissions(target, meta.permissions())
        } else {
            if is_dir(target) {
                fs::remove_dir_all(target)?;
            }
            self.copy_file(src, target, &meta)
        }
    }

    fn copy_file(&mut self, src: &Path, target: &Path, meta: &fs::Metadata) -> io::Result<()> {
        let result = (|| {
            let mut reader = File::open(src)?;
            let mut writer = File::create(target)?;
            let mut buffer =
                vec![0; CHUNK.min(usize::try_from(meta.len()).unwrap_or(CHUNK).max(1))];
            // What was read, to compare with what ends up written.
            let mut read_sum = self.control.verify.then(blake3::Hasher::new);
            loop {
                if self.cancelled() {
                    return Err(interrupted());
                }
                let read = reader.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                writer.write_all(&buffer[..read])?;
                if let Some(sum) = &mut read_sum {
                    sum.update(&buffer[..read]);
                }
                self.done += read as u64;
                self.progress(src, false);
            }
            if let Ok(modified) = meta.modified() {
                let _ = writer.set_times(fs::FileTimes::new().set_modified(modified));
            }
            if read_sum.is_some() {
                // On the disk, not in a cache, before it is read back.
                writer.sync_all()?;
            }
            drop(writer);
            if let Some(sum) = read_sum {
                self.verify_copy(src, target, &sum.finalize())?;
            }
            #[cfg(target_os = "macos")]
            copy_metadata(src, target);
            fs::set_permissions(target, meta.permissions())
        })();
        if result.is_err() {
            // Don't leave a partial copy behind.
            let _ = fs::remove_file(target);
        }
        result
    }

    /// Reads the copy at `target` back and compares it with what was read
    /// from `src`.
    fn verify_copy(&mut self, src: &Path, target: &Path, read: &blake3::Hash) -> io::Result<()> {
        self.verifying = true;
        self.progress(src, true);
        let written = crate::hashing::fingerprint(target, || self.cancelled());
        self.verifying = false;
        match written? {
            None => Err(interrupted()),
            Some(written) if written == *read => {
                self.verified += 1;
                Ok(())
            }
            Some(_) => Err(io::Error::other(
                "the copy is not the same as the original and was removed",
            )),
        }
    }

    fn duplicate(&mut self, srcs: &[PathBuf]) -> Option<Undo> {
        self.total = srcs.iter().map(|src| tree_size(src)).sum();
        self.in_bytes = true;
        for src in srcs {
            if self.cancelled() {
                break;
            }
            let (Some(parent), Some(name)) = (src.parent(), src.file_name()) else {
                continue;
            };
            let target = copy_name(parent, &name.to_string_lossy(), is_folder(src));
            self.touch_parent(src);
            match self.copy_item(src, &target) {
                Ok(()) => self.created.push(target),
                Err(error) => self.fail(src, &error),
            }
        }
        (!self.created.is_empty()).then(|| Undo::Created(self.created.clone()))
    }

    fn trash(&mut self, paths: &[PathBuf]) -> Option<Undo> {
        for path in paths {
            self.touch_parent(path);
        }
        match trash::delete_all(paths) {
            Ok(()) => Some(Undo::Trashed(paths.to_vec())),
            Err(error) => {
                self.errors
                    .push(format!("Could not move to the Recycle Bin: {error}"));
                None
            }
        }
    }

    /// Puts items from the Recycle Bin view back; undone by trashing them again.
    fn restore_trashed(&mut self, keys: &[super::trash_bin::TrashKey]) -> Option<Undo> {
        for key in keys {
            self.touch_parent(&key.path);
        }
        let restored = self.bin_result(super::trash_bin::restore(keys))?;
        self.created.clone_from(&restored);
        Some(Undo::Created(restored))
    }

    fn bin_result<T>(&mut self, result: anyhow::Result<T>) -> Option<T> {
        result
            .map_err(|error| self.errors.push(error.to_string()))
            .ok()
    }

    fn delete(&mut self, paths: &[PathBuf]) {
        self.total = paths.len() as u64;
        for path in paths {
            if self.cancelled() {
                break;
            }
            self.touch_parent(path);
            if let Err(error) = remove_any(path) {
                self.fail(path, &error);
            }
            self.done += 1;
            self.progress(path, false);
        }
    }

    fn rename(&mut self, from: &Path, to: &Path) -> Option<Undo> {
        self.touch_parent(from);
        self.touch_parent(to);
        // A change of letter case only is allowed on case-insensitive disks.
        if exists(to) && !super::is_respelling(from, to) {
            self.errors
                .push(format!("{} already exists", display_name(to)));
            return None;
        }
        match fs::rename(from, to) {
            Ok(()) => {
                self.created.push(to.to_path_buf());
                Some(Undo::Rename {
                    from: to.to_path_buf(),
                    to: from.to_path_buf(),
                })
            }
            Err(error) => {
                self.fail(from, &error);
                None
            }
        }
    }

    /// Renames every pair, as one step: all of them, or none. Names may be
    /// swapped or handed down a line (a to b, b to c): what is in the way of
    /// another's new name waits under a temporary one. When a rename fails,
    /// or the operation is stopped, the renames done are taken back.
    fn rename_many(&mut self, pairs: &[(PathBuf, PathBuf)]) -> Option<Undo> {
        let pairs: Vec<(PathBuf, PathBuf)> = pairs
            .iter()
            .filter(|(from, to)| from != to)
            .cloned()
            .collect();
        self.total = pairs.len() as u64;
        // Every rename done: where the item is, and where it was.
        let mut steps: Vec<(PathBuf, PathBuf)> = Vec::new();
        let renamed = (|| -> io::Result<()> {
            let at = step_aside(&pairs, |now, was| {
                steps.push((now.to_path_buf(), was.to_path_buf()));
            })?;
            for ((from, to), now) in pairs.iter().zip(&at) {
                if self.cancelled() {
                    return Err(interrupted());
                }
                self.touch_parent(from);
                self.touch_parent(to);
                // A change of letter case only is allowed on
                // case-insensitive disks.
                let respelled = now == from && super::is_respelling(from, to);
                if exists(to) && !respelled {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("{} already exists", display_name(to)),
                    ));
                }
                fs::rename(now, to).map_err(|error| {
                    io::Error::new(error.kind(), format!("{}: {error}", display_name(from)))
                })?;
                steps.push((to.clone(), now.clone()));
                self.done += 1;
                self.progress(to, false);
            }
            Ok(())
        })();
        if let Err(error) = renamed {
            if error.kind() != io::ErrorKind::Interrupted {
                self.errors.push(error.to_string());
            }
            // Everything back where it was, the last rename first.
            for (now, was) in steps.into_iter().rev() {
                if let Err(error) = fs::rename(&now, &was) {
                    self.errors.push(format!(
                        "{} could not be given its name back: {error}",
                        display_name(&was)
                    ));
                }
            }
            return None;
        }
        self.created = pairs.iter().map(|(_, to)| to.clone()).collect();
        // Undo moves each item from its new name back to the old one.
        let back: Vec<(PathBuf, PathBuf)> =
            pairs.into_iter().map(|(from, to)| (to, from)).collect();
        (!back.is_empty()).then_some(Undo::Moves(back))
    }

    fn create(&mut self, parent: &Path, folder: bool) -> Option<Undo> {
        self.affected.insert(parent.to_path_buf());
        let (name, result) = if folder {
            let path = unique_name(parent, "New folder", true);
            let result = fs::create_dir(&path);
            (path, result)
        } else {
            let path = unique_name(parent, "New Text Document.txt", false);
            let result = File::create_new(&path).map(drop);
            (path, result)
        };
        match result {
            Ok(()) => {
                self.created.push(name.clone());
                Some(Undo::Created(vec![name]))
            }
            Err(error) => {
                self.fail(&name, &error);
                None
            }
        }
    }

    fn undo(&mut self, undo: Undo) -> Option<Undo> {
        match undo {
            Undo::Rename { from, to } => self.rename(&from, &to),
            Undo::Moves(pairs) => {
                self.total = pairs.len() as u64;
                let mut moved = Vec::new();
                // What is where another is to go back to waits aside, so
                // swapped names swap back.
                let at = step_aside(&pairs, |_, _| {})
                    .unwrap_or_else(|_| pairs.iter().map(|(now, _)| now.clone()).collect());
                for ((now, original), at) in pairs.into_iter().zip(at) {
                    self.touch_parent(&now);
                    self.touch_parent(&original);
                    let respelled = at == now && super::is_respelling(&now, &original);
                    let result = if exists(&original) && !respelled {
                        Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            "its old name is taken",
                        ))
                    } else {
                        self.move_item(&at, &original, false)
                    };
                    match result {
                        Ok(()) => moved.push((original, now)),
                        Err(error) => {
                            self.errors
                                .push(format!("{}: {error}", display_name(&original)));
                            // What waited aside takes its name back.
                            if at != now {
                                let _ = fs::rename(&at, &now);
                            }
                        }
                    }
                    self.done += 1;
                }
                self.created = moved.iter().map(|(now, _)| now.clone()).collect();
                (!moved.is_empty()).then_some(Undo::Moves(moved))
            }
            Undo::Created(paths) => {
                let existing: Vec<_> = paths.into_iter().filter(|p| exists(p)).collect();
                if existing.is_empty() {
                    return None;
                }
                self.trash(&existing)
            }
            Undo::Trashed(paths) => self.restore(&paths),
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn restore(&mut self, paths: &[PathBuf]) -> Option<Undo> {
        for path in paths {
            self.touch_parent(path);
        }
        let items = match trash::os_limited::list() {
            Ok(items) => items,
            Err(error) => {
                self.errors
                    .push(format!("Could not read the Recycle Bin: {error}"));
                return None;
            }
        };
        // The most recently deleted item for each original path.
        let mut latest: HashMap<PathBuf, trash::TrashItem> = HashMap::new();
        for item in items {
            let Some(original) = paths.iter().find(|path| was_trashed_from(&item, path)) else {
                continue;
            };
            let original = original.clone();
            if latest
                .get(&original)
                .is_none_or(|known| known.time_deleted < item.time_deleted)
            {
                latest.insert(original, item);
            }
        }
        if latest.is_empty() {
            self.errors
                .push("The items are no longer in the Recycle Bin".to_owned());
            return None;
        }
        let restored: Vec<PathBuf> = latest.keys().cloned().collect();
        // Where each item actually reappears, which differs from the original
        // path when the display name dropped the extension.
        let landing: Vec<(PathBuf, PathBuf)> = latest
            .iter()
            .map(|(original, item)| (item.original_path(), original.clone()))
            .filter(|(landed, original)| landed != original)
            .collect();
        match trash::os_limited::restore_all(latest.into_values()) {
            Ok(()) => {
                for (landed, original) in landing {
                    if exists(&landed)
                        && !exists(&original)
                        && let Err(error) = fs::rename(&landed, &original)
                    {
                        self.fail(&landed, &error);
                    }
                }
                self.created.clone_from(&restored);
                Some(Undo::Created(restored))
            }
            Err(error) => {
                self.errors.push(format!("Could not restore: {error}"));
                None
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn restore(&mut self, _paths: &[PathBuf]) -> Option<Undo> {
        self.errors
            .push("Restoring from the Trash isn't supported on macOS".to_owned());
        None
    }
}

/// Whether a Recycle Bin entry was deleted from `path`. Windows reports the
/// display name, which leaves out the extension when Explorer hides known
/// extensions, so the stem is accepted too.
#[cfg(not(target_os = "macos"))]
fn was_trashed_from(item: &trash::TrashItem, path: &Path) -> bool {
    if path.parent() != Some(item.original_parent.as_path()) {
        return false;
    }
    path.file_name() == Some(item.name.as_os_str())
        || (cfg!(windows)
            && path.extension().is_some()
            && path.file_stem() == Some(item.name.as_os_str()))
}
