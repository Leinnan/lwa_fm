//! The open windows, the tabs each shows, and how they are laid out.
//!
//! Tabs live in one store keyed by [`TabId`]; a window's dock layout only
//! refers to them, so a tab moves between panes and windows without being
//! copied, and closing one is a single lookup however the panes are split.

use super::{MyTabViewer, TabData, TabId, TabRequests, TabStore};
use crate::app::assets::AssetManager;
use egui::Ui;
use egui_dock::{DockArea, DockState, Style};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Names one OS window. [`WindowId::MAIN`] is egui's root viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowId(NonZeroU32);

impl WindowId {
    pub const MAIN: Self = Self(NonZeroU32::MIN);

    fn next() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(2);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        Self(NonZeroU32::new(id).expect("window ids start at 2"))
    }

    /// The egui viewport this window is drawn in.
    pub fn viewport(self) -> egui::ViewportId {
        if self == Self::MAIN {
            egui::ViewportId::ROOT
        } else {
            egui::ViewportId::from_hash_of(("lwa_fm_window", self.0))
        }
    }
}

/// One OS window: its tab layout and the panels drawn over it.
#[derive(Debug)]
pub struct Window {
    pub id: WindowId,
    /// Which pane shows which tabs, by id.
    pub(in crate::app) dock_state: DockState<TabId>,
    /// The sidebar is shown over the files (a narrow window).
    pub sidebar_overlay: bool,
    /// The inspector is shown over the files (a narrow window).
    pub inspector_overlay: bool,
    /// Where the window was on screen in its last frame, for drops.
    pub outer_rect: Option<egui::Rect>,
    /// The area inside the window, as of its last frame.
    pub content_rect: Option<egui::Rect>,
    /// Where a new window should open, in screen points.
    pub open_at: Option<egui::Rect>,
}

impl Window {
    fn new(id: WindowId, tabs: Vec<TabId>) -> Self {
        Self {
            id,
            dock_state: DockState::new(tabs),
            sidebar_overlay: false,
            inspector_overlay: false,
            outer_rect: None,
            content_rect: None,
            open_at: None,
        }
    }

    pub fn tab_ids(&self) -> impl Iterator<Item = TabId> + '_ {
        self.dock_state.iter_all_tabs().map(|(_, id)| *id)
    }

    fn tab_count(&self) -> usize {
        self.dock_state.iter_all_tabs().count()
    }

    /// When no pane has focus (a split was just made, or the focused pane
    /// was closed) the first pane takes it.
    fn ensure_focus(&mut self) {
        if self.dock_state.find_active_focused().is_some() {
            return;
        }
        let first = self
            .dock_state
            .iter_leaves()
            .find(|(_, leaf)| !leaf.tabs().is_empty())
            .map(|(path, _)| path);
        if let Some(path) = first {
            self.dock_state.set_focused_node_and_surface(path);
        }
    }

    fn active_tab(&mut self) -> Option<TabId> {
        self.ensure_focus();
        self.dock_state.find_active_focused().map(|(_, id)| *id)
    }

    /// Adds a tab to the focused pane.
    fn push(&mut self, id: TabId) {
        self.ensure_focus();
        self.dock_state.push_to_focused_leaf(id);
    }
}

/// A window as saved between runs: its panes, with the folder each tab showed.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SavedWindow {
    layout: DockState<PathBuf>,
    /// Where it was on screen; the main window's place is kept by eframe.
    open_at: Option<egui::Rect>,
}

/// Where a tab dragged out of a window was let go, in screen points.
#[derive(Debug, Clone, Copy)]
struct TearOff {
    pointer: egui::Pos2,
    /// Size for a new window there.
    size: egui::Vec2,
}

/// Every window and every tab. Methods without a window argument act on the
/// current window: the one being drawn, or the one a command came from.
#[derive(Debug)]
pub struct Workspace {
    tabs: TabStore,
    /// `windows[0]` is the main window and is never removed.
    windows: Vec<Window>,
    current: WindowId,
    /// Closed tabs whose watchers and loads still need releasing.
    retired: Vec<TabData>,
    /// The current window's file view takes keyboard input.
    pub focused: bool,
    /// Items cut to the clipboard; drawn dimmed until pasted or replaced.
    pub cut: Arc<std::collections::HashSet<PathBuf>>,
    /// Folders of recently closed tabs, newest last, for "Reopen closed tab".
    pub closed: Vec<PathBuf>,
    /// Mounted removable drives, offered as "Copy to" / "Move to" targets.
    pub removable_drives: Vec<PathBuf>,
}

