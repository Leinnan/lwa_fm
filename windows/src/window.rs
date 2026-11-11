//! One native window for one of the core's windows: the title bar, and the
//! panes laid out as the core arranges them.
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use dirfleet_core::app::{
    Appearance, CommandId, Event, FilterGroup, FlatLayout, NodeRef, PaletteTarget, Side, TabTarget,
    ViewModel, WindowFrame, WindowView,
};
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, Color, Component, ComponentContext, ComponentTimer,
    ContentDialogExt as _, ContentDialogResult, Grid, GridLength, InputModifiers, Key,
    KeyEventInfo, Menu, MenuExt as _, MenuItem, PointerEventInfo, RoutedCallback, StackPanel,
    TabView, TabViewItem, TextBlock, Thickness, TitleBar, VerticalAlignment, View, ViewContext,
    WindowBackdrop, WindowConstraints, WindowPlacement, WindowSize, WindowTheme, WindowVisuals,
    component, keyed,
};

use crate::commands::{self, Local, Target};
use crate::core_link::CoreLink;
use crate::dialogs::{Asking, Dialog, Here, Hooks};
use crate::files::{ColumnSet, Dropped, glyph};
use crate::hwnd::{self, ActivationWatch};
use crate::inspector;
use crate::menus::{self, Action, MenuOn, SidebarMenu};
use crate::notices;
use crate::pane::{self, Pane, PaneInput};
use crate::plugins;
use crate::popup;
use crate::saved::{self, Draft, SearchDraft};
use crate::shortcuts::{Chord, Shortcuts};
use crate::sidebar;
use crate::store::PaneParts;
use crate::theme;
use crate::ui_thread;

/// The client size of a window the core has no frame for.
const DEFAULT_SIZE: (f64, f64) = (1100.0, 720.0);
/// Smaller than this the panes no longer fit.
const MIN_SIZE: (f64, f64) = (640.0, 420.0);
/// The bar between two panes, dragged to resize them.
const SPLITTER: f64 = 6.0;
/// The title bar's height, taken off the window's to size the panes.
const TITLE_BAR: f64 = 48.0;
/// The space left of the title bar's own content.
const TITLE_INSET: f64 = 16.0;
/// The least room the app's name takes in the title bar.
const NAME_WIDTH: f64 = 80.0;
/// What the More button and the window's own buttons take on the right.
const TITLE_BUTTONS: f64 = 220.0;
/// The height of a pane's strip of tabs.
const TAB_STRIP: f64 = 40.0;
/// A pane keeps at least this share of a split, in thousandths.
const MIN_SHARE: f64 = 100.0;
/// How long a menu waits for the core to answer the click that asked for it.
const MENU_DELAY: Duration = Duration::from_millis(30);

#[derive(Clone, PartialEq)]
pub struct WindowInput {
    pub core: CoreLink,
    /// The window without its panes.
    pub chrome: Rc<WindowView>,
    pub panes: Vec<PaneParts>,
    pub columns: Rc<ColumnSet>,
    /// What belongs to no window: settings, commands, toasts, dialogs.
    pub shared: Rc<ViewModel>,
    pub shortcuts: Rc<Shortcuts>,
    /// The preset filters the search menu offers.
    pub filters: Rc<Vec<FilterGroup>>,
    /// The user's home folder.
    pub home: Rc<str>,
    /// Told the window's number when its native window goes away.
    pub on_closed: Callback<u32>,
    /// This window asks the questions that belong to the whole app.
    pub leads: bool,
}

pub struct WindowRoot {
    id: u32,
    core: CoreLink,
    on_closed: Callback<u32>,
    // Observers and input callbacks are made once: a fresh one each render
    // would subscribe again.
    on_size: Callback<WindowSize>,
    on_placement: Callback<WindowPlacement>,
    on_key: RoutedCallback<KeyEventInfo>,
    on_pointer: Callback<PointerEventInfo>,
    on_menu: Callback<Key>,
    on_command: Callback<CommandId>,
    on_split_pressed: Callback<(Drag, PointerEventInfo)>,
    on_split_moved: Callback<PointerEventInfo>,
    on_split_released: Callback<PointerEventInfo>,
    on_dialog_closed: Callback<ContentDialogResult>,
    on_menu_asked: Callback<(u32, MenuOn)>,
    on_dropped: Callback<Dropped>,
    on_palette: Callback<PaletteTarget>,
    on_sidebar_menu: Callback<SidebarMenu>,
    /// The saved search being named or edited in a dialog.
    draft: Draft,
    /// Where the window opens: read once, when it is first declared.
    opens_at: Option<WindowPlacement>,
    /// The frame last told to the core.
    framed: Option<WindowFrame>,
    size: (f64, f64),
    /// Kept so the native window stays watched while this lives.
    activation: Option<ActivationWatch>,
    /// What commands act on, as of the last input.
    target: Target,
    /// The split being resized.
    drag: Option<Drag>,
    sidebar_shown: bool,
    /// The dialog the core's state calls for, as of the last input.
    dialog: Dialog,
    /// What the window itself is asking before it acts.
    asking: Option<Asking>,
    /// The last input, for what is built when a message arrives.
    input: WindowInput,
    /// The native window, once known; 0 before.
    handle: isize,
    /// Dropping it cancels the menu about to open.
    menu_timer: Option<ComponentTimer>,
    /// Dropped files waiting for the answer to "move or copy?".
    dropped: Option<Dropped>,
    /// The palette was asked for in this window, so it shows here.
    palette_here: bool,
    /// Counted up each time a dialog or menu gives the keyboard back.
    keys_back: u32,
    /// Counted up each time Find is asked for.
    find: u32,
    /// A plugin's action waiting for the answer to its question: the tab,
    /// the action and the question.
    plugin_asked: Option<(u32, String, String)>,
}

