//! The Recycle Bin / Trash as a place a tab shows: listed by the effects
//! crate (`~/.Trash` on macOS, the platform's bin elsewhere), with its rows
//! deleted for good, put back where the platform allows it, or emptied.
use crux_core::Command;
use crux_core::capability::Operation;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::{DirFleet, Effect, Event, ListDirOutput, ListView, Model, Tab, tab_id};
use crate::CurrentPath;
use crate::fileops::TrashKey;
use crate::history::HistoryEntry;
use crate::ids::TabId;

/// The key the Trash's own view is kept under among folder views.
pub(super) const TRASH_VIEW: &str = ":trash";

/// List the Trash, sorted and filtered for display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListTrash {
    pub view: ListView,
}

impl Operation for ListTrash {
    type Output = ListDirOutput;
}

/// Change the Trash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrashOp {
    Empty,
    /// Delete these trashed items for good.
    Purge(Vec<TrashKey>),
    /// Put these back where they were deleted from (not on macOS).
    Restore(Vec<TrashKey>),
}

/// How a [`TrashOp`] went: why it failed, if it did.
#[derive(Clone, Debug, PartialEq, Eq, Facet)]
#[facet(opaque)]
pub struct TrashDone(pub Result<(), String>);

impl Operation for TrashOp {
    type Output = TrashDone;
}

/// What the Trash is called here and what it allows, for shells.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct TrashInfo {
    pub label: String,
    /// Whether items can be put back (not on macOS).
    pub can_restore: bool,
}

impl TrashInfo {
    pub fn here() -> Self {
        Self {
            label: crate::current_path::TRASH_LABEL.to_owned(),
            can_restore: cfg!(not(target_os = "macos")),
        }
    }
}

impl Tab {
    /// The selected rows as trashed items, in display order.
    fn selected_trash_keys(&self) -> Vec<TrashKey> {
        if !self.trash {
            return Vec::new();
        }
        self.selection
            .rows()
            .filter_map(|row| {
                let entry = *self.order.get(row)? as usize;
                let at = self.listing.entry(entry);
                Some(TrashKey {
                    path: at.get_path(),
                    deleted_at: *at.meta.modified_at,
                })
            })
            .collect()
    }
}

impl DirFleet {
    /// Shows the Trash in the tab, recording the visit.
    pub(super) fn open_trash(model: &mut Model, tab: u32) -> Command<Effect, Event> {
        let Some(id) = tab_id(tab) else {
            return Command::done();
        };
        if let Some(state) = model.tabs.get_mut(&id)
            && (state.path.is_some() || state.trash)
        {
            let place = state.current_place();
            state.history.save_place(place);
        }
        Self::go_trash(model, id)
    }

    /// Lists the Trash in the tab, dropping what it showed before.
    pub(super) fn go_trash(model: &mut Model, id: TabId) -> Command<Effect, Event> {
        let view = model.view_for(TRASH_VIEW);
        let Some(tab) = model.tabs.get_mut(&id) else {
            return Command::done();
        };
        tab.view = view;
        tab.history
            .visit(HistoryEntry::new(CurrentPath::Trash, None));
        model.generation += 1;
        let generation = model.generation;
        tab.reset_for(generation);
        tab.path = None;
        tab.trash = true;
        tab.committed = None;
        tab.set_search(None);
        let stop_old = tab.stop_watching().and(tab.leave_results());
        render().and(stop_old).and(Self::load(tab, id.number()))
    }

    pub(super) fn list_trash(tab: u32, view: ListView, generation: u64) -> Command<Effect, Event> {
        Command::request_from_shell(ListTrash { view }).then_send(move |output| Event::Listed {
            tab,
            generation,
            output,
        })
    }

    /// The events that change the Trash.
    pub(super) fn trash_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        let op = match event {
            Event::EmptyTrash => TrashOp::Empty,
            Event::PurgeSelection { tab } | Event::RestoreSelection { tab } => {
                let keys = tab_id(tab)
                    .and_then(|id| model.tabs.get(&id))
                    .map(Tab::selected_trash_keys)
                    .unwrap_or_default();
                if keys.is_empty() {
                    return Command::done();
                }
                if matches!(event, Event::RestoreSelection { .. }) {
                    if !TrashInfo::here().can_restore {
                        return Command::done();
                    }
                    TrashOp::Restore(keys)
                } else {
                    TrashOp::Purge(keys)
                }
            }
            Event::TrashChanged { done } => return Self::trash_changed(model, &done),
            _ => return Command::done(),
        };
        Command::request_from_shell(op).then_send(|done| Event::TrashChanged { done })
    }

    /// Lists every Trash tab again, showing what went wrong in them.
    fn trash_changed(model: &mut Model, done: &TrashDone) -> Command<Effect, Event> {
        let lists: Vec<_> = model
            .tabs
            .iter_mut()
            .filter(|(_, tab)| tab.trash)
            .map(|(id, tab)| {
                tab.error = done.0.clone().err();
                tab.loading = true;
                Self::list_trash(id.number(), tab.list_view(), tab.generation)
            })
            .collect();
        render().and(Command::all(lists))
    }
}
