//! File operations that reach a server: copies and moves to, from and
//! between servers, and making, renaming and removing things on one.
//!
//! A place is this machine's disk or an open connection. Both are used
//! through the same few calls here, so a copy is one loop whichever two
//! places it runs between: read a piece, write a piece, with the
//! operation's pause and stop looked for in between.
//!
//! Nothing here can be undone, and a server has no Recycle Bin: what is
//! removed is gone.
use std::fs;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use dirfleet_core::app::FindConflicts;
use dirfleet_core::fileops::{ConflictPolicy, FileOp, FileOpEvent, Origin, Outcome};
use dirfleet_core::location::Location;
use dirfleet_remote::Session;

use crate::fileops::{OpControl, split_name};
use crate::vfs::Vfs;

const CHUNK: usize = 1024 * 1024;
const REPORT_INTERVAL: Duration = Duration::from_millis(60);

/// What a copy says when it was told to stop: not an error to show.
const STOPPED: &str = "Stopped";

fn is_remote(path: &Path) -> bool {
    matches!(
        Location::parse(&path.to_string_lossy()),
        Location::Remote { .. }
    )
}

/// Whether `op` reads or writes anything on a server.
pub fn reaches_server(op: &FileOp) -> bool {
    op.paths().into_iter().any(is_remote)
}

/// What is at a spot.
struct Found {
    is_dir: bool,
    size: u64,
    /// Unix seconds, when known.
    modified: Option<i64>,
}

/// A file or folder somewhere: on this machine, or on a server through
/// its open connection.
#[derive(Clone)]
struct Spot {
    session: Option<Arc<Session>>,
    at: Location,
}

fn seconds_of(meta: &fs::Metadata) -> Option<i64> {
    let since = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(since.as_secs()).ok()
}

impl Spot {
    fn of(vfs: &Vfs, path: &Path) -> Result<Self, String> {
        let at = Location::parse(&path.to_string_lossy());
        let session = match &at {
            Location::Local(_) => None,
            Location::Remote { connection, .. } => {
                Some(vfs.session(connection).map_err(|error| error.to_string())?)
            }
            Location::Archive { .. } => {
                return Err("What is inside an archive can't be changed".to_owned());
            }
        };
        Ok(Self { session, at })
    }

    /// The path in its place.
    fn path(&self) -> &str {
        match &self.at {
            Location::Local(path) | Location::Remote { path, .. } => path,
            Location::Archive { inner, .. } => inner,
        }
    }

    fn name(&self) -> String {
        self.at.file_name().unwrap_or_default().to_owned()
    }

    fn join(&self, name: &str) -> Self {
        Self {
            session: self.session.clone(),
            at: self.at.join(name),
        }
    }

    fn parent(&self) -> Option<Self> {
        Some(Self {
            session: self.session.clone(),
            at: self.at.parent()?,
        })
    }

    /// As the core writes it.
    fn as_path(&self) -> PathBuf {
        PathBuf::from(self.at.to_string())
    }

    /// The connection both spots are on, when they are on one.
    fn same_server(&self, other: &Self) -> Option<&Session> {
        match (&self.at, &other.at) {
            (Location::Remote { connection: a, .. }, Location::Remote { connection: b, .. })
                if a == b =>
            {
                self.session.as_deref()
            }
            _ => None,
        }
    }

