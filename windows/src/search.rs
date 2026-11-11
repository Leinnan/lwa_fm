//! Searching the folder a tab shows: the box to type in, the filters set so
//! far, and the menu of ways to search.
use std::rc::Rc;

use dirfleet_core::app::{Event, FilterGroup, SearchScope, SearchView, TabViewModel};
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, Color, ElementRef, Key, KeyEventInfo, Menu,
    MenuExt as _, MenuItem, Orientation, RoutedCallback, StackPanel, TextBlock, TextBox, Thickness,
    TooltipExt as _, VerticalAlignment, View, VirtualKey,
};

use crate::core_link::CoreLink;
use crate::files::glyph;

/// The width of the box.
pub const WIDTH: f64 = 220.0;
/// How deep "subfolders" can look, in levels below the folder.
const DEPTHS: std::ops::RangeInclusive<u32> = 2..=7;

/// What a choice in the search menu asks of the core.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    Case,
    Any,
    Depth(u32),
    Favorites,
    Filter(String),
}

impl Choice {
    fn key(&self) -> Key {
        match self {
            Self::Case => Key::from("case"),
            Self::Any => Key::from("any"),
            Self::Depth(depth) => Key::from(format!("depth-{depth}")),
            Self::Favorites => Key::from("favorites"),
            Self::Filter(token) => Key::from(format!("filter-{token}")),
        }
    }

    /// Every choice the menu offers, to find the one a key names.
    fn all(filters: &[FilterGroup]) -> Vec<Self> {
        let mut all = vec![Self::Case, Self::Any, Self::Depth(1), Self::Favorites];
        all.extend(DEPTHS.map(Self::Depth));
        all.extend(
            filters
                .iter()
                .flat_map(|group| &group.choices)
                .map(|choice| Self::Filter(choice.token.clone())),
        );
        all
    }

    /// The choice the menu item `key` stands for.
    pub fn of(key: &Key, filters: &[FilterGroup]) -> Option<Self> {
        Self::all(filters)
            .into_iter()
            .find(|choice| choice.key() == *key)
    }

    /// What the core is told, given how the tab searches now.
    pub fn event(self, tab: u32, search: Option<&SearchView>) -> Event {
        match self {
            Self::Case => Event::SetSearchCase {
                tab,
                on: !search.is_some_and(|search| search.case_sensitive),
            },
            Self::Any => Event::SetSearchAny {
                tab,
                any: !search.is_some_and(|search| search.any),
            },
            Self::Depth(depth) => Event::SetSearchDepth { tab, depth },
            Self::Favorites => Event::SetSearchFavorites {
                tab,
                on: !search.is_some_and(|search| search.scope == SearchScope::Favorites),
            },
            Self::Filter(token) => Event::ToggleSearchFilter { tab, token },
        }
    }
}

/// A menu label with a mark when the choice is on.
fn marked(label: &str, on: bool) -> String {
    format!("{}{label}", if on { "✓ " } else { "    " })
}

/// The menu of ways to search.
pub fn options(
    search: Option<&SearchView>,
    filters: &[FilterGroup],
    on_chosen: Callback<Key>,
) -> Menu {
    let depth = search.map_or(1, |search| search.depth);
    let mut items = vec![
        MenuItem::item(
            Choice::Case.key(),
            marked(
                "Match Case",
                search.is_some_and(|search| search.case_sensitive),
            ),
        ),
        MenuItem::item(
            Choice::Any.key(),
            marked("Match Any Word", search.is_some_and(|search| search.any)),
        ),
        MenuItem::separator("look-in"),
        MenuItem::item(Choice::Depth(1).key(), marked("This Folder", depth <= 1)),
        MenuItem::submenu(
            "subfolders",
            marked("Subfolders", depth > 1),
            DEPTHS.map(|levels| {
                MenuItem::item(
                    Choice::Depth(levels).key(),
                    marked(&format!("{levels} levels"), depth == levels),
                )
            }),
        ),
        MenuItem::item(
            Choice::Favorites.key(),
            marked(
                "Favorites",
                search.is_some_and(|search| search.scope == SearchScope::Favorites),
            ),
        ),
    ];
    if !filters.is_empty() {
        items.push(MenuItem::separator("filters"));
    }
    items.extend(filters.iter().zip(0u32..).map(|(group, position)| {
        MenuItem::submenu(
            format!("group-{position}"),
            group.label.clone(),
            group.choices.iter().map(|choice| {
                let on = search.is_some_and(|search| search.filters_on.contains(&choice.token));
                MenuItem::item(
                    Choice::Filter(choice.token.clone()).key(),
                    marked(&choice.label, on),
                )
            }),
        )
    }));
    Menu::new(items, on_chosen)
}