/// A splitter held down: which split, and where it started.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drag {
    split: u32,
    vertical: bool,
    /// The first side's share when the drag began, in thousandths.
    from_share: f64,
    /// The length the two sides share, along the split.
    extent: f64,
    /// Where the pointer went down, along the split.
    from: f64,
}

impl Drag {
    /// The first side's share with the pointer at `info`, in thousandths.
    fn share_at(&self, info: &PointerEventInfo) -> u16 {
        let at = if self.vertical {
            info.window_y
        } else {
            info.window_x
        };
        let moved = (at - self.from) / self.extent.max(1.0) * 1000.0;
        (self.from_share + moved).clamp(MIN_SHARE, 1000.0 - MIN_SHARE) as u16
    }
}

#[derive(Clone, Debug)]
pub enum WindowMessage {
    Resized(WindowSize),
    Placed(WindowPlacement),
    /// The native window's handle, once there is one.
    Handle(isize),
    /// The native window became the active one.
    Activated,
    Run(CommandId),
    Split(Side),
    SplitPressed(Drag, PointerEventInfo),
    SplitMoved(PointerEventInfo),
    SplitReleased,
    DialogClosed(ContentDialogResult),
    /// A right click asked for a tab's menu.
    MenuAsked(u32, MenuOn),
    /// The core has answered the click: time to show the menu.
    OpenMenu(u32, MenuOn),
    MenuChose(u32, Action),
    Dropped(Dropped),
    PaletteChose(PaletteTarget),
    // The title bar's tabs, which are the window's only pane's.
    TabSelected(Option<usize>),
    TabClosed(Key),
    TabAdded,
    TabsReordered(Vec<Key>),
    /// A plugin's action, from its shortcut.
    RunPlugin(String),
    /// A right click on a place in the sidebar.
    SidebarMenu(SidebarMenu),
}

impl Drop for WindowRoot {
    fn drop(&mut self) {
        self.on_closed.call(self.id);
    }
}

impl WindowRoot {
    fn target(input: &WindowInput) -> Target {
        let tab = &input.chrome.active;
        Target {
            window: input.chrome.id,
            tab: tab.id,
            path: tab.path.clone(),
            show_hidden: tab.view.show_hidden,
            in_trash: tab.in_trash,
            selected: tab.selected_count,
            inspector_shown: input.shared.inspector_shown,
            confirm_trash: input.shared.settings.confirm_trash,
            home: input.home.to_string(),
        }
    }

    /// The tabs of the window's only pane, in order; none when it has several panes.
    fn lone_tabs(&self) -> Vec<u32> {
        match self.input.panes.as_slice() {
            [pane] => pane.tabs.iter().map(|tab| tab.id).collect(),
            _ => Vec::new(),
        }
    }

    /// The dialog the core's state calls for in this window.
    fn dialog(input: &WindowInput, palette: bool) -> Dialog {
        let tabs: Vec<u32> = input.chrome.tabs.iter().map(|tab| tab.id).collect();
        Dialog::of(
            &input.shared,
            &input.chrome.active,
            &Here {
                leads: input.leads,
                palette,
                tabs: &tabs,
            },
            None,
        )
    }

    /// The key handler: runs the command a chord is bound to. Chords that
    /// type or edit text are left to whatever has the keyboard.
    fn key_handler(
        shortcuts: &Rc<Shortcuts>,
        context: &ComponentContext<Self>,
    ) -> RoutedCallback<KeyEventInfo> {
        let shortcuts = Rc::clone(shortcuts);
        let sender = context.sender();
        RoutedCallback::new(move |info: KeyEventInfo| {
            let chord = Chord {
                key: info.key.0,
                ctrl: info.modifiers.contains(InputModifiers::CONTROL),
                shift: info.modifiers.contains(InputModifiers::SHIFT),
                alt: info.modifiers.contains(InputModifiers::ALT),
            };
            if chord.edits_text() {
                return false;
            }
            if let Some(command) = shortcuts.command(chord) {
                return sender.send(WindowMessage::Run(command));
            }
            shortcuts
                .plugin(chord)
                .is_some_and(|action| sender.send(WindowMessage::RunPlugin(action.to_owned())))
        })
    }
}

