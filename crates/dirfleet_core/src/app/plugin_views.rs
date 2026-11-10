//! What plugins add to the window, as egui's host: segments of the bar below
//! a folder (where the plugin applies), sections of the inspector for the
//! selection, and the events plugins subscribe to.
//!
//! Like thumbnails, missing or stale contents are asked for after each
//! event. A plugin's contents go stale when one of its actions or jobs ends,
//! when it says so, when the folder changes, and, for the bar, after its
//! `refresh_secs`.
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use crux_core::Command;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::plugins::{PluginControl, control};
use super::{Delay, DirFleet, Effect, Event, Model, Tab, tab_id};
use crate::plugins::{BarSegment, CallContext, InspectorRow, Request, TargetInfo};

/// How long the selection rests before `selection_changed`, as egui's.
const SELECTION_SETTLE_MILLIS: u64 = 250;

#[derive(Default)]
pub(super) struct BarState {
    segments: Vec<BarSegment>,
    error: Option<String>,
    loading: bool,
    /// It arrived once.
    fetched: bool,
    /// When, in Unix seconds; 0 when the clock hadn't ticked yet, until it
    /// first does.
    fetched_at: u64,
    stale: bool,
}

#[derive(Default)]
pub(super) struct SectionState {
    /// Which selection it is for.
    key: u64,
    rows: Vec<InspectorRow>,
    error: Option<String>,
    loading: bool,
    stale: bool,
}

/// Plugin contents of the window, and the events' bookkeeping.
#[derive(Default)]
pub(super) struct PluginViews {
    /// By plugin id and folder.
    bars: HashMap<(String, String), BarState>,
    /// By plugin id.
    sections: HashMap<String, SectionState>,
    /// Plugins that were told the app started.
    pub(super) started: Vec<String>,
    /// Counts selection changes; a pause in them sends `selection_changed`
    /// for the tab they were in.
    selection_changes: u64,
    selection_tab: Option<u32>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BarSegmentView {
    pub text: String,
    /// A small rounded label, such as a count.
    pub badge: String,
    /// A button's text; it runs `action`.
    pub button: String,
    /// `<plugin id>.<action id>`, or "".
    pub action: String,
    pub icon: String,
    pub tooltip: String,
    /// "normal", "muted", "good", "warn" or "bad".
    pub tone: String,
    pub spinner: bool,
}

/// One plugin's part of the bar below a folder.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PluginBarView {
    pub plugin: String,
    pub segments: Vec<BarSegmentView>,
    /// Why it couldn't be made, or "".
    pub error: String,
    pub loading: bool,
}

/// One plugin's section of the inspector.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PluginSectionView {
    pub title: String,
    /// Label and value of each row.
    pub rows: Vec<PluginSectionRow>,
    pub error: String,
    pub loading: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PluginSectionRow {
    pub label: String,
    pub value: String,
}

impl PluginViews {
    /// A plugin acted, or says its contents changed.
    pub(super) fn invalidate(&mut self, plugin: &str, bar: bool, inspector: bool) {
        if bar {
            for ((owner, _), state) in &mut self.bars {
                if owner == plugin {
                    state.stale = true;
                }
            }
        }
        if inspector && let Some(state) = self.sections.get_mut(plugin) {
            state.stale = true;
        }
    }

    /// A folder changed: its bars, and the inspector, may say something else.
    pub(super) fn folder_changed(&mut self, dir: &str) {
        for ((_, shown), state) in &mut self.bars {
            if shown == dir {
                state.stale = true;
            }
        }
        for state in self.sections.values_mut() {
            state.stale = true;
        }
    }

    /// Everything goes when plugins load again.
    pub(super) fn clear(&mut self) {
        self.bars.clear();
        self.sections.clear();
    }
}

/// Which selection a section is for: the items and when they changed.
pub(super) fn targets_key(targets: &[TargetInfo]) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    for target in targets {
        target.path.hash(&mut hasher);
        target.modified.hash(&mut hasher);
    }
    hasher.finish()
}

impl Tab {
    /// What plugins' handlers get for this tab: its folder and selection.
    pub(super) fn plugin_context(&self, number: u32) -> CallContext {
        CallContext {
            dir: self.path.clone().map(PathBuf::from),
            targets: self.plugin_targets().as_ref().clone(),
            tab: Some(number),
            values: None,
        }
    }
}

