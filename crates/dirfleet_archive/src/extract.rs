use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rars::ReadCancellation;

use crate::archive::{ArchiveHandle, Kind, Member};
use crate::destination::unique_sibling;
use crate::error::{CoreError, Result};
use crate::formats::{Attributes, Job, Link, Sink};
use crate::names;
use crate::output::{FileMeta, WriterPool};
use crate::platform;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    Overwrite,
    Skip,
    KeepBoth,
}

#[derive(Debug, Clone)]
pub struct ExtractRequest {
    pub output_dir: String,
    /// Entry ids to extract, folders including everything inside; `None` for all.
    pub selection: Option<Vec<u32>>,
    /// Overrides the password the archive was opened with.
    pub password: Option<String>,
    pub conflict: ConflictPolicy,
    pub restore_metadata: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ExtractSummary {
    pub output_dir: String,
    pub files: u64,
    pub bytes: u64,
    /// Existing paths left untouched under `ConflictPolicy::Skip`.
    pub skipped_existing: Vec<String>,
    /// Items placed under a new name: `ConflictPolicy::KeepBoth`, or names
    /// that differ only in case and would overwrite each other.
    pub renamed: Vec<String>,
    /// Members whose names would escape the destination.
    pub skipped_unsafe: Vec<String>,
    /// Links that point outside the destination or can't be recreated.
    pub skipped_links: Vec<String>,
    /// Final paths of the top-level items placed in `output_dir`.
    pub top_level_paths: Vec<String>,
}

pub trait ProgressObserver: Send + Sync {
    /// Called at most ~30 times a second, and once more at the end.
    fn on_progress(&self, done_bytes: u64, total_bytes: u64, current_file: String);
}

struct NoProgress;

impl ProgressObserver for NoProgress {
    fn on_progress(&self, _: u64, _: u64, _: String) {}
}

/// Cancels a running extraction from any thread.
#[derive(Debug, Default)]
pub struct CancelToken(pub(crate) ReadCancellation);

impl CancelToken {
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    pub fn cancel(&self) {
        self.0.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

impl ArchiveHandle {
    /// Relative paths that already exist in `output_dir` and would collide.
    pub fn find_conflicts(&self, output_dir: String, selection: Option<Vec<u32>>) -> Vec<String> {
        let output_dir = Path::new(&output_dir);
        let wanted = self.wanted_members(selection.as_deref());
        self.members
            .iter()
            .zip(wanted)
            .filter(|(_, wanted)| *wanted)
            .filter_map(|(m, _)| m.rel_path.as_ref().map(|rel| (m, rel)))
            .filter(|(m, rel)| match output_dir.join(rel).symlink_metadata() {
                Ok(existing) => !(existing.is_dir() && m.kind == Kind::Dir),
                Err(_) => false,
            })
            .map(|(_, rel)| names::display_rel(rel))
            .collect()
    }

    /// Unpacked bytes a request for `selection` writes, for a free-space check.
    pub fn selected_size(&self, selection: Option<Vec<u32>>) -> u64 {
        let wanted = self.wanted_members(selection.as_deref());
        self.members
            .iter()
            .zip(wanted)
            .filter(|(m, wanted)| *wanted && m.kind != Kind::Dir)
            .map(|(m, _)| m.size)
            .sum()
    }

    /// Extracts into a hidden staging folder inside `output_dir`, then moves the
    /// result into place. On failure or cancel the staging folder is removed,
    /// so no partial files are left behind.
    pub fn extract(
        &self,
        request: ExtractRequest,
        observer: Arc<dyn ProgressObserver>,
        cancel: Arc<CancelToken>,
    ) -> Result<ExtractSummary> {
        crate::with_large_stack(|| self.extract_with(request, observer, &cancel))
    }

    /// Extracts `ids` into `temp_dir`, replacing earlier copies, for opening
    /// or previewing. Returns the extracted path of each id, in order.
    pub fn extract_to_temp(
        &self,
        ids: Vec<u32>,
        temp_dir: String,
        password: Option<String>,
    ) -> Result<Vec<String>> {
        let request = ExtractRequest {
            output_dir: temp_dir.clone(),
            selection: Some(ids.clone()),
            password,
            conflict: ConflictPolicy::Overwrite,
            restore_metadata: true,
        };
        crate::with_large_stack(|| {
            self.extract_with(request, Arc::new(NoProgress), &CancelToken::default())
        })?;
        Ok(ids
            .iter()
            .filter_map(|&id| self.entries.get(id as usize))
            .map(|entry| {
                Path::new(&temp_dir)
                    .join(names::native_rel(&entry.path))
                    .display()
                    .to_string()
            })
            .collect())
    }

    /// Decodes and checks every member without writing anything. Decoding
    /// into counting sinks verifies the same checksums as extraction and
    /// reports progress within large files too.
    pub fn verify(
        &self,
        password: Option<String>,
        observer: Arc<dyn ProgressObserver>,
        cancel: Arc<CancelToken>,
    ) -> Result<()> {
        crate::with_large_stack(|| {
            let wanted = vec![true; self.members.len()];
            let total = self.backend.planned_bytes(&self.members, &wanted);
            let sink = VerifySink {
                handle: self,
                progress: Arc::new(Progress::new(observer, total)),
            };
            let password = password.as_ref().or(self.password.as_ref());
            let job = Job {
                members: &self.members,
                wanted: &wanted,
                password: password.map(|p| p.as_bytes()),
                cancel: &cancel,
            };
            self.backend
                .verify(&job, &sink)
                .map_err(|e| self.with_archive_path(e))?;
            sink.progress.finish();
            Ok(())
        })
    }
}

impl ArchiveHandle {
    fn extract_with(
        &self,
        request: ExtractRequest,
        observer: Arc<dyn ProgressObserver>,
        cancel: &CancelToken,
    ) -> Result<ExtractSummary> {
        let output_dir = PathBuf::from(&request.output_dir);
        if !output_dir.is_absolute() {
            return Err(CoreError::InvalidInput {
                message: format!(
                    "Destination must be an absolute path: {}",
                    request.output_dir
                ),
            });
        }
        let created_output = output_dir.symlink_metadata().is_err();
        fs::create_dir_all(&output_dir).map_err(|e| CoreError::io(&output_dir, e))?;
        remove_stale_staging(&output_dir);

        let result = Staging::create(&output_dir).and_then(|staging| {
            let mut summary = self.extract_into(&staging.path, &request, observer, cancel)?;
            publish(&staging.path, &output_dir, request.conflict, &mut summary)?;
            summary.output_dir = output_dir.display().to_string();
            Ok(summary)
        });
        if result.is_err() && created_output {
            let _ = fs::remove_dir(&output_dir);
        }
        result
    }

    fn extract_into(
        &self,
        staging: &Path,
        request: &ExtractRequest,
        observer: Arc<dyn ProgressObserver>,
        cancel: &CancelToken,
    ) -> Result<ExtractSummary> {
        let wanted = self.wanted_members(request.selection.as_deref());
        let total = self.backend.planned_bytes(&self.members, &wanted);
        let run = Run {
            handle: self,
            staging,
            progress: Arc::new(Progress::new(observer, total)),
            restore: request.restore_metadata,
            // Archive Utility marks everything extracted from a quarantined
            // archive, so Gatekeeper checks extracted apps and scripts.
            quarantine: platform::quarantine(Path::new(&self.info().path)).map(Arc::from),
            case_sensitive: platform::case_sensitive(staging),
            pool: WriterPool::new(),
            staged: Mutex::default(),
            folders: Mutex::default(),
            written: Mutex::default(),
            renamed: Mutex::default(),
            skipped_links: Mutex::default(),
            xattrs: Mutex::default(),
            #[cfg(windows)]
            deferred_links: Mutex::default(),
        };
        let password = request.password.as_ref().or(self.password.as_ref());
        let job = Job {
            members: &self.members,
            wanted: &wanted,
            password: password.map(|p| p.as_bytes()),
            cancel,
        };
        let outcome = self.backend.extract(&job, &run);
        // Every queued file is on disk before anything looks at the staging folder.
        run.pool.wait_idle();
        outcome
            .and_then(|()| run.pool.take_failure().map_or(Ok(()), Err))
            .map_err(|e| self.with_archive_path(e))?;
        run.progress.finish();
        #[cfg(windows)]
        run.finish_links();

        let written = into_inner(run.written);
        let xattrs = into_inner(run.xattrs);
        if !xattrs.is_empty() {
            let staged: HashMap<usize, &PathBuf> =
                written.iter().map(|(i, rel)| (*i, rel)).collect();
            for (index, attributes) in xattrs {
                if let Some(rel) = staged.get(&index) {
                    for (name, value) in attributes {
                        platform::set_xattr(&staging.join(rel), &name, &value);
                    }
                }
            }
        }
        if request.restore_metadata {
            self.restore_folder_metadata(staging, &written);
        }
        if let Some(value) = &run.quarantine {
            for folder in folders(staging, &self.members, &written) {
                platform::set_quarantine(&folder, value);
            }
        }
        let files: Vec<_> = written
            .iter()
            .filter(|(i, _)| self.members[*i].kind != Kind::Dir)
            .collect();
        Ok(ExtractSummary {
            files: files.len() as u64,
            bytes: files.iter().map(|(i, _)| self.members[*i].size).sum(),
            renamed: into_inner(run.renamed),
            skipped_unsafe: self.info().unsafe_names,
            skipped_links: into_inner(run.skipped_links),
            ..Default::default()
        })
    }

    /// I/O errors raised inside a decoder carry no path; they concern the archive.
    fn with_archive_path(&self, error: CoreError) -> CoreError {
        match error {
            CoreError::Io { path, message } if path.is_empty() => CoreError::Io {
                path: self.info().path,
                message,
            },
            other => other,
        }
    }

    /// Best effort, like file metadata. Runs after every file is written,
    /// deepest first, because filling a folder changes its modification time.
    fn restore_folder_metadata(&self, staging: &Path, written: &[(usize, PathBuf)]) {
        let mut dirs: Vec<_> = written
            .iter()
            .map(|(i, rel)| (&self.members[*i], staging.join(rel)))
            .filter(|(m, _)| m.kind == Kind::Dir)
            .collect();
        dirs.sort_by_key(|(_, path)| std::cmp::Reverse(path.components().count()));
        for (member, path) in dirs {
            if let Some(mtime) = member.mtime {
                let _ = platform::set_dir_mtime(&path, mtime);
            }
            if let Some(mode) = member.mode {
                // Keep the owner's access so the folder can still be moved into place.
                platform::set_dir_mode(&path, mode | 0o700);
            }
        }
    }
}

/// The sink of one extraction: wanted members go into the staging folder.
struct Run<'a> {
    handle: &'a ArchiveHandle,
    staging: &'a Path,
    progress: Arc<Progress>,
    restore: bool,
    quarantine: Option<Arc<[u8]>>,
    case_sensitive: bool,
    pool: Arc<WriterPool>,
    /// Staged file paths, case-folded, mapped to their exact spelling.
    staged: Mutex<HashMap<String, PathBuf>>,
    /// Folders known to exist below the staging folder.
    folders: Mutex<HashSet<PathBuf>>,
    written: Mutex<Vec<(usize, PathBuf)>>,
    renamed: Mutex<Vec<String>>,
    skipped_links: Mutex<Vec<String>>,
    xattrs: Mutex<Vec<(usize, Attributes)>>,
    /// Symbolic links and their targets, created once everything else is staged.
    #[cfg(windows)]
    deferred_links: Mutex<Vec<(usize, Vec<u8>)>>,
}

fn into_inner<T>(mutex: Mutex<T>) -> T {
    mutex
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Run<'_> {
    /// The staged path for a file. A name that differs from an earlier one
    /// only in case would overwrite it on a case-insensitive volume, so it
    /// gets a free name instead. An identical name is an updated copy of the
    /// same file and replaces it, as RAR intends.
    fn claim(&self, rel: &Path) -> PathBuf {
        let key = rel.to_string_lossy().to_lowercase();
        let mut staged = lock(&self.staged);
        let earlier = staged.get(&key).cloned();
        if earlier.as_deref() == Some(rel) {
            // The replacement must land after the queued original.
            self.pool.wait_idle();
        }
        if earlier.is_none_or(|earlier| earlier == rel) || self.case_sensitive {
            staged.insert(key, rel.to_path_buf());
            return rel.to_path_buf();
        }
        // A free name is found on disk, where queued files aren't yet.
        self.pool.wait_idle();
        let free = unique_sibling(&self.staging.join(rel));
        let free = free
            .strip_prefix(self.staging)
            .unwrap_or(&free)
            .to_path_buf();
        lock(&self.renamed).push(format!(
            "{} → {}",
            names::display_rel(rel),
            free.file_name().unwrap_or_default().to_string_lossy()
        ));
        staged.insert(free.to_string_lossy().to_lowercase(), free.clone());
        free
    }

    /// Creates `dir` and its parents below staging, once.
    fn ensure_dir(&self, dir: &Path) -> Result<()> {
        if lock(&self.folders).contains(dir) {
            return Ok(());
        }
        fs::create_dir_all(dir).map_err(|e| CoreError::io(dir, e))?;
        let mut folders = lock(&self.folders);
        let mut current = dir;
        while current != self.staging && folders.insert(current.to_path_buf()) {
            match current.parent() {
                Some(parent) => current = parent,
                None => break,
            }
        }
        Ok(())
    }

    fn meta(&self, index: usize) -> FileMeta {
        let member = &self.handle.members[index];
        FileMeta {
            mtime: member.mtime.filter(|_| self.restore),
            mode: member.mode.filter(|_| self.restore),
            quarantine: self.quarantine.clone(),
        }
    }
}

impl Sink for Run<'_> {
    /// Creates the staged file or folder for a wanted member.
    fn open(&self, index: usize) -> Result<Box<dyn Write + Send>> {
        if let Some(error) = self.pool.take_failure() {
            return Err(error);
        }
        let member = &self.handle.members[index];
        let rel = member
            .rel_path
            .as_ref()
            .expect("wanted members have safe paths");
        if member.kind == Kind::Dir {
            self.ensure_dir(&self.staging.join(rel))?;
            lock(&self.written).push((index, rel.clone()));
            return Ok(Box::new(io::sink()));
        }
        // The file's folder is created when the file is, maybe on a writer thread.
        let rel = self.claim(rel);
        let dest = self.staging.join(&rel);
        self.progress.set_file(names::display_rel(&rel));
        lock(&self.written).push((index, rel));
        let file = self
            .pool
            .file(
                dest.clone(),
                member.size,
                self.meta(index),
                self.progress.clone(),
            )
            .map_err(|e| CoreError::io(&dest, e))?;
        Ok(Box::new(file))
    }

