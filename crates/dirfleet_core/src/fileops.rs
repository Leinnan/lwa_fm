//! File operations as data: what to do, how conflicts are handled, and how
//! each finished operation is undone. `dirfleet_effects` runs them.

use std::path::{Path, PathBuf};

use facet::Facet;
use serde::{Deserialize, Serialize};

/// The kinds of archive the app makes.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(C)]
pub enum ArchiveFormat {
    #[default]
    Zip,
    SevenZip,
    TarGz,
    TarZst,
    TarXz,
}

impl ArchiveFormat {
    pub const ALL: [Self; 5] = [
        Self::Zip,
        Self::SevenZip,
        Self::TarGz,
        Self::TarZst,
        Self::TarXz,
    ];

    /// What its files end with, without the dot.
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::SevenZip => "7z",
            Self::TarGz => "tar.gz",
            Self::TarZst => "tar.zst",
            Self::TarXz => "tar.xz",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Zip => "Zip",
            Self::SevenZip => "7z",
            Self::TarGz => "Tar, gzip",
            Self::TarZst => "Tar, Zstandard",
            Self::TarXz => "Tar, xz",
        }
    }
}

/// One trashed item, as shown in the view: where it was deleted from, and when.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrashKey {
    /// The original path on Windows and Linux; the path inside `~/.Trash` on macOS.
    pub path: PathBuf,
    pub deleted_at: u32,
}

/// What to do when a target name already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConflictPolicy {
    /// Overwrite files; merge folders into the existing folder.
    Replace,
    /// Leave the existing item alone and skip the source.
    Skip,
    /// Give the new item a free name such as `name (2).txt`.
    #[default]
    KeepBoth,
}

/// What the worker is asked to do.
///
/// Where a path is asked for, a copy, a move, a deletion, a rename and a new
/// folder or file also take a server's location (`sftp://connection/path`),
/// written as a path. Nothing done on a server can be undone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    Copy {
        srcs: Vec<PathBuf>,
        dst: PathBuf,
        policy: ConflictPolicy,
    },
    Move {
        srcs: Vec<PathBuf>,
        dst: PathBuf,
        policy: ConflictPolicy,
    },
    /// Move to the Recycle Bin / trash.
    Trash(Vec<PathBuf>),
    /// Delete permanently.
    Delete(Vec<PathBuf>),
    /// Put items from the Recycle Bin back where they came from.
    RestoreTrashed(Vec<TrashKey>),
    /// Delete items in the Recycle Bin permanently.
    PurgeTrashed(Vec<TrashKey>),
    /// Delete everything in the Recycle Bin permanently.
    EmptyTrash,
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
    /// Several renames as one undoable step (batch rename).
    RenameMany(Vec<(PathBuf, PathBuf)>),
    NewFolder {
        parent: PathBuf,
    },
    NewFile {
        parent: PathBuf,
    },
    /// Copy each item next to itself as "name - Copy".
    Duplicate(Vec<PathBuf>),
    /// Make the archive `dst` of `srcs`, folders with all they hold.
    Compress {
        srcs: Vec<PathBuf>,
        /// The archive's file, which must not exist yet.
        dst: PathBuf,
        format: ArchiveFormat,
    },
    /// Take items out of an archive into the folder `dst`.
    Extract {
        /// The archive's file: a path, or the location of an archive that
        /// is itself inside another.
        archive: PathBuf,
        /// What to take, by path inside the archive, folders with all they
        /// hold; everything when empty.
        members: Vec<String>,
        dst: PathBuf,
        policy: ConflictPolicy,
        /// Keep what comes out together: an archive with several items at
        /// its top goes into a new folder named after it.
        own_folder: bool,
    },
    /// Copy each file to the place named for it, making the folders it
    /// needs on the way and replacing a file already there. Many files in
    /// many folders, as one step.
    CopyPairs {
        /// Each file, and where its copy goes.
        pairs: Vec<(PathBuf, PathBuf)>,
        /// The folder all the copies end up under, for what is said of it.
        into: PathBuf,
    },
    /// Reverts an earlier operation.
    Undo(Undo),
}

