//! What Undo and Redo act on, as egui's: file operations, search edits and
//! changes to saved searches share one history, in the order they happened.
use crate::fileops::Undo;
use crate::ids::TabId;
use crate::search::saved::SavedSearch;
use crate::search::{MatchMode, Search, query};

#[derive(Debug, Clone)]
pub(super) enum Step {
    /// A finished file operation; undone by running its inverse.
    File(Undo),
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

impl Step {
    /// What the step does ("remove filter ext:pdf"), for menus and toasts.
    pub(super) fn label(&self) -> String {
        match self {
            Self::File(undo) => undo.describe(),
            Self::Search { label, .. } | Self::SavedSearches { label, .. } => label.clone(),
        }
    }
}

/// How a search changed, in words: "type “report”", "add filter ext:pdf".
pub(super) fn describe(before: Option<&Search>, after: Option<&Search>) -> String {
    let (before, after) = match (before, after) {
        (None, Some(after)) if after.value.is_empty() => return "open search".to_owned(),
        (None, Some(after)) => return format!("type \u{201c}{}\u{201d}", after.value),
        (Some(_), None) => return "close search".to_owned(),
        (Some(before), Some(after)) => (before, after),
        (None, None) => return "search".to_owned(),
    };
    if after.saved_from.is_some() && after.saved_from != before.saved_from {
        return "run saved search".to_owned();
    }
    if let Some(added) = after.chips.iter().find(|chip| !before.chips.contains(chip)) {
        return format!("add filter {}", query::chip_token(added));
    }
    if let Some(removed) = before.chips.iter().find(|chip| !after.chips.contains(chip)) {
        return format!("remove filter {}", query::chip_token(removed));
    }
    if before.value != after.value {
        return if after.value.is_empty() {
            "clear search text".to_owned()
        } else {
            format!("type \u{201c}{}\u{201d}", after.value)
        };
    }
    if before.case_sensitive != after.case_sensitive {
        return "match case".to_owned();
    }
    if before.depth != after.depth {
        return if after.depth > 1 {
            "search subfolders".to_owned()
        } else {
            "search this folder only".to_owned()
        };
    }
    if before.match_mode != after.match_mode {
        return if after.match_mode == MatchMode::Any {
            "match any word".to_owned()
        } else {
            "match all words".to_owned()
        };
    }
    "search".to_owned()
}
