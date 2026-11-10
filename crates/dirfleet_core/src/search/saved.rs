//! Saved searches: a named query plus where it runs.

use super::{Search, query};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Where a saved search runs.
#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, Eq)]
pub enum SavedScope {
    /// The folder open in the tab when the search is run.
    #[default]
    CurrentFolder,
    /// Every favorite folder.
    Favorites,
    /// These folders, wherever the search is run from.
    Folders(Vec<PathBuf>),
}

impl SavedScope {
    pub fn describe(&self) -> String {
        match self {
            Self::CurrentFolder => "the open folder".to_owned(),
            Self::Favorites => "favorites".to_owned(),
            Self::Folders(paths) => match paths.as_slice() {
                [one] => one.display().to_string(),
                many => format!("{} folders", many.len()),
            },
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct SavedSearch {
    /// Stable identity, so renaming keeps "Update" working.
    #[serde(default)]
    pub id: u64,
    pub name: String,
    /// The query (`search` in the format before scopes existed).
    #[serde(alias = "search")]
    pub query: Search,
    #[serde(default)]
    pub scope: SavedScope,
}

impl SavedSearch {
    /// "report ext:pdf — in the open folder", for tooltips.
    pub fn summary(&self) -> String {
        let text = query::format(&self.query);
        let depth = if self.query.depth > 1 {
            " and its subfolders"
        } else {
            ""
        };
        format!(
            "{} — in {}{depth}",
            if text.is_empty() {
                "(everything)"
            } else {
                &text
            },
            self.scope.describe()
        )
    }
}

#[derive(Deserialize, Serialize, Default, Debug, Clone, PartialEq, Eq)]
pub struct SavedSearches {
    pub searches: Vec<SavedSearch>,
}

impl SavedSearches {
    /// Gives every entry a unique id; entries saved before ids existed have 0.
    #[must_use]
    pub fn normalized(mut self) -> Self {
        let mut next = self.searches.iter().map(|s| s.id).max().unwrap_or(0) + 1;
        let mut seen = std::collections::HashSet::new();
        for search in &mut self.searches {
            if search.id == 0 || !seen.insert(search.id) {
                search.id = next;
                seen.insert(next);
                next += 1;
            }
        }
        self
    }

    pub fn next_id(&self) -> u64 {
        self.searches.iter().map(|s| s.id).max().unwrap_or(0) + 1
    }

    pub fn get(&self, id: u64) -> Option<&SavedSearch> {
        self.searches.iter().find(|s| s.id == id)
    }

    /// A name not used yet, based on `base`.
    pub fn free_name(&self, base: &str) -> String {
        let base = if base.trim().is_empty() {
            "Search"
        } else {
            base.trim()
        };
        if !self.searches.iter().any(|s| s.name == base) {
            return base.to_owned();
        }
        (2..u32::MAX)
            .map(|n| format!("{base} ({n})"))
            .find(|name| !self.searches.iter().any(|s| &s.name == name))
            .unwrap_or_else(|| base.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::{SavedScope, SavedSearches};

    #[test]
    fn old_saved_searches_load_with_ids_and_scope() {
        let old = r#"{"searches":[{"name":"pdfs","search":{"value":"pdf","depth":1,
            "case_sensitive":false}},{"name":"b","search":{"value":"b","depth":2,
            "case_sensitive":false}}]}"#;
        let saved: SavedSearches = serde_json::from_str(old).expect("parse");
        let saved = saved.normalized();
        assert_eq!(saved.searches[0].query.value, "pdf");
        assert_eq!(saved.searches[0].scope, SavedScope::CurrentFolder);
        assert_ne!(saved.searches[0].id, saved.searches[1].id);
        assert_eq!(saved.free_name("pdfs"), "pdfs (2)");
        assert_eq!(saved.next_id(), 3);
    }
}
