//! Commands as data, and the command palette, as egui's: every command with
//! its label and the shortcut the platform uses (no "Ctrl" on macOS), and a
//! palette that matches what is typed against the commands, favorites,
//! recent folders, saved searches and drives. Shells run what is chosen.
use std::cmp::Reverse;

use facet::Facet;
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use serde::{Deserialize, Serialize};

use super::keymap::{KeyView, Keymap};
use super::{Model, title_of};
use crate::shortcut::{Shortcut, parse_shown};

/// Items the palette shows at most.
const SHOWN: usize = 40;

#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(C)]
pub enum CommandId {
    #[default]
    Back,
    Forward,
    Up,
    Refresh,
    Find,
    GoToFolder,
    Palette,
    Settings,
    ToggleSidebar,
    ToggleInspector,
    ToggleHidden,
    ViewList,
    ViewIcons,
    NewTab,
    NewWindow,
    CloseTab,
    ReopenClosedTab,
    NewFolder,
    NewTextFile,
    Rename,
    Duplicate,
    MoveToTrash,
    DeleteImmediately,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    CopyAsPath,
    SelectAll,
    InvertSelection,
    QuickLook,
    ShowInFinder,
    GetInfo,
    OpenInTerminal,
    SaveSearch,
    OpenTrash,
    EmptyTrash,
    AddToFavorites,
    ShortcutHelp,
    CopyToOtherPane,
    MoveToOtherPane,
    SwapPanes,
    MirrorPane,
    SelectByPattern,
    SelectSameExtension,
    FindDuplicates,
    CompareFolders,
}

impl CommandId {
    pub const ALL: [Self; 48] = [
        Self::Back,
        Self::Forward,
        Self::Up,
        Self::Refresh,
        Self::Find,
        Self::GoToFolder,
        Self::Palette,
        Self::Settings,
        Self::ToggleSidebar,
        Self::ToggleInspector,
        Self::ToggleHidden,
        Self::ViewList,
        Self::ViewIcons,
        Self::NewTab,
        Self::NewWindow,
        Self::CloseTab,
        Self::ReopenClosedTab,
        Self::NewFolder,
        Self::NewTextFile,
        Self::Rename,
        Self::Duplicate,
        Self::MoveToTrash,
        Self::DeleteImmediately,
        Self::Undo,
        Self::Redo,
        Self::Cut,
        Self::Copy,
        Self::Paste,
        Self::CopyAsPath,
        Self::SelectAll,
        Self::InvertSelection,
        Self::QuickLook,
        Self::ShowInFinder,
        Self::GetInfo,
        Self::OpenInTerminal,
        Self::SaveSearch,
        Self::OpenTrash,
        Self::EmptyTrash,
        Self::AddToFavorites,
        Self::ShortcutHelp,
        Self::CopyToOtherPane,
        Self::MoveToOtherPane,
        Self::SwapPanes,
        Self::MirrorPane,
        Self::SelectByPattern,
        Self::SelectSameExtension,
        Self::FindDuplicates,
        Self::CompareFolders,
    ];

