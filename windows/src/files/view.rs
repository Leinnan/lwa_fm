//! The folder's entries as a list or an icon grid, built only for what is in sight.
//!
//! The folder can hold far more entries than the core sends, so the view is
//! as long as the folder and asks for each line by position; positions the
//! core has not sent yet draw empty until it does. The view tells the core
//! which entries are in sight from its scroll position, scrolls where the core
//! asks, and turns keys into moves of the cursor.
use std::rc::Rc;
use std::time::Duration;

use dirfleet_core::app::{CommandId, Event, FolderView, IconSize};
use dirfleet_core::columns::Column;
use windows_reactor::{
    Border, Callback, CharacterEventInfo, Color, Component, ComponentContext, ComponentTimer,
    DragDropAction, DragDropOperation, DragDropPolicy, DroppedData, ElementObservation, ElementRef,
    Grid, GridLength, InputModifiers, ItemsRepeater, Key, KeyEventInfo, PointerEventInfo,
    RoutedCallback, ScrollBarVisibility, ScrollPosition, ScrollViewer, View, ViewContext,
    VirtualKey, VirtualSource,
};

use super::Dropped;
use super::columns::{self, Cell, ColumnSet, HEADER_HEIGHT, HeaderCallbacks, Resize};
use super::layout::{Metrics, Motion, Span};
use super::press::{EntryHooks, PressClaim};
use super::row::{Row, RowInput};
use super::tiles::{TileLine, TileLineInput};
use crate::core_link::CoreLink;
use crate::menus::MenuOn;
use crate::shortcuts::{Chord, Shortcuts};
use crate::store::TabRows;

/// Space kept free on the right for the scroll bar.
const SCROLL_BAR: f64 = 16.0;
/// A scroll asked for before the view is laid out at its new length is cut
/// short, so it is asked again this often until it holds, this many times.
const SCROLL_RETRY: Duration = Duration::from_millis(30);
const SCROLL_TRIES: u8 = 10;
/// Likewise for the keyboard focus, refused until the view is laid out.
const FOCUS_RETRY: Duration = Duration::from_millis(40);
const FOCUS_TRIES: u8 = 10;
/// A drop on a folder may be told twice, by the folder's entry and by the
/// view around it, in either order: the view waits this long for both.
const DROP_SETTLE: Duration = Duration::from_millis(60);

#[derive(Clone, PartialEq)]
pub struct FileViewInput {
    pub core: CoreLink,
    pub rows: Rc<TabRows>,
    pub columns: Rc<ColumnSet>,
    /// How the folder is shown: list or icons, and the order.
    pub view: FolderView,
    pub icon_size: IconSize,
    /// Results come from several folders: the list shows each entry's folder.
    pub show_folder: bool,
    pub shortcuts: Rc<Shortcuts>,
    /// Runs a command the view's keys are bound to.
    pub on_command: Callback<CommandId>,
    /// Asks for the menu of the tab's selection or folder.
    pub on_menu: Callback<(u32, MenuOn)>,
    /// Files were dropped on the view.
    pub on_drop: Callback<Dropped>,
    /// The view's pane has the window's focus, so the view takes the keyboard.
    pub focused: bool,
    /// Changes when the keyboard should come back to the view: a dialog or a
    /// menu that had it has closed.
    pub keys_back: u32,
    /// The width to fill until the view has measured itself.
    pub width: f64,
}