    fn discard(&self) -> Box<dyn Write + Send> {
        Box::new(CountingWriter {
            inner: io::sink(),
            progress: self.progress.clone(),
        })
    }

    fn xattrs(&self, index: usize, attributes: Attributes) {
        lock(&self.xattrs).push((index, attributes));
    }

    fn link(&self, index: usize, link: Link<'_>) -> Result<()> {
        let rel = self.handle.members[index]
            .rel_path
            .as_ref()
            .expect("wanted members have safe paths");
        let dest = self.staging.join(rel);
        if let Some(parent) = dest.parent() {
            self.ensure_dir(parent)?;
        }
        let created = match link {
            // Which kind of link Windows needs depends on what it points
            // at, which may not be staged yet.
            #[cfg(windows)]
            Link::Symlink(target) if link_stays_inside(rel, target) => {
                lock(&self.deferred_links).push((index, target.to_vec()));
                return Ok(());
            }
            #[cfg(not(windows))]
            Link::Symlink(target) if link_stays_inside(rel, target) => {
                let created = platform::symlink(target, &dest);
                if created && let Some(value) = &self.quarantine {
                    platform::set_quarantine(&dest, value);
                }
                created
            }
            Link::Copy(target) => {
                // The source may still be queued.
                self.pool.wait_idle();
                let copied = names::safe_relative_path(target, true)
                    .map(|t| self.staging.join(t))
                    .is_some_and(|source| source.is_file() && fs::copy(source, &dest).is_ok());
                if copied && let Ok(file) = fs::File::options().write(true).open(&dest) {
                    self.meta(index).apply(&file, &dest);
                }
                copied
            }
            _ => false,
        };
        if created {
            lock(&self.written).push((index, rel.clone()));
        } else {
            lock(&self.skipped_links).push(names::display_rel(rel));
        }
        Ok(())
    }
}

#[cfg(windows)]
impl Run<'_> {
    /// Creates the symbolic links of the run. Windows has one kind of link
    /// for files and another for folders, and making either takes a privilege
    /// that only Developer Mode or an administrator has. Without it a link to
    /// a file becomes a copy of that file, and a link to a folder is skipped.
    fn finish_links(&self) {
        use std::os::windows::fs::{symlink_dir, symlink_file};
        let links = std::mem::take(&mut *lock(&self.deferred_links));
        for (index, target) in links {
            let rel = self.handle.members[index]
                .rel_path
                .as_ref()
                .expect("wanted members have safe paths");
            let dest = self.staging.join(rel);
            let target = names::link_target(&target);
            let pointed_at = dest.parent().unwrap_or(self.staging).join(&target);
            let created = match fs::metadata(&pointed_at) {
                Ok(found) if found.is_dir() => symlink_dir(&target, &dest).is_ok(),
                Ok(_) => {
                    symlink_file(&target, &dest).is_ok()
                        || self.copy_file(index, &pointed_at, &dest)
                }
                Err(_) => symlink_file(&target, &dest).is_ok(),
            };
            if created {
                lock(&self.written).push((index, rel.clone()));
            } else {
                lock(&self.skipped_links).push(names::display_rel(rel));
            }
        }
    }

