//! The menus a right click opens: for the selection, for a Recycle Bin item,
//! and for the folder itself. They are described here as plain data, the same
//! items in the same order as the Mac app's, and shown by [`crate::popup`].
use dirfleet_core::app::{
    CommandId, Event, SidebarSectionView, TabViewModel, ThumbnailChoice, ViewModel,
};
use dirfleet_core::view_settings::DisplayType;

/// What a right click was on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuOn {
    /// An entry: the menu is the selection's.
    Selection,
    /// The space no entry covers: the menu is the folder's.
    Folder,
}

/// What a right click in the sidebar was on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarMenu {
    /// A folder or a drive.
    Place(String),
    /// The Recycle Bin.
    Bin,
    /// A saved search.
    Saved(u64),
}

/// What choosing a menu item does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// One of the core's commands, run as its shortcut would.
    Run(CommandId),
    /// An event told to the core as it is.
    Send(Box<Event>),
    /// Pick a folder, then move or copy the selection there.
    SendTo { copy: bool },
    /// Pick a folder, then extract into it.
    ExtractTo,
    /// Explorer's own menu for the selection.
    Shell,
    /// A plugin's action; `confirm` is asked first when it is not empty.
    Plugin { action: String, confirm: String },
    /// Open the editor of a saved search.
    EditSearch(u64),
}

/// One line of a menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Item {
        label: String,
        action: Action,
        enabled: bool,
        checked: bool,
    },
    Separator,
    Submenu {
        label: String,
        entries: Vec<Self>,
    },
}

impl Entry {
    fn item(label: impl Into<String>, action: Action) -> Self {
        Self::Item {
            label: label.into(),
            action,
            enabled: true,
            checked: false,
        }
    }

    fn checked(label: impl Into<String>, action: Action, checked: bool) -> Self {
        Self::Item {
            label: label.into(),
            action,
            enabled: true,
            checked,
        }
    }

    fn enabled(self, enabled: bool) -> Self {
        match self {
            Self::Item {
                label,
                action,
                checked,
                ..
            } => Self::Item {
                label,
                action,
                enabled,
                checked,
            },
            other => other,
        }
    }
}

/// What the menus are built from.
pub struct Context<'a> {
    pub shared: &'a ViewModel,
    pub window: u32,
    pub tab: &'a TabViewModel,
    pub sidebar: &'a [SidebarSectionView],
    /// The one selected entry, when there is just one: its row, and whether
    /// its thumbnail can be chosen.
    pub only: Option<(u32, ThumbnailChoice)>,
}