    /// The command's name, in the words of the system it runs on.
    pub const fn label(self) -> &'static str {
        if cfg!(windows)
            && let Some(label) = self.windows_label()
        {
            return label;
        }
        match self {
            Self::Back => "Back",
            Self::Forward => "Forward",
            Self::Up => "Enclosing Folder",
            Self::Refresh => "Refresh",
            Self::Find => "Find",
            Self::GoToFolder => "Go to Folder…",
            Self::Palette => "Commands…",
            Self::Settings => "Settings…",
            Self::ToggleSidebar => "Show or Hide Sidebar",
            Self::ToggleInspector => "Show or Hide Inspector",
            Self::ToggleHidden => "Show or Hide Hidden Items",
            Self::ViewList => "View as List",
            Self::ViewIcons => "View as Icons",
            Self::NewTab => "New Tab",
            Self::NewWindow => "New Window",
            Self::CloseTab => "Close Tab",
            Self::ReopenClosedTab => "Reopen Closed Tab",
            Self::NewFolder => "New Folder",
            Self::NewTextFile => "New Text Document",
            Self::Rename => "Rename",
            Self::Duplicate => "Duplicate",
            Self::MoveToTrash => "Move to Trash",
            Self::DeleteImmediately => "Delete Immediately…",
            Self::Undo => "Undo",
            Self::Redo => "Redo",
            Self::Cut => "Cut",
            Self::Copy => "Copy",
            Self::Paste => "Paste",
            Self::CopyAsPath => "Copy as Pathname",
            Self::SelectAll => "Select All",
            Self::InvertSelection => "Invert Selection",
            Self::QuickLook => "Quick Look",
            Self::ShowInFinder => "Show in Finder",
            Self::GetInfo => "Get Info",
            Self::OpenInTerminal => "Open in Terminal",
            Self::SaveSearch => "Save Search…",
            Self::OpenTrash => "Go to Trash",
            Self::EmptyTrash => "Empty Trash…",
            Self::AddToFavorites => "Add to Favorites",
            Self::ShortcutHelp => "Keyboard Shortcuts",
            Self::CopyToOtherPane => "Copy to Other Pane",
            Self::MoveToOtherPane => "Move to Other Pane",
            Self::SwapPanes => "Swap Panes",
            Self::MirrorPane => "Show This Folder in Other Pane",
            Self::SelectByPattern => "Select by Pattern…",
            Self::SelectSameExtension => "Select Same Extension",
            Self::FindDuplicates => "Find Duplicates",
            Self::CompareFolders => "Compare Folders",
        }
    }

    /// The names Windows has its own words for.
    const fn windows_label(self) -> Option<&'static str> {
        Some(match self {
            Self::Up => "Up",
            Self::MoveToTrash => "Move to Recycle Bin",
            Self::CopyAsPath => "Copy as Path",
            Self::QuickLook => "Preview",
            Self::ShowInFinder => "Show in Explorer",
            Self::GetInfo => "Properties",
            Self::OpenTrash => "Go to Recycle Bin",
            Self::EmptyTrash => "Empty Recycle Bin…",
            _ => return None,
        })
    }

    /// The shortcut on macOS, as its menus show it.
    pub const fn mac_shortcut(self) -> &'static str {
        match self {
            Self::Back => "⌘[",
            Self::Forward => "⌘]",
            Self::Up => "⌘↑",
            Self::Refresh => "⌘R",
            Self::Find => "⌘F",
            Self::GoToFolder => "⇧⌘G",
            Self::Palette => "⇧⌘P",
            Self::Settings => "⌘,",
            Self::ToggleSidebar => "⌃⌘S",
            Self::ToggleInspector => "⌥⌘I",
            Self::ToggleHidden => "⇧⌘.",
            Self::ViewList => "⌘2",
            Self::ViewIcons => "⌘1",
            Self::NewTab => "⌘T",
            Self::NewWindow => "⌘N",
            Self::CloseTab => "⌘W",
            Self::ReopenClosedTab => "⇧⌘T",
            Self::NewFolder => "⇧⌘N",
            Self::Duplicate => "⌘D",
            Self::MoveToTrash => "⌘⌫",
            Self::DeleteImmediately => "⌥⌘⌫",
            Self::Undo => "⌘Z",
            Self::Redo => "⇧⌘Z",
            Self::Cut => "⌘X",
            Self::Copy => "⌘C",
            Self::Paste => "⌘V",
            Self::CopyAsPath => "⌥⌘C",
            Self::SelectAll => "⌘A",
            Self::InvertSelection => "⇧⌘I",
            Self::QuickLook => "Space",
            Self::ShowInFinder => "⌥⌘R",
            Self::GetInfo => "⌘I",
            Self::SaveSearch => "⌘S",
            Self::EmptyTrash => "⇧⌘⌫",
            Self::AddToFavorites => "⌃⌘T",
            Self::ShortcutHelp => "⌘/",
            Self::Rename => "↩",
            // The Mac's keys for these are chosen with its menus.
            Self::NewTextFile
            | Self::OpenInTerminal
            | Self::OpenTrash
            | Self::CopyToOtherPane
            | Self::MoveToOtherPane
            | Self::SwapPanes
            | Self::MirrorPane
            | Self::SelectByPattern
            | Self::SelectSameExtension
            | Self::FindDuplicates
            | Self::CompareFolders => "",
        }
    }

    /// The shortcut elsewhere, as egui's keymap has it.
    pub const fn other_shortcut(self) -> &'static str {
        match self {
            Self::Back => "Alt+Left",
            Self::Forward => "Alt+Right",
            Self::Up => "Alt+Up",
            Self::Refresh => "F5",
            Self::Find => "Ctrl+F",
            Self::GoToFolder => "Ctrl+L",
            Self::Palette => "Ctrl+Shift+P",
            Self::Settings => "Ctrl+,",
            Self::ToggleSidebar => "Ctrl+Alt+S",
            Self::ToggleInspector => "Ctrl+Alt+I",
            Self::ToggleHidden => "Ctrl+H",
            Self::NewTab => "Ctrl+T",
            Self::NewWindow => "Ctrl+N",
            Self::CloseTab => "Ctrl+W",
            Self::ReopenClosedTab => "Ctrl+Shift+T",
            Self::NewFolder => "Ctrl+Shift+N",
            Self::Duplicate => "Ctrl+D",
            Self::MoveToTrash => "Del",
            Self::DeleteImmediately => "Shift+Del",
            Self::Undo => "Ctrl+Z",
            Self::Redo => "Ctrl+Y",
            Self::Cut => "Ctrl+X",
            Self::Copy => "Ctrl+C",
            Self::Paste => "Ctrl+V",
            Self::CopyAsPath => "Ctrl+Shift+C",
            Self::SelectAll => "Ctrl+A",
            Self::InvertSelection => "Ctrl+Shift+I",
            Self::QuickLook => "Space",
            Self::SaveSearch => "Ctrl+S",
            Self::Rename => "F2",
            Self::ShortcutHelp => "F1",
            // F5 and F6 alone are taken (Refresh) or the system's.
            Self::CopyToOtherPane => "Shift+F5",
            Self::MoveToOtherPane => "Shift+F6",
            Self::SwapPanes => "Ctrl+U",
            Self::SelectByPattern => "Ctrl+Shift+A",
            _ => "",
        }
    }

    /// The shortcut the app gives the command on this system, as it is
    /// shown; the user may have given it another (`CommandView::shortcut`).
    pub const fn shortcut(self) -> &'static str {
        if cfg!(target_os = "macos") {
            self.mac_shortcut()
        } else {
            self.other_shortcut()
        }
    }

    /// That shortcut, read.
    pub fn default_shortcut(self) -> Option<Shortcut> {
        parse_shown(self.shortcut())
    }

    /// The heading the keyboard settings list the command under.
    pub const fn category(self) -> &'static str {
        match self {
            Self::Back
            | Self::Forward
            | Self::Up
            | Self::Refresh
            | Self::GoToFolder
            | Self::OpenTrash
            | Self::MirrorPane => "Go",
            Self::ToggleSidebar
            | Self::ToggleInspector
            | Self::ToggleHidden
            | Self::ViewList
            | Self::ViewIcons
            | Self::QuickLook
            | Self::SwapPanes => "View",
            Self::NewTab | Self::NewWindow | Self::CloseTab | Self::ReopenClosedTab => {
                "Tabs and Windows"
            }
            Self::NewFolder
            | Self::NewTextFile
            | Self::Rename
            | Self::Duplicate
            | Self::MoveToTrash
            | Self::DeleteImmediately
            | Self::CopyToOtherPane
            | Self::MoveToOtherPane
            | Self::ShowInFinder
            | Self::GetInfo
            | Self::OpenInTerminal
            | Self::EmptyTrash
            | Self::AddToFavorites => "Files",
            Self::Undo
            | Self::Redo
            | Self::Cut
            | Self::Copy
            | Self::Paste
            | Self::CopyAsPath
            | Self::SelectAll
            | Self::InvertSelection
            | Self::SelectByPattern
            | Self::SelectSameExtension => "Edit",
            Self::Find | Self::SaveSearch | Self::FindDuplicates | Self::CompareFolders => {
                "Search and Tools"
            }
            Self::Palette | Self::Settings | Self::ShortcutHelp => "App",
        }
    }
}