    fn copy_file(&self, index: usize, source: &Path, dest: &Path) -> bool {
        let copied = fs::copy(source, dest).is_ok();
        if copied && let Ok(file) = fs::File::options().write(true).open(dest) {
            self.meta(index).apply(&file, dest);
        }
        copied
    }
}

/// Every staged folder: written folders and the parents of everything written.
fn folders(staging: &Path, members: &[Member], written: &[(usize, PathBuf)]) -> HashSet<PathBuf> {
    let mut folders = HashSet::new();
    for (index, rel) in written {
        let skip = usize::from(members[*index].kind != Kind::Dir);
        for ancestor in rel.ancestors().skip(skip) {
            if ancestor.as_os_str().is_empty() || !folders.insert(staging.join(ancestor)) {
                break;
            }
        }
    }
    folders
}

/// The sink of a verification: everything is decoded and dropped.
struct VerifySink<'a> {
    handle: &'a ArchiveHandle,
    progress: Arc<Progress>,
}

impl Sink for VerifySink<'_> {
    fn open(&self, index: usize) -> Result<Box<dyn Write + Send>> {
        self.progress
            .set_file(self.handle.members[index].display.clone());
        Ok(self.discard())
    }

    fn discard(&self) -> Box<dyn Write + Send> {
        Box::new(CountingWriter {
            inner: io::sink(),
            progress: self.progress.clone(),
        })
    }

    fn link(&self, _: usize, _: Link<'_>) -> Result<()> {
        Ok(())
    }

    fn checked(&self, index: usize) {
        let member = &self.handle.members[index];
        self.progress.set_file(member.display.clone());
        if member.kind != Kind::Dir {
            self.progress.add(member.size);
        }
    }
}