impl DirFleet {
    /// The bar below the tab's folder: the plugins with a bar that apply.
    pub(super) fn plugin_bar_view(model: &Model, tab: &Tab) -> Vec<PluginBarView> {
        let Some(dir) = tab.path.as_deref() else {
            return Vec::new();
        };
        model
            .plugins
            .with_bar(dir)
            .into_iter()
            .filter_map(|(id, name)| {
                let state = model
                    .plugins
                    .views
                    .bars
                    .get(&(id.clone(), dir.to_owned()))?;
                Some(PluginBarView {
                    plugin: name,
                    segments: state
                        .segments
                        .iter()
                        .map(|segment| BarSegmentView {
                            text: segment.text.clone().unwrap_or_default(),
                            badge: segment.badge.clone().unwrap_or_default(),
                            button: segment.button.clone().unwrap_or_default(),
                            action: segment
                                .action
                                .as_ref()
                                .map(|action| format!("{id}.{action}"))
                                .unwrap_or_default(),
                            icon: segment.icon.clone().unwrap_or_default(),
                            tooltip: segment.tooltip.clone().unwrap_or_default(),
                            tone: segment.tone.clone().unwrap_or_default(),
                            spinner: segment.spinner,
                        })
                        .collect(),
                    error: state.error.clone().unwrap_or_default(),
                    loading: state.loading,
                })
            })
            .collect()
    }

    /// The inspector's plugin sections for the tab's selection.
    pub(super) fn plugin_sections_view(
        model: &Model,
        id: u32,
        tab: &Tab,
    ) -> Vec<PluginSectionView> {
        if !model.inspector_shown || !model.plugins.has_sections() {
            return Vec::new();
        }
        let _ = id;
        let targets = tab.plugin_targets();
        let key = tab.plugin_targets_key();
        model
            .plugins
            .with_section(tab.path.as_deref(), &targets)
            .into_iter()
            .filter_map(|(plugin, title)| {
                let state = model
                    .plugins
                    .views
                    .sections
                    .get(&plugin)
                    .filter(|state| state.key == key)?;
                Some(PluginSectionView {
                    title,
                    rows: state
                        .rows
                        .iter()
                        .map(|row| {
                            let (label, value) = match row {
                                InspectorRow::Pair(label, value)
                                | InspectorRow::Named { label, value } => (label, value),
                            };
                            PluginSectionRow {
                                label: label.clone(),
                                value: value.clone(),
                            }
                        })
                        .collect(),
                    error: state.error.clone().unwrap_or_default(),
                    loading: state.loading,
                })
            })
            .collect()
    }

    /// Asks plugins for missing or stale bars of the shown folders and
    /// sections of the focused tabs' selection; after every event.
    pub(super) fn request_plugin_views(model: &mut Model) -> Command<Effect, Event> {
        let now = model.now;
        let mut wanted_bars: Vec<(String, String)> = Vec::new();
        let mut wanted_sections: Vec<(String, super::TabId, u64)> = Vec::new();
        for window in &model.windows {
            for id in window.layout.shown() {
                let Some(dir) = model.tabs.get(&id).and_then(|tab| tab.path.clone()) else {
                    continue;
                };
                for (plugin, _) in model.plugins.with_bar(&dir) {
                    wanted_bars.push((plugin, dir.clone()));
                }
            }
            if model.inspector_shown
                && model.plugins.has_sections()
                && let Some(tab) = model.tabs.get(&window.focused)
            {
                let targets = tab.plugin_targets();
                let key = tab.plugin_targets_key();
                for (plugin, _) in model.plugins.with_section(tab.path.as_deref(), &targets) {
                    wanted_sections.push((plugin, window.focused, key));
                }
            }
        }
        let mut commands = Vec::new();
        for (plugin, dir) in wanted_bars {
            let refresh = model.plugins.bar_refresh_secs(&plugin);
            let state = model
                .plugins
                .views
                .bars
                .entry((plugin.clone(), dir.clone()))
                .or_default();
            if state.fetched && state.fetched_at == 0 {
                state.fetched_at = now;
            }
            if refresh.is_some_and(|secs| {
                state.fetched_at > 0 && now.saturating_sub(state.fetched_at) >= secs
            }) {
                state.stale = true;
            }
            if state.loading || (state.fetched && !state.stale) {
                continue;
            }
            state.loading = true;
            state.stale = false;
            commands.push(control(PluginControl::Send {
                id: plugin,
                request: Request::Bar {
                    dir: PathBuf::from(&dir),
                    context: CallContext {
                        dir: Some(PathBuf::from(dir)),
                        ..Default::default()
                    },
                },
            }));
        }
        for (plugin, tab, key) in wanted_sections {
            let state = model
                .plugins
                .views
                .sections
                .entry(plugin.clone())
                .or_default();
            if state.key == key && !state.stale {
                continue;
            }
            if state.key != key {
                state.rows.clear();
                state.error = None;
            }
            state.key = key;
            state.stale = false;
            state.loading = true;
            // The selection is copied for the plugin only when it is asked.
            let Some(context) = model
                .tabs
                .get(&tab)
                .map(|state| state.plugin_context(tab.number()))
            else {
                continue;
            };
            commands.push(control(PluginControl::Send {
                id: plugin,
                request: Request::Inspector { key, context },
            }));
        }
        Command::all(commands)
    }