/// A command with its label and its shortcut, for menus, the palette and
/// the keyboard settings.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct CommandView {
    pub id: CommandId,
    pub label: String,
    /// The shortcut it has now, as the system writes shortcuts; "" for
    /// none.
    pub shortcut: String,
    /// The same, as a key and its modifiers; `None` for none.
    pub key: Option<KeyView>,
    /// The shortcut the app gives it; "" for none.
    pub default_shortcut: String,
    /// The user gave it another, or took it away.
    pub customized: bool,
    /// The heading it is listed under in the keyboard settings.
    pub category: String,
}

pub(super) fn command_views(keymap: &Keymap) -> Vec<CommandView> {
    CommandId::ALL
        .into_iter()
        .map(|id| CommandView {
            id,
            label: id.label().to_owned(),
            shortcut: keymap.shown(id),
            key: keymap.shortcut(id).map(KeyView::from),
            default_shortcut: id.shortcut().to_owned(),
            customized: keymap.customized(id),
            category: id.category().to_owned(),
        })
        .collect()
}

/// What a palette item does when chosen.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum PaletteTarget {
    Command(CommandId),
    Folder(String),
    SavedSearch(u64),
    /// A plugin action, `<plugin id>.<action id>`, on the tab's selection
    /// or folder.
    PluginAction(String),
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PaletteItem {
    pub title: String,
    /// Where it comes from ("Favorite", "Recent", a shortcut).
    pub detail: String,
    pub target: PaletteTarget,
}

