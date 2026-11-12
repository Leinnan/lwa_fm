//! One pane of a window: its tabs, and under them the folder the shown tab
//! is in, with its header and its status line.
use std::rc::Rc;

use dirfleet_core::app::{
    CommandId, Event, FilterGroup, IconSize, SearchView, TabTarget, TabViewModel,
};
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, Component, ComponentContext, ElementRef, Grid,
    GridLength, HorizontalAlignment, Key, Orientation, PointerEventInfo, ProgressRing, StackPanel,
    TabView, TabViewItem, TextBlock, TextBox, TextTrimming, TextWrapping, Thickness,
    TooltipExt as _, VerticalAlignment, View, ViewContext, keyed,
};

use crate::core_link::CoreLink;
use crate::files::{ColumnSet, Dropped, FileView, FileViewInput, glyph};
use crate::menus::MenuOn;
use crate::pathbar::path_bar;
use crate::plugins;
use crate::search::{self, Choice};
use crate::shortcuts::Shortcuts;
use crate::store::PaneParts;
use crate::theme;

/// What the header's buttons take beside the path bar.
const HEADER_BUTTONS: f64 = 190.0 + search::WIDTH + 48.0;

#[derive(Clone, PartialEq)]
pub struct PaneInput {
    pub core: CoreLink,
    pub window: u32,
    pub pane: PaneParts,
    pub columns: Rc<ColumnSet>,
    pub icon_size: IconSize,
    pub shortcuts: Rc<Shortcuts>,
    /// Runs a command on the window's focused tab.
    pub on_command: Callback<CommandId>,
    /// Asks for the menu of a tab's selection or folder.
    pub on_menu: Callback<(u32, MenuOn)>,
    /// Files were dropped on a tab.
    pub on_drop: Callback<Dropped>,
    /// The preset filters the search menu offers.
    pub filters: Rc<Vec<FilterGroup>>,
    /// Changes when the search box should take the keyboard (Find).
    pub find: u32,
    /// The pane has the window's focus.
    pub focused: bool,
    /// Changes when the keyboard should come back to the entries.
    pub keys_back: u32,
    /// The window has other panes, so the focused one is marked, and each
    /// pane shows its own tabs; a lone pane's tabs are in the title bar.
    pub marked: bool,
    /// The width the pane is given.
    pub width: f64,
}

pub struct Pane {
    core: CoreLink,
    window: u32,
    /// The tab shown, and the pane's tabs in order.
    shown: u32,
    tabs: Vec<u32>,
    path: String,
    focused: bool,
    on_pressed: Callback<PointerEventInfo>,
    search_field: ElementRef<TextBox>,
    on_search_option: Callback<Key>,
    on_search_left: Callback<()>,
    /// The Find request last answered.
    find: u32,
    /// Counted up each time the search box gives the keyboard back.
    left_search: u32,
    filters: Rc<Vec<FilterGroup>>,
    /// How the tab shown searches now.
    search: Option<SearchView>,
}

#[derive(Clone, Debug)]
pub enum PaneMessage {
    /// A press anywhere in the pane.
    Pressed,
    Selected(Option<usize>),
    Close(Key),
    Add,
    Reordered(Vec<Key>),
    SearchOption(Key),
    /// Escape was pressed in the search box.
    SearchLeft,
}

impl Pane {
    fn remember(&mut self, input: &PaneInput) {
        self.shown = input.pane.tab.id;
        self.tabs = input.pane.tabs.iter().map(|tab| tab.id).collect();
        self.path.clone_from(&input.pane.tab.path);
        self.filters = Rc::clone(&input.filters);
        self.search.clone_from(&input.pane.tab.search);
        self.focused = input.focused;
    }

    fn tab_of(&self, key: &Key) -> Option<u32> {
        self.tabs
            .iter()
            .copied()
            .find(|tab| Key::from(*tab) == *key)
    }
}