impl FileOp {
    /// Every path the operation reads or writes, for finding the operations
    /// that would keep a drive busy. Recycle Bin entries are not listed.
    pub fn paths(&self) -> Vec<&Path> {
        match self {
            Self::Copy { srcs, dst, .. }
            | Self::Move { srcs, dst, .. }
            | Self::Compress { srcs, dst, .. } => srcs
                .iter()
                .map(PathBuf::as_path)
                .chain(std::iter::once(dst.as_path()))
                .collect(),
            Self::Trash(paths) | Self::Delete(paths) | Self::Duplicate(paths) => {
                paths.iter().map(PathBuf::as_path).collect()
            }
            Self::Rename { from, to } => vec![from, to],
            Self::RenameMany(pairs) | Self::CopyPairs { pairs, .. } => pairs
                .iter()
                .flat_map(|(from, to)| [from.as_path(), to.as_path()])
                .collect(),
            Self::NewFolder { parent } | Self::NewFile { parent } => vec![parent],
            // The archive is only read, and may not be a file of its own.
            Self::Extract { dst, .. } => vec![dst],
            Self::RestoreTrashed(_) | Self::PurgeTrashed(_) | Self::EmptyTrash | Self::Undo(_) => {
                Vec::new()
            }
        }
    }

    /// Whether this can take long because of how much data it handles:
    /// copies, moves and deletions. Such operations wait their turn on the
    /// drives they use and can be paused; the others are done at once, in
    /// the order they were asked for.
    pub const fn is_bulk(&self) -> bool {
        matches!(
            self,
            Self::Copy { .. }
                | Self::Move { .. }
                | Self::Duplicate(_)
                | Self::Delete(_)
                | Self::Extract { .. }
                | Self::Compress { .. }
                | Self::CopyPairs { .. }
        )
    }

    /// Whether it can be held where it is and let go on. An extraction
    /// waits its turn like a copy but, once started, runs to its end.
    pub const fn can_pause(&self) -> bool {
        self.is_bulk() && !matches!(self, Self::Extract { .. })
    }

    /// Whether running this can add to or take from the Recycle Bin.
    pub const fn changes_trash(&self) -> bool {
        matches!(
            self,
            Self::Trash(_)
                | Self::RestoreTrashed(_)
                | Self::PurgeTrashed(_)
                | Self::EmptyTrash
                | Self::Undo(_)
        )
    }

    /// A past-tense summary for the message shown when it finishes.
    pub fn describe_done(&self) -> String {
        match self {
            Self::Copy { srcs, dst, .. } => {
                format!("Copied {} to {}", count(srcs.len()), display_name(dst))
            }
            Self::Move { srcs, dst, .. } => {
                format!("Moved {} to {}", count(srcs.len()), display_name(dst))
            }
            Self::Trash(paths) => format!("Moved {} to the Recycle Bin", count(paths.len())),
            Self::Delete(paths) => format!("Deleted {}", count(paths.len())),
            Self::RestoreTrashed(keys) => format!("Restored {}", count(keys.len())),
            Self::PurgeTrashed(keys) => format!("Deleted {}", count(keys.len())),
            Self::EmptyTrash => format!("Emptied the {}", crate::current_path::TRASH_LABEL),
            Self::Rename { to, .. } => format!("Renamed to {}", display_name(to)),
            Self::RenameMany(pairs) => format!("Renamed {}", count(pairs.len())),
            Self::NewFolder { .. } => "Created a folder".to_owned(),
            Self::NewFile { .. } => "Created a file".to_owned(),
            Self::Duplicate(paths) => format!("Duplicated {}", count(paths.len())),
            Self::Extract { archive, dst, .. } => format!(
                "Extracted {} to {}",
                display_name(archive),
                display_name(dst)
            ),
            Self::Compress { srcs, dst, .. } => {
                format!("Compressed {} to {}", count(srcs.len()), display_name(dst))
            }
            Self::CopyPairs { pairs, into } => {
                format!("Copied {} to {}", count(pairs.len()), display_name(into))
            }
            Self::Undo(undo) => format!("Undid {}", undo.describe()),
        }
    }

    /// A short description for progress UI and messages.
    pub fn describe(&self) -> String {
        fn items(count: usize) -> String {
            if count == 1 {
                "1 item".to_owned()
            } else {
                format!("{count} items")
            }
        }
        match self {
            Self::Copy { srcs, dst, .. } => {
                format!("Copying {} to {}", items(srcs.len()), display_name(dst))
            }
            Self::Move { srcs, dst, .. } => {
                format!("Moving {} to {}", items(srcs.len()), display_name(dst))
            }
            Self::Trash(paths) => format!("Moving {} to the Recycle Bin", items(paths.len())),
            Self::Delete(paths) => format!("Deleting {}", items(paths.len())),
            Self::RestoreTrashed(keys) => format!("Restoring {}", items(keys.len())),
            Self::PurgeTrashed(keys) => format!("Deleting {}", items(keys.len())),
            Self::EmptyTrash => format!("Emptying the {}", crate::current_path::TRASH_LABEL),
            Self::RenameMany(pairs) => format!("Renaming {}", items(pairs.len())),
            Self::Rename { from, to } => {
                format!("Renaming {} to {}", display_name(from), display_name(to))
            }
            Self::NewFolder { .. } => "Creating a folder".to_owned(),
            Self::NewFile { .. } => "Creating a file".to_owned(),
            Self::Duplicate(paths) => format!("Duplicating {}", items(paths.len())),
            Self::Extract { archive, dst, .. } => format!(
                "Extracting {} to {}",
                display_name(archive),
                display_name(dst)
            ),
            Self::Compress { srcs, dst, .. } => {
                format!("Compressing {} to {}", items(srcs.len()), display_name(dst))
            }
            Self::CopyPairs { pairs, into } => {
                format!("Copying {} to {}", items(pairs.len()), display_name(into))
            }
            Self::Undo(undo) => format!("Undo {}", undo.describe()),
        }
    }
}