/// What the palette offers for `text`: commands first when nothing is
/// typed, otherwise every source ranked by how well it matches.
pub(super) fn palette_items(model: &Model, text: &str) -> Vec<PaletteItem> {
    let mut items: Vec<PaletteItem> = CommandId::ALL
        .into_iter()
        .filter(|id| *id != CommandId::Palette)
        .map(|id| PaletteItem {
            title: id.label().to_owned(),
            detail: model.keymap.shown(id),
            target: PaletteTarget::Command(id),
        })
        .collect();
    let folder = |title: String, detail: &str, path: &str| PaletteItem {
        title,
        detail: detail.to_owned(),
        target: PaletteTarget::Folder(path.to_owned()),
    };
    items.extend(
        model
            .sidebar
            .favorites
            .iter()
            .map(|favorite| folder(favorite.name.clone(), "Favorite", &favorite.path)),
    );
    // The plugin actions of the first window's tab: on its selection, or its
    // folder without one, as egui's palette offers them.
    if let Some(tab) = model
        .windows
        .first()
        .and_then(|window| model.tabs.get(&window.focused))
    {
        items.extend(
            model
                .plugins
                .palette_actions(tab)
                .into_iter()
                .map(|action| PaletteItem {
                    title: action.title,
                    detail: action.plugin,
                    target: PaletteTarget::PluginAction(action.id),
                }),
        );
    }
    items.extend(model.saved.searches.iter().map(|saved| PaletteItem {
        title: saved.name.clone(),
        detail: "Saved search".to_owned(),
        target: PaletteTarget::SavedSearch(saved.id),
    }));
    items.extend(
        model
            .sidebar
            .recent
            .iter()
            .map(|path| folder(title_of(path), "Recent", path)),
    );
    items.extend(model.sidebar.drives.iter().map(|drive| {
        folder(
            drive.display_name(),
            "Drive",
            &drive.mount.to_string_lossy(),
        )
    }));
    if text.trim().is_empty() {
        items.truncate(SHOWN);
        return items;
    }
    let matcher = SkimMatcherV2::default().ignore_case();
    let mut scored: Vec<(i64, usize, PaletteItem)> = items
        .into_iter()
        .enumerate()
        .filter_map(|(index, item)| {
            Some((matcher.fuzzy_match(&item.title, text.trim())?, index, item))
        })
        .collect();
    scored.sort_by_key(|(score, index, _)| (Reverse(*score), *index));
    scored
        .into_iter()
        .take(SHOWN)
        .map(|(_, _, item)| item)
        .collect()
}

/// The palette, while open.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PaletteView {
    pub text: String,
    pub items: Vec<PaletteItem>,
}
