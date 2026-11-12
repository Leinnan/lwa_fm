//! The menus a right click opens: for the selection, for a Recycle Bin item,
//! and for the folder itself. They are described here as plain data, the same
//! items in the same order as the Mac app's, and shown by [`crate::popup`].
use dirfleet_core::app::{
    CommandId, ComparePair, CompareView, Event, Keep, OtherPane, SidebarSectionView, TabViewModel,
    ThumbnailChoice, ViewModel,
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
    /// A tag.
    Tag(u32),
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
    /// Every selected entry in sight is a folder, and one is at least.
    pub folders: bool,
    /// The two folders the window would compare, when it shows two.
    pub compare: Option<&'a ComparePair>,
    /// The window's other pane, when the menu is the focused pane's.
    pub other: Option<&'a OtherPane>,
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

    /// A command under a name of the menu's own, with its shortcut.
    fn named(&self, label: &str, command: CommandId) -> Entry {
        let shortcut = self
            .shared
            .commands
            .iter()
            .find(|view| view.id == command)
            .map_or("", |view| view.shortcut.as_str());
        Entry::item(with_shortcut(label, shortcut), Action::Run(command))
    }

    /// Copying and moving the selection into the folder the other pane
    /// shows, named; not offered where that pane takes nothing.
    fn to_other_pane(&self) -> Vec<Entry> {
        let Some(other) = self.other else {
            return Vec::new();
        };
        let named = |verb: &str, command| {
            self.named(
                &format!("{verb} to \u{201c}{}\u{201d}", other.title),
                command,
            )
            .enabled(other.writable)
        };
        vec![
            named("Copy", CommandId::CopyToOtherPane),
            named("Move", CommandId::MoveToOtherPane),
        ]
    }

    /// The menu of the selected entries.
    pub fn selection(&self) -> Vec<Entry> {
        let tab = self.tab.id;
        // Inside an archive an entry is no file yet: it can only be opened,
        // which brings a copy of it out.
        if !self.tab.location.caps.real_paths {
            let mut entries = vec![Self::send("Open", Event::OpenSelection { tab })];
            entries.extend(self.extracting());
            if self.tab.location.caps.writable {
                entries.extend(self.on_server());
            }
            return entries;
        }
        let several = self.tab.selected_count > 1;
        let mut entries = vec![
            Self::send("Open", Event::OpenSelection { tab }),
            Self::send("Open With…", Event::OpenWith { tab }),
            self.command(CommandId::QuickLook),
        ];
        entries.extend(self.extracting());
        entries.push(Entry::Separator);
        // What a scan found is not renamed, copied beside itself or packed.
        if self.tab.results.is_none() {
            entries.extend([
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
            if self.folders {
                entries.push(Self::send(
                    "Find Duplicates in Selected Folders",
                    Event::FindDuplicates {
                        tab,
                        selected: true,
                    },
                ));
            }
        }
        // What differs between two folders is copied to either.
        if let Some(compare) = self.compared() {
            entries.extend(self.copying(compare));
        }
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
        ]);
        entries.extend(self.to_other_pane());
        entries.push(Entry::Submenu {
            label: "Tags".to_owned(),
            entries: self.tagging(),
        });
        entries.extend([
            Entry::Separator,
            self.command(CommandId::Copy),
            self.command(CommandId::Cut),
            self.command(CommandId::CopyAsPath),
            Entry::Separator,
            self.command(CommandId::SelectSameExtension),
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

    /// What can be done with entries on a server: no program of this
    /// machine can be handed them, but they can be renamed, removed, copied
    /// and moved. There is no Recycle Bin there, so the only removal is for
    /// good.
    fn on_server(&self) -> Vec<Entry> {
        let mut entries = vec![Entry::Separator];
        if self.tab.selected_count == 1 {
            entries.push(self.command(CommandId::Rename));
        }
        entries.extend([
            self.command(CommandId::Duplicate),
            self.command(CommandId::DeleteImmediately),
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
        ]);
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

    /// What the folder's entries can be put into groups by, the way in use
    /// marked.
    fn grouping(&self) -> Vec<Entry> {
        let tab = self.tab.id;
        self.shared
            .groupings
            .iter()
            .map(|choice| {
                Entry::checked(
                    choice.label.clone(),
                    Action::Send(Box::new(Event::SetGroupBy {
                        tab,
                        group: choice.group,
                    })),
                    choice.group == self.tab.view.group,
                )
            })
            .collect()
    }

    /// The tags, the ones every selected item has marked: choosing one
    /// gives it to the selection, or takes it when all of it has it.
    fn tagging(&self) -> Vec<Entry> {
        let tab = self.tab.id;
        let mut entries: Vec<Entry> = self
            .shared
            .tags
            .iter()
            .map(|tag| {
                Entry::checked(
                    tag.name.clone(),
                    Action::Send(Box::new(Event::ToggleTag { tab, tag: tag.id })),
                    self.tab.selection_tags.contains(&tag.id),
                )
            })
            .collect();
        if !entries.is_empty() {
            entries.push(Entry::Separator);
        }
        entries.extend([
            Self::send("Clear Tags", Event::ClearTags { tab }),
            Self::send("New Tag…", Event::EditTag { tag: None }),
        ]);
        entries
    }

    /// The menu of a tag in the sidebar: show its items, change it, move
    /// it among the others, or delete it.
    fn tag(&self, id: u32) -> Vec<Entry> {
        let tab = self.tab.id;
        let tags = &self.shared.tags;
        let Some(position) = tags.iter().position(|tag| tag.id == id) else {
            return Vec::new();
        };
        let at = position as u32;
        vec![
            Self::send("Show Items", Event::OpenTag { tab, tag: id }),
            Self::send("Edit…", Event::EditTag { tag: Some(id) }),
            Entry::Separator,
            Self::send(
                "Move Up",
                Event::MoveTag {
                    tag: id,
                    to: at.saturating_sub(1),
                },
            )
            .enabled(position > 0),
            Self::send(
                "Move Down",
                Event::MoveTag {
                    tag: id,
                    to: at + 1,
                },
            )
            .enabled(position + 1 < tags.len()),
            Entry::Separator,
            Self::send("Delete Tag", Event::DeleteTag { tag: id }),
            Self::send("New Tag…", Event::EditTag { tag: None }),
        ]
    }

    /// The folders the tab compares, when that is what it shows.
    fn compared(&self) -> Option<&CompareView> {
        self.tab.results.as_ref()?.compare.as_ref()
    }

    /// Among compared folders: copying the selection, or everything when
    /// nothing is selected, to the left folder and to the right one.
    fn copying(&self, compare: &CompareView) -> Vec<Entry> {
        [true, false]
            .into_iter()
            .map(|to_left| {
                let copying = crate::results::copying(self.tab, compare, to_left);
                Self::send(copying.label, copying.event).enabled(copying.enabled)
            })
            .collect()
    }

    /// What the tab's rows are put together by: a folder's by the groups
    /// chosen; duplicates set by set, with ways to select all but one of
    /// each; what differs between two folders kind by kind, with what is
    /// done about it.
    fn arranging(&self) -> Vec<Entry> {
        let tab = self.tab.id;
        // A tag's items are grouped as a folder's are.
        let Some(results) = self
            .tab
            .results
            .as_ref()
            .filter(|results| results.tag.is_none())
        else {
            return vec![Entry::Submenu {
                label: "Group By".to_owned(),
                entries: self.grouping(),
            }];
        };
        let Some(compare) = &results.compare else {
            return vec![Entry::Submenu {
                label: "Select All But".to_owned(),
                entries: self.keeping(),
            }];
        };
        let mut entries = vec![Entry::checked(
            "Read Files to Tell Them Apart",
            Action::Send(Box::new(Event::SetCompareOptions {
                tab,
                contents: !compare.contents,
            })),
            compare.contents,
        )];
        entries.extend(self.copying(compare));
        entries
    }

    /// Among duplicates: selecting every file of each set but one.
    fn keeping(&self) -> Vec<Entry> {
        let tab = self.tab.id;
        [
            ("The Newest", Keep::Newest),
            ("The Oldest", Keep::Oldest),
            ("The First Listed", Keep::First),
        ]
        .into_iter()
        .map(|(label, keep)| Self::send(label, Event::SelectDuplicates { tab, keep }))
        .collect()
    }

    /// How the folder is sorted and shown, the ways in use marked.
    fn viewing(&self) -> Vec<Entry> {
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
        sorting
    }

    /// The menu of the folder shown, opened on the space no entry covers.
    pub fn folder(&self) -> Vec<Entry> {
        let tab = self.tab.id;
        let sorting = self.viewing();
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
        entries.push(Entry::Submenu {
            label: "View".to_owned(),
            entries: sorting,
        });
        // What a scan found is kept its own way, whatever the view groups by.
        entries.extend(self.arranging());
        entries.push(self.command(CommandId::SelectByPattern));
        entries.push(self.command(CommandId::Refresh));
        // What the two panes do together.
        if self.other.is_some() {
            entries.push(self.command(CommandId::SwapPanes));
            if !self.tab.path.is_empty() {
                entries.push(self.command(CommandId::MirrorPane));
            }
        }
        // Only folders on this machine are read through.
        if !self.tab.path.is_empty() && crate::store::is_on_disk(&self.tab.path) {
            entries.push(Self::send(
                "Find Duplicates",
                Event::FindDuplicates {
                    tab,
                    selected: false,
                },
            ));
        }
        // The folders the window shows side by side.
        if let Some(pair) = self.compare.filter(|_| self.tab.results.is_none()) {
            entries.push(Self::send(
                format!(
                    "Compare \u{201c}{}\u{201d} with \u{201c}{}\u{201d}",
                    pair.left, pair.right
                ),
                Event::CompareFolders {
                    window: self.window,
                },
            ));
        }
        if let Some(undo) = &self.shared.undo {
            // Under the name of what it undoes, with the key Undo has now.
            entries.push(self.named(undo, CommandId::Undo));
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
            SidebarMenu::Tag(id) => self.tag(*id),
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
            folders: false,
            compare: None,
            other: None,
        }
    }

    /// Whether the menu runs `command`.
    fn runs(entries: &[Entry], command: CommandId) -> bool {
        entries.iter().any(|entry| {
            matches!(entry, Entry::Item { action: Action::Run(run), .. } if *run == command)
        })
    }

    /// The event the menu's item `label` sends.
    fn sends<'a>(entries: &'a [Entry], label: &str) -> Option<&'a Event> {
        entries.iter().find_map(|entry| match entry {
            Entry::Item {
                label: named,
                action: Action::Send(event),
                ..
            } if named == label => Some(&**event),
            _ => None,
        })
    }

    #[test]
    fn duplicates_are_looked_for_in_a_folder_of_this_machine_or_in_selected_folders() {
        let shared = ViewModel::default();
        let mut tab = TabViewModel {
            id: 4,
            path: "C:\\Users".to_owned(),
            selected_count: 2,
            ..TabViewModel::default()
        };
        let folder = context(&shared, &tab).folder();
        assert_eq!(
            sends(&folder, "Find Duplicates"),
            Some(&Event::FindDuplicates {
                tab: 4,
                selected: false
            })
        );
        // With folders selected, their menu looks in them.
        let label = "Find Duplicates in Selected Folders";
        assert_eq!(sends(&context(&shared, &tab).selection(), label), None);
        let folders = Context {
            folders: true,
            ..context(&shared, &tab)
        };
        assert_eq!(
            sends(&folders.selection(), label),
            Some(&Event::FindDuplicates {
                tab: 4,
                selected: true
            })
        );
        // A server's folders are not read through.
        tab.path = "sftp://office/files".to_owned();
        let folder = context(&shared, &tab).folder();
        assert_eq!(sends(&folder, "Find Duplicates"), None);
    }

    #[test]
    fn tags_are_given_from_the_selection_s_menu_and_kept_from_the_sidebar_s() {
        use dirfleet_core::app::{TagColor, TagView};
        let tag = |id, name: &str| TagView {
            id,
            name: name.to_owned(),
            color: TagColor::Red,
            count: 0,
        };
        let shared = ViewModel {
            tags: vec![tag(1, "Red"), tag(5, "Work")],
            ..ViewModel::default()
        };
        let mut tab = TabViewModel {
            id: 4,
            path: "C:\\a".to_owned(),
            selected_count: 2,
            // Every selected item has the second tag.
            selection_tags: vec![5],
            ..TabViewModel::default()
        };
        tab.location.caps.real_paths = true;
        let menu = context(&shared, &tab).selection();
        let tags = menu
            .iter()
            .find_map(|entry| match entry {
                Entry::Submenu { label, entries } if label == "Tags" => Some(entries.clone()),
                _ => None,
            })
            .expect("a Tags menu");
        assert_eq!(
            labels(&tags),
            ["Red", "Work", "-", "Clear Tags", "New Tag…"]
        );
        let marked: Vec<bool> = tags
            .iter()
            .filter_map(|entry| match entry {
                Entry::Item { checked, .. } => Some(*checked),
                _ => None,
            })
            .collect();
        assert_eq!(marked, [false, true, false, false]);
        assert_eq!(
            sends(&tags, "Work"),
            Some(&Event::ToggleTag { tab: 4, tag: 5 })
        );
        assert_eq!(
            sends(&tags, "Clear Tags"),
            Some(&Event::ClearTags { tab: 4 })
        );

        // A tag's own menu, in the sidebar.
        let own = context(&shared, &tab).sidebar(&SidebarMenu::Tag(5));
        assert_eq!(
            sends(&own, "Show Items"),
            Some(&Event::OpenTag { tab: 4, tag: 5 })
        );
        assert_eq!(sends(&own, "Edit…"), Some(&Event::EditTag { tag: Some(5) }));
        assert_eq!(
            sends(&own, "Move Up"),
            Some(&Event::MoveTag { tag: 5, to: 0 })
        );
        assert_eq!(
            sends(&own, "Delete Tag"),
            Some(&Event::DeleteTag { tag: 5 })
        );
        let enabled = |label: &str| {
            own.iter().find_map(|entry| match entry {
                Entry::Item {
                    label: named,
                    enabled,
                    ..
                } if named == label => Some(*enabled),
                _ => None,
            })
        };
        assert_eq!(
            (enabled("Move Up"), enabled("Move Down")),
            (Some(true), Some(false)),
            "the last tag goes no further down"
        );
        assert_eq!(
            context(&shared, &tab).sidebar(&SidebarMenu::Tag(99)),
            Vec::new()
        );
    }

    #[test]
    fn the_other_pane_is_copied_and_moved_to_by_name_and_swapped_with() {
        use dirfleet_core::app::CommandView;
        let shared = ViewModel {
            commands: vec![CommandView {
                id: CommandId::CopyToOtherPane,
                label: "Copy to Other Pane".to_owned(),
                shortcut: "Shift+F5".to_owned(),
                ..CommandView::default()
            }],
            ..ViewModel::default()
        };
        let mut tab = TabViewModel {
            id: 4,
            path: "C:\\a".to_owned(),
            selected_count: 1,
            ..TabViewModel::default()
        };
        tab.location.caps.real_paths = true;
        let copy = "Copy to \u{201c}Downloads\u{201d}\tShift+F5";
        let moving = "Move to \u{201c}Downloads\u{201d}";
        // One pane: nothing to copy to or swap with.
        let alone = context(&shared, &tab);
        assert!(!labels(&alone.selection()).contains(&copy));
        assert!(!runs(&alone.folder(), CommandId::SwapPanes));
        assert!(runs(&alone.folder(), CommandId::SelectByPattern));
        assert!(runs(&alone.selection(), CommandId::SelectSameExtension));

        let mut other = OtherPane {
            tab: 5,
            title: "Downloads".to_owned(),
            path: "C:\\Downloads".to_owned(),
            writable: true,
            reason: String::new(),
        };
        let enabled = |other: &OtherPane, label: &str| {
            let menu = Context {
                other: Some(other),
                ..context(&shared, &tab)
            }
            .selection();
            menu.iter().find_map(|entry| match entry {
                Entry::Item {
                    label: named,
                    enabled,
                    action,
                    ..
                } if named == label => Some((*enabled, action.clone())),
                _ => None,
            })
        };
        assert_eq!(
            enabled(&other, copy),
            Some((true, Action::Run(CommandId::CopyToOtherPane)))
        );
        assert_eq!(
            enabled(&other, moving),
            Some((true, Action::Run(CommandId::MoveToOtherPane)))
        );
        let folder = Context {
            other: Some(&other),
            ..context(&shared, &tab)
        }
        .folder();
        assert!(runs(&folder, CommandId::SwapPanes));
        assert!(runs(&folder, CommandId::MirrorPane));
        // A pane that takes nothing is named but can't be chosen.
        other.writable = false;
        assert_eq!(
            enabled(&other, copy).map(|(enabled, _)| enabled),
            Some(false)
        );
    }

    #[test]
    fn two_folders_side_by_side_are_compared_and_what_differs_is_copied() {
        use dirfleet_core::app::{ResultsView, SyncCount};
        let shared = ViewModel::default();
        let tab = TabViewModel {
            id: 4,
            path: "C:\\a".to_owned(),
            ..TabViewModel::default()
        };
        let label = "Compare \u{201c}a\u{201d} with \u{201c}b\u{201d}";
        assert_eq!(sends(&context(&shared, &tab).folder(), label), None);
        let pair = ComparePair {
            left: "a".to_owned(),
            right: "b".to_owned(),
        };
        let beside = Context {
            compare: Some(&pair),
            ..context(&shared, &tab)
        };
        assert_eq!(
            sends(&beside.folder(), label),
            Some(&Event::CompareFolders { window: 1 })
        );

        // What differs is copied either way from the rows' menu and the
        // tab's, where there is something to copy.
        let results = TabViewModel {
            id: 5,
            selected_count: 1,
            results: Some(ResultsView {
                compare: Some(CompareView {
                    left: "a".to_owned(),
                    right: "b".to_owned(),
                    to_right: SyncCount {
                        copies: 1,
                        ..SyncCount::default()
                    },
                    ..CompareView::default()
                }),
                ..ResultsView::default()
            }),
            ..TabViewModel::default()
        };
        let sync = |to_left| Event::SyncSelection { tab: 5, to_left };
        for menu in [
            context(&shared, &results).selection(),
            context(&shared, &results).folder(),
        ] {
            assert_eq!(sends(&menu, "Copy to a"), Some(&sync(true)));
            assert_eq!(sends(&menu, "Copy to b"), Some(&sync(false)));
            let enabled: Vec<bool> = menu
                .iter()
                .filter_map(|entry| match entry {
                    Entry::Item { label, enabled, .. } if label.starts_with("Copy to ") => {
                        Some(*enabled)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(enabled, [false, true], "nothing goes left");
        }
        // The tab's menu says whether files are read, and has no sets to
        // select from nor groups to choose.
        let folder = context(&shared, &results).folder();
        assert_eq!(
            sends(&folder, "Read Files to Tell Them Apart"),
            Some(&Event::SetCompareOptions {
                tab: 5,
                contents: true
            })
        );
        assert!(!labels(&folder).contains(&"Select All But"));
        assert!(!labels(&folder).contains(&"Group By"));
        // Nor is another comparison offered from among the results.
        let among = Context {
            compare: Some(&pair),
            ..context(&shared, &results)
        };
        assert_eq!(sends(&among.folder(), label), None);
    }

    #[test]
    fn what_a_scan_found_is_opened_and_removed_but_not_renamed_or_copied() {
        let shared = ViewModel::default();
        let tab = TabViewModel {
            id: 4,
            selected_count: 1,
            results: Some(dirfleet_core::app::ResultsView::default()),
            ..TabViewModel::default()
        };
        let folders = Context {
            folders: true,
            ..context(&shared, &tab)
        };
        let menu = folders.selection();
        assert!(labels(&menu).contains(&"Open"));
        assert!(runs(&menu, CommandId::MoveToTrash));
        assert!(runs(&menu, CommandId::ShowInFinder));
        assert!(!runs(&menu, CommandId::Rename));
        assert!(!runs(&menu, CommandId::Duplicate));
        assert!(!labels(&menu).contains(&"Compress…"));
        assert!(!labels(&menu).contains(&"Find Duplicates in Selected Folders"));

        // Its own menu selects all but one of each set, and groups nothing.
        let folder = context(&shared, &tab).folder();
        assert!(!labels(&folder).contains(&"Group By"));
        assert!(!labels(&folder).contains(&"Find Duplicates"));
        let keeping = folder
            .iter()
            .find_map(|entry| match entry {
                Entry::Submenu { label, entries } if label == "Select All But" => Some(entries),
                _ => None,
            })
            .expect("a menu of what to keep");
        assert_eq!(
            labels(keeping),
            ["The Newest", "The Oldest", "The First Listed"]
        );
        assert_eq!(
            sends(keeping, "The Oldest"),
            Some(&Event::SelectDuplicates {
                tab: 4,
                keep: Keep::Oldest
            })
        );
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
    fn on_a_server_entries_are_renamed_removed_copied_and_moved_but_handed_to_no_program() {
        let shared = ViewModel::default();
        let server = dirfleet_core::Location::parse("sftp://c1/docs");
        let mut tab = TabViewModel {
            selected_count: 1,
            ..TabViewModel::default()
        };
        tab.location.kind = server.kind();
        tab.location.caps = server.default_caps();
        let one = context(&shared, &tab).selection();
        assert_eq!(
            labels(&one),
            [
                "Open",
                "-",
                CommandId::Rename.label(),
                CommandId::Duplicate.label(),
                CommandId::DeleteImmediately.label(),
                "Move To",
                "Copy To",
                "-",
                CommandId::Copy.label(),
                CommandId::Cut.label(),
            ]
        );
        // Several are not renamed at once there.
        tab.selected_count = 2;
        let several = context(&shared, &tab).selection();
        assert!(!labels(&several).contains(&CommandId::Rename.label()));
        assert!(labels(&several).contains(&CommandId::DeleteImmediately.label()));
        // And the folder takes new items and what is pasted.
        let folder = context(&shared, &tab).folder();
        assert!(labels(&folder).contains(&CommandId::NewFolder.label()));
        assert!(labels(&folder).contains(&CommandId::Paste.label()));
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
    fn the_folder_s_menu_offers_the_groupings_with_the_one_in_use_marked() {
        use dirfleet_core::GroupBy;
        use dirfleet_core::app::GroupChoice;
        let choice = |group: GroupBy| GroupChoice {
            group,
            label: group.label().to_owned(),
        };
        let shared = ViewModel {
            groupings: vec![choice(GroupBy::None), choice(GroupBy::Kind)],
            ..ViewModel::default()
        };
        let mut tab = TabViewModel::default();
        tab.view.group = GroupBy::Kind;
        let menu = context(&shared, &tab).folder();
        let groupings = menu
            .iter()
            .find_map(|entry| match entry {
                Entry::Submenu { label, entries } if label == "Group By" => Some(entries),
                _ => None,
            })
            .expect("a Group By menu");
        let marked: Vec<(&str, bool)> = groupings
            .iter()
            .filter_map(|entry| match entry {
                Entry::Item { label, checked, .. } => Some((label.as_str(), *checked)),
                _ => None,
            })
            .collect();
        assert_eq!(marked, [("None", false), ("Kind", true)]);
        assert!(matches!(
            &groupings[1],
            Entry::Item { action: Action::Send(event), .. }
                if **event == Event::SetGroupBy { tab: tab.id, group: GroupBy::Kind }
        ));
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