impl Component for Pane {
    type Input = PaneInput;
    type Message = PaneMessage;

    fn create(input: &PaneInput, context: &ComponentContext<Self>) -> Self {
        let sender = context.sender();
        let mut pane = Self {
            core: input.core.clone(),
            window: input.window,
            shown: 0,
            tabs: Vec::new(),
            path: String::new(),
            focused: false,
            on_pressed: Callback::new(move |_: PointerEventInfo| {
                _ = sender.send(PaneMessage::Pressed);
            }),
            search_field: ElementRef::new(),
            on_search_option: context.callback(PaneMessage::SearchOption),
            on_search_left: context.callback(|()| PaneMessage::SearchLeft),
            find: input.find,
            left_search: 0,
            filters: Rc::clone(&input.filters),
            search: None,
        };
        pane.remember(input);
        pane
    }

    fn input_changed(&mut self, input: &PaneInput, _: &ComponentContext<Self>) {
        self.remember(input);
        // Find puts the keyboard in the search box of the pane with the focus.
        if input.find != self.find {
            self.find = input.find;
            if input.focused {
                _ = self.search_field.request_focus();
            }
        }
    }

    fn update(&mut self, message: PaneMessage, _: &ComponentContext<Self>) {
        match message {
            PaneMessage::Pressed => {
                // A press in a pane without the focus gives it the focus.
                if !self.focused {
                    self.core.send(Event::ActivateTab { tab: self.shown });
                }
            }
            PaneMessage::Selected(index) => {
                let chosen = index.and_then(|index| self.tabs.get(index).copied());
                if let Some(tab) = chosen.filter(|tab| *tab != self.shown || !self.focused) {
                    self.core.send(Event::ActivateTab { tab });
                }
            }
            PaneMessage::Close(key) => {
                if let Some(tab) = self.tab_of(&key) {
                    self.core.send(Event::CloseTab { tab });
                }
            }
            PaneMessage::Add => {
                // A new tab joins the pane with the focus: this one, from now on.
                if !self.focused {
                    self.core.send(Event::ActivateTab { tab: self.shown });
                }
                self.core.send(Event::NewTab {
                    window: self.window,
                    path: Some(self.path.clone()).filter(|path| !path.is_empty()),
                });
            }
            PaneMessage::SearchOption(key) => {
                if let Some(choice) = Choice::of(&key, &self.filters) {
                    self.core
                        .send(choice.event(self.shown, self.search.as_ref()));
                }
            }
            PaneMessage::SearchLeft => self.left_search = self.left_search.wrapping_add(1),
            PaneMessage::Reordered(keys) => {
                let order: Vec<u32> = keys.iter().filter_map(|key| self.tab_of(key)).collect();
                if let Some((tab, index, beside)) = moved(&self.tabs, &order) {
                    self.core.send(Event::MoveTab {
                        tab,
                        to: TabTarget::Beside {
                            tab: beside,
                            index: index as u32,
                        },
                    });
                }
            }
        }
    }