/// The box the search is typed in, with the menu of ways to search beside it.
/// Enter turns the words typed into filters; Escape steps out of the search.
pub fn search_box(
    core: &CoreLink,
    tab: &TabViewModel,
    field: &ElementRef<TextBox>,
    filters: &Rc<Vec<FilterGroup>>,
    on_option: Callback<Key>,
    on_left: &Callback<()>,
) -> View {
    let id = tab.id;
    let typed = {
        let core = core.clone();
        Callback::new(move |text: Rc<str>| {
            core.send(Event::SetSearchText {
                tab: id,
                text: text.to_string(),
            });
        })
    };
    let keys = {
        let core = core.clone();
        let on_left = on_left.clone();
        RoutedCallback::new(move |info: KeyEventInfo| match info.key {
            VirtualKey::ENTER => {
                core.send(Event::CommitSearchText { tab: id });
                true
            }
            VirtualKey::ESCAPE => {
                core.send(Event::SearchEscape { tab: id });
                on_left.call(());
                true
            }
            _ => false,
        })
    };
    let text = tab
        .search
        .as_ref()
        .map_or("", |search| search.text.as_str());
    StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(4.0)
        .children((
            Border::new().on_preview_key_down(keys).content(
                TextBox::new()
                    .element_ref(field)
                    .text(text)
                    .placeholder_text("Search")
                    .automation_name("Search")
                    .width(WIDTH)
                    .on_text_changed(typed),
            ),
            Button::new()
                .style(ButtonStyle::Subtle)
                .automation_name("Search options")
                .content(glyph("\u{E71C}", 13.0))
                .menu(options(tab.search.as_ref(), filters, on_option)),
        ))
        .into()
}

/// The filters set so far, each removed by pressing it; and what a search
/// through subfolders is doing, with a way to stop it.
pub fn chips(core: &CoreLink, tab: &TabViewModel) -> Option<View> {
    let search = tab.search.as_ref()?;
    if search.chips.is_empty() && !search.searching {
        return None;
    }
    let id = tab.id;
    let mut parts: Vec<View> = search
        .chips
        .iter()
        .zip(0u32..)
        .map(|(chip, index)| {
            let core = core.clone();
            let label = TextBlock::new()
                .text(format!("{}  ✕", chip.token))
                .font_size(12.0);
            let button = Button::new()
                .automation_name(format!("Remove filter {}", chip.token))
                .content(match &chip.error {
                    Some(_) => label.foreground(Color::rgb(0xE0, 0x50, 0x50)),
                    None => label,
                })
                .on_click(move || core.send(Event::RemoveSearchChip { tab: id, index }));
            match &chip.error {
                Some(error) => button.tooltip(error),
                None => button.into(),
            }
        })
        .collect();
    if search.searching {
        let core = core.clone();
        parts.push(
            Button::new()
                .content("Stop")
                .on_click(move || core.send(Event::StopSearch { tab: id }))
                .into(),
        );
    }
    Some(
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(6.0)
            .margin(Thickness::new(12.0, 0.0, 12.0, 6.0))
            .vertical_alignment(VerticalAlignment::Center)
            .children(parts)
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use dirfleet_core::app::FilterChoice;

    fn filters() -> Vec<FilterGroup> {
        vec![FilterGroup {
            label: "Type".to_owned(),
            choices: vec![FilterChoice {
                label: "Images".to_owned(),
                token: "type:image".to_owned(),
            }],
        }]
    }

    #[test]
    fn a_menu_key_names_its_choice() {
        let filters = filters();
        for choice in Choice::all(&filters) {
            assert_eq!(Choice::of(&choice.key(), &filters), Some(choice));
        }
        assert_eq!(Choice::of(&Key::from("nothing"), &filters), None);
    }

    #[test]
    fn switches_turn_what_is_on_off_and_back() {
        assert_eq!(
            Choice::Case.event(4, None),
            Event::SetSearchCase { tab: 4, on: true }
        );
        assert_eq!(
            Choice::Depth(3).event(4, None),
            Event::SetSearchDepth { tab: 4, depth: 3 }
        );
        assert_eq!(
            Choice::Filter("type:image".to_owned()).event(4, None),
            Event::ToggleSearchFilter {
                tab: 4,
                token: "type:image".to_owned()
            }
        );
    }
}