impl Workspace {
    pub fn new(path: &Path) -> Self {
        Self::from_tabs(vec![TabData::from_path(path)])
    }

    /// A main window with one pane holding these tabs; the first is active.
    pub fn from_tabs(tabs: Vec<TabData>) -> Self {
        let ids = tabs.iter().map(|tab| tab.id).collect();
        Self {
            tabs: tabs.into_iter().map(|tab| (tab.id, tab)).collect(),
            windows: vec![Window::new(WindowId::MAIN, ids)],
            current: WindowId::MAIN,
            retired: Vec::new(),
            focused: false,
            cut: Arc::default(),
            closed: Vec::new(),
            removable_drives: Vec::new(),
        }
    }

    // ── Windows ─────────────────────────────────────────────────────────

    pub const fn current_window(&self) -> WindowId {
        self.current
    }

    /// Makes `id` the current window; an id that was closed means the main one.
    pub fn set_current_window(&mut self, id: WindowId) {
        self.current = if self.window_index(id).is_some() {
            id
        } else {
            WindowId::MAIN
        };
    }

    pub fn window_ids(&self) -> Vec<WindowId> {
        self.windows.iter().map(|window| window.id).collect()
    }

    fn window_index(&self, id: WindowId) -> Option<usize> {
        self.windows.iter().position(|window| window.id == id)
    }

    pub fn window(&self) -> &Window {
        let index = self.window_index(self.current).unwrap_or(0);
        &self.windows[index]
    }

    pub fn window_mut(&mut self) -> &mut Window {
        let index = self.window_index(self.current).unwrap_or(0);
        &mut self.windows[index]
    }

    pub fn window_by_id_mut(&mut self, id: WindowId) -> Option<&mut Window> {
        let index = self.window_index(id)?;
        Some(&mut self.windows[index])
    }

    /// The window showing this tab.
    pub fn window_of(&self, tab: TabId) -> Option<WindowId> {
        self.windows
            .iter()
            .find(|window| window.dock_state.find_tab(&tab).is_some())
            .map(|window| window.id)
    }

    /// The current window's layout.
    pub(in crate::app) fn dock_mut(&mut self) -> &mut DockState<TabId> {
        &mut self.window_mut().dock_state
    }

    /// Opens a window showing these tabs (taken from wherever they were).
    pub fn new_window(&mut self, tabs: Vec<TabId>, open_at: Option<egui::Rect>) -> WindowId {
        for &tab in &tabs {
            self.detach(tab);
        }
        let id = WindowId::next();
        let mut window = Window::new(id, tabs);
        window.open_at = open_at;
        self.windows.push(window);
        self.drop_empty_windows();
        id
    }

    /// Opens a window with one tab at `path`.
    pub fn open_window_at(&mut self, path: &Path) -> WindowId {
        let tab = TabData::from_path(path);
        let id = tab.id;
        self.tabs.insert(id, tab);
        self.new_window(vec![id], None)
    }

    /// Moves a tab to the focused pane of another window.
    pub fn move_tab(&mut self, tab: TabId, to: WindowId) {
        if self.window_index(to).is_none() || !self.tabs.contains_key(&tab) {
            return;
        }
        self.detach(tab);
        if let Some(window) = self.window_by_id_mut(to) {
            window.push(tab);
        }
        self.drop_empty_windows();
    }

    /// Closes a window and its tabs. The main window can't be closed this
    /// way; see [`Self::adopt_into_main`].
    pub fn close_window(&mut self, id: WindowId) {
        if id == WindowId::MAIN {
            return;
        }
        let Some(index) = self.window_index(id) else {
            return;
        };
        let window = self.windows.remove(index);
        for tab in window.tab_ids() {
            self.retire(tab);
        }
        self.set_current_window(self.current);
    }