    fn view(&self, input: &PaneInput, context: &mut ViewContext<Self>) -> View {
        let pane = &input.pane;
        let shown = pane.tab.id;
        // The strip only holds the tabs' titles. The folder is drawn below
        // it, once, for the tab shown: content moved from one tab's item to
        // another's was left blank when the shown tab was closed.
        let items = pane
            .tabs
            .iter()
            .map(|tab| keyed(tab.id, TabViewItem::new().header(tab.title.clone())));
        let strip = TabView::new()
            .can_reorder_tabs(true)
            .is_add_tab_button_visible(true)
            .selected_index(pane.tabs.iter().position(|tab| tab.id == shown))
            .keyed_tab_items(items)
            .on_selection_changed(context.callback(PaneMessage::Selected))
            .on_close_requested(context.callback(PaneMessage::Close))
            .on_add_tab_button_click(context.message(PaneMessage::Add))
            .on_reordered(context.callback(PaneMessage::Reordered));
        let find = search::search_box(
            &input.core,
            &pane.tab,
            &self.search_field,
            &input.filters,
            self.on_search_option.clone(),
            &self.on_search_left,
        );
        // The folder sits on a surface of its own; among several panes the
        // one with the focus has the accent for an edge.
        let edge = if input.marked && input.focused {
            theme::focused_stroke()
        } else {
            theme::stroke()
        };
        let folder = Border::new()
            .grid_row(1)
            .background(theme::surface())
            .border_brush(edge)
            .border_thickness(Thickness::uniform(1.0))
            .corner_radius(theme::RADIUS)
            .content(content(input, find, self.left_search));
        Border::new()
            .on_pointer_pressed(self.on_pressed.clone())
            .content(
                Grid::new()
                    .rows([GridLength::Auto, GridLength::STAR])
                    .children((
                        if input.marked {
                            strip.into()
                        } else {
                            View::from(Border::new())
                        },
                        folder,
                    )),
            )
            .into()
    }
}

/// The tab that changed place when `before` became `after`: itself, its new
/// position, and another tab of the pane to place it by.
pub fn moved(before: &[u32], after: &[u32]) -> Option<(u32, usize, u32)> {
    if before.len() != after.len() || before == after {
        return None;
    }
    // Taking the moved tab out of both orders leaves the same order.
    let tab = after.iter().copied().find(|candidate| {
        before
            .iter()
            .filter(|tab| *tab != candidate)
            .eq(after.iter().filter(|tab| *tab != candidate))
    })?;
    let index = after.iter().position(|placed| *placed == tab)?;
    let beside = after.iter().copied().find(|other| *other != tab)?;
    Some((tab, index, beside))
}

/// The folder the pane's tab shows: header, entries, status line.
fn content(input: &PaneInput, find: View, left_search: u32) -> View {
    let tab = &input.pane.tab;
    let chips = search::chips(&input.core, tab).unwrap_or_else(|| Border::new().into());
    // What a scan found has its bar under the filters.
    let found = crate::results::bar(&input.core, tab).unwrap_or_else(|| Border::new().into());
    Grid::new()
        .rows([
            GridLength::Auto,
            GridLength::Auto,
            GridLength::STAR,
            GridLength::Auto,
            GridLength::Auto,
        ])
        .children((
            header(input, find),
            StackPanel::new()
                .grid_row(1)
                .orientation(Orientation::Vertical)
                .children((chips, found)),
            Border::new().grid_row(2).content(body(input, left_search)),
            Border::new()
                .grid_row(3)
                .content(plugins::bar(&input.core, tab).unwrap_or_else(|| Border::new().into())),
            status(input).grid_row(4),
        ))
        .automation_name(tab.title.clone())
        .into()
}