pub struct FileView {
    input: FileViewInput,
    /// Kept between renders: an unchanged source leaves the lines alone.
    source: VirtualSource,
    metrics: Metrics,
    cells: Rc<Vec<Cell>>,
    viewer: ElementRef<ScrollViewer>,
    /// Takes the keyboard for the view.
    focus: ElementRef<Border>,
    /// Dropping it would stop the scroll reports.
    _observation: ElementObservation,
    position: ScrollPosition,
    /// The width the lines were built for.
    width: f64,
    /// The entries last reported to the core as in sight, with their tab.
    reported: Option<(u32, Span)>,
    /// Where the view should scroll, until it is there.
    wanted: Option<Wanted>,
    /// The cursor after the moves the core has not answered yet.
    cursor: Option<u32>,
    resize: Option<Resize>,
    /// The keyboard was asked for once the view was first on screen.
    took_keys: bool,
    /// Dropping it cancels the next try at taking the keyboard.
    keys_timer: Option<ComponentTimer>,
    claim: PressClaim,
    hooks: EntryHooks,
    /// Dropped files and the folder they were dropped on (the tab's own when
    /// `None`), until both tellings of the drop are in.
    dropping: Option<(Vec<String>, Option<String>)>,
    /// Dropping it would lose the drop.
    drop_timer: Option<ComponentTimer>,
    /// A name is being typed in a row, so keys are left to its text box.
    renaming: Rc<std::cell::Cell<bool>>,
    on_key: RoutedCallback<KeyEventInfo>,
    on_character: RoutedCallback<CharacterEventInfo>,
    on_background: Callback<PointerEventInfo>,
    header: HeaderCallbacks,
}

struct Wanted {
    offset: f64,
    tries: u8,
    /// Dropping it cancels the next try.
    _timer: ComponentTimer,
}

#[derive(Clone, Debug)]
pub enum ViewMessage {
    Scrolled(ScrollPosition),
    /// Time to check that a wanted scroll held.
    RetryScroll,
    Move {
        motion: Motion,
        command: bool,
        shift: bool,
    },
    Open,
    Clear,
    Typed(String),
    /// A press on the space no entry covers.
    BackgroundPressed(PointerEventInfo),
    /// A right click, on an entry or beside them.
    Menu(MenuOn),
    Dropped(DroppedData),
    /// Files were dropped on the entry at this position, a folder.
    EntryDropped(u32, DroppedData),
    /// Both tellings of a drop have had their time.
    DropSettled,
    /// The entry at this position is being dragged.
    EntryDragged(u32),
    GripPressed(Column, f64, PointerEventInfo),
    GripMoved(PointerEventInfo),
    GripReleased,
    ToggleColumn(Key),
    /// Time to ask for the keyboard: the given try.
    TakeKeys(u8),
    /// Whether the keyboard was given, and on which try.
    KeysTaken(bool, u8),
}

impl FileView {
    fn source(
        input: &FileViewInput,
        metrics: Metrics,
        cells: &Rc<Vec<Cell>>,
        width: f64,
        (claim, hooks): (&PressClaim, &EntryHooks),
    ) -> VirtualSource {
        let input = input.clone();
        let cells = Rc::clone(cells);
        let claim = claim.clone();
        let hooks = hooks.clone();
        let total = input.rows.total_rows;
        // Keys are line positions; they mean other entries when the tab or
        // the number of entries on a line changes.
        let revision = (u64::from(input.rows.tab) << 32) | u64::from(metrics.per_line);
        VirtualSource::new(
            revision,
            metrics.lines(total) as usize,
            |line| line,
            move |line| {
                let first = line as u32 * metrics.per_line;
                match metrics.tile {
                    None => View::component::<Row>(RowInput {
                        core: input.core.clone(),
                        tab: input.rows.tab,
                        index: first,
                        row: input.rows.row(first).cloned(),
                        cells: Rc::clone(&cells),
                        width,
                        claim: claim.clone(),
                        hooks: hooks.clone(),
                        rename: input
                            .rows
                            .renaming
                            .as_ref()
                            .filter(|rename| rename.row == first)
                            .cloned(),
                    }),
                    Some(tile) => View::component::<TileLine>(TileLineInput {
                        core: input.core.clone(),
                        tab: input.rows.tab,
                        first,
                        entries: (first..(first + metrics.per_line).min(total))
                            .map(|entry| input.rows.row(entry).cloned())
                            .collect(),
                        tile,
                        claim: claim.clone(),
                        hooks: hooks.clone(),
                    }),
                }
            },
        )
    }

    /// Lays the entries out again for the current input and width.
    fn rebuild(&mut self) {
        let input = &self.input;
        self.metrics = Metrics::of(input.view.display, input.icon_size, self.width);
        self.cells = Rc::new(columns::cells(&input.columns, input.show_folder));
        self.source = Self::source(
            input,
            self.metrics,
            &self.cells,
            self.width,
            (&self.claim, &self.hooks),
        );
    }

