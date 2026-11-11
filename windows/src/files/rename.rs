//! Renaming an entry where it stands: a text box in place of its name, with
//! the name's stem selected so typing replaces it and keeps the extension.
use std::rc::Rc;
use std::time::Duration;

use dirfleet_core::app::{Event, RenameView};
use windows_reactor::{
    Border, Color, Component, ComponentContext, ComponentTimer, ElementRef, FocusEventInfo,
    KeyEventInfo, RoutedCallback, TextBox, Thickness, TooltipExt as _, VerticalAlignment, View,
    ViewContext, VirtualKey,
};

use crate::core_link::CoreLink;

/// A new text box refuses the focus until it is laid out, so it is asked
/// again this often, this many times.
const FOCUS_RETRY: Duration = Duration::from_millis(40);
const FOCUS_TRIES: u8 = 10;

#[derive(Clone, PartialEq)]
pub struct RenameInput {
    pub core: CoreLink,
    pub tab: u32,
    pub rename: Rc<RenameView>,
    /// A folder's whole name is selected; a file's only up to its extension.
    pub is_dir: bool,
}

pub struct RenameBox {
    core: CoreLink,
    tab: u32,
    /// The name as typed so far.
    draft: String,
    /// The name the entry had.
    original: String,
    is_dir: bool,
    field: ElementRef<TextBox>,
    /// Enter or Escape answered already; losing the focus then says nothing more.
    answered: bool,
    on_key: RoutedCallback<KeyEventInfo>,
    /// Dropping it cancels the next try at taking the focus.
    focus_timer: Option<ComponentTimer>,
}

#[derive(Clone, Debug)]
pub enum RenameMessage {
    Typed(Rc<str>),
    Commit,
    Cancel,
    LostFocus,
    /// Time to ask for the focus: the given try.
    TakeFocus(u8),
    Focused(bool, u8),
}

/// How many UTF-16 units of `name` to select: all of a folder's name, a
/// file's up to its last dot. A leading dot is part of the name, not an extension.
pub fn stem_length(name: &str, is_dir: bool) -> u32 {
    let stem = if is_dir {
        name
    } else {
        match name.rfind('.') {
            Some(dot) if dot > 0 => &name[..dot],
            _ => name,
        }
    };
    stem.encode_utf16().count() as u32
}

impl RenameBox {
    fn take_focus(&mut self, attempt: u8, context: &ComponentContext<Self>) {
        self.focus_timer =
            Some(context.set_local_timeout(FOCUS_RETRY, move || RenameMessage::TakeFocus(attempt)));
    }
}

impl Component for RenameBox {
    type Input = RenameInput;
    type Message = RenameMessage;

    fn create(input: &RenameInput, context: &ComponentContext<Self>) -> Self {
        let sender = context.sender();
        let mut rename = Self {
            core: input.core.clone(),
            tab: input.tab,
            draft: input.rename.name.clone(),
            original: input.rename.name.clone(),
            is_dir: input.is_dir,
            field: ElementRef::new(),
            answered: false,
            on_key: RoutedCallback::new(move |info: KeyEventInfo| match info.key {
                VirtualKey::ENTER => sender.send(RenameMessage::Commit),
                VirtualKey::ESCAPE => sender.send(RenameMessage::Cancel),
                _ => false,
            }),
            focus_timer: None,
        };
        rename.take_focus(0, context);
        rename
    }

    fn input_changed(&mut self, input: &RenameInput, _: &ComponentContext<Self>) {
        // A name the core refused comes back with the reason: typing goes on.
        if input.rename.error.is_some() {
            self.answered = false;
        }
    }

    fn update(&mut self, message: RenameMessage, context: &ComponentContext<Self>) {
        let tab = self.tab;
        match message {
            RenameMessage::Typed(text) => self.draft = text.to_string(),
            RenameMessage::Commit => {
                self.answered = true;
                self.core.send(Event::Rename {
                    tab,
                    name: self.draft.clone(),
                });
            }
            RenameMessage::Cancel => {
                self.answered = true;
                self.core.send(Event::CancelRename { tab });
            }
            RenameMessage::LostFocus => {
                if !self.answered {
                    self.answered = true;
                    // Clicking away keeps what was typed, as Explorer does.
                    self.core.send(if self.draft == self.original {
                        Event::CancelRename { tab }
                    } else {
                        Event::Rename {
                            tab,
                            name: self.draft.clone(),
                        }
                    });
                }
            }
            RenameMessage::TakeFocus(attempt) => {
                let sender = context.sender();
                _ = self.field.request_focus_result(move |result| {
                    _ = sender.send(RenameMessage::Focused(result == Ok(true), attempt));
                });
            }
            RenameMessage::Focused(true, _) => {
                self.focus_timer = None;
                _ = self
                    .field
                    .request_select(0, stem_length(&self.original, self.is_dir), |_| {});
            }
            RenameMessage::Focused(false, attempt) => {
                self.focus_timer = None;
                if attempt < FOCUS_TRIES {
                    self.take_focus(attempt + 1, context);
                }
            }
        }
    }

    fn view(&self, input: &RenameInput, context: &mut ViewContext<Self>) -> View {
        let refused = input.rename.error.as_deref();
        let field = TextBox::new()
            .element_ref(&self.field)
            .text(self.draft.clone())
            .automation_name("Name")
            .on_text_changed(context.callback(RenameMessage::Typed));
        let frame = Border::new()
            .vertical_alignment(VerticalAlignment::Center)
            .border_thickness(Thickness::uniform(if refused.is_some() {
                1.0
            } else {
                0.0
            }))
            .border_brush(Color::rgb(0xE0, 0x50, 0x50))
            .corner_radius(4.0)
            .on_preview_key_down(self.on_key.clone())
            .on_lost_focus(context.callback(|_: FocusEventInfo| RenameMessage::LostFocus))
            .content(field);
        match refused {
            Some(reason) => frame.tooltip(reason),
            None => frame.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_files_name_is_selected_up_to_its_extension() {
        assert_eq!(stem_length("notes.txt", false), 5);
        assert_eq!(stem_length("archive.tar.gz", false), 11);
        assert_eq!(stem_length("README", false), 6);
    }

    #[test]
    fn a_leading_dot_is_not_an_extension() {
        assert_eq!(stem_length(".gitignore", false), 10);
    }

    #[test]
    fn a_folders_whole_name_is_selected() {
        assert_eq!(stem_length("my.folder", true), 9);
    }

    #[test]
    fn the_length_counts_utf16_units() {
        // One emoji is two units; the selection API counts in those.
        assert_eq!(stem_length("😀.png", false), 2);
    }
}