    fn stat(&self) -> Result<Option<Found>, String> {
        if let Some(session) = &self.session {
            return Ok(session.stat(self.path())?.map(|entry| Found {
                is_dir: entry.is_dir,
                size: entry.size,
                modified: entry.modified,
            }));
        }
        match fs::metadata(self.path()) {
            Ok(meta) => Ok(Some(Found {
                is_dir: meta.is_dir(),
                size: meta.len(),
                modified: seconds_of(&meta),
            })),
            // A link to nothing counts as there: its name is taken.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(fs::symlink_metadata(self.path()).ok().map(|_| Found {
                    is_dir: false,
                    size: 0,
                    modified: None,
                }))
            }
            Err(error) => Err(error.to_string()),
        }
    }

    /// What the folder holds. On this machine links to folders are left
    /// out, so a link back up can't send a copy round for ever.
    fn list(&self) -> Result<Vec<(String, Found)>, String> {
        if let Some(session) = &self.session {
            return Ok(session
                .list(self.path())?
                .into_iter()
                .map(|entry| {
                    let found = Found {
                        is_dir: entry.is_dir,
                        size: entry.size,
                        modified: entry.modified,
                    };
                    (entry.name, found)
                })
                .collect());
        }
        let read = fs::read_dir(self.path()).map_err(|error| error.to_string())?;
        Ok(read
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let link = entry.file_type().ok()?.is_symlink();
                let meta = fs::metadata(entry.path()).ok()?;
                if link && meta.is_dir() {
                    return None;
                }
                let found = Found {
                    is_dir: meta.is_dir(),
                    size: meta.len(),
                    modified: seconds_of(&meta),
                };
                Some((entry.file_name().to_string_lossy().into_owned(), found))
            })
            .collect())
    }

    fn make_dir(&self) -> Result<(), String> {
        if let Some(session) = &self.session {
            return session.make_dir(self.path());
        }
        fs::create_dir(self.path()).map_err(|error| error.to_string())
    }

    fn remove_file(&self) -> Result<(), String> {
        if let Some(session) = &self.session {
            return session.remove_file(self.path());
        }
        fs::remove_file(self.path()).map_err(|error| error.to_string())
    }

    /// Removes the folder, which holds nothing any more.
    fn remove_dir(&self) -> Result<(), String> {
        if let Some(session) = &self.session {
            return session.remove_dir(self.path());
        }
        fs::remove_dir(self.path()).map_err(|error| error.to_string())
    }

    fn reader(&self) -> Result<Box<dyn Read>, String> {
        match &self.session {
            Some(session) => Ok(Box::new(session.read(self.path())?)),
            None => fs::File::open(self.path())
                .map(|file| Box::new(file) as Box<dyn Read>)
                .map_err(|error| error.to_string()),
        }
    }

    /// Opens the file for writing `size` bytes, in place of what is there.
    fn writer(&self, size: u64) -> Result<Sink, String> {
        if let Some(session) = &self.session {
            return session.write(self.path(), size).map(Sink::Server);
        }
        // Under another name until it is whole, so a copy that fails
        // leaves what was there.
        let target = PathBuf::from(self.path());
        let partial = target.with_file_name(format!(".{}.partial", self.name()));
        let file = fs::File::create(&partial).map_err(|error| error.to_string())?;
        Ok(Sink::Disk {
            file,
            partial,
            target,
        })
    }

    /// The first name like `name`, `name (2)`, `name (3)`… that nothing in
    /// this folder has.
    fn free(&self, name: &str, is_dir: bool) -> Result<Self, String> {
        let first = self.join(name);
        if first.stat()?.is_none() {
            return Ok(first);
        }
        let (stem, ext) = split_name(name, is_dir);
        for n in 2..10_000 {
            let spot = self.join(&format!("{stem} ({n}){ext}"));
            if spot.stat()?.is_none() {
                return Ok(spot);
            }
        }
        Err(format!("{name}: no free name is left"))
    }
}

/// A file being written.
enum Sink {
    Disk {
        file: fs::File,
        partial: PathBuf,
        target: PathBuf,
    },
    Server(dirfleet_remote::Writing),
}

impl Sink {
    fn write_all(&mut self, piece: &[u8]) -> Result<(), String> {
        match self {
            Self::Disk { file, .. } => file.write_all(piece),
            Self::Server(writing) => writing.write_all(piece),
        }
        .map_err(|error| error.to_string())
    }

    /// Makes the file whole, with the time its original was changed where
    /// the place keeps one.
    fn finish(self, modified: Option<i64>) -> Result<(), String> {
        match self {
            Self::Disk {
                mut file,
                partial,
                target,
            } => {
                let written = file.flush().and_then(|()| {
                    if let Some(seconds) = modified.and_then(|at| u64::try_from(at).ok()) {
                        // Without it the copy is only newer than it could be.
                        let _ = file.set_modified(UNIX_EPOCH + Duration::from_secs(seconds));
                    }
                    drop(file);
                    fs::rename(&partial, &target)
                });
                written.map_err(|error| {
                    let _ = fs::remove_file(&partial);
                    error.to_string()
                })
            }
            Self::Server(writing) => writing.finish(),
        }
    }

    /// Gives the file up, leaving nothing of it at `spot`.
    fn abandon(self, spot: &Spot) {
        match self {
            Self::Disk { file, partial, .. } => {
                drop(file);
                let _ = fs::remove_file(partial);
            }
            Self::Server(writing) => {
                writing.abandon();
                let _ = spot.remove_file();
            }
        }
    }
}

/// One operation as it runs: how far it is, and what went wrong.
struct Run<'a> {
    id: u64,
    control: &'a OpControl,
    report: &'a dyn Fn(FileOpEvent),
    /// Bytes copied and to copy.
    done: u64,
    total: u64,
    /// Items done and to do, for what is counted in items.
    items: (usize, usize),
    current: String,
    last: Instant,
    /// When the speed was last worked out, and how far the copy was then.
    mark: (Instant, u64),
    rate: u64,
    errors: Vec<String>,
    stopped: bool,
    /// Whether a file was read back and found the same, and whether one
    /// was copied without that.
    checked: bool,
    unchecked: bool,
    created: Vec<PathBuf>,
    affected: Vec<PathBuf>,
}