    /// Tells the core which entries are in sight, when that changed.
    fn report(&mut self) {
        // Mid-scroll the position is the old one; the core would send its rows.
        if self.wanted.is_some() {
            return;
        }
        let rows = &self.input.rows;
        let Some(span) = self.metrics.in_sight(
            self.position.vertical_offset,
            self.position.viewport_height,
            rows.total_rows,
        ) else {
            return;
        };
        if self.reported != Some((rows.tab, span)) {
            self.reported = Some((rows.tab, span));
            crate::trace::note(|| {
                format!(
                    "in sight: entries {}..={} (offset {:.0}, viewport {:.0}, extent {:.0})",
                    span.first,
                    span.last,
                    self.position.vertical_offset,
                    self.position.viewport_height,
                    self.position.extent_height
                )
            });
            self.input.core.send(Event::ViewportChanged {
                tab: rows.tab,
                first_row: span.first,
                rows: span.len(),
            });
        }
    }

    /// Scrolls to `offset`, asking again until it holds.
    fn scroll_to(&mut self, offset: f64, tries: u8, context: &ComponentContext<Self>) {
        _ = self
            .viewer
            .request_scroll_to(None, Some(offset), false, |_| {});
        self.wanted = (tries < SCROLL_TRIES).then(|| Wanted {
            offset,
            tries: tries + 1,
            _timer: context.set_local_timeout(SCROLL_RETRY, || ViewMessage::RetryScroll),
        });
    }

    /// Waits for the other telling of a drop, when it is not waited for yet.
    fn settle_drop(&mut self, context: &ComponentContext<Self>) {
        if self.drop_timer.is_none() {
            self.drop_timer =
                Some(context.set_local_timeout(DROP_SETTLE, || ViewMessage::DropSettled));
        }
    }

    /// Asks for the keyboard a moment from now, when the view is laid out.
    fn take_keys(&mut self, attempt: u8, context: &ComponentContext<Self>) {
        self.keys_timer =
            Some(context.set_local_timeout(FOCUS_RETRY, move || ViewMessage::TakeKeys(attempt)));
    }

    /// Whether the view is laid out at the folder's length and scrolled to
    /// `offset`, or as near to it as the folder goes.
    fn holds(&self, offset: f64) -> bool {
        let position = &self.position;
        // The top is the top at any length.
        let laid_out = offset <= 0.0
            || position.extent_height + 1.0 >= self.metrics.extent(self.input.rows.total_rows);
        let furthest = (position.extent_height - position.viewport_height).max(0.0);
        laid_out && (position.vertical_offset - offset.min(furthest)).abs() < 1.0
    }

    /// The keys the view answers: moves of the cursor, opening, and the
    /// commands bound to keys that would otherwise edit text.
    fn key_handler(
        input: &FileViewInput,
        renaming: &Rc<std::cell::Cell<bool>>,
        context: &ComponentContext<Self>,
    ) -> RoutedCallback<KeyEventInfo> {
        let renaming = Rc::clone(renaming);
        let shortcuts = Rc::clone(&input.shortcuts);
        let on_command = input.on_command.clone();
        let sender = context.sender();
        RoutedCallback::new(move |info: KeyEventInfo| {
            // While a name is being typed, the keys are the text box's.
            if renaming.get() {
                return false;
            }
            let command = info.modifiers.contains(InputModifiers::CONTROL);
            let shift = info.modifiers.contains(InputModifiers::SHIFT);
            let alt = info.modifiers.contains(InputModifiers::ALT);
            let motion = match info.key {
                _ if alt => None,
                VirtualKey::UP => Some(Motion::Up),
                VirtualKey::DOWN => Some(Motion::Down),
                VirtualKey::LEFT => Some(Motion::Left),
                VirtualKey::RIGHT => Some(Motion::Right),
                VirtualKey::HOME => Some(Motion::Home),
                VirtualKey::END => Some(Motion::End),
                VirtualKey::PAGE_UP => Some(Motion::PageUp),
                VirtualKey::PAGE_DOWN => Some(Motion::PageDown),
                _ => None,
            };
            if let Some(motion) = motion {
                return sender.send(ViewMessage::Move {
                    motion,
                    command,
                    shift,
                });
            }
            let plain = !command && !alt;
            match info.key {
                VirtualKey::ENTER if plain => return sender.send(ViewMessage::Open),
                VirtualKey::ESCAPE if plain => return sender.send(ViewMessage::Clear),
                // The Menu key opens the selection's menu, as a right click does.
                VirtualKey(0x5D) if plain => {
                    return sender.send(ViewMessage::Menu(MenuOn::Selection));
                }
                VirtualKey::BACK if plain => {
                    on_command.call(CommandId::Up);
                    return true;
                }
                _ => {}
            }
            let chord = Chord {
                key: info.key.0,
                ctrl: command,
                shift,
                alt,
            };
            // The window runs the other chords before they get here.
            if chord.edits_text()
                && let Some(bound) = shortcuts.command(chord)
            {
                on_command.call(bound);
                return true;
            }
            false
        })
    }
}

