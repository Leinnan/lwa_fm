//! File operations run in `dirfleet_effects::fileops`; the types are in
//! `dirfleet_core::fileops`.

pub mod trash_bin;

pub use dirfleet_effects::fileops::{
    ConflictPolicy, FileOp, FileOpEvent, Origin, Outcome, Undo, UndoStack, Worker, display_name,
    is_respelling, validate_name,
};
