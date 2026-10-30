//! What Ctrl+Z and Ctrl+Y act on: file operations, search edits and changes
//! to saved searches share one undo history, in the order they happened.

use super::{SavedSearch, Search};
use crate::app::dock::TabId;
use crate::fileops;

#[derive(Debug, Clone)]
pub enum UndoItem {
    /// A finished file operation; undone by running its inverse.
    File(fileops::Undo),
    /// The search of a tab changed from `before` to `after`.
    Search {
        tab: TabId,
        before: Option<Search>,
        after: Option<Search>,
        label: String,
    },
    /// The list of saved searches changed.
    SavedSearches {
        before: Vec<SavedSearch>,
        after: Vec<SavedSearch>,
        label: String,
    },
}

impl UndoItem {
    /// What the item does, e.g. "remove filter ext:pdf", for menus and toasts.
    pub fn label(&self) -> String {
        match self {
            Self::File(undo) => undo.describe(),
            Self::Search { label, .. } | Self::SavedSearches { label, .. } => label.clone(),
        }
    }
}

pub type UndoStack = fileops::UndoStack<UndoItem>;