/// How to revert a finished operation. Running an `Undo` produces the
/// inverse `Undo`, which is what makes redo work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Undo {
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
    /// Items now at `.0` that were moved from `.1`.
    Moves(Vec<(PathBuf, PathBuf)>),
    /// Items the operation created; undone by trashing them.
    Created(Vec<PathBuf>),
    /// Items moved to the trash, by their original path.
    Trashed(Vec<PathBuf>),
}

impl Undo {
    pub fn describe(&self) -> String {
        match self {
            Self::Rename { .. } => "rename".to_owned(),
            Self::Moves(pairs) => format!("move of {}", count(pairs.len())),
            Self::Created(paths) => format!("creation of {}", count(paths.len())),
            Self::Trashed(paths) => format!("delete of {}", count(paths.len())),
        }
    }
}

/// What a running operation reports.
#[derive(Debug, Clone, PartialEq)]
pub enum FileOpEvent {
    Started {
        id: u64,
        description: String,
    },
    Progress {
        id: u64,
        /// 0.0 ..= 1.0
        fraction: f32,
        current: String,
        /// Bytes written so far and bytes to write, while data is copied;
        /// both zero for operations counted in items.
        bytes_done: u64,
        bytes_total: u64,
        /// How fast it copies now; zero when not known.
        bytes_per_second: u64,
    },
    Finished(Outcome),
}

/// How a finished operation went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub id: u64,
    pub origin: Origin,
    pub description: String,
    /// One message per item that failed.
    pub errors: Vec<String>,
    pub cancelled: bool,
    /// How to revert what was done, when it can be reverted.
    pub undo: Option<Undo>,
    /// Folders whose listing changed.
    pub affected_dirs: Vec<PathBuf>,
    /// New items, e.g. the folder made by "New folder" or pasted copies.
    pub created: Vec<PathBuf>,
    /// The Recycle Bin's contents may have changed.
    pub trash_changed: bool,
    /// Every file it copied was read back and found the same as its
    /// original.
    pub verified: bool,
    /// It stopped because the archive wants a password: `Some(true)` when
    /// the one it had did not fit.
    pub needs_password: Option<bool>,
}

impl Outcome {
    pub const fn succeeded(&self) -> bool {
        self.errors.is_empty() && !self.cancelled
    }
}

fn count(n: usize) -> String {
    if n == 1 {
        "1 item".to_owned()
    } else {
        format!("{n} items")
    }
}

pub fn display_name(path: &Path) -> String {
    // A server's top has no name of its own.
    if let crate::location::Location::Remote { path: at, .. } =
        crate::location::Location::parse(&path.to_string_lossy())
        && at == "/"
    {
        return "the server".to_owned();
    }
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Checks a single file name typed by the user. Returns a message that can
/// be shown next to the text field.
///
/// # Errors
/// What is wrong with the name.
pub fn validate_name(name: &str) -> Result<(), &'static str> {
    if name.trim().is_empty() {
        return Err("The name can't be empty.");
    }
    if name == "." || name == ".." {
        return Err("This name is reserved.");
    }
    if name.contains(['/', '\\']) {
        return Err("The name can't contain / or \\.");
    }
    if cfg!(windows) {
        if name.contains([':', '*', '?', '"', '<', '>', '|']) || name.chars().any(char::is_control)
        {
            return Err("The name can't contain \\ / : * ? \" < > |.");
        }
        if name.ends_with(['.', ' ']) {
            return Err("The name can't end with a dot or a space.");
        }
        let stem = name.split('.').next().unwrap_or(name).trim_end();
        let reserved = [
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ];
        if reserved.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
            return Err("This name is reserved by Windows.");
        }
    } else if name.contains('\0') {
        return Err("The name can't contain a NUL character.");
    }
    Ok(())
}

pub use egui_mev_core::history::Origin;

/// Undo and redo history. Generic so the app can keep file operations and
/// its own edits in one history.
pub type UndoStack<T = Undo> = egui_mev_core::history::UndoStack<T>;