impl Component for WindowRoot {
    type Input = WindowInput;
    type Message = WindowMessage;

    fn create(input: &WindowInput, context: &ComponentContext<Self>) -> Self {
        if !context.run_window(|handle| WindowMessage::Handle(handle.as_raw() as isize)) {
            crate::trace::note(|| "the window has no handle to give".to_owned());
        }
        let sender = context.sender();
        let menu_sender = context.sender();
        let split_sender = context.sender();
        Self {
            id: input.chrome.id,
            core: input.core.clone(),
            on_closed: input.on_closed.clone(),
            on_size: context.callback(WindowMessage::Resized),
            on_placement: context.callback(WindowMessage::Placed),
            on_key: Self::key_handler(&input.shortcuts, context),
            // The extra mouse buttons go back and forward, wherever they are pressed.
            on_pointer: Callback::new(move |info: PointerEventInfo| {
                let command = if info.is_x_button1_pressed {
                    CommandId::Back
                } else if info.is_x_button2_pressed {
                    CommandId::Forward
                } else {
                    return;
                };
                _ = sender.send(WindowMessage::Run(command));
            }),
            on_menu: Callback::new(move |key: Key| {
                let message = if key == Key::from(SPLIT_RIGHT) {
                    WindowMessage::Split(Side::Right)
                } else if key == Key::from(SPLIT_BELOW) {
                    WindowMessage::Split(Side::Below)
                } else if let Some(command) = CommandId::ALL
                    .into_iter()
                    .find(|command| command_key(*command) == key)
                {
                    WindowMessage::Run(command)
                } else {
                    return;
                };
                _ = menu_sender.send(message);
            }),
            on_command: context.callback(WindowMessage::Run),
            on_split_pressed: Callback::new(move |(drag, info): (Drag, PointerEventInfo)| {
                _ = split_sender.send(WindowMessage::SplitPressed(drag, info));
            }),
            on_split_moved: context.callback(WindowMessage::SplitMoved),
            on_split_released: context.callback(|_: PointerEventInfo| WindowMessage::SplitReleased),
            on_dialog_closed: context.callback(WindowMessage::DialogClosed),
            on_dropped: context.callback(WindowMessage::Dropped),
            on_palette: context.callback(WindowMessage::PaletteChose),
            on_sidebar_menu: context.callback(WindowMessage::SidebarMenu),
            draft: Draft::default(),
            on_menu_asked: context
                .callback(|(tab, on): (u32, MenuOn)| WindowMessage::MenuAsked(tab, on)),
            opens_at: input.chrome.frame.and_then(|frame| {
                Some(WindowPlacement {
                    x: frame.x,
                    y: frame.y,
                    width: i32::try_from(frame.width).ok().filter(|width| *width > 0)?,
                    height: i32::try_from(frame.height)
                        .ok()
                        .filter(|height| *height > 0)?,
                    maximized: false,
                })
            }),
            framed: input.chrome.frame,
            size: DEFAULT_SIZE,
            activation: None,
            target: Self::target(input),
            drag: None,
            sidebar_shown: true,
            dialog: Self::dialog(input, false),
            asking: None,
            input: input.clone(),
            handle: 0,
            menu_timer: None,
            dropped: None,
            palette_here: false,
            keys_back: 0,
            find: 0,
            plugin_asked: None,
        }
    }

    fn input_changed(&mut self, input: &WindowInput, context: &ComponentContext<Self>) {
        self.target = Self::target(input);
        self.input = input.clone();
        if input.shared.palette.is_none() {
            self.palette_here = false;
        }
        self.dialog = Self::dialog(input, self.palette_here);
        // The handler holds the bindings it was made with.
        self.on_key = Self::key_handler(&input.shortcuts, context);
    }

