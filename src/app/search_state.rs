//! Turns search edits into undo steps and history entries.
//!
//! Chip, scope and option changes are committed at once. Typing is committed
//! after a short pause (or on Enter / leaving the box), so one word is one
//! undo step rather than one per key.

use super::history::HistoryEntry;
use super::undo::UndoItem;
use super::{App, Search, TabAction};
use crate::app::dock::TabId;
use crate::fileops::Origin;
use std::time::{Duration, Instant};

/// How long typing must pause before it becomes an undo step.
const TYPING_COMMIT_DELAY: Duration = Duration::from_millis(700);

impl App {
    /// Records the tab's current search as an undo step and a history entry
    /// if it differs from the last recorded one.
    pub(crate) fn commit_search(&mut self, tab_id: TabId, label: &str) {
        let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
            return;
        };
        tab.search_dirty_since = None;
        let unchanged = match (&tab.search_committed, &tab.search) {
            (None, None) => true,
            (Some(a), Some(b)) => a.same_query(b),
            (Some(s), None) | (None, Some(s)) => !s.has_query(),
        };
        if unchanged {
            tab.search_committed.clone_from(&tab.search);
            return;
        }
        let before = tab.search_committed.take();
        let after = tab.search.clone();
        tab.search_committed.clone_from(&after);
        tab.history
            .visit(HistoryEntry::new(tab.current_path.clone(), after.clone()));
        self.file_ops.undo.record(
            Origin::User,
            UndoItem::Search {
                tab: tab_id,
                before,
                after,
                label: label.to_owned(),
            },
        );
    }

    /// Notes that the search text changed; it is committed after a pause.
    pub(crate) fn search_typed(&mut self, tab_id: TabId) {
        if let Some(tab) = self.tabs.get_tab_by_id(tab_id) {
            tab.search_dirty_since.get_or_insert_with(Instant::now);
            tab.search_dirty_since = Some(Instant::now());
        }
    }

    /// Commits typing that has paused, for every tab.
    pub(crate) fn commit_paused_typing(&mut self, ctx: &egui::Context) {
        let due: Vec<(TabId, String)> = self
            .tabs
            .tabs()
            .filter_map(|tab| {
                let since = tab.search_dirty_since?;
                if since.elapsed() >= TYPING_COMMIT_DELAY {
                    let text = tab
                        .search
                        .as_ref()
                        .map(|s| s.value.clone())
                        .unwrap_or_default();
                    Some((tab.id, text))
                } else {
                    ctx.request_repaint_after(TYPING_COMMIT_DELAY.saturating_sub(since.elapsed()));
                    None
                }
            })
            .collect();
        for (tab_id, text) in due {
            let label = if text.is_empty() {
                "clear search text".to_owned()
            } else {
                format!("search \"{text}\"")
            };
            self.commit_search(tab_id, &label);
        }
    }

    /// Puts a search back on a tab (undo, redo) without recording it again.
    pub(crate) fn restore_search(&mut self, tab_id: TabId, search: Option<Search>) {
        let Some(tab) = self.tabs.get_tab_by_id(tab_id) else {
            return;
        };
        tab.search_dirty_since = None;
        tab.search_committed.clone_from(&search);
        let path = tab.current_path.clone();
        tab.search.clone_from(&search);
        tab.history.visit(HistoryEntry::new(path, search));
        tab.update_visible_entries();
        TabAction::RequestFilesRefresh.schedule_tab(tab_id);
    }
}