impl Component for FileView {
    type Input = FileViewInput;
    type Message = ViewMessage;

    fn create(input: &FileViewInput, context: &ComponentContext<Self>) -> Self {
        let viewer = ElementRef::new();
        let scrolled = context.sender();
        let observation = viewer.observe_view(move |position| {
            _ = scrolled.send(ViewMessage::Scrolled(position));
        });
        let width = (input.width - SCROLL_BAR).max(120.0);
        let metrics = Metrics::of(input.view.display, input.icon_size, width);
        let cells = Rc::new(columns::cells(&input.columns, input.show_folder));
        let claim = PressClaim::default();
        let renaming = Rc::new(std::cell::Cell::new(input.rows.renaming.is_some()));
        let typing_name = Rc::clone(&renaming);
        let typed = context.sender();
        let background = context.sender();
        let background_claim = claim.clone();
        let grip_pressed = context.sender();
        let (entry_dropped, entry_dragged) = (context.sender(), context.sender());
        let hooks = EntryHooks {
            on_drop: Callback::new(move |(entry, data): (u32, DroppedData)| {
                _ = entry_dropped.send(ViewMessage::EntryDropped(entry, data));
            }),
            on_drag: Callback::new(move |entry: u32| {
                _ = entry_dragged.send(ViewMessage::EntryDragged(entry));
            }),
        };
        Self {
            input: input.clone(),
            source: Self::source(input, metrics, &cells, width, (&claim, &hooks)),
            metrics,
            cells,
            viewer,
            focus: ElementRef::new(),
            _observation: observation,
            position: ScrollPosition::default(),
            width,
            reported: None,
            wanted: None,
            cursor: None,
            resize: None,
            took_keys: false,
            keys_timer: None,
            on_key: Self::key_handler(input, &renaming, context),
            on_character: RoutedCallback::new(move |info: CharacterEventInfo| {
                if typing_name.get() {
                    return false;
                }
                let held = info.modifiers;
                if held.contains(InputModifiers::CONTROL) || held.contains(InputModifiers::ALT) {
                    return false;
                }
                // Space previews; control characters are keys, not text.
                match char::from_u32(u32::from(info.character)) {
                    Some(typed_char) if typed_char > ' ' => {
                        typed.send(ViewMessage::Typed(typed_char.to_string()))
                    }
                    _ => false,
                }
            }),
            on_background: Callback::new(move |info: PointerEventInfo| {
                // An entry sees its press first and claims it.
                let on_entry = background_claim.taken(&info);
                let message = if info.is_right_button_pressed {
                    ViewMessage::Menu(if on_entry {
                        MenuOn::Selection
                    } else {
                        MenuOn::Folder
                    })
                } else if on_entry {
                    return;
                } else {
                    ViewMessage::BackgroundPressed(info)
                };
                _ = background.send(message);
            }),
            header: HeaderCallbacks {
                grip_pressed: Callback::new(
                    move |(column, width, info): (Column, f64, PointerEventInfo)| {
                        _ = grip_pressed.send(ViewMessage::GripPressed(column, width, info));
                    },
                ),
                grip_moved: context.callback(ViewMessage::GripMoved),
                grip_released: context.callback(|_: PointerEventInfo| ViewMessage::GripReleased),
                toggle: context.callback(ViewMessage::ToggleColumn),
            },
            claim,
            hooks,
            dropping: None,
            drop_timer: None,
            renaming,
        }
    }