    #[expect(clippy::too_many_lines, reason = "one arm per message")]
    fn update(&mut self, message: WindowMessage, context: &ComponentContext<Self>) {
        let window = self.id;
        match message {
            WindowMessage::Resized(size) => self.size = (size.width, size.height),
            WindowMessage::Placed(placement) => {
                let frame = WindowFrame {
                    x: placement.x,
                    y: placement.y,
                    width: placement.width.unsigned_abs(),
                    height: placement.height.unsigned_abs(),
                };
                if self.framed != Some(frame) {
                    self.framed = Some(frame);
                    self.core.send(Event::WindowFramed { window, frame });
                }
            }
            WindowMessage::Handle(handle) => {
                self.handle = handle;
                let sender = context.sender();
                self.activation = Some(hwnd::watch_activation(handle, move || {
                    _ = sender.send(WindowMessage::Activated);
                }));
            }
            WindowMessage::Activated => self.core.send(Event::WindowFocused { window }),
            WindowMessage::Run(command) => {
                if command == CommandId::Palette {
                    self.palette_here = true;
                }
                match commands::run(&self.core, command, &self.target) {
                    None => {}
                    Some(Local::ToggleSidebar) => self.sidebar_shown = !self.sidebar_shown,
                    Some(Local::MoveToTrash) => {
                        let target = &self.target;
                        if target.selected == 0 || target.in_trash {
                            // Nothing to move, or it is in the Recycle Bin already.
                        } else if target.confirm_trash {
                            self.asking = Some(Asking::Trash);
                        } else {
                            self.core.send(Event::TrashSelection { tab: target.tab });
                        }
                    }
                    Some(Local::DeleteImmediately) => {
                        if self.target.selected > 0 {
                            self.asking = Some(Asking::Delete);
                        }
                    }
                    Some(Local::EmptyTrash) => self.asking = Some(Asking::EmptyTrash),
                    Some(Local::ShortcutHelp) => self.asking = Some(Asking::Shortcuts),
                    Some(Local::Find) => self.find = self.find.wrapping_add(1),
                    Some(Local::QuickLook) => self.core.send(Event::QuickLook {
                        tab: self.target.tab,
                    }),
                    Some(Local::Settings) => self.asking = Some(Asking::Settings),
                    Some(Local::SaveSearch) => {
                        // Nothing to save while nothing is searched for.
                        let search = self.input.chrome.active.search.as_ref();
                        if let Some(draft) = search
                            .filter(|_| saved::can_save(&self.input.chrome.active))
                            .and_then(SearchDraft::naming)
                        {
                            self.draft.replace(draft);
                            self.asking = Some(Asking::SaveSearch);
                        }
                    }
                }
            }
            WindowMessage::DialogClosed(result) => {
                self.keys_back = self.keys_back.wrapping_add(1);
                if self.asking == Some(Asking::Plugin)
                    && let Some((tab, action, _)) = self.plugin_asked.take()
                    && result == ContentDialogResult::Primary
                {
                    self.core.send(Event::RunPluginAction { tab, action });
                }
                if matches!(self.asking, Some(Asking::SaveSearch | Asking::EditSearch))
                    && result == ContentDialogResult::Primary
                    && let Some(event) = self.draft.borrow().confirmed(self.target.tab)
                {
                    self.core.send(event);
                }
                if let Some(Asking::Drop(_)) = self.asking
                    && let Some(dropped) = self.dropped.take()
                {
                    match result {
                        ContentDialogResult::Primary => self.drop_files(dropped, false, true),
                        ContentDialogResult::Secondary => self.drop_files(dropped, true, false),
                        // Closed without an answer: nothing is moved or copied.
                        _ => {}
                    }
                }
                let target = &self.target;
                self.dialog.with(self.asking.take()).closed(
                    &self.core,
                    target.tab,
                    target.in_trash,
                    result,
                );
            }
            WindowMessage::MenuAsked(tab, on) => {
                // The click also selected: the menu waits for the core's answer.
                self.menu_timer = Some(
                    context.set_local_timeout(MENU_DELAY, move || WindowMessage::OpenMenu(tab, on)),
                );
            }
            WindowMessage::OpenMenu(tab, on) => {
                self.menu_timer = None;
                self.open_menu(tab, on, context);
            }
            WindowMessage::MenuChose(tab, action) => {
                self.keys_back = self.keys_back.wrapping_add(1);
                self.chose(tab, action, context);
            }
            WindowMessage::PaletteChose(target) => {
                self.keys_back = self.keys_back.wrapping_add(1);
                self.core.send(Event::ClosePalette);
                let tab = self.target.tab;
                match target {
                    PaletteTarget::Command(command) => {
                        self.update(WindowMessage::Run(command), context);
                    }
                    PaletteTarget::Folder(path) => self.core.send(Event::Navigate { tab, path }),
                    PaletteTarget::SavedSearch(id) => {
                        self.core.send(Event::RunSavedSearch { tab, id });
                    }
                    PaletteTarget::PluginAction(action) => {
                        self.core.send(Event::RunPluginAction { tab, action });
                    }
                }
            }
            WindowMessage::Dropped(dropped) => {
                // Explorer's keys decide: Ctrl copies, Shift moves.
                let (copy, force_move) = hwnd::ctrl_and_shift();
                let decided = copy || force_move || dropped.on_trash;
                if self.input.shared.settings.confirm_drop && !decided {
                    self.asking = Some(Asking::Drop(dropped.paths.len() as u32));
                    self.dropped = Some(dropped);
                } else {
                    self.drop_files(dropped, copy, force_move);
                }
            }
            WindowMessage::TabSelected(index) => {
                let chosen = index.and_then(|index| self.lone_tabs().get(index).copied());
                if let Some(tab) = chosen.filter(|tab| *tab != self.target.tab) {
                    self.core.send(Event::ActivateTab { tab });
                }
            }
            WindowMessage::TabClosed(key) => {
                let closed = self
                    .lone_tabs()
                    .into_iter()
                    .find(|tab| Key::from(*tab) == key);
                if let Some(tab) = closed {
                    self.core.send(Event::CloseTab { tab });
                }
            }
            WindowMessage::RunPlugin(action) => {
                // An action that asks first does so from its shortcut too.
                let tab = self.target.tab;
                let confirm = self
                    .input
                    .panes
                    .iter()
                    .find(|pane| pane.tab.id == tab)
                    .and_then(|pane| {
                        pane.tab
                            .plugin_actions
                            .iter()
                            .chain(&pane.tab.folder_plugin_actions)
                            .find(|known| known.id == action)
                    })
                    .map_or_else(String::new, |known| known.confirm.clone());
                self.chose(tab, Action::Plugin { action, confirm }, context);
            }
            WindowMessage::TabAdded => self.update(WindowMessage::Run(CommandId::NewTab), context),
            WindowMessage::SidebarMenu(on) => self.open_sidebar_menu(&on, context),
            WindowMessage::TabsReordered(keys) => {
                let before = self.lone_tabs();
                let after: Vec<u32> = keys
                    .iter()
                    .filter_map(|key| before.iter().copied().find(|tab| Key::from(*tab) == *key))
                    .collect();
                if let Some((tab, index, beside)) = pane::moved(&before, &after) {
                    self.core.send(Event::MoveTab {
                        tab,
                        to: TabTarget::Beside {
                            tab: beside,
                            index: index as u32,
                        },
                    });
                }
            }
            WindowMessage::Split(side) => self.core.send(Event::SplitNew {
                tab: self.target.tab,
                side,
            }),
            WindowMessage::SplitPressed(drag, info) => {
                self.drag = Some(Drag {
                    from: if drag.vertical {
                        info.window_y
                    } else {
                        info.window_x
                    },
                    ..drag
                });
            }
            WindowMessage::SplitMoved(info) => {
                if let Some(drag) = self.drag.filter(|_| info.is_captured) {
                    self.core.send(Event::ResizeSplit {
                        window,
                        split: drag.split,
                        permille: drag.share_at(&info),
                    });
                }
            }
            WindowMessage::SplitReleased => self.drag = None,
        }
    }

