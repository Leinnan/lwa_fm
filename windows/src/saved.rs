//! Saved searches: the name asked for when one is saved, the editor of one
//! already saved, and the mark a tab shows while its search is one of them.
use std::cell::RefCell;
use std::rc::Rc;

use dirfleet_core::app::{Event, SavedSearchView, SearchView, TabViewModel};
use windows_reactor::{
    Border, Button, ComboBox, Grid, GridLength, Orientation, StackPanel, TextBlock, TextBox,
    TextWrapping, VerticalAlignment, View,
};

use crate::core_link::CoreLink;
use crate::files::glyph;
use crate::theme;

/// How deep a saved search can look below its folder.
const DEEPEST: u32 = 7;

/// A saved search while it is named or edited. The dialog's fields write what
/// is typed into it, and the window reads it when the dialog is confirmed, so
/// typing draws nothing again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchDraft {
    /// The saved search edited; `None` while a tab's search is being named.
    pub id: Option<u64>,
    pub name: String,
    pub text: String,
    pub case_sensitive: bool,
    pub depth: u32,
    pub any: bool,
    pub favorites: bool,
}

/// A draft the window and its dialog share.
pub type Draft = Rc<RefCell<SearchDraft>>;

impl SearchDraft {
    /// The draft that names a tab's search: what was typed, or its first
    /// filter. `None` when there is nothing to save.
    pub fn naming(search: &SearchView) -> Option<Self> {
        let name = if search.text.trim().is_empty() {
            search.chips.first()?.token.clone()
        } else {
            search.text.trim().to_owned()
        };
        Some(Self {
            name,
            ..Self::default()
        })
    }

    /// The draft that edits a saved search.
    pub fn editing(saved: &SavedSearchView) -> Self {
        Self {
            id: Some(saved.id),
            name: saved.name.clone(),
            text: saved.text.clone(),
            case_sensitive: saved.case_sensitive,
            depth: saved.depth,
            any: saved.any,
            favorites: saved.favorites,
        }
    }

    /// What the core is told when the dialog is confirmed: save `tab`'s
    /// search under the name, or change the saved search edited. Nothing for
    /// an empty name.
    pub fn confirmed(&self, tab: u32) -> Option<Event> {
        let name = self.name.trim();
        if name.is_empty() {
            return None;
        }
        let name = name.to_owned();
        Some(self.id.map_or_else(
            || Event::SaveSearch {
                tab,
                name: name.clone(),
            },
            |id| Event::EditSavedSearch {
                id,
                name: name.clone(),
                text: self.text.clone(),
                case_sensitive: self.case_sensitive,
                depth: self.depth.clamp(1, DEEPEST),
                any: self.any,
                favorites: self.favorites,
            },
        ))
    }
}

/// A box that shows one text of the draft and writes what is typed back.
fn text_field(draft: &Draft, name: &str, set: fn(&mut SearchDraft, String)) -> TextBox {
    let shared = Rc::clone(draft);
    TextBox::new()
        .automation_name(name)
        .on_text_changed(move |text: Rc<str>| set(&mut shared.borrow_mut(), text.to_string()))
}

/// A line of the editor: what it is, and the control that sets it.
fn line(label: &str, control: impl Into<View>) -> View {
    Grid::new()
        .columns([GridLength::Pixel(130.0), GridLength::STAR])
        .column_spacing(10.0)
        .children((
            TextBlock::new()
                .text(label)
                .vertical_alignment(VerticalAlignment::Center),
            Border::new().grid_column(1).content(control),
        ))
        .into()
}

/// What the Save Search dialog shows: the name to save under.
pub fn naming(draft: &Draft) -> View {
    let name = draft.borrow().name.clone();
    StackPanel::new()
        .orientation(Orientation::Vertical)
        .spacing(10.0)
        .children((
            TextBlock::new()
                .text("The search is kept in the sidebar under this name.")
                .text_wrapping(TextWrapping::Wrap),
            text_field(draft, "Name", |draft, text| draft.name = text).text(name),
        ))
        .into()
}