    /// The main window was asked to close while others are open: it takes
    /// over the first other window's tabs and that window closes instead,
    /// so the app keeps running. Returns false when no other window exists.
    pub fn adopt_into_main(&mut self) -> bool {
        if self.windows.len() < 2 {
            return false;
        }
        let other = self.windows.remove(1);
        let main = std::mem::replace(&mut self.windows[0].dock_state, other.dock_state);
        for (_, tab) in main.iter_all_tabs() {
            self.retire(*tab);
        }
        self.windows[0].sidebar_overlay = other.sidebar_overlay;
        self.windows[0].inspector_overlay = other.inspector_overlay;
        self.set_current_window(self.current);
        true
    }

    /// Takes a tab out of whichever layout shows it, keeping its data.
    fn detach(&mut self, tab: TabId) {
        for window in &mut self.windows {
            if let Some(path) = window.dock_state.find_tab(&tab) {
                window.dock_state.remove_tab(path);
                return;
            }
        }
    }

    /// Closes every window but the main one whose last tab left.
    fn drop_empty_windows(&mut self) {
        let mut index = 1;
        while index < self.windows.len() {
            if self.windows[index].tab_count() == 0 {
                self.windows.remove(index);
            } else {
                index += 1;
            }
        }
        self.set_current_window(self.current);
    }

    // ── Saving ──────────────────────────────────────────────────────────

    /// Every window's layout, with folders in place of tabs.
    pub fn save(&self) -> Vec<SavedWindow> {
        self.windows
            .iter()
            .map(|window| SavedWindow {
                layout: window
                    .dock_state
                    .filter_map_tabs(|id| self.tabs.get(id)?.current_path.single_path()),
                open_at: window
                    .outer_rect
                    .zip(window.content_rect)
                    .map(|(outer, content)| egui::Rect::from_min_size(outer.min, content.size())),
            })
            .filter(|saved| saved.layout.iter_all_tabs().next().is_some())
            .collect()
    }

    /// The windows saved by [`Self::save`], minus folders that are gone.
    /// The first window left becomes the main one. `None` when nothing is left.
    pub fn restore(saved: Vec<SavedWindow>) -> Option<Self> {
        let mut tabs = TabStore::default();
        let mut windows: Vec<Window> = Vec::new();
        for saved in saved {
            let mut layout = saved.layout.filter_map_tabs(|path| {
                let tab = path.is_dir().then(|| TabData::unloaded(path))?;
                let id = tab.id;
                tabs.insert(id, tab);
                Some(id)
            });
            if layout.iter_all_tabs().next().is_none() {
                continue;
            }
            // Filtering keeps each pane's active index, which can now be past
            // its tabs, both here and in `save`. Such a pane shows nothing,
            // and when focused leaves the window without a current tab.
            for (_, leaf) in layout.iter_leaves_mut() {
                leaf.active.0 = leaf.active.0.min(leaf.len().saturating_sub(1));
            }
            let id = if windows.is_empty() {
                WindowId::MAIN
            } else {
                WindowId::next()
            };
            let mut window = Window::new(id, Vec::new());
            window.dock_state = layout;
            window.open_at = saved.open_at.filter(|_| id != WindowId::MAIN);
            windows.push(window);
        }
        if windows.is_empty() {
            return None;
        }
        Some(Self {
            tabs,
            windows,
            ..Self::from_tabs(Vec::new())
        })
    }

    // ── Tabs ────────────────────────────────────────────────────────────

    /// Re-maps each tab's selected rows after its rows were re-sorted,
    /// filtered or reloaded, so the selection keeps pointing at the same files.
    pub(crate) fn reconcile_selection(&mut self) {
        for tab in self.tabs.values_mut() {
            tab.reconcile_selection();
        }
    }
    fn remember_selection(&mut self) {
        for tab in self.tabs.values_mut() {
            tab.remember_selection();
        }
    }

    pub fn get_current_path(&mut self) -> Option<PathBuf> {
        let active_tab = self.get_current_tab()?;
        active_tab.current_path.single_path()
    }

    /// The active tab of the current window's focused pane.
    pub fn get_current_index(&mut self) -> Option<TabId> {
        self.window_mut().active_tab()
    }

    /// The active tab of the current window's focused pane.
    pub fn get_current_tab(&mut self) -> Option<&mut TabData> {
        let id = self.get_current_index()?;
        self.tabs.get_mut(&id)
    }

    pub fn has_loading(&self) -> bool {
        self.tabs.values().any(|tab| tab.load.is_loading())
    }

    #[inline]
    pub fn get_tab_by_id(&mut self, id: TabId) -> Option<&mut TabData> {
        self.tabs.get_mut(&id)
    }