    #[expect(clippy::too_many_lines, reason = "the window, top to bottom")]
    fn view(&self, input: &WindowInput, context: &mut ViewContext<Self>) -> View {
        let tab = &input.chrome.active;
        context.window_title(format!("{} - DirFleet", tab.title));
        // The menus at the pointer are the system's, and are told separately.
        popup::follow(input.shared.settings.appearance);
        let visuals = WindowVisuals::new()
            .backdrop(WindowBackdrop::Mica)
            .theme(match input.shared.settings.appearance {
                Appearance::System => WindowTheme::System,
                Appearance::Light => WindowTheme::Light,
                Appearance::Dark => WindowTheme::Dark,
            })
            .constraints(WindowConstraints {
                min_width: Some(MIN_SIZE.0),
                min_height: Some(MIN_SIZE.1),
                ..WindowConstraints::default()
            });
        context.window_visuals(match self.opens_at {
            Some(placement) => visuals.initial_placement(placement),
            None => visuals.client_size(DEFAULT_SIZE.0, DEFAULT_SIZE.1),
        });
        context.on_window_size(self.on_size.clone());
        context.on_window_placement(self.on_placement.clone());

        // A window with one pane has its tabs in the title bar, as Explorer
        // does; with several, each pane shows its own and the bar names the folder.
        let lone = match input.panes.as_slice() {
            [pane] => Some(pane),
            _ => None,
        };
        let tabs_above = if lone.is_some() { 0.0 } else { TAB_STRIP };
        let more = Button::new()
            .style(ButtonStyle::Subtle)
            .automation_name("More")
            .content(glyph("\u{E712}", 14.0))
            .menu(more_menu(&input.shared, self.on_menu.clone()));
        let side = if self.sidebar_shown {
            sidebar::WIDTH
        } else {
            0.0
        };
        let title_bar = TitleBar::new().right_header(more);
        let title_bar = match lone {
            Some(pane) => {
                let shown = pane.tab.id;
                let items = pane
                    .tabs
                    .iter()
                    .map(|tab| keyed(tab.id, TabViewItem::new().header(tab.title.clone())));
                // The app's name stands over the sidebar, so the tabs begin
                // over the folder they belong to and run to the window's buttons.
                let name_width = (side - TITLE_INSET).max(NAME_WIDTH);
                let tabs_width =
                    (self.size.0 - name_width - TITLE_INSET - TITLE_BUTTONS).max(240.0);
                title_bar
                    .left_header(
                        Border::new()
                            .width(name_width)
                            .padding(Thickness::new(TITLE_INSET, 0.0, 0.0, 0.0))
                            .content(
                                TextBlock::new()
                                    .text("DirFleet")
                                    .font_size(12.0)
                                    .vertical_alignment(VerticalAlignment::Center),
                            ),
                    )
                    .content(
                        // The strip stands on the bar's lower edge, so the
                        // chosen tab meets the folder it names.
                        TabView::new()
                            .width(tabs_width)
                            .height(TAB_STRIP)
                            .margin(Thickness::new(0.0, TITLE_BAR - TAB_STRIP, 0.0, 0.0))
                            .vertical_alignment(VerticalAlignment::Bottom)
                            .can_reorder_tabs(true)
                            .is_add_tab_button_visible(true)
                            .selected_index(pane.tabs.iter().position(|tab| tab.id == shown))
                            .keyed_tab_items(items)
                            .on_selection_changed(context.callback(WindowMessage::TabSelected))
                            .on_close_requested(context.callback(WindowMessage::TabClosed))
                            .on_add_tab_button_click(context.message(WindowMessage::TabAdded))
                            .on_reordered(context.callback(WindowMessage::TabsReordered)),
                    )
            }
            None => title_bar.title("DirFleet").subtitle(tab.title.clone()),
        };
        let inspected = if input.shared.inspector_shown {
            inspector::WIDTH
        } else {
            0.0
        };
        // What belongs to the whole app shows in one window.
        let operations: &[_] = if input.leads {
            &input.shared.operations
        } else {
            &[]
        };
        let toasts: &[_] = if input.leads {
            &input.shared.toasts
        } else {
            &[]
        };
        let jobs: &[_] = if input.leads {
            &input.shared.plugin_jobs
        } else {
            &[]
        };
        let busy = (operations.len() + jobs.len()) as f64 * notices::OPERATION_HEIGHT;
        let area = (
            (self.size.0 - side - inspected).max(1.0),
            (self.size.1 - TITLE_BAR - busy).max(1.0),
        );
        let panes = self.node(input, &input.chrome.layout, input.chrome.layout.root, area);
        let details: View = if input.shared.inspector_shown {
            // The focused tab's selection, as its pane has it.
            let inspected_tab = input
                .panes
                .iter()
                .find(|pane| pane.tab.id == tab.id)
                .map_or(tab, |pane| &*pane.tab);
            inspector::inspector(&input.core, inspected_tab, &self.on_command)
        } else {
            Border::new().into()
        };
        let places: View = if self.sidebar_shown {
            sidebar::sidebar(
                &input.core,
                tab.id,
                &input.chrome.sidebar,
                sidebar::Hooks {
                    on_menu: &self.on_sidebar_menu,
                    on_drop: &self.on_dropped,
                },
            )
        } else {
            Border::new().into()
        };
        Border::new()
            .on_preview_key_down(self.on_key.clone())
            .on_pointer_pressed(self.on_pointer.clone())
            .content(
                Grid::new()
                    .rows([GridLength::Auto, GridLength::Auto, GridLength::STAR])
                    .children((
                        title_bar,
                        Border::new()
                            .grid_row(1)
                            .content(StackPanel::new().children((
                                notices::operations(&input.core, operations),
                                plugins::jobs(&input.core, jobs),
                            ))),
                        Grid::new()
                            .grid_row(2)
                            .columns([
                                GridLength::Pixel(side),
                                GridLength::STAR,
                                GridLength::Pixel(inspected),
                            ])
                            .children((
                                Border::new().content(places),
                                Border::new()
                                    .grid_column(1)
                                    .margin(Thickness::new(0.0, 0.0, theme::GAP, theme::GAP))
                                    .content(panes),
                                // A surface like a folder's, level with them
                                // under the strip of tabs.
                                Border::new()
                                    .grid_column(2)
                                    .margin(Thickness::new(0.0, tabs_above, theme::GAP, theme::GAP))
                                    .background(theme::surface())
                                    .border_brush(theme::stroke())
                                    .border_thickness(Thickness::uniform(
                                        if input.shared.inspector_shown {
                                            1.0
                                        } else {
                                            0.0
                                        },
                                    ))
                                    .corner_radius(theme::RADIUS)
                                    .content(details),
                            )),
                        // Last, so it lies over the panes.
                        Border::new()
                            .grid_row(2)
                            .content(notices::toasts(&input.core, toasts)),
                    )),
            )
            .content_dialog(
                self.dialog.with(self.asking).build(
                    &input.core,
                    &input.shared,
                    tab,
                    &Hooks {
                        on_closed: &self.on_dialog_closed,
                        on_palette: &self.on_palette,
                        owner: self.handle,
                        confirm: self
                            .plugin_asked
                            .as_ref()
                            .map_or("", |(_, _, confirm)| confirm),
                        draft: &self.draft,
                    },
                ),
            )
    }
}

