//! Runs the core's commands: from the keyboard, a menu or the palette, a
//! command is the same thing. Most are one event to the core; the few that
//! need something of the window come back for it to do.
use dirfleet_core::app::{CommandId, Event};
use dirfleet_core::view_settings::DisplayType;

use crate::core_link::CoreLink;

/// What a command acts on: the window and the tab that has the focus.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "facts about the tab, one flag each"
)]
pub struct Target {
    pub window: u32,
    pub tab: u32,
    /// The folder the tab shows; "" when it shows none (the Recycle Bin, a search).
    pub path: String,
    pub show_hidden: bool,
    /// The tab shows the Recycle Bin.
    pub in_trash: bool,
    /// The tab shows a place where things can be removed but that has no
    /// Recycle Bin: a server.
    pub no_bin: bool,
    /// How many of the tab's entries are selected.
    pub selected: u32,
    pub inspector_shown: bool,
    /// The settings ask before anything goes to the Recycle Bin.
    pub confirm_trash: bool,
    /// The user's home folder.
    pub home: String,
}

/// A command only the window can carry out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Local {
    Find,
    Settings,
    ToggleSidebar,
    QuickLook,
    SaveSearch,
    ShortcutHelp,
    /// Asks first when the settings say so.
    MoveToTrash,
    /// Always asks first.
    DeleteImmediately,
    /// Always asks first.
    EmptyTrash,
}

/// Runs `command` on `target`; a command the window must carry out comes back.
pub fn run(core: &CoreLink, command: CommandId, target: &Target) -> Option<Local> {
    let (window, tab) = (target.window, target.tab);
    let event = match command {
        CommandId::Back => Event::Back { tab },
        CommandId::Forward => Event::Forward { tab },
        CommandId::Up => Event::Up { tab },
        CommandId::Refresh => Event::Refresh { tab },
        CommandId::GoToFolder => Event::OpenGoTo { tab },
        CommandId::Palette => Event::SetPaletteText {
            text: String::new(),
        },
        CommandId::ToggleInspector => Event::SetInspector {
            shown: !target.inspector_shown,
        },
        CommandId::ToggleHidden => Event::SetShowHidden {
            tab,
            show: !target.show_hidden,
        },
        CommandId::ViewList => Event::SetDisplay {
            tab,
            display: DisplayType::List,
        },
        CommandId::ViewIcons => Event::SetDisplay {
            tab,
            display: DisplayType::Icons,
        },
        CommandId::NewTab => Event::NewTab {
            window,
            path: Some(target.folder().to_owned()),
        },
        CommandId::NewWindow => Event::NewWindow {
            path: Some(target.folder().to_owned()),
        },
        CommandId::CloseTab => Event::CloseTab { tab },
        CommandId::ReopenClosedTab => Event::ReopenClosedTab { window },
        CommandId::NewFolder => Event::NewFolder { tab },
        CommandId::NewTextFile => Event::NewTextFile { tab },
        CommandId::Rename => Event::StartRename { tab },
        CommandId::Duplicate => Event::Duplicate { tab },
        CommandId::Undo => Event::Undo,
        CommandId::Redo => Event::Redo,
        CommandId::Cut => Event::CutSelection { tab },
        CommandId::Copy => Event::CopySelection { tab },
        CommandId::Paste => Event::Paste { tab },
        CommandId::CopyAsPath => Event::CopyPaths { tab },
        CommandId::SelectAll => Event::SelectAll { tab },
        CommandId::InvertSelection => Event::InvertSelection { tab },
        CommandId::ShowInFinder => Event::RevealSelection { tab },
        CommandId::GetInfo => Event::GetInfo { tab },
        CommandId::OpenInTerminal => Event::OpenInTerminal { tab },
        CommandId::OpenTrash => Event::OpenTrash { tab },
        CommandId::AddToFavorites => {
            if target.path.is_empty() {
                return None;
            }
            Event::AddFavorite {
                path: target.path.clone(),
            }
        }
        CommandId::Find => return Some(Local::Find),
        CommandId::Settings => return Some(Local::Settings),
        CommandId::ToggleSidebar => return Some(Local::ToggleSidebar),
        CommandId::QuickLook => return Some(Local::QuickLook),
        CommandId::SaveSearch => return Some(Local::SaveSearch),
        CommandId::ShortcutHelp => return Some(Local::ShortcutHelp),
        CommandId::MoveToTrash => return Some(Local::MoveToTrash),
        CommandId::DeleteImmediately => return Some(Local::DeleteImmediately),
        CommandId::EmptyTrash => return Some(Local::EmptyTrash),
    };
    core.send(event);
    None
}

impl Target {
    /// Where a new tab or window opens: the tab's folder, or home when it shows none.
    fn folder(&self) -> &str {
        if self.path.is_empty() {
            &self.home
        } else {
            &self.path
        }
    }
}