impl Context<'_> {
    /// A command as a menu item, under the core's name for it, with its shortcut.
    fn command(&self, command: CommandId) -> Entry {
        let (label, shortcut) = self
            .shared
            .commands
            .iter()
            .find(|view| view.id == command)
            .map_or_else(
                || (command.label(), ""),
                |view| (view.label.as_str(), view.shortcut.as_str()),
            );
        Entry::item(with_shortcut(label, shortcut), Action::Run(command))
    }

    fn send(label: impl Into<String>, event: Event) -> Entry {
        Entry::item(label, Action::Send(Box::new(event)))
    }

    /// The menu of the selected entries.
    pub fn selection(&self) -> Vec<Entry> {
        let tab = self.tab.id;
        // Inside an archive an entry is no file yet: it can only be opened,
        // which brings a copy of it out.
        if !self.tab.location.caps.real_paths {
            let mut entries = vec![Self::send("Open", Event::OpenSelection { tab })];
            entries.extend(self.extracting());
            return entries;
        }
        let several = self.tab.selected_count > 1;
        let mut entries = vec![
            Self::send("Open", Event::OpenSelection { tab }),
            Self::send("Open With…", Event::OpenWith { tab }),
            self.command(CommandId::QuickLook),
        ];
        entries.extend(self.extracting());
        entries.extend([
            Entry::Separator,
            if several {
                Self::send(
                    format!("Rename {} Items…", self.tab.selected_count),
                    Event::StartBatchRename { tab },
                )
            } else {
                self.command(CommandId::Rename)
            },
            self.command(CommandId::Duplicate),
            Self::send("Compress…", Event::StartCompress { tab }),
        ]);
        // A video's thumbnail can be chosen, and a chosen one given up.
        if let Some((row, choice)) = self
            .only
            .filter(|(_, choice)| *choice != ThumbnailChoice::None)
        {
            entries.push(Self::send(
                "Select Thumbnail…",
                Event::SelectThumbnail { tab, row },
            ));
            if choice == ThumbnailChoice::Chosen {
                entries.push(Self::send(
                    "Reset Thumbnail",
                    Event::ResetThumbnail { tab, row },
                ));
            }
        }
        entries.extend([
            self.command(CommandId::MoveToTrash),
            Entry::Submenu {
                label: "Move To".to_owned(),
                entries: self.send_to(false),
            },
            Entry::Submenu {
                label: "Copy To".to_owned(),
                entries: self.send_to(true),
            },
            Entry::Separator,
            self.command(CommandId::Copy),
            self.command(CommandId::Cut),
            self.command(CommandId::CopyAsPath),
            Entry::Separator,
            self.command(CommandId::ShowInFinder),
            self.command(CommandId::GetInfo),
            Self::send("Remove from Open History", Event::ForgetOpened { tab }),
        ]);
        if !self.tab.plugin_actions.is_empty() {
            entries.push(Entry::Separator);
            entries.extend(self.tab.plugin_actions.iter().map(|action| {
                Entry::item(
                    with_shortcut(&action.title, &action.shortcut),
                    Action::Plugin {
                        action: action.id.clone(),
                        confirm: action.confirm.clone(),
                    },
                )
            }));
        }
        entries.push(Entry::Separator);
        entries.push(Entry::item("More options", Action::Shell));
        entries
    }

    /// Taking things out of archives, where there is something to take:
    /// selected archives, or what is shown inside one.
    fn extracting(&self) -> Vec<Entry> {
        if !self.tab.can_extract {
            return Vec::new();
        }
        let tab = self.tab.id;
        vec![
            Entry::Separator,
            Self::send("Extract Here", Event::ExtractHere { tab }),
            Entry::item("Extract To…", Action::ExtractTo),
        ]
    }

    /// The places the selection can be moved or copied to, and "choose one".
    fn send_to(&self, copy: bool) -> Vec<Entry> {
        let tab = self.tab.id;
        let mut entries: Vec<Entry> = self
            .tab
            .transfer_targets
            .iter()
            .map(|target| {
                let folder = target.path.clone();
                Self::send(
                    format!("{}\t{}", target.title, target.detail),
                    if copy {
                        Event::CopyTo { tab, folder }
                    } else {
                        Event::MoveTo { tab, folder }
                    },
                )
            })
            .collect();
        if !entries.is_empty() {
            entries.push(Entry::Separator);
        }
        entries.push(Entry::item("Choose Folder…", Action::SendTo { copy }));
        entries
    }

    /// The menu of selected Recycle Bin items.
    pub fn trashed(&self) -> Vec<Entry> {
        let tab = self.tab.id;
        let mut entries = Vec::new();
        if self.shared.trash.can_restore {
            entries.push(Self::send("Put Back", Event::RestoreSelection { tab }));
        }
        entries.extend([
            self.command(CommandId::DeleteImmediately),
            Entry::Separator,
            self.command(CommandId::ShowInFinder),
            self.command(CommandId::CopyAsPath),
        ]);
        entries
    }

    /// The menu of the folder shown, opened on the space no entry covers.
    pub fn folder(&self) -> Vec<Entry> {
        let tab = self.tab.id;
        let view = self.tab.view;
        let mut sorting: Vec<Entry> = self
            .shared
            .sorts
            .iter()
            .map(|choice| {
                Entry::checked(
                    choice.label.clone(),
                    Action::Send(Box::new(Event::SortBy {
                        tab,
                        sort: choice.sort,
                    })),
                    choice.sort == view.sort,
                )
            })
            .collect();
        sorting.extend([
            Entry::Separator,
            Entry::checked(
                "Reverse order",
                Action::Send(Box::new(Event::SetReverse {
                    tab,
                    reverse: !view.invert,
                })),
                view.invert,
            ),
            Entry::checked(
                "Show hidden items",
                Action::Run(CommandId::ToggleHidden),
                view.show_hidden,
            ),
            Entry::Separator,
            Entry::checked(
                "as Icons",
                Action::Run(CommandId::ViewIcons),
                view.display == DisplayType::Icons,
            ),
            Entry::checked(
                "as List",
                Action::Run(CommandId::ViewList),
                view.display == DisplayType::List,
            ),
        ]);
        // Nothing is made or pasted where nothing can be changed.
        let mut entries = if self.tab.location.caps.writable {
            vec![
                self.command(CommandId::NewFolder),
                self.command(CommandId::NewTextFile),
                self.command(CommandId::Paste),
                Entry::Separator,
            ]
        } else {
            // Inside an archive: what is shown can be taken out.
            let mut taking = self.extracting();
            if !taking.is_empty() {
                taking.rotate_left(1);
            }
            taking
        };
        entries.extend([
            Entry::Submenu {
                label: "View".to_owned(),
                entries: sorting,
            },
            self.command(CommandId::Refresh),
        ]);
        if let Some(undo) = &self.shared.undo {
            entries.push(Entry::item(
                with_shortcut(undo, CommandId::Undo.shortcut()),
                Action::Run(CommandId::Undo),
            ));
        }
        if !self.tab.folder_plugin_actions.is_empty() {
            entries.push(Entry::Separator);
            entries.extend(self.tab.folder_plugin_actions.iter().map(|action| {
                Entry::item(
                    with_shortcut(&action.title, &action.shortcut),
                    Action::Plugin {
                        action: action.id.clone(),
                        confirm: action.confirm.clone(),
                    },
                )
            }));
        }
        if !self.tab.path.is_empty() {
            entries.push(Entry::Separator);
            entries.extend(self.place(&self.tab.path));
        }
        entries
    }

    /// What can be done with the folder at `path`, wherever it is shown.
    pub fn place(&self, path: &str) -> Vec<Entry> {
        let tab = self.tab.id;
        let path = path.to_owned();
        let favorite = self
            .sidebar
            .iter()
            .flat_map(|section| &section.items)
            .any(|item| item.favorite && item.path == path);
        vec![
            Self::send(
                "Open in New Tab",
                Event::NewTab {
                    window: self.window,
                    path: Some(path.clone()),
                },
            ),
            Self::send(
                "Paste Into Folder",
                Event::PasteInto {
                    tab,
                    folder: path.clone(),
                },
            ),
            if favorite {
                Self::send(
                    "Remove from Favorites",
                    Event::RemoveFavorite { path: path.clone() },
                )
            } else {
                Self::send(
                    "Add to Favorites",
                    Event::AddFavorite { path: path.clone() },
                )
            },
            Self::send(
                "Open in Explorer",
                Event::OpenInFileManager { path: path.clone() },
            ),
            Self::send(
                self.command_label(CommandId::ShowInFinder),
                Event::RevealPath { path: path.clone() },
            ),
            Self::send(
                self.command_label(CommandId::CopyAsPath),
                Event::CopyPath { path },
            )
            .enabled(true),
        ]
    }

    /// The menu of a place in the sidebar.
    pub fn sidebar(&self, on: &SidebarMenu) -> Vec<Entry> {
        match on {
            SidebarMenu::Place(path) => self.place(path),
            SidebarMenu::Bin => vec![
                self.command(CommandId::OpenTrash),
                self.command(CommandId::EmptyTrash),
            ],
            SidebarMenu::Saved(id) => self.saved(*id),
        }
    }

    /// The menu of a saved search: run it, change it, move it among the
    /// others, or delete it.
    fn saved(&self, id: u64) -> Vec<Entry> {
        let tab = self.tab.id;
        let searches = &self.shared.saved_searches;
        let Some(position) = searches.iter().position(|saved| saved.id == id) else {
            return Vec::new();
        };
        let at = position as u32;
        vec![
            Self::send("Run", Event::RunSavedSearch { tab, id }),
            Self::send(
                "Run in New Window",
                Event::RunSavedSearchInNewWindow { tab, id },
            ),
            Entry::item("Edit…", Action::EditSearch(id)),
            Self::send("Duplicate", Event::DuplicateSavedSearch { id }),
            // The core copies the text it is given as it is.
            Self::send(
                "Copy Query",
                Event::CopyPath {
                    path: searches[position].text.clone(),
                },
            ),
            Entry::Separator,
            Self::send(
                "Move Up",
                Event::MoveSavedSearch {
                    from: at,
                    to: at.saturating_sub(1),
                },
            )
            .enabled(position > 0),
            Self::send(
                "Move Down",
                Event::MoveSavedSearch {
                    from: at,
                    to: at + 1,
                },
            )
            .enabled(position + 1 < searches.len()),
            Entry::Separator,
            Self::send("Delete", Event::DeleteSavedSearch { id }),
        ]
    }

    fn command_label(&self, command: CommandId) -> String {
        self.shared
            .commands
            .iter()
            .find(|view| view.id == command)
            .map_or_else(|| command.label().to_owned(), |view| view.label.clone())
    }
}