    /// A plugin's bar for `dir` arrived, or why not.
    pub(super) fn plugin_bar_made(
        model: &mut Model,
        plugin: &str,
        dir: &std::path::Path,
        result: Result<Vec<BarSegment>, String>,
    ) -> Command<Effect, Event> {
        let now = model.now;
        let state = model
            .plugins
            .views
            .bars
            .entry((plugin.to_owned(), dir.to_string_lossy().into_owned()))
            .or_default();
        state.loading = false;
        state.fetched = true;
        state.fetched_at = now;
        match result {
            Ok(segments) => {
                state.segments = segments;
                state.error = None;
                render()
            }
            Err(error) => {
                let first = state.error.is_none();
                state.error = Some(error.clone());
                if first {
                    Self::plugin_error(model, plugin, format!("bar: {error}"))
                } else {
                    render()
                }
            }
        }
    }

    /// A plugin's inspector section arrived, or why not.
    pub(super) fn plugin_section_made(
        model: &mut Model,
        plugin: &str,
        key: u64,
        result: Result<Vec<InspectorRow>, String>,
    ) -> Command<Effect, Event> {
        let Some(state) = model
            .plugins
            .views
            .sections
            .get_mut(plugin)
            .filter(|state| state.key == key)
        else {
            return Command::done();
        };
        state.loading = false;
        match result {
            Ok(rows) => {
                state.rows = rows;
                state.error = None;
                render()
            }
            Err(error) => {
                state.rows.clear();
                state.error = Some(error.clone());
                Self::plugin_error(model, plugin, format!("inspector: {error}"))
            }
        }
    }

    /// Tells the plugins subscribed to `name`.
    pub(super) fn emit_plugin_event(
        model: &Model,
        name: &str,
        context: &CallContext,
        data: &serde_json::Value,
    ) -> Command<Effect, Event> {
        Command::all(model.plugins.subscribed(name).into_iter().map(|id| {
            control(PluginControl::Send {
                id,
                request: Request::Event {
                    name: name.to_owned(),
                    context: context.clone(),
                    data: data.clone(),
                },
            })
        }))
    }

    /// The selection changed: once it rests, plugins hear of it.
    pub(super) fn selection_moved(model: &mut Model, tab: u32) -> Command<Effect, Event> {
        if model.plugins.subscribed("selection_changed").is_empty() {
            return Command::done();
        }
        model.plugins.views.selection_tab = Some(tab);
        model.plugins.views.selection_changes += 1;
        let count = model.plugins.views.selection_changes;
        Command::request_from_shell(Delay {
            millis: SELECTION_SETTLE_MILLIS,
        })
        .then_send(move |()| Event::SelectionSettled { count })
    }

    pub(super) fn selection_settled(model: &Model, count: u64) -> Command<Effect, Event> {
        if count != model.plugins.views.selection_changes {
            return Command::done();
        }
        let tab = model.plugins.views.selection_tab;
        let Some((number, state)) = tab.and_then(|tab| Some((tab, model.tabs.get(&tab_id(tab)?)?)))
        else {
            return Command::done();
        };
        let context = state.plugin_context(number);
        Self::emit_plugin_event(
            model,
            "selection_changed",
            &context,
            &serde_json::Value::Null,
        )
    }
}