impl Run<'_> {
    fn tell(&mut self, force: bool) {
        if !force && self.last.elapsed() < REPORT_INTERVAL {
            return;
        }
        self.last = Instant::now();
        let since = self.mark.0.elapsed();
        if since >= Duration::from_secs(1) {
            self.rate = ((self.done - self.mark.1) as f64 / since.as_secs_f64()) as u64;
            self.mark = (self.last, self.done);
        }
        let fraction = if self.total > 0 {
            self.done as f64 / self.total as f64
        } else if self.items.1 > 0 {
            self.items.0 as f64 / self.items.1 as f64
        } else {
            0.0
        };
        (self.report)(FileOpEvent::Progress {
            id: self.id,
            fraction: fraction.min(1.0) as f32,
            current: self.current.clone(),
            bytes_done: self.done,
            bytes_total: self.total,
            bytes_per_second: self.rate,
        });
    }

    /// Waits while the operation is paused; says whether it is to stop.
    fn stop_asked(&mut self) -> bool {
        if !self.stopped && self.control.wait_if_paused() {
            self.stopped = true;
        }
        self.stopped
    }

    fn fail(&mut self, name: &str, reason: &str) {
        if reason != STOPPED {
            self.errors.push(format!("{name}: {reason}"));
        }
    }

    fn touched(&mut self, dir: Option<&Spot>) {
        if let Some(dir) = dir.map(Spot::as_path)
            && !self.affected.contains(&dir)
        {
            self.affected.push(dir);
        }
    }

    /// How many bytes copying `spot` moves.
    fn weigh(&mut self, spot: &Spot, found: &Found) -> u64 {
        if !found.is_dir {
            return found.size;
        }
        if self.stop_asked() {
            return 0;
        }
        spot.list().map_or(0, |items| {
            items
                .iter()
                .map(|(name, item)| self.weigh(&spot.join(name), item))
                .sum()
        })
    }

    /// Reads the copy at `dst` back and compares it with what was read
    /// from its original. A copy that differs is removed.
    fn check(&mut self, dst: &Spot, read: &blake3::Hash) -> Result<(), String> {
        let mut reader = dst.reader()?;
        let mut sum = blake3::Hasher::new();
        let mut piece = vec![0; CHUNK];
        loop {
            if self.stop_asked() {
                return Err(STOPPED.to_owned());
            }
            match reader.read(&mut piece) {
                Ok(0) => break,
                Ok(read) => sum.update(&piece[..read]),
                Err(error) => return Err(error.to_string()),
            };
        }
        drop(reader);
        if sum.finalize() == *read {
            return Ok(());
        }
        let _ = dst.remove_file();
        Err("the copy differs from the original and was removed".to_owned())
    }

    /// Copies the file `src` to `dst`, over what is there.
    fn copy_file(&mut self, src: &Spot, found: &Found, dst: &Spot) -> Result<(), String> {
        self.current = src.name();
        self.tell(false);
        if self.stop_asked() {
            return Err(STOPPED.to_owned());
        }
        // A server that can copy within itself does: nothing comes here.
        if let Some(session) = src.same_server(dst)
            && session.copy_file(src.path(), dst.path())?
        {
            self.done += found.size;
            self.unchecked = true;
            self.tell(false);
            return Ok(());
        }
        let mut reader = src.reader()?;
        let mut sink = dst.writer(found.size)?;
        let mut sum = self.control.verifies().then(blake3::Hasher::new);
        let mut piece = vec![0; CHUNK];
        let copied = loop {
            if self.stop_asked() {
                break Err(STOPPED.to_owned());
            }
            let read = match reader.read(&mut piece) {
                Ok(0) => break Ok(()),
                Ok(read) => read,
                Err(error) => break Err(error.to_string()),
            };
            if let Some(sum) = &mut sum {
                sum.update(&piece[..read]);
            }
            if let Err(reason) = sink.write_all(&piece[..read]) {
                break Err(reason);
            }
            self.done += read as u64;
            self.tell(false);
        };
        drop(reader);
        if let Err(reason) = copied {
            sink.abandon(dst);
            return Err(reason);
        }
        if let Err(reason) = sink.finish(found.modified) {
            if dst.session.is_some() {
                let _ = dst.remove_file();
            }
            return Err(reason);
        }
        match sum {
            Some(sum) => {
                self.check(dst, &sum.finalize())?;
                self.checked = true;
            }
            None => self.unchecked = true,
        }
        Ok(())
    }

    /// Copies `src` to `dst`: a file over the file there, a folder into
    /// the folder there. Says whether all of it was copied.
    fn copy_tree(&mut self, src: &Spot, found: &Found, dst: &Spot) -> bool {
        if !found.is_dir {
            return match self.copy_file(src, found, dst) {
                Ok(()) => true,
                Err(reason) => {
                    self.fail(&src.name(), &reason);
                    false
                }
            };
        }
        if self.stop_asked() {
            return false;
        }
        let made = match dst.stat() {
            Ok(Some(there)) if there.is_dir => Ok(()),
            Ok(Some(_)) => Err("a file is where the folder goes".to_owned()),
            Ok(None) => dst.make_dir(),
            Err(reason) => Err(reason),
        };
        let items = match made.and_then(|()| src.list()) {
            Ok(items) => items,
            Err(reason) => {
                self.fail(&src.name(), &reason);
                return false;
            }
        };
        let mut whole = true;
        for (name, item) in items {
            whole &= self.copy_tree(&src.join(&name), &item, &dst.join(&name));
            if self.stopped {
                return false;
            }
        }
        whole
    }

    /// Removes `spot` and all it holds. Says whether all of it is gone.
    fn remove_tree(&mut self, spot: &Spot, found: &Found) -> bool {
        if self.stop_asked() {
            return false;
        }
        let removed = if found.is_dir {
            let items = match spot.list() {
                Ok(items) => items,
                Err(reason) => {
                    self.fail(&spot.name(), &reason);
                    return false;
                }
            };
            let mut whole = true;
            for (name, item) in items {
                whole &= self.remove_tree(&spot.join(&name), &item);
                if self.stopped {
                    return false;
                }
            }
            if !whole {
                return false;
            }
            spot.remove_dir()
        } else {
            spot.remove_file()
        };
        match removed {
            Ok(()) => true,
            Err(reason) => {
                self.fail(&spot.name(), &reason);
                false
            }
        }
    }

    /// Where `src` goes in `dst` under `policy`; `None` to leave it out.
    fn target(
        src: &Spot,
        found: &Found,
        dst: &Spot,
        policy: ConflictPolicy,
        moving: bool,
    ) -> Result<Option<Spot>, String> {
        let name = src.name();
        let target = dst.join(&name);
        let onto_itself = src.same_server(&target).is_some() && src.path() == target.path()
            || (src.session.is_none() && target.session.is_none() && src.path() == target.path());
        if onto_itself {
            // Moved to where it is: nothing to do. Copied there: a copy
            // beside it.
            return if moving {
                Ok(None)
            } else {
                dst.free(&name, found.is_dir).map(Some)
            };
        }
        let Some(there) = target.stat()? else {
            return Ok(Some(target));
        };
        match policy {
            ConflictPolicy::Skip => Ok(None),
            ConflictPolicy::KeepBoth => dst.free(&name, found.is_dir).map(Some),
            ConflictPolicy::Replace if there.is_dir == found.is_dir => Ok(Some(target)),
            ConflictPolicy::Replace => {
                Err("a folder and a file can't take each other's place".to_owned())
            }
        }
    }

    /// Copies or moves one item into `dst`.
    fn send(
        &mut self,
        src: &Spot,
        dst: &Spot,
        policy: ConflictPolicy,
        moving: bool,
    ) -> Result<(), String> {
        let found = src
            .stat()?
            .ok_or_else(|| "it is not there any more".to_owned())?;
        let Some(target) = Self::target(src, &found, dst, policy, moving)? else {
            self.done += self.weigh(src, &found);
            return Ok(());
        };
        // On one server a move is a rename, where the server has one and
        // nothing is in the way.
        if moving
            && let Some(session) = src.same_server(&target)
            && target.stat()?.is_none()
            && session.rename(src.path(), target.path(), found.is_dir)?
        {
            self.done += self.weigh(src, &found);
            self.unchecked = true;
            self.created.push(target.as_path());
            return Ok(());
        }
        let whole = self.copy_tree(src, &found, &target);
        if !self.stopped || whole {
            self.created.push(target.as_path());
        }
        if whole && moving && !self.stopped {
            self.remove_tree(src, &found);
        }
        Ok(())
    }

    fn transfer(
        &mut self,
        vfs: &Vfs,
        srcs: &[PathBuf],
        dst: &Path,
        policy: ConflictPolicy,
        moving: bool,
    ) -> Result<(), String> {
        let dst = Spot::of(vfs, dst)?;
        let srcs: Vec<Spot> = srcs
            .iter()
            .map(|src| Spot::of(vfs, src))
            .collect::<Result<_, _>>()?;
        for src in &srcs {
            if let Ok(Some(found)) = src.stat() {
                self.total += self.weigh(src, &found);
            }
        }
        self.touched(Some(&dst));
        for src in &srcs {
            if self.stop_asked() {
                break;
            }
            self.current = src.name();
            self.tell(false);
            if moving {
                self.touched(src.parent().as_ref());
            }
            if let Err(reason) = self.send(src, &dst, policy, moving) {
                self.fail(&src.name(), &reason);
            }
        }
        Ok(())
    }

    fn delete(&mut self, vfs: &Vfs, paths: &[PathBuf]) -> Result<(), String> {
        let spots: Vec<Spot> = paths
            .iter()
            .map(|path| Spot::of(vfs, path))
            .collect::<Result<_, _>>()?;
        self.items.1 = spots.len();
        for spot in &spots {
            if self.stop_asked() {
                break;
            }
            self.current = spot.name();
            self.tell(false);
            self.touched(spot.parent().as_ref());
            match spot.stat() {
                Ok(Some(found)) => {
                    self.remove_tree(spot, &found);
                }
                // Gone already: what was asked for.
                Ok(None) => {}
                Err(reason) => self.fail(&spot.name(), &reason),
            }
            self.items.0 += 1;
        }
        Ok(())
    }

    fn rename(&mut self, vfs: &Vfs, from: &Path, to: &Path) -> Result<(), String> {
        let (from, to) = (Spot::of(vfs, from)?, Spot::of(vfs, to)?);
        let Some(session) = from.same_server(&to) else {
            return Err("An item is renamed where it is".to_owned());
        };
        let found = from
            .stat()?
            .ok_or_else(|| format!("{} is not there any more", from.name()))?;
        if to.stat()?.is_some() {
            return Err(format!("{} already exists", to.name()));
        }
        self.touched(from.parent().as_ref());
        self.touched(to.parent().as_ref());
        if !session.rename(from.path(), to.path(), found.is_dir)? {
            // A server without renaming: a copy under the new name, and
            // the original removed.
            self.total = self.weigh(&from, &found);
            if self.copy_tree(&from, &found, &to) && !self.stopped {
                self.remove_tree(&from, &found);
            }
        }
        self.created.push(to.as_path());
        Ok(())
    }

    /// Makes a new folder or an empty file in `parent`.
    fn new_item(&mut self, vfs: &Vfs, parent: &Path, folder: bool) -> Result<(), String> {
        let parent = Spot::of(vfs, parent)?;
        let spot = if folder {
            let spot = parent.free("New folder", true)?;
            spot.make_dir()?;
            spot
        } else {
            let spot = parent.free("New Text Document.txt", false)?;
            spot.writer(0)?.finish(None)?;
            spot
        };
        self.touched(Some(&parent));
        self.created.push(spot.as_path());
        Ok(())
    }
}