    /// Ids of all tabs in every window, in layout order.
    pub fn get_tab_ids(&self) -> Vec<TabId> {
        self.windows.iter().flat_map(Window::tab_ids).collect()
    }

    pub fn tabs(&self) -> impl Iterator<Item = &TabData> {
        self.tabs.values()
    }

    /// Puts `tab` in a new pane right of everything, as if dragged there.
    #[cfg(test)]
    pub(crate) fn split_right(&mut self, tab: TabData) -> [egui_dock::NodeIndex; 2] {
        let id = tab.id;
        self.tabs.insert(id, tab);
        self.dock_mut()
            .main_surface_mut()
            .split_right(egui_dock::NodeIndex::root(), 0.5, vec![id])
    }

    /// Takes the tabs closed since the last call, for the app to release
    /// their watchers and cancel their loads.
    pub fn take_retired(&mut self) -> Vec<TabData> {
        std::mem::take(&mut self.retired)
    }

    /// Drops a tab no layout shows any more and remembers its folder.
    fn retire(&mut self, id: TabId) {
        let Some(tab) = self.tabs.remove(&id) else {
            return;
        };
        if let Some(folder) = tab.current_path.single_path() {
            self.remember_closed(vec![folder]);
        }
        self.retired.push(tab);
    }

    /// Opens a tab at `path` in the current window and returns its id.
    pub fn open_in_new_tab(&mut self, path: &Path) -> TabId {
        let tab = TabData::from_path(path);
        let id = tab.id;
        self.tabs.insert(id, tab);
        self.window_mut().push(id);
        id
    }

