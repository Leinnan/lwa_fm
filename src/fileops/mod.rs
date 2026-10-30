//! File operations (copy, move, trash, delete, rename, create) run on a
//! background worker, with progress reporting, cancellation and undo.
//!
//! The UI decides how name conflicts are handled before an operation starts
//! ([`ConflictPolicy`]), so the worker never has to stop and ask.

#[cfg(target_os = "macos")]
mod clone;
mod naming;
pub mod trash_bin;
mod worker;

pub use naming::{copy_name, is_respelling, unique_name, validate_name};
pub use worker::{FileOpEvent, Outcome, Worker};

use std::path::{Path, PathBuf};

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

#[derive(Debug, Clone)]
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
    RestoreTrashed(Vec<trash_bin::TrashKey>),
    /// Delete items in the Recycle Bin permanently.
    PurgeTrashed(Vec<trash_bin::TrashKey>),
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
    /// Reverts an earlier operation.
    Undo(Undo),
}

impl FileOp {
    /// Every path the operation reads or writes, for finding the operations
    /// that would keep a drive busy. Recycle Bin entries are not listed.
    pub fn paths(&self) -> Vec<&Path> {
        match self {
            Self::Copy { srcs, dst, .. } | Self::Move { srcs, dst, .. } => srcs
                .iter()
                .map(PathBuf::as_path)
                .chain(std::iter::once(dst.as_path()))
                .collect(),
            Self::Trash(paths) | Self::Delete(paths) | Self::Duplicate(paths) => {
                paths.iter().map(PathBuf::as_path).collect()
            }
            Self::Rename { from, to } => vec![from, to],
            Self::RenameMany(pairs) => pairs
                .iter()
                .flat_map(|(from, to)| [from.as_path(), to.as_path()])
                .collect(),
            Self::NewFolder { parent } | Self::NewFile { parent } => vec![parent],
            Self::RestoreTrashed(_) | Self::PurgeTrashed(_) | Self::EmptyTrash | Self::Undo(_) => {
                Vec::new()
            }
        }
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
            Self::EmptyTrash => format!("Emptied the {}", trash_bin::LABEL),
            Self::Rename { to, .. } => format!("Renamed to {}", display_name(to)),
            Self::RenameMany(pairs) => format!("Renamed {}", count(pairs.len())),
            Self::NewFolder { .. } => "Created a folder".to_owned(),
            Self::NewFile { .. } => "Created a file".to_owned(),
            Self::Duplicate(paths) => format!("Duplicated {}", count(paths.len())),
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
            Self::EmptyTrash => format!("Emptying the {}", trash_bin::LABEL),
            Self::RenameMany(pairs) => format!("Renaming {}", items(pairs.len())),
            Self::Rename { from, to } => {
                format!("Renaming {} to {}", display_name(from), display_name(to))
            }
            Self::NewFolder { .. } => "Creating a folder".to_owned(),
            Self::NewFile { .. } => "Creating a file".to_owned(),
            Self::Duplicate(paths) => format!("Duplicating {}", items(paths.len())),
            Self::Undo(undo) => format!("Undo {}", undo.describe()),
        }
    }
}

/// How to revert a finished operation. Running an `Undo` produces the
/// inverse `Undo`, which is what makes redo work.
#[derive(Debug, Clone)]
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

fn count(n: usize) -> String {
    if n == 1 {
        "1 item".to_owned()
    } else {
        format!("{n} items")
    }
}

pub fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Undo and redo history. Generic so the app can keep file operations and
/// its own edits in one history.
#[derive(Debug)]
pub struct UndoStack<T = Undo> {
    undo: Vec<T>,
    redo: Vec<T>,
}

impl<T> Default for UndoStack<T> {
    fn default() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }
}

const MAX_UNDO: usize = 50;

/// Where a finished operation came from, so its inverse lands on the right stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    User,
    Undo,
    Redo,
}

impl<T> UndoStack<T> {
    pub fn record(&mut self, origin: Origin, undo: T) {
        let stack = match origin {
            Origin::User => {
                self.redo.clear();
                &mut self.undo
            }
            Origin::Redo => &mut self.undo,
            Origin::Undo => &mut self.redo,
        };
        stack.push(undo);
        if stack.len() > MAX_UNDO {
            stack.remove(0);
        }
    }

    pub fn pop_undo(&mut self) -> Option<T> {
        self.undo.pop()
    }

    pub fn pop_redo(&mut self) -> Option<T> {
        self.redo.pop()
    }

    /// The item Ctrl+Z would undo.
    pub fn peek_undo(&self) -> Option<&T> {
        self.undo.last()
    }

    /// The item Ctrl+Y would redo.
    pub fn peek_redo(&self) -> Option<&T> {
        self.redo.last()
    }
}

#[cfg(test)]
mod tests;