/// What the editor of a saved search shows.
pub fn editor(draft: &Draft) -> View {
    let now = draft.borrow().clone();
    let switch = |label: &str, on: bool, set: fn(&mut SearchDraft, bool)| {
        let shared = Rc::clone(draft);
        line(
            label,
            theme::switch(on)
                .automation_name(label)
                .on_toggled(move |on: bool| set(&mut shared.borrow_mut(), on)),
        )
    };
    let depths = {
        let shared = Rc::clone(draft);
        let chosen = now.depth.clamp(1, DEEPEST) as usize - 1;
        ComboBox::new()
            .automation_name("Look in")
            .items_source(
                std::iter::once("This folder".to_owned())
                    .chain((2..=DEEPEST).map(|levels| format!("Subfolders, {levels} levels"))),
            )
            .selected_index(Some(chosen))
            .on_selection_changed(move |index: Option<usize>| {
                if let Some(index) = index {
                    shared.borrow_mut().depth = index as u32 + 1;
                }
            })
    };
    StackPanel::new()
        .orientation(Orientation::Vertical)
        .spacing(10.0)
        .width(420.0)
        .children((
            line(
                "Name",
                text_field(draft, "Name", |draft, text| draft.name = text).text(now.name),
            ),
            line(
                "Search for",
                text_field(draft, "Search for", |draft, text| draft.text = text)
                    .text(now.text)
                    .placeholder_text("report ext:pdf"),
            ),
            line("Look in", depths),
            switch("Match case", now.case_sensitive, |draft, on| {
                draft.case_sensitive = on;
            }),
            switch("Match any word", now.any, |draft, on| draft.any = on),
            switch("Search the favorites", now.favorites, |draft, on| {
                draft.favorites = on;
            }),
        ))
        .into()
}

/// The saved search a tab's search came from, with Update and Revert once
/// the search was changed.
pub fn badge(core: &CoreLink, tab: &TabViewModel) -> Vec<View> {
    let Some(saved) = &tab.saved else {
        return Vec::new();
    };
    let id = tab.id;
    let mut parts: Vec<View> = vec![
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(6.0)
            .vertical_alignment(VerticalAlignment::Center)
            .children((
                glyph("\u{E721}", 12.0).opacity(0.7),
                TextBlock::new()
                    .text(if saved.modified {
                        format!("{} (changed)", saved.name)
                    } else {
                        saved.name.clone()
                    })
                    .font_size(12.0)
                    .opacity(0.8)
                    .vertical_alignment(VerticalAlignment::Center),
            ))
            .into(),
    ];
    if saved.modified {
        let update = core.clone();
        let revert = core.clone();
        parts.push(
            Button::new()
                .content("Update")
                .automation_name("Update Saved Search")
                .on_click(move || update.send(Event::UpdateSavedSearch { tab: id }))
                .into(),
        );
        parts.push(
            Button::new()
                .content("Revert")
                .automation_name("Revert to Saved Search")
                .on_click(move || revert.send(Event::RevertSearch { tab: id }))
                .into(),
        );
    }
    parts
}

/// Whether a tab's search can be saved: it has words or filters.
pub fn can_save(tab: &TabViewModel) -> bool {
    tab.search
        .as_ref()
        .is_some_and(|search| !search.text.trim().is_empty() || !search.chips.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved() -> SavedSearchView {
        SavedSearchView {
            id: 7,
            name: "Reports".to_owned(),
            summary: String::new(),
            text: "report ext:pdf".to_owned(),
            case_sensitive: true,
            depth: 3,
            any: false,
            favorites: true,
        }
    }

    #[test]
    fn a_new_search_is_saved_under_its_name_and_an_old_one_changed() {
        let naming = SearchDraft {
            name: "  Reports ".to_owned(),
            ..SearchDraft::default()
        };
        assert_eq!(
            naming.confirmed(4),
            Some(Event::SaveSearch {
                tab: 4,
                name: "Reports".to_owned()
            })
        );
        let mut editing = SearchDraft::editing(&saved());
        editing.text = "invoice".to_owned();
        assert_eq!(
            editing.confirmed(4),
            Some(Event::EditSavedSearch {
                id: 7,
                name: "Reports".to_owned(),
                text: "invoice".to_owned(),
                case_sensitive: true,
                depth: 3,
                any: false,
                favorites: true,
            })
        );
    }

    #[test]
    fn a_search_without_a_name_is_not_saved() {
        let unnamed = SearchDraft {
            name: "   ".to_owned(),
            ..SearchDraft::editing(&saved())
        };
        assert_eq!(unnamed.confirmed(4), None);
    }
}