impl WindowRoot {
    /// A pane, or a split of two sides, given `area` (width, height) to fill.
    fn node(
        &self,
        input: &WindowInput,
        layout: &FlatLayout,
        node: NodeRef,
        area: (f64, f64),
    ) -> View {
        match node {
            NodeRef::Pane(index) => {
                let Some(pane) = input.panes.get(index as usize) else {
                    return Border::new().into();
                };
                // Keyed by its place: the pane stays itself when its tab changes.
                component::<Pane>(
                    index,
                    PaneInput {
                        core: input.core.clone(),
                        window: input.chrome.id,
                        pane: pane.clone(),
                        columns: Rc::clone(&input.columns),
                        icon_size: input.shared.settings.icon_size,
                        shortcuts: Rc::clone(&input.shortcuts),
                        on_command: self.on_command.clone(),
                        focused: pane.tab.id == input.chrome.active.id,
                        keys_back: self.keys_back,
                        filters: Rc::clone(&input.filters),
                        find: self.find,
                        on_menu: self.on_menu_asked.clone(),
                        on_drop: self.on_dropped.clone(),
                        marked: input.panes.len() > 1,
                        width: area.0,
                    },
                )
                .into()
            }
            NodeRef::Split(index) => {
                let Some(split) = layout.splits.get(index as usize) else {
                    return Border::new().into();
                };
                let share = f64::from(split.permille.clamp(1, 999));
                let lengths = [
                    GridLength::Star(share),
                    GridLength::Pixel(SPLITTER),
                    GridLength::Star(1000.0 - share),
                ];
                // A vertical split stacks its sides; the other puts them side by side.
                let extent = if split.vertical { area.1 } else { area.0 } - SPLITTER;
                let side = |share: f64| {
                    let length = (extent * share / 1000.0).max(1.0);
                    if split.vertical {
                        (area.0, length)
                    } else {
                        (length, area.1)
                    }
                };
                let first = self.node(input, layout, split.first, side(share));
                let second = self.node(input, layout, split.second, side(1000.0 - share));
                let drag = Drag {
                    split: index,
                    vertical: split.vertical,
                    from_share: share,
                    extent,
                    from: 0.0,
                };
                let pressed = self.on_split_pressed.clone();
                // The gap between two surfaces is the handle that resizes them.
                let bar = Border::new()
                    .background(Color::transparent())
                    .capture_pointer_on_press(true)
                    .on_pointer_pressed(move |info: PointerEventInfo| pressed.call((drag, info)))
                    .on_pointer_moved(self.on_split_moved.clone())
                    .on_pointer_released(self.on_split_released.clone());
                let grid = Grid::new();
                if split.vertical {
                    grid.rows(lengths).children((
                        Border::new().content(first),
                        bar.grid_row(1),
                        Border::new().grid_row(2).content(second),
                    ))
                } else {
                    grid.columns(lengths).children((
                        Border::new().content(first),
                        bar.grid_column(1),
                        Border::new().grid_column(2).content(second),
                    ))
                }
                .into()
            }
        }
    }
}