/// Back, Forward, Up, the path, and what the folder is busy with.
fn header(input: &PaneInput, find: View) -> Border {
    let tab = &input.pane.tab;
    let id = tab.id;
    let button = |icon: &str, name: &str, enabled: bool, event: Event| {
        let core = input.core.clone();
        Button::new()
            .style(ButtonStyle::Subtle)
            .is_enabled(enabled)
            .automation_name(name)
            .content(glyph(icon, 14.0))
            .on_click(move || core.send(event.clone()))
            .tooltip(name)
    };
    let mut busy: Vec<View> = Vec::new();
    // A scan's progress is on its own bar, under the header.
    if !tab.progress.is_empty() && tab.results.is_none() {
        busy.push(
            TextBlock::new()
                .text(tab.progress.clone())
                .font_size(12.0)
                .opacity(0.7)
                .vertical_alignment(VerticalAlignment::Center)
                .into(),
        );
    }
    if tab.loading {
        busy.push(
            ProgressRing::new()
                .is_indeterminate(true)
                .is_active(true)
                .width(16.0)
                .height(16.0)
                .into(),
        );
    }
    // The header is the surface's top band, closed off by a line.
    Border::new()
        .border_brush(theme::stroke())
        .border_thickness(Thickness::new(0.0, 0.0, 0.0, 1.0))
        .padding(Thickness::new(8.0, 6.0, 8.0, 6.0))
        .content(
            Grid::new()
                .columns([
                    GridLength::Auto,
                    GridLength::STAR,
                    GridLength::Auto,
                    GridLength::Auto,
                ])
                .column_spacing(8.0)
                .children((
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(2.0)
                        .children((
                            button("\u{E72B}", "Back", tab.nav.back, Event::Back { tab: id }),
                            button(
                                "\u{E72A}",
                                "Forward",
                                tab.nav.forward,
                                Event::Forward { tab: id },
                            ),
                            button("\u{E74A}", "Up", tab.nav.up, Event::Up { tab: id }),
                            button("\u{E72C}", "Refresh", true, Event::Refresh { tab: id }),
                        )),
                    // The path sits in a box of its own, as an address does.
                    Border::new()
                        .grid_column(1)
                        .background(theme::field())
                        .border_brush(theme::stroke())
                        .border_thickness(Thickness::uniform(1.0))
                        .corner_radius(4.0)
                        .padding(Thickness::new(4.0, 0.0, 4.0, 0.0))
                        .content(path_bar(
                            &input.core,
                            tab,
                            (input.width - HEADER_BUTTONS).max(120.0),
                        )),
                    StackPanel::new()
                        .grid_column(2)
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children(busy),
                    Border::new().grid_column(3).content(find),
                )),
        )
}

/// The entries, or what there is to say when there are none.
fn body(input: &PaneInput, left_search: u32) -> View {
    let tab = &input.pane.tab;
    let rows = &input.pane.rows;
    if let Some(error) = &tab.error {
        return notice(
            if tab.denied { "\u{E72E}" } else { "\u{E783}" },
            if tab.denied {
                "You don't have permission to open this folder"
            } else {
                "This folder can't be shown"
            },
            error,
            None,
        );
    }
    if rows.total_rows == 0 {
        if tab.loading {
            return Border::new().into();
        }
        let show_hidden = (tab.hidden_count > 0).then(|| {
            let core = input.core.clone();
            let id = tab.id;
            let label = if tab.hidden_count == 1 {
                "Show 1 hidden item".to_owned()
            } else {
                format!("Show {} hidden items", tab.hidden_count)
            };
            Button::new()
                .content(label)
                .on_click(move || {
                    core.send(Event::SetShowHidden {
                        tab: id,
                        show: true,
                    });
                })
                .into()
        });
        let wider = tab.search.as_ref().and_then(|search| {
            let core = input.core.clone();
            let id = tab.id;
            if search.offer_subfolders {
                Some(
                    Button::new()
                        .content("Search Subfolders")
                        .on_click(move || {
                            core.send(Event::SetSearchDepth { tab: id, depth: 4 });
                        })
                        .into(),
                )
            } else if search.offer_favorites {
                Some(
                    Button::new()
                        .content("Search Favorites")
                        .on_click(move || {
                            core.send(Event::SetSearchFavorites { tab: id, on: true });
                        })
                        .into(),
                )
            } else {
                None
            }
        });
        return notice("\u{E8B7}", empty_title(tab), "", wider.or(show_hidden));
    }
    View::component::<FileView>(FileViewInput {
        core: input.core.clone(),
        rows: Rc::clone(rows),
        columns: Rc::clone(&input.columns),
        view: tab.view,
        icon_size: input.icon_size,
        show_folder: tab.show_folder,
        show_match: tab.show_match,
        shortcuts: Rc::clone(&input.shortcuts),
        on_command: input.on_command.clone(),
        on_menu: input.on_menu.clone(),
        on_drop: input.on_drop.clone(),
        focused: input.focused,
        keys_back: input.keys_back.wrapping_add(left_search),
        width: input.width,
    })
}