/// Removes staging folders left in `dir` by uunpack processes that crashed or
/// were force-quit; their partial output is useless.
fn remove_stale_staging(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let own = std::process::id() as i32;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_prefix(".uunpack-"))
            .and_then(|rest| rest.split('-').next())
            .and_then(|pid| pid.parse::<i32>().ok())
        else {
            continue;
        };
        if pid != own && !platform::process_alive(pid) {
            let _ = platform::make_writable(&entry.path());
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Whether a symlink at `rel` pointing at `target` resolves inside the destination.
fn link_stays_inside(rel: &Path, target: &[u8]) -> bool {
    if target.is_empty() || target.starts_with(b"/") || target.contains(&0) {
        return false;
    }
    // Windows also reads `\x` and `C:x` as paths that start somewhere else.
    if cfg!(windows) && (target.starts_with(b"\\") || target.get(1) == Some(&b':')) {
        return false;
    }
    let mut depth = rel.components().count() as isize - 1;
    for part in platform::raw_path(target).components() {
        match part {
            Component::Normal(_) => depth += 1,
            Component::ParentDir => depth -= 1,
            Component::CurDir => {}
            _ => return false,
        }
        if depth < 0 {
            return false;
        }
    }
    true
}

/// Moves the staged tree into `output`, merging into existing folders and
/// resolving collisions with `policy`.
fn publish(
    staging: &Path,
    output: &Path,
    policy: ConflictPolicy,
    summary: &mut ExtractSummary,
) -> Result<()> {
    for name in sorted_children(staging)? {
        if let Some(placed) = place(
            &staging.join(&name),
            &output.join(&name),
            Path::new(&name),
            policy,
            summary,
        )? {
            summary.top_level_paths.push(placed.display().to_string());
        }
    }
    Ok(())
}

fn place(
    src: &Path,
    dst: &Path,
    rel: &Path,
    policy: ConflictPolicy,
    summary: &mut ExtractSummary,
) -> Result<Option<PathBuf>> {
    let src_is_dir = src
        .symlink_metadata()
        .map_err(|e| CoreError::io(src, e))?
        .is_dir();
    let existing = match dst.symlink_metadata() {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            platform::rename(src, dst).map_err(|e| CoreError::io(dst, e))?;
            return Ok(Some(dst.to_path_buf()));
        }
        Err(e) => return Err(CoreError::io(dst, e)),
    };
    if existing.is_dir() && src_is_dir {
        for name in sorted_children(src)? {
            place(
                &src.join(&name),
                &dst.join(&name),
                &rel.join(&name),
                policy,
                summary,
            )?;
        }
        return Ok(Some(dst.to_path_buf()));
    }
    match policy {
        // Never delete a whole folder to make room for a file.
        ConflictPolicy::Overwrite if !existing.is_dir() => {
            platform::remove_file(dst).map_err(|e| CoreError::io(dst, e))?;
            platform::rename(src, dst).map_err(|e| CoreError::io(dst, e))?;
            Ok(Some(dst.to_path_buf()))
        }
        ConflictPolicy::Skip => {
            summary.skipped_existing.push(names::display_rel(rel));
            Ok(None)
        }
        _ => {
            let alternative = unique_sibling(dst);
            platform::rename(src, &alternative).map_err(|e| CoreError::io(&alternative, e))?;
            summary.renamed.push(format!(
                "{} → {}",
                names::display_rel(rel),
                alternative
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            ));
            Ok(Some(alternative))
        }
    }
}