    fn input_changed(&mut self, input: &FileViewInput, context: &ComponentContext<Self>) {
        let (old, new) = (&self.input.rows, &input.rows);
        let relaid = (self.input.view.display, self.input.icon_size)
            != (input.view.display, input.icon_size);
        let another = (old.tab, &old.path, old.in_trash) != (new.tab, &new.path, new.in_trash);
        let asked = (old.scroll_count != new.scroll_count)
            .then_some(new.scroll_to)
            .flatten()
            .filter(|&entry| entry < new.total_rows);
        let cursor = (old.cursor != new.cursor).then_some(new.cursor).flatten();
        if self.input.shortcuts != input.shortcuts || self.input.on_command != input.on_command {
            self.on_key = Self::key_handler(input, &self.renaming, context);
        }
        let renamed = self.input.rows.renaming.is_some() && input.rows.renaming.is_none();
        self.renaming.set(input.rows.renaming.is_some());
        // The keys come back from the text box, or with the window's focus.
        let handed_back = self.input.keys_back != input.keys_back;
        if input.focused && (renamed || handed_back || !self.input.focused) {
            self.take_keys(0, context);
        }
        self.input = input.clone();
        self.cursor = None;
        self.rebuild();

        let (offset, height) = (self.position.vertical_offset, self.position.viewport_height);
        let target = if another || relaid {
            // Another folder, or another layout of this one, starts at its top.
            Some(0.0)
        } else if let Some(entry) = asked {
            // Back and Forward return to where the folder was left: its first
            // entry goes back to the top, unless it is in sight anyway.
            self.metrics
                .offset_showing(entry, offset, height)
                .map(|_| self.metrics.offset_of(entry))
        } else {
            // The cursor moved: it stays in sight, moving as little as it takes.
            cursor.and_then(|entry| self.metrics.offset_showing(entry, offset, height))
        };
        if let Some(target) = target {
            self.scroll_to(target, 0, context);
        }
        self.report();
    }

