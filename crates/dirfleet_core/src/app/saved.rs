//! Saved searches, as egui's: a tab's search saved under a name with where
//! it runs (the open folder, the favorites, or chosen folders), updated or
//! reverted from the tab, run again from the sidebar, edited, duplicated,
//! reordered and deleted. They are kept between launches.
use crux_core::Command;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::{DirFleet, Effect, Event, Model, Tab, tab_id};
use crate::search::saved::{SavedScope, SavedSearch};
use crate::search::{MatchMode, Search, query};

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SavedSearchView {
    pub id: u64,
    pub name: String,
    /// "report ext:pdf — in the open folder", for tooltips.
    pub summary: String,
    /// The query as it is typed, for the editor.
    pub text: String,
    pub case_sensitive: bool,
    pub depth: u32,
    pub any: bool,
    /// Runs in the favorites rather than the open folder.
    pub favorites: bool,
}

/// The saved search a tab's search came from.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SavedBadge {
    pub id: u64,
    pub name: String,
    /// The search was changed since: Update saves it, Revert undoes it.
    pub modified: bool,
}

pub(super) fn views(saved: &[SavedSearch]) -> Vec<SavedSearchView> {
    saved
        .iter()
        .map(|search| SavedSearchView {
            id: search.id,
            name: search.name.clone(),
            summary: search.summary(),
            text: query::format(&search.query),
            case_sensitive: search.query.case_sensitive,
            depth: u32::try_from(search.query.depth.max(1)).unwrap_or(u32::MAX),
            any: search.query.match_mode == MatchMode::Any,
            favorites: search.scope == SavedScope::Favorites,
        })
        .collect()
}

impl Tab {
    pub(super) fn saved_badge(&self, saved: &[SavedSearch]) -> Option<SavedBadge> {
        let search = self.search.as_ref()?;
        let found = saved.iter().find(|s| Some(s.id) == search.saved_from)?;
        Some(SavedBadge {
            id: found.id,
            name: found.name.clone(),
            modified: !found.query.same_query(search),
        })
    }
}