fn sorted_children(dir: &Path) -> Result<Vec<std::ffi::OsString>> {
    let mut names = fs::read_dir(dir)
        .and_then(|entries| {
            entries
                .map(|e| e.map(|e| e.file_name()))
                .collect::<io::Result<Vec<_>>>()
        })
        .map_err(|e| CoreError::io(dir, e))?;
    names.sort();
    Ok(names)
}

/// Hidden working folder, removed when dropped.
struct Staging {
    path: PathBuf,
}

impl Staging {
    fn create(parent: &Path) -> Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = parent.join(format!(".uunpack-{}-{nanos:x}", std::process::id()));
        fs::create_dir(&path).map_err(|e| CoreError::io(&path, e))?;
        #[cfg(windows)]
        platform::hide(&path);
        Ok(Self { path })
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = platform::make_writable(&self.path);
        let _ = fs::remove_dir_all(&self.path);
    }
}

const PROGRESS_INTERVAL: Duration = Duration::from_millis(33);

/// Byte progress shared by the writers of one run. Counting is a single
/// atomic add; the observer hears from it at most every `PROGRESS_INTERVAL`.
pub(crate) struct Progress {
    observer: Arc<dyn ProgressObserver>,
    total: u64,
    done: AtomicU64,
    start: Instant,
    /// Nanoseconds after `start` of the last report, plus one; 0 before any.
    last: AtomicU64,
    file: Mutex<String>,
}