    #[expect(clippy::too_many_lines, reason = "one arm per message")]
    fn update(&mut self, message: ViewMessage, context: &ComponentContext<Self>) {
        let tab = self.input.rows.tab;
        let core = self.input.core.clone();
        match message {
            ViewMessage::Scrolled(position) => {
                self.position = position;
                // The first report says the view is on screen: time to take the keys.
                if self.input.focused && !self.took_keys {
                    self.took_keys = true;
                    self.take_keys(0, context);
                }
                if position.viewport_width > 0.0
                    && (position.viewport_width - self.width).abs() > 0.5
                {
                    self.width = position.viewport_width;
                    self.rebuild();
                }
                if self
                    .wanted
                    .as_ref()
                    .is_some_and(|wanted| self.holds(wanted.offset))
                {
                    self.wanted = None;
                }
                self.report();
            }
            ViewMessage::RetryScroll => {
                let Some(wanted) = self.wanted.take() else {
                    return;
                };
                if !self.holds(wanted.offset) {
                    self.scroll_to(wanted.offset, wanted.tries, context);
                }
                self.report();
            }
            ViewMessage::Move {
                motion,
                command,
                shift,
            } => {
                let rows = &self.input.rows;
                let from = self.cursor.or(rows.cursor);
                let height = self.position.viewport_height;
                if let Some(row) = self.metrics.moved(motion, from, rows.total_rows, height) {
                    self.cursor = Some(row);
                    core.send(Event::MoveCursor {
                        tab,
                        row,
                        command,
                        shift,
                    });
                }
            }
            ViewMessage::Open => core.send(Event::OpenSelection { tab }),
            ViewMessage::Clear => core.send(Event::ClearSelection { tab }),
            ViewMessage::Typed(text) => core.send(Event::TypeSelect { tab, text }),
            ViewMessage::BackgroundPressed(info) => {
                if info.is_left_button_pressed {
                    core.send(Event::ClearSelection { tab });
                }
            }
            ViewMessage::Menu(on) => {
                // With nothing selected there is only the folder to act on.
                let on = if self.input.rows.selection.is_empty() {
                    MenuOn::Folder
                } else {
                    on
                };
                self.input.on_menu.call((tab, on));
            }
            ViewMessage::Dropped(data) => {
                // Told by a folder's entry already: that says more.
                if let DroppedData::StorageItems(items) = data
                    && self.dropping.is_none()
                {
                    let paths = items.into_iter().map(|item| item.path).collect();
                    self.dropping = Some((paths, None));
                    self.settle_drop(context);
                }
            }
            ViewMessage::EntryDropped(entry, data) => {
                let rows = &self.input.rows;
                let folder = rows
                    .row(entry)
                    .filter(|row| row.is_dir)
                    .and_then(|row| rows.path_of(row));
                if let DroppedData::StorageItems(items) = data {
                    let paths = items.into_iter().map(|item| item.path).collect();
                    let target = folder.map(|folder| folder.to_string_lossy().into_owned());
                    self.dropping = Some((paths, target));
                    self.settle_drop(context);
                }
            }
            ViewMessage::DropSettled => {
                self.drop_timer = None;
                if let Some((paths, target)) = self.dropping.take() {
                    self.input.on_drop.call(Dropped {
                        tab,
                        paths,
                        target,
                        on_trash: self.input.rows.in_trash,
                    });
                }
            }
            ViewMessage::EntryDragged(entry) => {
                let paths = self.input.rows.dragged_from(entry);
                if !paths.is_empty() {
                    let owner = crate::hwnd::active();
                    // The drag runs its own message loop until the drop.
                    crate::ui_thread::defer(move || {
                        let outcome = dirfleet_winshell::start_polled_drag(owner, &paths);
                        crate::trace::note(|| format!("dragged out: {outcome:?}"));
                    });
                }
            }
            ViewMessage::GripPressed(column, from_width, info) => {
                self.resize = Some(Resize {
                    column,
                    from_x: info.window_x,
                    from_width,
                });
            }
            ViewMessage::GripMoved(info) => {
                if let Some(resize) = self.resize.filter(|_| info.is_captured) {
                    core.send(Event::SetColumnWidth {
                        column: resize.column,
                        width: resize.width_at(info.window_x),
                    });
                }
            }
            ViewMessage::GripReleased => self.resize = None,
            ViewMessage::TakeKeys(attempt) => {
                let sender = context.sender();
                _ = self.focus.request_focus_result(move |result| {
                    _ = sender.send(ViewMessage::KeysTaken(result == Ok(true), attempt));
                });
            }
            ViewMessage::KeysTaken(taken, attempt) => {
                self.keys_timer = None;
                // An element just made refuses the focus until it is laid out.
                if !taken && attempt < FOCUS_TRIES && self.input.focused {
                    self.take_keys(attempt + 1, context);
                }
            }
            ViewMessage::ToggleColumn(key) => {
                if let Some(column) = columns::toggled(&self.input.columns, &key) {
                    core.send(Event::ToggleColumn { column });
                }
            }
        }
    }

    fn view(&self, input: &FileViewInput, context: &mut ViewContext<Self>) -> View {
        let entries = ScrollViewer::new()
            .element_ref(&self.viewer)
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
            .content(ItemsRepeater::new().virtual_source(self.source.clone()));
        let body: View = if self.metrics.tile.is_some() {
            entries.into()
        } else {
            Grid::new()
                .rows([GridLength::Pixel(HEADER_HEIGHT), GridLength::STAR])
                .children((
                    columns::header(
                        &input.core,
                        input.rows.tab,
                        input.view,
                        &input.columns,
                        &self.cells,
                        self.width,
                        &self.header,
                    ),
                    entries.grid_row(1),
                ))
                .into()
        };
        Border::new()
            .element_ref(&self.focus)
            .is_tab_stop(true)
            .focus_on_pointer_release(true)
            // Transparent, not absent, so empty space takes the pointer too.
            .background(Color::transparent())
            .on_preview_key_down(self.on_key.clone())
            .on_character_received(self.on_character.clone())
            .on_pointer_pressed(self.on_background.clone())
            .drop_policy(
                DragDropPolicy::new().storage_items(DragDropAction::new(DragDropOperation::Move)),
            )
            .on_drop(context.callback(ViewMessage::Dropped))
            .content(body)
            .into()
    }
}