    /// Draws the current window's tabs.
    pub fn ui(&mut self, ui: &mut Ui, assets: &mut AssetManager) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::Workspace::ui");
        let mut tab_paths = self.get_tabs_paths();
        tab_paths.extend(self.removable_drives.iter().cloned());
        let Some(active_tab) = self.get_current_index() else {
            return;
        };
        let index = self.window_index(self.current).unwrap_or(0);
        let window = &mut self.windows[index];
        let tabs_len = window.tab_count();
        let mut my_tab_viewer = MyTabViewer {
            tabs: &mut self.tabs,
            // The main window keeps one tab; closing another's last tab
            // closes that window.
            closeable: tabs_len > 1 || window.id != WindowId::MAIN,
            has_other_tabs: tabs_len > 1,
            tab_paths,
            active_tab,
            focused: self.focused,
            assets,
            cut: Arc::clone(&self.cut),
            tab_requests: TabRequests::default(),
            show_full_paths: false,
        };
        ui.spacing_mut().item_spacing = [0.0, 0.0].into();
        DockArea::new(&mut window.dock_state)
            .id(egui::Id::unique(("dock_area", window.id)))
            .show_leaf_close_all_buttons(false)
            .show_leaf_collapse_buttons(false)
            .show_add_popup(false)
            .show_add_buttons(true)
            .style(Self::get_dock_style(ui.style().as_ref(), tabs_len))
            .show_inside(ui, &mut my_tab_viewer);
        let requests = my_tab_viewer.tab_requests;
        self.apply_tab_requests(requests);
        let drop = ui.ctx().input(|i| {
            let origin = i.viewport().inner_rect?.min;
            Some(TearOff {
                pointer: origin + i.pointer.latest_pos()?.to_vec2(),
                size: i.content_rect().size() * 0.8,
            })
        });
        if let Some(target) = self.take_torn_off_tabs(drop) {
            ui.ctx()
                .send_viewport_cmd_to(target.viewport(), egui::ViewportCommand::Focus);
        }
        self.remember_selection();
    }

    /// `egui_dock` floats a tab dragged out of the dock in a window inside
    /// this one. Such tabs go to the OS window under the pointer, or to a
    /// new OS window there. Returns the window that received them.
    fn take_torn_off_tabs(&mut self, drop: Option<TearOff>) -> Option<WindowId> {
        let index = self.window_index(self.current).unwrap_or(0);
        let dock = &mut self.windows[index].dock_state;
        let floating: Vec<egui_dock::SurfaceIndex> = dock
            .iter_surfaces_indexed()
            .filter(|(surface, _)| !surface.is_main())
            .map(|(surface, _)| surface)
            .collect();
        let mut tabs = Vec::new();
        for surface in floating.into_iter().rev() {
            if let Some(surface) = dock.remove_surface(surface) {
                tabs.extend(surface.iter_all_tabs().map(|(_, id)| *id));
            }
        }
        if tabs.is_empty() {
            return None;
        }
        let into = drop.and_then(|drop| {
            self.windows
                .iter()
                .filter(|window| window.id != self.current)
                .find(|window| window.outer_rect.is_some_and(|r| r.contains(drop.pointer)))
                .map(|window| window.id)
        });
        let target = if let Some(window) = into {
            for tab in tabs {
                self.move_tab(tab, window);
            }
            window
        } else {
            let at = drop.map(|drop| {
                egui::Rect::from_min_size(drop.pointer - egui::vec2(40.0, 15.0), drop.size)
            });
            self.new_window(tabs, at)
        };
        self.drop_empty_windows();
        Some(target)
    }

    pub(super) fn apply_tab_requests(&mut self, requests: TabRequests) {
        for id in requests.closed {
            self.retire(id);
        }
        let index = self.window_index(self.current).unwrap_or(0);
        for (node, path) in requests.add {
            let dock = &mut self.windows[index].dock_state;
            // A new tab opens at the folder of the pane's active tab.
            let path = path.or_else(|| {
                let leaf = dock.leaf(node).ok()?;
                let id = leaf.tabs().get(leaf.active.0)?;
                self.tabs.get(id)?.current_path.single_path()
            });
            let Some(path) = path else {
                continue;
            };
            if let Ok(leaf) = dock.leaf_mut(node) {
                let tab = TabData::from_path(&path);
                leaf.append_tab(tab.id);
                self.tabs.insert(tab.id, tab);
            }
        }
        if let Some(id) = requests.to_new_window {
            self.new_window(vec![id], None);
        }
        // Closing can empty a pane and reshape the tree, so every tab is
        // looked up afresh rather than by a path recorded while drawing.
        for id in requests.close {
            self.close_tab(id);
        }
        if let Some(keep) = requests.close_others
            && let Some(window) = self.window_of(keep)
            && let Some(index) = self.window_index(window)
        {
            let others: Vec<TabId> = self.windows[index]
                .tab_ids()
                .filter(|&id| id != keep)
                .collect();
            for id in others {
                self.close_tab(id);
            }
        }
        self.drop_empty_windows();
    }

    /// Closes the tab with this id. The main window keeps its last tab;
    /// another window closes along with its last tab.
    pub fn close_tab(&mut self, id: TabId) {
        let Some(window) = self.window_of(id) else {
            return;
        };
        let Some(index) = self.window_index(window) else {
            return;
        };
        let dock = &mut self.windows[index].dock_state;
        if window == WindowId::MAIN && dock.iter_all_tabs().nth(1).is_none() {
            return;
        }
        if let Some(path) = dock.find_tab(&id) {
            dock.remove_tab(path);
        }
        self.retire(id);
        self.drop_empty_windows();
    }

    fn remember_closed(&mut self, paths: Vec<PathBuf>) {
        self.closed.extend(paths);
        let excess = self.closed.len().saturating_sub(20);
        self.closed.drain(..excess);
    }

    /// Opens the most recently closed tab again.
    pub fn reopen_closed_tab(&mut self) {
        if let Some(path) = self.closed.pop() {
            self.open_in_new_tab(&path);
        }
    }

    /// The folder of every tab, in tab order, for restoring after a restart.
    pub fn open_paths(&self) -> Vec<PathBuf> {
        self.get_tabs_paths()
    }

    /// Closes the focused tab of the current window.
    pub fn close_active_tab(&mut self) {
        if let Some(id) = self.get_current_index() {
            self.close_tab(id);
        }
    }

    /// Activates tab `n` (1-based) of the focused pane; 9 means the last tab.
    pub fn select_tab(&mut self, n: u8) {
        let dock = self.dock_mut();
        let Some(node) = dock.focused_leaf() else {
            return;
        };
        let Ok(leaf) = dock.leaf_mut(node) else {
            return;
        };
        let count = leaf.len();
        if count == 0 {
            return;
        }
        let index = if n == 9 {
            count - 1
        } else {
            usize::from(n.saturating_sub(1))
        };
        if index < count {
            let _ = leaf.set_active_tab(index);
        }
    }

    /// Activates the next (or previous) tab in the focused pane, wrapping around.
    pub fn cycle_active_tab(&mut self, forward: bool) {
        let dock = self.dock_mut();
        let Some(node) = dock.focused_leaf() else {
            return;
        };
        let Ok(leaf) = dock.leaf_mut(node) else {
            return;
        };
        let count = leaf.len();
        if count < 2 {
            return;
        }
        let active = leaf.active.0;
        let next = if forward {
            (active + 1) % count
        } else {
            (active + count - 1) % count
        };
        let _ = leaf.set_active_tab(next);
    }

    /// Folders of the tabs in every window, in layout order.
    fn get_tabs_paths(&self) -> Vec<PathBuf> {
        self.get_tab_ids()
            .iter()
            .filter_map(|id| self.tabs.get(id)?.current_path.single_path())
            .collect()
    }

    fn get_dock_style(ui: &egui::Style, tabs_amount: usize) -> Style {
        let mut style = Style::from_egui(ui);
        style.dock_area_padding = None;
        style.tab_bar.fill_tab_bar = true;
        style.tab.tab_body.inner_margin = egui::Margin::same(0);
        style.tab.tab_body.stroke = egui::Stroke::NONE;
        style.tab.tab_body.bg_fill = ui.visuals.panel_fill;
        style.tab_bar.height = if tabs_amount > 1 {
            ui.spacing.interact_size.y.max(30.0)
        } else {
            0.0
        };
        style
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::dock::CurrentPath;

    fn tab(folder: &str) -> TabData {
        let mut tab = TabData::from_path(Path::new(folder));
        tab.current_path = CurrentPath::One(folder.into());
        tab
    }

    /// eframe's storage, in memory.
    #[derive(Default)]
    struct Memory(std::collections::HashMap<String, String>);
    impl eframe::Storage for Memory {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.to_owned(), value);
        }
        fn remove_string(&mut self, key: &str) {
            self.0.remove(key);
        }
        fn flush(&mut self) {}
    }
    /// Tests that touch the shared command queue run one at a time.
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        egui_mev_test::snapshot_lock()
    }

    fn drain() {
        while crate::app::commands::COMMANDS_QUEUE.pop().is_some() {}
    }

    #[test]
    fn moving_a_window_s_last_tab_closes_that_window() {
        let _guard = lock();
        let (a, b) = (tab("src"), tab("src/app"));
        let (a_id, b_id) = (a.id, b.id);
        let mut workspace = Workspace::from_tabs(vec![a, b]);
        let second = workspace.new_window(vec![b_id], None);
        drain();
        assert_eq!(workspace.window_ids(), vec![WindowId::MAIN, second]);
        assert_eq!(workspace.window_of(b_id), Some(second));

        workspace.move_tab(b_id, WindowId::MAIN);
        assert_eq!(workspace.window_ids(), vec![WindowId::MAIN]);
        assert_eq!(workspace.get_tab_ids(), vec![a_id, b_id]);
        assert!(
            workspace.get_tab_by_id(b_id).is_some(),
            "the data moved too"
        );
    }

    #[test]
    fn closing_a_window_retires_its_tabs_only() {
        let _guard = lock();
        let (a, b, c) = (tab("src"), tab("src/app"), tab("src/data"));
        let ids = [a.id, b.id, c.id];
        let mut workspace = Workspace::from_tabs(vec![a, b, c]);
        let second = workspace.new_window(vec![ids[1], ids[2]], None);
        drain();
        workspace.set_current_window(second);
        assert_eq!(workspace.get_current_index(), Some(ids[1]));

        workspace.close_window(second);
        assert_eq!(workspace.current_window(), WindowId::MAIN);
        assert_eq!(workspace.get_tab_ids(), vec![ids[0]]);
        assert_eq!(workspace.take_retired().len(), 2);
        assert_eq!(workspace.closed.len(), 2);
    }

    #[test]
    fn closing_another_window_s_last_tab_closes_the_window() {
        let _guard = lock();
        let (a, b) = (tab("src"), tab("src/app"));
        let (a_id, b_id) = (a.id, b.id);
        let mut workspace = Workspace::from_tabs(vec![a, b]);
        let second = workspace.new_window(vec![b_id], None);
        drain();
        workspace.close_tab(b_id);
        assert_eq!(workspace.window_ids(), vec![WindowId::MAIN]);
        // The main window keeps its last tab.
        workspace.close_tab(a_id);
        assert_eq!(workspace.get_tab_ids(), vec![a_id]);
        assert!(workspace.window_by_id_mut(second).is_none());
    }

    #[test]
    fn main_window_adopts_another_window_when_closed() {
        let _guard = lock();
        let (a, b) = (tab("src"), tab("src/app"));
        let (a_id, b_id) = (a.id, b.id);
        let mut workspace = Workspace::from_tabs(vec![a, b]);
        workspace.new_window(vec![b_id], None);
        drain();
        assert!(workspace.adopt_into_main());
        assert_eq!(workspace.window_ids(), vec![WindowId::MAIN]);
        assert_eq!(workspace.get_tab_ids(), vec![b_id]);
        assert!(workspace.get_tab_by_id(a_id).is_none());
        assert!(!workspace.adopt_into_main());
    }

    #[test]
    fn windows_and_splits_survive_save_and_restore() {
        let _guard = lock();
        let (a, b, c) = (tab("src"), tab("src/app"), tab("src/data"));
        let c_id = c.id;
        let mut workspace = Workspace::from_tabs(vec![a, b]);
        workspace.split_right(c);
        let gone = tab("src/no such folder");
        let gone_id = gone.id;
        workspace.tabs.insert(gone_id, gone);
        workspace.window_mut().push(gone_id);
        let second = workspace.new_window(vec![c_id], None);
        workspace
            .window_by_id_mut(second)
            .expect("window")
            .outer_rect = Some(egui::Rect::from_min_size(
            egui::pos2(50.0, 60.0),
            egui::vec2(800.0, 600.0),
        ));
        workspace
            .window_by_id_mut(second)
            .expect("window")
            .content_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(780.0, 560.0),
        ));
        drain();

        // Through eframe's own storage format, as on shutdown and startup.
        let mut storage = Memory::default();
        eframe::set_value(&mut storage, "windows", &workspace.save());
        let saved: Vec<SavedWindow> = eframe::get_value(&storage, "windows").expect("load");
        let mut restored = Workspace::restore(saved).expect("restored");
        drain();
        assert_eq!(restored.window_ids().len(), 2);
        assert_eq!(
            restored.get_tab_ids().len(),
            3,
            "the missing folder is dropped"
        );
        let main_panes = restored.windows[0].dock_state.iter_leaves().count();
        assert_eq!(main_panes, 1, "the split emptied when its tab moved out");
        let other = restored.window_ids()[1];
        assert_eq!(
            restored.window_by_id_mut(other).expect("window").open_at,
            Some(egui::Rect::from_min_size(
                egui::pos2(50.0, 60.0),
                egui::vec2(780.0, 560.0)
            ))
        );
        assert!(Workspace::restore(Vec::new()).is_none());
    }

    /// Saving leaves out tabs without a single folder, and restoring those
    /// whose folder is gone. The pane's active tab must still exist: without
    /// it there is no current tab, and the window stays blank on every start.
    #[test]
    fn a_restored_pane_keeps_an_active_tab_when_tabs_are_dropped() {
        let _guard = lock();
        let (a, mut trash, c) = (tab("src"), tab("src/app"), tab("src/data"));
        trash.current_path = CurrentPath::Trash;
        let c_id = c.id;
        let mut workspace = Workspace::from_tabs(vec![a, trash, c]);
        workspace.get_current_index();
        workspace.select_tab(3);
        assert_eq!(workspace.get_current_index(), Some(c_id));
        drain();

        let mut restored = Workspace::restore(workspace.save()).expect("restored");
        drain();
        assert_eq!(restored.get_tab_ids().len(), 2);
        let current = restored.get_current_tab().expect("a current tab");
        assert_eq!(current.current_path, CurrentPath::One("src/data".into()));
    }

    #[test]
    fn a_closed_window_id_falls_back_to_the_main_window() {
        let _guard = lock();
        let mut workspace = Workspace::from_tabs(vec![tab("src")]);
        let second = workspace.open_window_at(Path::new("src/app"));
        drain();
        workspace.close_window(second);
        workspace.set_current_window(second);
        assert_eq!(workspace.current_window(), WindowId::MAIN);
    }
}
