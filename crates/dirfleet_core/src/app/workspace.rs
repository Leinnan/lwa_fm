//! Tabs and panes within windows: opening, closing, moving and splitting
//! tabs, with egui's rules (new tabs go to the end of the focused pane, the
//! main window keeps its last tab, the last 20 closed tabs can be reopened).
use crux_core::Command;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::layout::{FlatLayout, Node};
use super::{DirFleet, Effect, Event, Model, Tab, Window, tab_id, window_id};
use crate::app::OpenWindow;
use crate::ids::{TabId, WindowId};

/// How many closed tabs Reopen remembers, as egui does.
pub(super) const CLOSED_KEPT: usize = 20;

/// A tab a shell made itself: its id and folder.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AdoptedTab {
    pub tab: u32,
    pub path: Option<String>,
}

/// Where [`Event::MoveTab`] puts a tab.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum TabTarget {
    /// A window of its own.
    NewWindow,
    /// The pane holding `tab`, at `index` among its tabs.
    Beside { tab: u32, index: u32 },
}

impl Model {
    pub(super) fn window_of(&self, tab: TabId) -> Option<usize> {
        self.windows
            .iter()
            .position(|window| window.layout.contains(tab))
    }

    fn window_index(&self, window: u32) -> Option<usize> {
        let id = window_id(window)?;
        self.windows.iter().position(|w| w.id == id)
    }
}

impl Window {
    /// Points focus at a shown tab again after the layout changed.
    fn refocus(&mut self) {
        let shown = self.layout.shown();
        if !shown.contains(&self.focused)
            && let Some(&first) = shown.first()
        {
            self.focused = first;
        }
    }

    fn focused_pane_tabs(&self) -> (Vec<TabId>, usize) {
        self.layout
            .panes()
            .into_iter()
            .find(|pane| pane.tabs.contains(&self.focused))
            .map_or_else(|| (Vec::new(), 0), |pane| (pane.tabs.clone(), pane.active))
    }
}