impl DirFleet {
    /// The events that change or run saved searches; changes to the list
    /// are undoable.
    pub(super) fn saved_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        let name_of = |id: u64| {
            model
                .saved
                .get(id)
                .map(|saved| saved.name.clone())
                .unwrap_or_default()
        };
        let label = match &event {
            Event::SaveSearch { name, .. } => format!("save search \u{201c}{name}\u{201d}"),
            Event::UpdateSavedSearch { .. } => "update saved search".to_owned(),
            Event::DeleteSavedSearch { id } => {
                format!("delete saved search \u{201c}{}\u{201d}", name_of(*id))
            }
            Event::DuplicateSavedSearch { .. } => "duplicate saved search".to_owned(),
            Event::MoveSavedSearch { .. } => "reorder saved searches".to_owned(),
            Event::EditSavedSearch { .. } => "edit saved search".to_owned(),
            _ => return Self::change_saved(model, event),
        };
        let before = model.saved.searches.clone();
        let command = Self::change_saved(model, event);
        Self::record_saved(model, before, label);
        command
    }

    fn change_saved(model: &mut Model, event: Event) -> Command<Effect, Event> {
        let searches = &mut model.saved.searches;
        match event {
            Event::SaveSearch { tab, name } => return Self::save_search(model, tab, &name),
            Event::UpdateSavedSearch { tab } => {
                let Some(search) = tab_id(tab)
                    .and_then(|id| model.tabs.get(&id))
                    .and_then(|state| state.search.clone())
                else {
                    return Command::done();
                };
                if let Some(saved) = searches
                    .iter_mut()
                    .find(|s| Some(s.id) == search.saved_from)
                {
                    saved.query = Search {
                        saved_from: None,
                        ..search
                    };
                }
            }
            Event::RevertSearch { tab } => {
                let saved_from = tab_id(tab)
                    .and_then(|id| model.tabs.get(&id))
                    .and_then(|state| state.search.as_ref()?.saved_from);
                return saved_from.map_or_else(Command::done, |id| Self::run_saved(model, tab, id));
            }
            Event::RunSavedSearch { tab, id } => return Self::run_saved(model, tab, id),
            Event::RunSavedSearchInNewWindow { tab, id } => {
                return Self::run_saved_in_new_window(model, tab, id);
            }
            Event::DeleteSavedSearch { id } => searches.retain(|s| s.id != id),
            Event::DuplicateSavedSearch { id } => {
                let Some(original) = model.saved.get(id).cloned() else {
                    return Command::done();
                };
                let copy = SavedSearch {
                    id: model.saved.next_id(),
                    name: model.saved.free_name(&original.name),
                    ..original
                };
                let at = model
                    .saved
                    .searches
                    .iter()
                    .position(|s| s.id == id)
                    .map_or(0, |i| i + 1);
                model.saved.searches.insert(at, copy);
            }
            Event::MoveSavedSearch { from, to } => {
                let (from, to) = (from as usize, to as usize);
                if from < searches.len() && to < searches.len() {
                    let moved = searches.remove(from);
                    searches.insert(to, moved);
                }
            }
            event @ Event::EditSavedSearch { .. } => Self::edit_saved(model, event),
            _ => return Command::done(),
        }
        render()
    }

    /// Saves the tab's search under a free name based on `name`.
    fn save_search(model: &mut Model, tab: u32, name: &str) -> Command<Effect, Event> {
        let Some(state) = tab_id(tab).and_then(|id| model.tabs.get_mut(&id)) else {
            return Command::done();
        };
        let Some(search) = state.search.clone().filter(Search::has_query) else {
            return Command::done();
        };
        let id = model.saved.next_id();
        let name = model.saved.free_name(name);
        let scope = if state.scope.is_some() {
            SavedScope::Favorites
        } else {
            SavedScope::CurrentFolder
        };
        model.saved.searches.push(SavedSearch {
            id,
            name,
            query: Search {
                saved_from: None,
                ..search.clone()
            },
            scope,
        });
        state.set_search(Some(Search {
            saved_from: Some(id),
            ..search
        }));
        render()
    }

    /// Applies the saved search editor's result.
    fn edit_saved(model: &mut Model, event: Event) {
        let Event::EditSavedSearch {
            id,
            name,
            text,
            case_sensitive,
            depth,
            any,
            favorites,
        } = event
        else {
            return;
        };
        let parsed = query::parse(&text);
        let Some(saved) = model.saved.searches.iter_mut().find(|s| s.id == id) else {
            return;
        };
        if !name.trim().is_empty() {
            name.trim().clone_into(&mut saved.name);
        }
        saved.query = Search {
            chips: parsed.chips,
            case_sensitive,
            depth: depth.max(1) as usize,
            match_mode: if any || parsed.any {
                MatchMode::Any
            } else {
                MatchMode::All
            },
            ..Search::default()
        };
        saved.scope = if favorites {
            SavedScope::Favorites
        } else if matches!(saved.scope, SavedScope::Folders(_)) {
            saved.scope.clone()
        } else {
            SavedScope::CurrentFolder
        };
    }

    /// Opens a window at the saved search's first folder (or `tab`'s) and
    /// runs the search there, as egui's Run in new tab.
    fn run_saved_in_new_window(model: &mut Model, tab: u32, id: u64) -> Command<Effect, Event> {
        let Some(saved) = model.saved.get(id) else {
            return Command::done();
        };
        let start = match &saved.scope {
            SavedScope::Folders(folders) => folders
                .first()
                .map(|folder| folder.to_string_lossy().into_owned()),
            _ => None,
        }
        .or_else(|| tab_id(tab).and_then(|id| model.tabs.get(&id)?.path.clone()))
        .unwrap_or_else(|| model.home.clone());
        let opened = Self::new_window(model, Some(start));
        let Some(new_tab) = model.windows.last().map(|window| window.focused) else {
            return opened;
        };
        opened.and(Self::run_saved(model, new_tab.number(), id))
    }

    /// Runs the saved search `id` in `tab`, where its scope says.
    fn run_saved(model: &mut Model, tab: u32, id: u64) -> Command<Effect, Event> {
        let Some(saved) = model.saved.get(id).cloned() else {
            return Command::done();
        };
        let favorites: Vec<String> = model
            .sidebar
            .favorites
            .iter()
            .map(|f| f.path.clone())
            .collect();
        let Model {
            tabs,
            generation,
            files,
            ..
        } = model;
        let Some(tab_key) = tab_id(tab) else {
            return Command::done();
        };
        let Some(state) = tabs.get_mut(&tab_key) else {
            return Command::done();
        };
        state.scope = match &saved.scope {
            SavedScope::CurrentFolder => None,
            SavedScope::Favorites => (!favorites.is_empty()).then_some(favorites),
            SavedScope::Folders(folders) => Some(
                folders
                    .iter()
                    .map(|folder| folder.to_string_lossy().into_owned())
                    .collect(),
            ),
        };
        state.set_search(Some(Search {
            saved_from: Some(id),
            ..saved.query
        }));
        Self::record_search(files, tab_key, state.commit_search());
        render().and(Self::refilter(state, tab, generation))
    }
}