impl WindowRoot {
    /// Shows the menu of `tab`'s selection or folder at the pointer.
    fn open_menu(&self, tab: u32, on: MenuOn, context: &ComponentContext<Self>) {
        let Some(pane) = self.input.panes.iter().find(|pane| pane.tab.id == tab) else {
            return;
        };
        let menu = menus::Context {
            shared: &self.input.shared,
            window: self.id,
            tab: &pane.tab,
            sidebar: &self.input.chrome.sidebar,
            only: (pane.tab.selected_count == 1)
                .then(|| pane.rows.rows.iter().find(|row| row.selected))
                .flatten()
                .map(|row| (row.index, row.thumbnail_choice)),
        };
        let entries = match on {
            MenuOn::Selection if pane.tab.in_trash => menu.trashed(),
            MenuOn::Selection => menu.selection(),
            MenuOn::Folder => menu.folder(),
        };
        let (owner, sender) = (self.handle, context.sender());
        // The menu runs its own message loop: it opens once this has returned.
        ui_thread::defer(move || {
            if let Some(action) = popup::show(owner, &entries) {
                _ = sender.send(WindowMessage::MenuChose(tab, action));
            }
        });
    }

    /// Shows the menu of a place in the sidebar at the pointer; what is
    /// chosen acts on the focused tab.
    fn open_sidebar_menu(&self, on: &SidebarMenu, context: &ComponentContext<Self>) {
        let tab = self.target.tab;
        let entries = menus::Context {
            shared: &self.input.shared,
            window: self.id,
            tab: &self.input.chrome.active,
            sidebar: &self.input.chrome.sidebar,
            only: None,
        }
        .sidebar(on);
        if entries.is_empty() {
            return;
        }
        let (owner, sender) = (self.handle, context.sender());
        ui_thread::defer(move || {
            if let Some(action) = popup::show(owner, &entries) {
                _ = sender.send(WindowMessage::MenuChose(tab, action));
            }
        });
    }