/// Runs `op`, which reaches a server, to its end.
pub fn run(
    vfs: &Vfs,
    id: u64,
    op: &FileOp,
    origin: Origin,
    control: &OpControl,
    report: &dyn Fn(FileOpEvent),
) -> Outcome {
    let description = op.describe();
    report(FileOpEvent::Started {
        id,
        description: description.clone(),
    });
    let now = Instant::now();
    let mut run = Run {
        id,
        control,
        report,
        done: 0,
        total: 0,
        items: (0, 0),
        current: String::new(),
        last: now.checked_sub(REPORT_INTERVAL).unwrap_or(now),
        mark: (now, 0),
        rate: 0,
        errors: Vec::new(),
        stopped: false,
        checked: false,
        unchecked: false,
        created: Vec::new(),
        affected: Vec::new(),
    };
    let done = match op {
        FileOp::Copy { srcs, dst, policy } => run.transfer(vfs, srcs, dst, *policy, false),
        FileOp::Move { srcs, dst, policy } => run.transfer(vfs, srcs, dst, *policy, true),
        FileOp::Delete(paths) => run.delete(vfs, paths),
        FileOp::Rename { from, to } => run.rename(vfs, from, to),
        FileOp::NewFolder { parent } => run.new_item(vfs, parent, true),
        FileOp::NewFile { parent } => run.new_item(vfs, parent, false),
        FileOp::Trash(_) => Err("A server has no Recycle Bin".to_owned()),
        _ => Err("This can't be done on a server".to_owned()),
    };
    if let Err(reason) = done {
        run.errors.push(reason);
    }
    // Where it got to, whatever the reports in between left out.
    run.tell(true);
    let cancelled = run.stopped || control.is_cancelled();
    Outcome {
        id,
        origin,
        description,
        verified: run.checked && !run.unchecked && run.errors.is_empty() && !cancelled,
        errors: run.errors,
        cancelled,
        undo: None,
        affected_dirs: run.affected,
        created: run.created,
        trash_changed: false,
        needs_password: None,
    }
}

