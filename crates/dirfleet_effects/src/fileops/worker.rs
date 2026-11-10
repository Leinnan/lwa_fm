//! Background execution of [`FileOp`]s.

use super::{
    ConflictPolicy, FileOp, FileOpEvent, Origin, Outcome, Undo, copy_name, display_name,
    unique_name,
};
use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Copies are done in chunks of this size so progress and cancel stay live.
const CHUNK: usize = 1024 * 1024;
const REPORT_INTERVAL: Duration = Duration::from_millis(60);

struct Job {
    id: u64,
    op: FileOp,
    origin: Origin,
    cancel: Arc<AtomicBool>,
}

/// Runs operations one after another on a dedicated thread.
pub struct Worker {
    sender: crossbeam::channel::Sender<Job>,
    next_id: AtomicU64,
    running: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>,
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
        let running: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>> = Arc::default();
        let thread_running = Arc::clone(&running);
        let spawned = std::thread::Builder::new()
            .name("file-ops".into())
            .spawn(move || {
                for job in receiver {
                    let outcome = execute(job.id, job.op, job.origin, &job.cancel, &report);
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
        let cancel = Arc::new(AtomicBool::new(false));
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, Arc::clone(&cancel));
        if self
            .sender
            .send(Job {
                id,
                op,
                origin,
                cancel,
            })
            .is_err()
        {
            log::error!("The file operation thread is not running");
        }
        id
    }

    /// Asks a queued or running operation to stop.
    pub fn cancel(&self, id: u64) {
        if let Some(cancel) = self
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
        {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// Runs one operation to completion on the calling thread.
pub fn execute(
    id: u64,
    op: FileOp,
    origin: Origin,
    cancel: &AtomicBool,
    report: &dyn Fn(FileOpEvent),
) -> Outcome {
    let description = op.describe();
    report(FileOpEvent::Started {
        id,
        description: description.clone(),
    });
    let mut run = Run {
        id,
        cancel,
        report,
        done: 0,
        total: 0,
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
        cancelled: cancel.load(Ordering::Relaxed),
        errors: run.errors,
        undo,
        affected_dirs: run.affected.into_iter().collect(),
        created: run.created,
        trash_changed,
    }
}

struct Run<'a> {
    id: u64,
    cancel: &'a AtomicBool,
    report: &'a dyn Fn(FileOpEvent),
    /// Progress in bytes for copies, in items otherwise.
    done: u64,
    total: u64,
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

/// Whether `inner` is `outer` or inside it.
fn is_within(inner: &Path, outer: &Path) -> bool {
    match (fs::canonicalize(inner), fs::canonicalize(outer)) {
        (Ok(inner), Ok(outer)) => inner.starts_with(outer),
        _ => inner.starts_with(outer),
    }
}

impl Run<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
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
        (self.report)(FileOpEvent::Progress {
            id: self.id,
            fraction,
            current: display_name(current),
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
            loop {
                if self.cancelled() {
                    return Err(interrupted());
                }
                let read = reader.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                writer.write_all(&buffer[..read])?;
                self.done += read as u64;
                self.progress(src, false);
            }
            if let Ok(modified) = meta.modified() {
                let _ = writer.set_times(fs::FileTimes::new().set_modified(modified));
            }
            drop(writer);
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

    fn duplicate(&mut self, srcs: &[PathBuf]) -> Option<Undo> {
        self.total = srcs.iter().map(|src| tree_size(src)).sum();
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

    fn rename_many(&mut self, pairs: &[(PathBuf, PathBuf)]) -> Option<Undo> {
        self.total = pairs.len() as u64;
        let mut done = Vec::new();
        for (from, to) in pairs {
            if self.cancelled() {
                break;
            }
            if from == to {
                continue;
            }
            if self.rename(from, to).is_some() {
                // Undo moves each item from its new name back to the old one.
                done.push((to.clone(), from.clone()));
            }
            self.done += 1;
            self.progress(to, false);
        }
        (!done.is_empty()).then_some(Undo::Moves(done))
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
                for (now, original) in pairs {
                    self.touch_parent(&now);
                    self.touch_parent(&original);
                    if exists(&original) {
                        self.errors
                            .push(format!("{} already exists", original.display()));
                        continue;
                    }
                    match self.move_item(&now, &original, false) {
                        Ok(()) => moved.push((original, now)),
                        Err(error) => self.fail(&now, &error),
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