/// A label with its shortcut after a tab, which menus draw right-aligned.
fn with_shortcut(label: &str, shortcut: &str) -> String {
    if shortcut.is_empty() {
        label.to_owned()
    } else {
        format!("{label}\t{shortcut}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(entries: &[Entry]) -> Vec<&str> {
        entries
            .iter()
            .map(|entry| match entry {
                Entry::Item { label, .. } | Entry::Submenu { label, .. } => label.as_str(),
                Entry::Separator => "-",
            })
            .collect()
    }

    fn context<'a>(shared: &'a ViewModel, tab: &'a TabViewModel) -> Context<'a> {
        Context {
            shared,
            window: 1,
            tab,
            sidebar: &[],
            only: None,
        }
    }

    #[test]
    fn one_item_is_renamed_in_place_and_several_together() {
        let shared = ViewModel::default();
        let mut tab = TabViewModel {
            selected_count: 1,
            ..TabViewModel::default()
        };
        let one = context(&shared, &tab).selection();
        assert!(labels(&one).contains(&CommandId::Rename.label()));
        tab.selected_count = 3;
        let several = context(&shared, &tab).selection();
        assert!(labels(&several).contains(&"Rename 3 Items…"));
    }

    #[test]
    fn inside_an_archive_entries_only_open_and_nothing_is_made() {
        let shared = ViewModel::default();
        let mut tab = TabViewModel {
            selected_count: 1,
            ..TabViewModel::default()
        };
        let folder = context(&shared, &tab).folder();
        assert!(labels(&folder).contains(&CommandId::NewFolder.label()));

        let archive = dirfleet_core::Location::parse("archive:C:\\a.zip!/docs");
        tab.location.kind = archive.kind();
        tab.location.caps = archive.default_caps();
        assert_eq!(labels(&context(&shared, &tab).selection()), ["Open"]);
        // With something to take out, both menus offer it.
        tab.can_extract = true;
        assert_eq!(
            labels(&context(&shared, &tab).selection()),
            ["Open", "-", "Extract Here", "Extract To…"]
        );
        assert_eq!(
            labels(&context(&shared, &tab).folder())[..3],
            ["Extract Here", "Extract To…", "-"]
        );
        tab.can_extract = false;
        let folder = context(&shared, &tab).folder();
        assert!(!labels(&folder).contains(&CommandId::NewFolder.label()));
        assert!(!labels(&folder).contains(&CommandId::Paste.label()));
        assert!(labels(&folder).contains(&"View"));
    }

    #[test]
    fn only_a_video_offers_its_thumbnail_and_only_a_chosen_one_its_reset() {
        let shared = ViewModel::default();
        let tab = TabViewModel::default();
        let menu = |only| {
            Context {
                only,
                ..context(&shared, &tab)
            }
            .selection()
        };
        let plain = menu(Some((2, ThumbnailChoice::None)));
        assert!(!labels(&plain).contains(&"Select Thumbnail…"));
        let video = menu(Some((2, ThumbnailChoice::Automatic)));
        assert!(labels(&video).contains(&"Select Thumbnail…"));
        assert!(!labels(&video).contains(&"Reset Thumbnail"));
        let chosen = menu(Some((2, ThumbnailChoice::Chosen)));
        assert!(labels(&chosen).contains(&"Reset Thumbnail"));
    }

    #[test]
    fn a_saved_search_moves_only_where_there_is_room() {
        use dirfleet_core::app::SavedSearchView;
        let saved = |id| SavedSearchView {
            id,
            name: format!("Search {id}"),
            summary: String::new(),
            text: "report".to_owned(),
            case_sensitive: false,
            depth: 1,
            any: false,
            favorites: false,
        };
        let shared = ViewModel {
            saved_searches: vec![saved(5), saved(6)],
            ..ViewModel::default()
        };
        let tab = TabViewModel::default();
        let can = |id: u64, label: &str| {
            context(&shared, &tab)
                .sidebar(&SidebarMenu::Saved(id))
                .into_iter()
                .find_map(|entry| match entry {
                    Entry::Item {
                        label: found,
                        enabled,
                        ..
                    } if found == label => Some(enabled),
                    _ => None,
                })
        };
        assert_eq!(can(5, "Move Up"), Some(false));
        assert_eq!(can(5, "Move Down"), Some(true));
        assert_eq!(can(6, "Move Up"), Some(true));
        assert_eq!(can(6, "Move Down"), Some(false));
        assert_eq!(can(9, "Move Up"), None, "a search that is gone has no menu");
    }

    #[test]
    fn the_selections_menu_ends_with_explorers() {
        let shared = ViewModel::default();
        let tab = TabViewModel::default();
        let entries = context(&shared, &tab).selection();
        assert_eq!(
            entries.last(),
            Some(&Entry::item("More options", Action::Shell))
        );
    }

    #[test]
    fn put_back_shows_only_where_the_bin_can_restore() {
        let mut shared = ViewModel::default();
        let tab = TabViewModel::default();
        assert!(!labels(&context(&shared, &tab).trashed()).contains(&"Put Back"));
        shared.trash.can_restore = true;
        assert_eq!(labels(&context(&shared, &tab).trashed())[0], "Put Back");
    }

    #[test]
    fn a_folder_without_a_path_has_no_folder_actions() {
        let shared = ViewModel::default();
        let mut tab = TabViewModel::default();
        let without = context(&shared, &tab).folder();
        assert!(!labels(&without).contains(&"Open in New Tab"));
        tab.path = "C:\\Users".to_owned();
        let with = context(&shared, &tab).folder();
        assert!(labels(&with).contains(&"Open in New Tab"));
        assert!(labels(&with).contains(&"Add to Favorites"));
    }

    #[test]
    fn a_shortcut_follows_its_label_after_a_tab() {
        assert_eq!(with_shortcut("Copy", "Ctrl+C"), "Copy\tCtrl+C");
        assert_eq!(with_shortcut("Open", ""), "Open");
    }
}