impl DirFleet {
    /// The events that open, close, move and arrange tabs.
    pub(super) fn workspace_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        match event {
            Event::NewTab { window, path } => Self::new_tab(model, window, path),
            Event::CloseTab { tab } => Self::close_tab(model, tab),
            Event::ActivateTab { tab } => Self::activate_tab(model, tab),
            Event::DuplicateTab { tab } => Self::duplicate_tab(model, tab),
            Event::CloseOtherTabs { tab } => Self::close_other_tabs(model, tab),
            Event::ReopenClosedTab { window } => {
                model.closed.pop().map_or_else(Command::done, |path| {
                    Self::new_tab(model, window, Some(path))
                })
            }
            Event::CycleTab { window, forward } => Self::cycle_tab(model, window, forward),
            Event::GoToTab { window, number } => Self::go_to_tab(model, window, number),
            Event::SplitTab { tab, side } => {
                let Some(id) = tab_id(tab) else {
                    return Command::done();
                };
                let Some(index) = model.window_of(id) else {
                    return Command::done();
                };
                let window = &mut model.windows[index];
                if window.layout.split(id, side) {
                    window.focused = id;
                    window.refocus();
                }
                render()
            }
            Event::ResizeSplit {
                window,
                split,
                permille,
            } => {
                if let Some(index) = model.window_index(window) {
                    let window = &mut model.windows[index];
                    let mut flat = window.layout.flatten(TabId::number);
                    if let Some(entry) = flat.splits.get_mut(split as usize) {
                        entry.permille = permille.clamp(50, 950);
                        if let Some(layout) = Node::unflatten(&flat, tab_id) {
                            window.layout = layout;
                        }
                    }
                }
                render()
            }
            Event::SplitNew { tab, side } => {
                let Some(id) = tab_id(tab) else {
                    return Command::done();
                };
                let opened = Self::duplicate_tab(model, tab);
                let Some(index) = model.window_of(id) else {
                    return opened;
                };
                let window = &mut model.windows[index];
                let copy = window.focused;
                if copy != id {
                    window.layout.split(copy, side);
                    window.refocus();
                }
                opened.and(render())
            }
            Event::ReopenClosedAsWindow => model
                .closed
                .pop()
                .map_or_else(Command::done, |path| Self::new_window(model, Some(path))),
            Event::AdoptWindow { tabs, active } => Self::adopt_window(model, tabs, active),
            Event::AdoptTab { window, tab } => Self::adopt_tab(model, window, tab),
            Event::CloseFirstWindow => Self::close_first_window(model),
            Event::MoveTab { tab, to } => Self::move_tab(model, tab, to),
            Event::SetLayout { window, layout } => Self::set_layout(model, window, &layout),
            Event::WindowGrouped { window, group } => {
                if let Some(index) = model.window_index(window) {
                    model.windows[index].group = group;
                }
                render()
            }
            Event::WindowFocused { window } => Self::window_focused(model, window),
            Event::WindowFramed { window, frame } => {
                if let Some(index) = model.window_index(window) {
                    model.windows[index].frame = Some(frame);
                }
                // The shell has it already: only the saved state changes.
                Command::done()
            }
            _ => Command::done(),
        }
    }

    /// Coming back to a window, as egui: files may have been opened in other
    /// apps meanwhile, so its folders sorted or filtered by opening are read
    /// again; and cut items are un-dimmed if the clipboard changed.
    fn window_focused(model: &mut Model, window: u32) -> Command<Effect, Event> {
        let Some(index) = model.window_index(window) else {
            return Command::done();
        };
        let stale: Vec<u32> = model.windows[index]
            .layout
            .tabs()
            .into_iter()
            .filter(|id| model.tabs.get(id).is_some_and(Tab::shows_by_opening))
            .map(TabId::number)
            .collect();
        let mut command = Self::check_cut(model);
        for tab in stale {
            command = command.and(Self::refresh(model, tab));
        }
        command
    }

    /// A tab at `path` (the focused tab's folder without one) at the end of
    /// the window's focused pane.
    fn new_tab(model: &mut Model, window: u32, path: Option<String>) -> Command<Effect, Event> {
        let Some(index) = model.window_index(window) else {
            return Command::done();
        };
        let focused = model.windows[index].focused;
        let Some(path) = path.or_else(|| model.tabs.get(&focused)?.path.clone()) else {
            return Command::done();
        };
        let tab = TabId::new(model.tab_ids.next_id());
        model.tabs.insert(tab, Tab::default());
        let window = &mut model.windows[index];
        let end = window.focused_pane_tabs().0.len();
        window.layout.insert(focused, tab, end);
        window.focused = tab;
        Self::go(model, tab, path, None)
    }

    /// Takes in a tab the shell made, unless its id is taken or invalid.
    fn adopt(model: &mut Model, adopted: AdoptedTab) -> Option<TabId> {
        let id = tab_id(adopted.tab).filter(|id| !model.tabs.contains_key(id))?;
        if let Some(number) = std::num::NonZeroU32::new(adopted.tab) {
            model.tab_ids.reserve_past(number);
        }
        model.tabs.insert(
            id,
            Tab {
                path: adopted.path,
                ..Tab::default()
            },
        );
        Some(id)
    }

    fn adopt_window(
        model: &mut Model,
        tabs: Vec<AdoptedTab>,
        active: u32,
    ) -> Command<Effect, Event> {
        let ids: Vec<TabId> = tabs
            .into_iter()
            .filter_map(|tab| Self::adopt(model, tab))
            .collect();
        let Some(&focused) = ids.get(active as usize).or_else(|| ids.first()) else {
            return Command::done();
        };
        let window = WindowId::new(model.window_ids.next_id());
        let mut layout = Node::Pane(super::Pane {
            tabs: ids,
            active: 0,
        });
        layout.activate(focused);
        model.windows.push(Window {
            id: window,
            layout,
            focused,
            group: None,
            frame: None,
        });
        let open: Command<Effect, Event> = Command::notify_shell(OpenWindow {
            window: window.number(),
        })
        .into();
        open.and(Self::show_tab(model, focused)).and(render())
    }

    fn adopt_tab(model: &mut Model, window: u32, tab: AdoptedTab) -> Command<Effect, Event> {
        let Some(index) = model.window_index(window) else {
            return Command::done();
        };
        let Some(id) = Self::adopt(model, tab) else {
            return Command::done();
        };
        let window = &mut model.windows[index];
        let end = window.focused_pane_tabs().0.len();
        window.layout.insert(window.focused, id, end);
        window.focused = id;
        Self::show_tab(model, id).and(render())
    }

    fn close_first_window(model: &mut Model) -> Command<Effect, Event> {
        if model.windows.len() < 2 {
            return Command::done();
        }
        let second = model.windows.remove(1);
        let first = &mut model.windows[0];
        let old = std::mem::replace(&mut first.layout, second.layout);
        first.focused = second.focused;
        let stops: Vec<_> = old
            .tabs()
            .into_iter()
            .filter_map(|tab| model.tabs.remove(&tab))
            .map(|mut tab| {
                if let Some(path) = tab.path.take() {
                    model.closed.push(path);
                }
                tab.stop_reading()
            })
            .collect();
        let excess = model.closed.len().saturating_sub(CLOSED_KEPT);
        model.closed.drain(..excess);
        Command::all(stops).and(render())
    }

    fn duplicate_tab(model: &mut Model, tab: u32) -> Command<Effect, Event> {
        let Some((id, path)) =
            tab_id(tab).and_then(|id| Some((id, model.tabs.get(&id)?.path.clone()?)))
        else {
            return Command::done();
        };
        let Some(index) = model.window_of(id) else {
            return Command::done();
        };
        let copy = TabId::new(model.tab_ids.next_id());
        model.tabs.insert(copy, Tab::default());
        let window = &mut model.windows[index];
        window.layout.insert_after(id, copy);
        window.focused = copy;
        Self::go(model, copy, path, None)
    }

    /// Shows a tab in its pane and focuses it, listing it if it never was.
    fn activate_tab(model: &mut Model, tab: u32) -> Command<Effect, Event> {
        let Some(id) = tab_id(tab) else {
            return Command::done();
        };
        let Some(index) = model.window_of(id) else {
            return Command::done();
        };
        let window = &mut model.windows[index];
        window.layout.activate(id);
        window.focused = id;
        Self::show_tab(model, id).and(render())
    }

    fn cycle_tab(model: &mut Model, window: u32, forward: bool) -> Command<Effect, Event> {
        let Some(index) = model.window_index(window) else {
            return Command::done();
        };
        let (tabs, active) = model.windows[index].focused_pane_tabs();
        if tabs.len() < 2 {
            return Command::done();
        }
        let next = if forward {
            (active + 1) % tabs.len()
        } else {
            (active + tabs.len() - 1) % tabs.len()
        };
        Self::activate_tab(model, tabs[next].number())
    }

    /// Shows tab `number` (from 1) of the focused pane; 9 is the last.
    fn go_to_tab(model: &mut Model, window: u32, number: u8) -> Command<Effect, Event> {
        let Some(index) = model.window_index(window) else {
            return Command::done();
        };
        let (tabs, _) = model.windows[index].focused_pane_tabs();
        let target = if number == 9 {
            tabs.last()
        } else {
            tabs.get(usize::from(number.saturating_sub(1)))
        };
        match target {
            Some(&tab) => Self::activate_tab(model, tab.number()),
            None => Command::done(),
        }
    }

    /// Closes a tab, remembering its folder for Reopen. The first window
    /// keeps its last tab; another window closes with its last tab.
    fn close_tab(model: &mut Model, tab: u32) -> Command<Effect, Event> {
        let Some(id) = tab_id(tab) else {
            return Command::done();
        };
        let Some(index) = model.window_of(id) else {
            return Command::done();
        };
        if index == 0 && model.windows[0].layout.tabs().len() == 1 {
            return Command::done();
        }
        let stop = Self::retire(model, index, id);
        stop.and(render())
    }

    fn close_other_tabs(model: &mut Model, tab: u32) -> Command<Effect, Event> {
        let Some(id) = tab_id(tab) else {
            return Command::done();
        };
        let Some(index) = model.window_of(id) else {
            return Command::done();
        };
        let stops: Vec<_> = model.windows[index]
            .layout
            .tabs()
            .into_iter()
            .filter(|&other| other != id)
            .map(|other| Self::retire(model, index, other))
            .collect();
        Command::all(stops).and(render())
    }

    /// Takes a tab out of window `index` for good: its watch stops, its
    /// folder goes on the reopen list, and an emptied window goes.
    fn retire(model: &mut Model, index: usize, id: TabId) -> Command<Effect, Event> {
        let window = &mut model.windows[index];
        window.layout.remove(id);
        window.refocus();
        let emptied = window.layout.is_empty();
        if emptied {
            model.windows.remove(index);
        }
        let Some(mut tab) = model.tabs.remove(&id) else {
            return Command::done();
        };
        if let Some(path) = tab.path.take() {
            model.closed.push(path);
            let excess = model.closed.len().saturating_sub(CLOSED_KEPT);
            model.closed.drain(..excess);
        }
        tab.stop_reading()
    }

    fn move_tab(model: &mut Model, tab: u32, to: TabTarget) -> Command<Effect, Event> {
        let Some(id) = tab_id(tab) else {
            return Command::done();
        };
        let Some(from) = model.window_of(id) else {
            return Command::done();
        };
        let target = match to {
            TabTarget::NewWindow => None,
            TabTarget::Beside { tab, index } => {
                let Some(beside) = tab_id(tab).filter(|&beside| beside != id) else {
                    return Command::done();
                };
                let Some(window) = model.window_of(beside) else {
                    return Command::done();
                };
                Some((window, beside, index as usize))
            }
        };
        // The first window keeps its last tab.
        if from == 0 && model.windows[0].layout.tabs().len() == 1 {
            return Command::done();
        }
        let source = &mut model.windows[from];
        source.layout.remove(id);
        source.refocus();
        let opened = match target {
            None => {
                let window = WindowId::new(model.window_ids.next_id());
                model.windows.push(Window {
                    id: window,
                    layout: Node::single(id),
                    focused: id,
                    group: None,
                    frame: None,
                });
                Command::notify_shell(OpenWindow {
                    window: window.number(),
                })
                .into()
            }
            Some((window, beside, index)) => {
                let window = &mut model.windows[window];
                window.layout.insert(beside, id, index);
                window.focused = id;
                Command::done()
            }
        };
        if model.windows[from].layout.is_empty() {
            model.windows.remove(from);
        }
        opened.and(render())
    }

    /// egui's own arrangement after a drag or split, adopted when it holds
    /// exactly the window's tabs.
    fn set_layout(model: &mut Model, window: u32, layout: &FlatLayout) -> Command<Effect, Event> {
        let Some(index) = model.window_index(window) else {
            return Command::done();
        };
        let window = &mut model.windows[index];
        let mut current = window.layout.tabs();
        let Some(new) = Node::unflatten(layout, |number| {
            tab_id(number).filter(|tab| current.contains(tab))
        }) else {
            return Command::done();
        };
        let mut tabs = new.tabs();
        tabs.sort_unstable();
        current.sort_unstable();
        if tabs != current {
            return Command::done();
        }
        window.layout = new;
        window.refocus();
        let lists: Vec<_> = window
            .layout
            .shown()
            .into_iter()
            .map(|tab| Self::show_tab(model, tab))
            .collect();
        Command::all(lists).and(render())
    }
}