const fn empty_title(tab: &TabViewModel) -> &'static str {
    if tab.search.is_some() {
        "No results"
    } else if let Some(results) = &tab.results {
        if results.compare.is_some() {
            "No differences to show"
        } else {
            "No duplicates to show"
        }
    } else if tab.in_trash {
        "The Recycle Bin is empty"
    } else {
        "This folder is empty"
    }
}

/// A message in the middle of the pane: a symbol, a title, details, and what to do.
fn notice(symbol: &str, title: &str, detail: &str, action: Option<View>) -> View {
    let mut lines: Vec<View> = vec![
        glyph(symbol, 36.0)
            .opacity(0.5)
            .horizontal_alignment(HorizontalAlignment::Center)
            .into(),
        TextBlock::new()
            .text(title)
            .font_size(16.0)
            .horizontal_alignment(HorizontalAlignment::Center)
            .into(),
    ];
    if !detail.is_empty() {
        lines.push(
            TextBlock::new()
                .text(detail)
                .opacity(0.7)
                .text_wrapping(TextWrapping::Wrap)
                .horizontal_alignment(HorizontalAlignment::Center)
                .into(),
        );
    }
    if let Some(action) = action {
        lines.push(
            Border::new()
                .horizontal_alignment(HorizontalAlignment::Center)
                .content(action)
                .into(),
        );
    }
    StackPanel::new()
        .orientation(Orientation::Vertical)
        .spacing(10.0)
        .margin(Thickness::uniform(24.0))
        .max_width(420.0)
        .vertical_alignment(VerticalAlignment::Center)
        .horizontal_alignment(HorizontalAlignment::Center)
        .children(lines)
        .into()
}

/// How many items there are and how many are selected; the drive's free space.
fn status(input: &PaneInput) -> Border {
    let tab = &input.pane.tab;
    let items = |count: u32| if count == 1 { "item" } else { "items" };
    let counts = if tab.selected_count == 0 {
        format!("{} {}", tab.total_rows, items(tab.total_rows))
    } else {
        format!(
            "{} of {} {} selected",
            tab.selected_count,
            tab.total_rows,
            items(tab.total_rows)
        )
    };
    let small = |text: String| {
        TextBlock::new()
            .text(text)
            .font_size(12.0)
            .opacity(0.7)
            .text_trimming(TextTrimming::CharacterEllipsis)
            .vertical_alignment(VerticalAlignment::Center)
    };
    let drive: View = tab
        .drive_status
        .as_ref()
        .filter(|drive| !drive.free.is_empty())
        .map_or_else(
            || Border::new().into(),
            |drive| small(drive.free.clone()).tooltip(&drive.detail),
        );
    // The status line is the surface's bottom band, under a line.
    Border::new()
        .border_brush(theme::stroke())
        .border_thickness(Thickness::new(0.0, 1.0, 0.0, 0.0))
        .padding(Thickness::new(12.0, 5.0, 12.0, 6.0))
        .content(
            Grid::new()
                .columns([GridLength::STAR, GridLength::Auto])
                .children((small(counts), Border::new().grid_column(1).content(drive))),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tab_dragged_is_told_from_the_ones_it_passed() {
        // Tab 1 dragged to the end: 2 and 3 only shifted.
        assert_eq!(moved(&[1, 2, 3], &[2, 3, 1]), Some((1, 2, 2)));
        // Tab 3 dragged to the front.
        assert_eq!(moved(&[1, 2, 3], &[3, 1, 2]), Some((3, 0, 1)));
        assert_eq!(moved(&[1, 2, 3], &[1, 2, 3]), None);
        assert_eq!(moved(&[1, 2], &[1]), None);
    }

    #[test]
    fn neighbours_swapping_name_either_one() {
        let (tab, index, beside) = moved(&[1, 2], &[2, 1]).expect("one of them moved");
        assert!(tab != beside && index < 2);
    }
}
