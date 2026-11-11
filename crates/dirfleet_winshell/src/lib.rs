//! Explorer integration for the Windows shells: showing a file in Explorer,
//! Properties, "Open with", the folder and program pickers, the classic shell
//! context menu and dragging files out to other applications.
//!
//! No `windows` crate type is part of the public API, so shells built on
//! different windows-rs revisions can share this crate. Window handles are
//! `isize` (0 for no owner window) and paths are `std::path` types.
//!
//! Everything but [`display_in_explorer`] talks to the shell through COM, and
//! takes part in COM for the length of the call when the thread hasn't joined
//! it yet. Call from a single-threaded apartment or a thread without COM,
//! never from the multithreaded apartment. What shows a window ([`open_with`],
//! the pickers, [`context_menu::show`], [`start_drag`]) blocks its thread in a
//! modal loop and belongs on the thread of the owner window.
//!
//! On other systems the crate is empty.

#[cfg(windows)]
mod com;
#[cfg(windows)]
pub mod context_menu;
#[cfg(windows)]
mod drag;
#[cfg(windows)]
mod error;
#[cfg(windows)]
mod explorer;
#[cfg(windows)]
mod picker;
#[cfg(windows)]
mod polled_drag;
#[cfg(windows)]
mod preview_handler;
#[cfg(windows)]
mod raw;

#[cfg(windows)]
pub use self::{
    drag::{DataObject, DragOutcome, data_object, start_drag, start_polled_drag},
    error::Error,
    explorer::{display_in_explorer, open_properties, open_with},
    picker::{pick_folder, pick_program},
    preview_handler::{Insets, PreviewHost, has_preview_handler},
};