    /// Carries out what was chosen in `tab`'s menu.
    fn chose(&mut self, tab: u32, action: Action, context: &ComponentContext<Self>) {
        let owner = self.handle;
        match action {
            Action::Run(command) => {
                self.update(WindowMessage::Run(command), context);
            }
            Action::Send(event) => self.core.send(*event),
            Action::SendTo { copy } => {
                let core = self.core.clone();
                ui_thread::defer(move || {
                    let Some(folder) = dirfleet_winshell::pick_folder(owner, None) else {
                        return;
                    };
                    let folder = folder.to_string_lossy().into_owned();
                    core.send(if copy {
                        Event::CopyTo { tab, folder }
                    } else {
                        Event::MoveTo { tab, folder }
                    });
                });
            }
            Action::Plugin { action, confirm } => {
                if confirm.is_empty() {
                    self.core.send(Event::RunPluginAction { tab, action });
                } else {
                    self.plugin_asked = Some((tab, action, confirm));
                    self.asking = Some(Asking::Plugin);
                }
            }
            Action::EditSearch(id) => {
                let saved = self.input.shared.saved_searches.iter();
                if let Some(saved) = saved.into_iter().find(|saved| saved.id == id) {
                    self.draft.replace(SearchDraft::editing(saved));
                    self.asking = Some(Asking::EditSearch);
                }
            }
            Action::Shell => {
                let paths = self.selected_paths(tab);
                if paths.is_empty() {
                    return;
                }
                let core = self.core.clone();
                ui_thread::defer(move || {
                    use dirfleet_winshell::context_menu::{Outcome, show};
                    if matches!(show(owner, &paths), Ok(Outcome::Rename)) {
                        core.send(Event::StartRename { tab });
                    }
                });
            }
        }
    }

    /// Tells the core what was dropped where, and how the user wants it taken.
    fn drop_files(&self, dropped: Dropped, copy: bool, force_move: bool) {
        let Dropped {
            tab,
            paths,
            target,
            on_trash,
        } = dropped;
        self.core.send(if on_trash {
            Event::DropOnTrash { tab, paths }
        } else {
            Event::DropItems {
                tab,
                paths,
                target,
                copy,
                force_move,
            }
        });
    }

    /// The paths of `tab`'s selected entries, as far as the core has sent them.
    fn selected_paths(&self, tab: u32) -> Vec<PathBuf> {
        let Some(pane) = self.input.panes.iter().find(|pane| pane.tab.id == tab) else {
            return Vec::new();
        };
        if pane.tab.path.is_empty() {
            return Vec::new();
        }
        let folder = Path::new(&pane.tab.path);
        pane.rows
            .rows
            .iter()
            .filter(|row| row.selected)
            .map(|row| folder.join(&row.folder).join(&row.name))
            .collect()
    }
}

const SPLIT_RIGHT: &str = "split-right";
const SPLIT_BELOW: &str = "split-below";

/// The key a command has in menus.
fn command_key(command: CommandId) -> Key {
    Key::from(command as u32)
}

/// The commands a window has no button for, grouped as the Mac app's menus are.
fn more_menu(shared: &ViewModel, on_click: Callback<Key>) -> Menu {
    const GROUPS: [&[CommandId]; 6] = [
        &[
            CommandId::NewWindow,
            CommandId::NewTab,
            CommandId::ReopenClosedTab,
            CommandId::CloseTab,
        ],
        &[
            CommandId::ViewIcons,
            CommandId::ViewList,
            CommandId::ToggleHidden,
            CommandId::ToggleInspector,
        ],
        &[
            CommandId::GoToFolder,
            CommandId::OpenInTerminal,
            CommandId::OpenTrash,
        ],
        &[CommandId::NewFolder, CommandId::NewTextFile],
        &[
            CommandId::Find,
            CommandId::SaveSearch,
            CommandId::ToggleSidebar,
            CommandId::AddToFavorites,
        ],
        &[
            CommandId::Palette,
            CommandId::ShortcutHelp,
            CommandId::Settings,
        ],
    ];
    let mut items = Vec::new();
    for (group, commands) in GROUPS.iter().enumerate() {
        if group > 0 {
            items.push(MenuItem::separator(format!("separator-{group}")));
        }
        for command in *commands {
            let Some(view) = shared.commands.iter().find(|view| view.id == *command) else {
                continue;
            };
            let label = if view.shortcut.is_empty() {
                view.label.clone()
            } else {
                format!("{}\t{}", view.label, view.shortcut)
            };
            items.push(MenuItem::item(command_key(*command), label));
        }
        if group == 1 {
            items.push(MenuItem::item(SPLIT_RIGHT, "Split Right"));
            items.push(MenuItem::item(SPLIT_BELOW, "Split Below"));
        }
    }
    Menu::new(items, on_click)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragging_a_splitter_moves_its_share_and_keeps_both_sides() {
        let drag = Drag {
            split: 0,
            vertical: false,
            from_share: 500.0,
            extent: 1000.0,
            from: 600.0,
        };
        let at = |x: f64| {
            drag.share_at(&PointerEventInfo {
                window_x: x,
                ..PointerEventInfo::default()
            })
        };
        assert_eq!(at(700.0), 600);
        assert_eq!(at(500.0), 400);
        assert_eq!(at(5000.0), 900);
        assert_eq!(at(-5000.0), 100);
    }
}
