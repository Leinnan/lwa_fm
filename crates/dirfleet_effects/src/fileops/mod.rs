//! File operations (copy, move, trash, delete, rename, create) run on a
//! background worker, with progress reporting, cancellation and undo.
//!
//! The caller decides how name conflicts are handled before an operation
//! starts ([`ConflictPolicy`]), so the worker never has to stop and ask.

#[cfg(target_os = "macos")]
mod clone;
mod naming;
mod worker;

pub use crate::trash_bin;
pub use naming::{copy_name, is_respelling, unique_name, validate_name};
pub use worker::{OpControl, Worker, execute};

pub use dirfleet_core::fileops::*;

#[cfg(test)]
mod tests;