impl Progress {
    fn new(observer: Arc<dyn ProgressObserver>, total: u64) -> Self {
        Self {
            observer,
            total,
            done: AtomicU64::new(0),
            start: Instant::now(),
            last: AtomicU64::new(0),
            file: Mutex::default(),
        }
    }

    fn set_file(&self, name: String) {
        *lock(&self.file) = name;
        self.emit();
    }

    pub(crate) fn add(&self, bytes: u64) {
        self.done.fetch_add(bytes, Ordering::Relaxed);
        self.emit();
    }

    fn emit(&self) {
        let now = self.start.elapsed().as_nanos() as u64 + 1;
        let last = self.last.load(Ordering::Relaxed);
        if last != 0 && now.saturating_sub(last) < PROGRESS_INTERVAL.as_nanos() as u64 {
            return;
        }
        // Another writer reported in the meantime.
        if self
            .last
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        let file = lock(&self.file).clone();
        self.observer
            .on_progress(self.done.load(Ordering::Relaxed), self.total, file);
    }

    fn finish(&self) {
        let done = self.done.load(Ordering::Relaxed);
        self.observer
            .on_progress(done, self.total.max(done), String::new());
    }
}

struct CountingWriter<W> {
    inner: W,
    progress: Arc<Progress>,
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.progress.add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::link_stays_inside;
    use std::path::Path;

    #[test]
    fn symlink_targets() {
        assert!(link_stays_inside(Path::new("a/link"), b"file"));
        assert!(link_stays_inside(Path::new("a/link"), b"../b/file"));
        assert!(!link_stays_inside(Path::new("a/link"), b"../../etc/passwd"));
        assert!(!link_stays_inside(Path::new("link"), b"../x"));
        assert!(!link_stays_inside(Path::new("link"), b"/etc/passwd"));
    }
}