/// The names among the sources that already exist in the destination,
/// wherever either is.
pub fn conflicts(vfs: &Vfs, check: &FindConflicts) -> Vec<String> {
    let names = check.srcs.iter().filter_map(|src| {
        Location::parse(&src.to_string_lossy())
            .file_name()
            .map(str::to_owned)
    });
    match Location::parse(&check.dst.to_string_lossy()) {
        Location::Remote {
            connection, path, ..
        } => {
            // One look at the folder, not one question for each name.
            let listed: Vec<String> = vfs
                .session(&connection)
                .ok()
                .and_then(|session| session.list(&path).ok())
                .unwrap_or_default()
                .into_iter()
                .map(|entry| entry.name)
                .collect();
            names.filter(|name| listed.contains(name)).collect()
        }
        _ => names
            .filter(|name| check.dst.join(name).symlink_metadata().is_ok())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// A `Vfs` with the folder `served` standing in for the server `c1`.
    fn serving(served: &Path, cache: &Path) -> Vfs {
        fs::create_dir_all(served).expect("served");
        let vfs = Vfs::new(cache.to_path_buf());
        let endpoint = dirfleet_remote::Endpoint {
            kind: dirfleet_remote::Kind::Folder,
            host: served.to_string_lossy().into_owned(),
            port: 0,
            user: String::new(),
            secret: String::new(),
            root: String::new(),
            secure: false,
            bucket: String::new(),
            region: String::new(),
            host_key: None,
            key_file: String::new(),
        };
        vfs.connect("c1", &endpoint).expect("connects");
        vfs
    }

    fn on_server(path: &str) -> PathBuf {
        PathBuf::from(format!("sftp://c1/{path}"))
    }

    fn go(vfs: &Vfs, op: &FileOp, control: &OpControl) -> Outcome {
        run(vfs, 1, op, Origin::User, control, &|_| {})
    }

    #[test]
    fn folders_and_files_go_to_a_server_and_names_that_clash_follow_the_policy() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (here, served) = (dir.path().join("here"), dir.path().join("served"));
        fs::create_dir_all(here.join("album/deep")).expect("folders");
        fs::write(here.join("album/deep/a.bin"), vec![1; 2_500_000]).expect("write");
        fs::write(here.join("album/b.txt"), b"bee").expect("write");
        fs::write(here.join("note.txt"), b"first").expect("write");
        let vfs = serving(&served, &dir.path().join("cache"));
        let control = OpControl::default();
        let copy = |policy| FileOp::Copy {
            srcs: vec![here.join("album"), here.join("note.txt")],
            dst: on_server("in"),
            policy,
        };
        fs::create_dir(served.join("in")).expect("folder");
        assert!(reaches_server(&copy(ConflictPolicy::Skip)));

        let seen = Mutex::new(Vec::new());
        let outcome = run(
            &vfs,
            1,
            &copy(ConflictPolicy::KeepBoth),
            Origin::User,
            &control,
            &|event| seen.lock().expect("lock").push(event),
        );
        assert!(outcome.succeeded(), "{:?}", outcome.errors);
        assert_eq!(outcome.undo, None);
        assert_eq!(
            fs::read(served.join("in/album/deep/a.bin")).expect("read"),
            vec![1; 2_500_000]
        );
        assert_eq!(
            fs::read(served.join("in/album/b.txt")).expect("read"),
            b"bee"
        );
        assert_eq!(
            fs::read(served.join("in/note.txt")).expect("read"),
            b"first"
        );
        assert_eq!(
            outcome.created,
            [on_server("in/album"), on_server("in/note.txt")]
        );
        assert_eq!(outcome.affected_dirs, [on_server("in")]);
        // The originals stay, and the bytes were counted.
        assert!(here.join("note.txt").exists());
        let seen = seen.into_inner().expect("lock");
        assert!(seen.iter().any(|event| matches!(
            event,
            FileOpEvent::Progress { bytes_total: 2_500_008, bytes_done, .. } if *bytes_done > 0
        )));

        // Again: skipped, kept beside, or replaced.
        fs::write(here.join("note.txt"), b"second").expect("write");
        assert!(go(&vfs, &copy(ConflictPolicy::Skip), &control).succeeded());
        assert_eq!(
            fs::read(served.join("in/note.txt")).expect("read"),
            b"first"
        );
        let both = go(&vfs, &copy(ConflictPolicy::KeepBoth), &control);
        assert_eq!(
            both.created,
            [on_server("in/album (2)"), on_server("in/note (2).txt")]
        );
        assert_eq!(
            fs::read(served.join("in/note (2).txt")).expect("read"),
            b"second"
        );
        assert!(go(&vfs, &copy(ConflictPolicy::Replace), &control).succeeded());
        assert_eq!(
            fs::read(served.join("in/note.txt")).expect("read"),
            b"second"
        );
        assert!(!served.join("in/note (3).txt").exists());
    }

    #[test]
    fn files_come_from_a_server_checked_and_with_their_times() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (here, served) = (dir.path().join("here"), dir.path().join("served"));
        fs::create_dir_all(served.join("docs")).expect("folders");
        fs::create_dir_all(&here).expect("folder");
        fs::write(served.join("docs/a.md"), b"# a").expect("write");
        fs::File::options()
            .write(true)
            .open(served.join("docs/a.md"))
            .and_then(|file| file.set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000)))
            .expect("time");
        fs::write(served.join("big.bin"), vec![9; 1_200_000]).expect("write");
        let vfs = serving(&served, &dir.path().join("cache"));

        let outcome = go(
            &vfs,
            &FileOp::Copy {
                srcs: vec![on_server("docs"), on_server("big.bin")],
                dst: here.clone(),
                policy: ConflictPolicy::KeepBoth,
            },
            &OpControl::with_verify(true),
        );
        assert!(outcome.succeeded(), "{:?}", outcome.errors);
        assert!(outcome.verified);
        assert_eq!(fs::read(here.join("docs/a.md")).expect("read"), b"# a");
        assert_eq!(
            fs::read(here.join("big.bin")).expect("read"),
            vec![9; 1_200_000]
        );
        let kept = fs::metadata(here.join("docs/a.md"))
            .and_then(|meta| meta.modified())
            .expect("time");
        assert_eq!(kept, UNIX_EPOCH + Duration::from_secs(1_700_000_000));
        assert_eq!(outcome.created, [here.join("docs"), here.join("big.bin")]);
        // Nothing half-written is left beside them.
        let left: Vec<_> = fs::read_dir(&here)
            .expect("read")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".partial"))
            .collect();
        assert_eq!(left, Vec::<String>::new());
    }

    #[test]
    fn a_move_takes_the_originals_away_and_on_one_server_only_renames() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (here, served) = (dir.path().join("here"), dir.path().join("served"));
        fs::create_dir_all(here.join("album")).expect("folders");
        fs::write(here.join("album/a.txt"), b"a").expect("write");
        fs::write(here.join("note.txt"), b"note").expect("write");
        let vfs = serving(&served, &dir.path().join("cache"));
        let control = OpControl::default();

        let up = go(
            &vfs,
            &FileOp::Move {
                srcs: vec![here.join("album"), here.join("note.txt")],
                dst: on_server(""),
                policy: ConflictPolicy::KeepBoth,
            },
            &control,
        );
        assert!(up.succeeded(), "{:?}", up.errors);
        assert!(!here.join("album").exists() && !here.join("note.txt").exists());
        assert_eq!(fs::read(served.join("album/a.txt")).expect("read"), b"a");
        assert_eq!(up.affected_dirs, [on_server(""), here]);

        fs::create_dir(served.join("kept")).expect("folder");
        let within = go(
            &vfs,
            &FileOp::Move {
                srcs: vec![on_server("album"), on_server("note.txt")],
                dst: on_server("kept"),
                policy: ConflictPolicy::KeepBoth,
            },
            &control,
        );
        assert!(within.succeeded(), "{:?}", within.errors);
        assert!(!served.join("album").exists() && !served.join("note.txt").exists());
        assert_eq!(
            fs::read(served.join("kept/album/a.txt")).expect("read"),
            b"a"
        );
        assert_eq!(
            within.created,
            [on_server("kept/album"), on_server("kept/note.txt")]
        );

        // Copied into the folder it is in: a copy beside it.
        let beside = go(
            &vfs,
            &FileOp::Copy {
                srcs: vec![on_server("kept/note.txt")],
                dst: on_server("kept"),
                policy: ConflictPolicy::KeepBoth,
            },
            &control,
        );
        assert_eq!(beside.created, [on_server("kept/note (2).txt")]);
        assert_eq!(
            fs::read(served.join("kept/note (2).txt")).expect("read"),
            b"note"
        );
    }

    #[test]
    fn things_on_a_server_are_made_renamed_and_removed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let served = dir.path().join("served");
        let vfs = serving(&served, &dir.path().join("cache"));
        let control = OpControl::default();

        let folder = FileOp::NewFolder {
            parent: on_server(""),
        };
        assert_eq!(
            go(&vfs, &folder, &control).created,
            [on_server("New folder")]
        );
        assert_eq!(
            go(&vfs, &folder, &control).created,
            [on_server("New folder (2)")]
        );
        let file = go(
            &vfs,
            &FileOp::NewFile {
                parent: on_server("New folder"),
            },
            &control,
        );
        assert_eq!(
            file.created,
            [on_server("New folder/New Text Document.txt")]
        );
        assert!(served.join("New folder/New Text Document.txt").is_file());

        let renamed = go(
            &vfs,
            &FileOp::Rename {
                from: on_server("New folder"),
                to: on_server("papers"),
            },
            &control,
        );
        assert!(renamed.succeeded(), "{:?}", renamed.errors);
        assert!(served.join("papers/New Text Document.txt").is_file());
        assert_eq!(renamed.affected_dirs, [on_server("")]);
        // A name that is taken is refused.
        let taken = go(
            &vfs,
            &FileOp::Rename {
                from: on_server("papers"),
                to: on_server("New folder (2)"),
            },
            &control,
        );
        assert_eq!(taken.errors, ["New folder (2) already exists"]);

        let removed = go(
            &vfs,
            &FileOp::Delete(vec![on_server("papers"), on_server("New folder (2)")]),
            &control,
        );
        assert!(removed.succeeded(), "{:?}", removed.errors);
        assert_eq!(fs::read_dir(&served).expect("read").count(), 0);
        // What can't be done there says so, and a server left says so too.
        assert!(!go(&vfs, &FileOp::Trash(vec![on_server("x")]), &control).succeeded());
        vfs.disconnect("c1");
        assert_eq!(
            go(&vfs, &folder, &control).errors,
            ["The server is not connected."]
        );
    }

    #[test]
    fn a_stopped_copy_leaves_nothing_half_written() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (here, served) = (dir.path().join("here"), dir.path().join("served"));
        let there = served.join("there");
        fs::create_dir_all(&here).expect("folder");
        fs::create_dir_all(&there).expect("folder");
        fs::write(here.join("up.bin"), vec![3; 3_000_000]).expect("write");
        fs::write(there.join("down.bin"), vec![4; 3_000_000]).expect("write");
        let vfs = serving(&served, &dir.path().join("cache"));
        // Stopped once the first piece has gone over. Reports are spaced
        // out: the wait makes the next one come while the file is copied.
        let stopped = |op: &FileOp| {
            let control = OpControl::default();
            let outcome = run(&vfs, 1, op, Origin::User, &control, &|event| match event {
                FileOpEvent::Progress { bytes_done: 0, .. } => {
                    std::thread::sleep(REPORT_INTERVAL * 2);
                }
                FileOpEvent::Progress { .. } => control.cancel(),
                _ => {}
            });
            assert!(outcome.cancelled);
            assert_eq!(outcome.errors, Vec::<String>::new());
        };
        let names = |dir: &Path| -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(dir)
                .expect("read")
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort_unstable();
            names
        };

        fs::create_dir_all(served.join("in")).expect("folder");
        stopped(&FileOp::Copy {
            srcs: vec![here.join("up.bin")],
            dst: on_server("in"),
            policy: ConflictPolicy::KeepBoth,
        });
        assert_eq!(names(&served.join("in")), Vec::<String>::new());
        stopped(&FileOp::Move {
            srcs: vec![on_server("there/down.bin")],
            dst: here.clone(),
            policy: ConflictPolicy::KeepBoth,
        });
        // Nothing arrived, and what was to move is where it was.
        assert_eq!(names(&here), ["up.bin"]);
        assert_eq!(names(&there), ["down.bin"]);
    }

    #[test]
    fn names_that_clash_are_found_on_a_server_and_for_what_comes_from_one() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (here, served) = (dir.path().join("here"), dir.path().join("served"));
        fs::create_dir_all(served.join("docs")).expect("folders");
        fs::create_dir_all(&here).expect("folder");
        fs::write(served.join("docs/a.md"), b"a").expect("write");
        fs::write(here.join("b.md"), b"b").expect("write");
        let vfs = serving(&served, &dir.path().join("cache"));

        let up = FindConflicts {
            srcs: vec![here.join("a.md"), here.join("b.md")],
            dst: on_server("docs"),
        };
        assert_eq!(conflicts(&vfs, &up), ["a.md"]);
        let down = FindConflicts {
            srcs: vec![on_server("docs/a.md"), on_server("docs/b.md")],
            dst: here,
        };
        assert_eq!(conflicts(&vfs, &down), ["b.md"]);
    }
}
