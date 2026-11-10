//! The sidebar, as egui's: Favorites, Recent (the last folders, not the one
//! shown), Locations (the user's folders and the Trash) and Drives (polled
//! by the effects, with capacity and eject). Each window gets its own copy so
//! rows can mark what that window shows.
use std::path::Path;

use crux_core::Command;
use crux_core::capability::Operation;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::{DirFleet, Effect, Event, Model, Settings, Tab, ToastKind, title_of};
use crate::drives::{Drive, DriveKind};
use crate::search::saved::SavedSearch;

/// Recent folders kept; the sidebar shows the first few.
const RECENT_KEPT: usize = 12;
const RECENT_SHOWN: usize = 5;

/// A folder with a name: a favorite or one of the user's folders.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct Bookmark {
    pub name: String,
    pub path: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(C)]
pub enum SidebarSection {
    Favorites,
    SavedSearches,
    Recent,
    Locations,
    Drives,
}

impl SidebarSection {
    pub const ALL: [Self; 5] = [
        Self::Favorites,
        Self::SavedSearches,
        Self::Recent,
        Self::Locations,
        Self::Drives,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Favorites => "Favorites",
            Self::SavedSearches => "Saved Searches",
            Self::Recent => "Recent",
            Self::Locations => "Locations",
            Self::Drives => "Drives",
        }
    }
}

#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum SidebarIcon {
    Folder,
    Home,
    Documents,
    Desktop,
    Downloads,
    Pictures,
    Music,
    Recent,
    Trash,
    SavedSearch,
    Drive(DriveKind),
}

/// The question before ejecting a drive that operations still use.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct EjectConfirmView {
    /// "Eject Backup?"
    pub title: String,
    pub message: String,
    /// "Eject" or "Disconnect".
    pub button: String,
}

/// The drive a tab's folder is on, for its status line.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DriveStatusView {
    /// "12.3 GB available"; "" when the capacity isn't known.
    pub free: String,
    /// Where it is mounted, how full it is and its file system.
    pub detail: String,
    /// "Eject Backup", when it can be ejected.
    pub eject: Option<String>,
    pub mount: String,
}

/// One section of a window's sidebar; empty sections are left out.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SidebarSectionView {
    pub section: SidebarSection,
    pub title: String,
    pub collapsed: bool,
    pub items: Vec<SidebarItem>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SidebarItem {
    pub title: String,
    /// The folder it goes to; empty for the Trash.
    pub path: String,
    pub trash: bool,
    pub icon: SidebarIcon,
    /// The window's focused tab shows it.
    pub active: bool,
    pub favorite: bool,
    /// A tooltip: the full path, or a drive's free space.
    pub detail: String,
    /// A drive's share in use, in thousandths, when its capacity is known.
    pub used_permille: Option<u16>,
    /// "Eject" or "Disconnect" for a drive that can be.
    pub eject: Option<String>,
    /// A saved search to run, rather than a folder.
    pub saved_search: Option<u64>,
}

/// The user's own folders (home, Documents, Desktop…), in sidebar order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserFolders;

impl Operation for UserFolders {
    type Output = Vec<Bookmark>;
}

/// The mounted drives now, then again whenever they change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchDrives;

/// Mounted drives. Opaque to facet: they never leave the core this way.
#[derive(Clone, Debug, PartialEq, Eq, Facet)]
#[facet(opaque)]
pub struct DriveList(pub Vec<Drive>);

impl Operation for WatchDrives {
    type Output = DriveList;
}

/// Eject (or disconnect) the drive mounted at `mount`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Eject {
    pub mount: String,
}

/// How an [`Eject`] went. Opaque to facet: it never leaves the core.
#[derive(Clone, Debug, PartialEq, Eq, Facet)]
#[facet(opaque)]
pub struct EjectDone(pub Result<(), String>);

impl Operation for Eject {
    type Output = EjectDone;
}

/// What the sidebar shows, kept in the model.
#[derive(Default)]
pub(super) struct Sidebar {
    pub(super) favorites: Vec<Bookmark>,
    /// Newest first.
    pub(super) recent: Vec<String>,
    pub(super) user_folders: Vec<Bookmark>,
    pub(super) drives: Vec<Drive>,
    pub(super) collapsed: Vec<SidebarSection>,
    /// The sections in the user's order; empty for the default.
    pub(super) order: Vec<SidebarSection>,
    /// A drive whose ejection waits for the user: operations run on it.
    pub(super) confirm_eject: Option<String>,
    /// The drives were listed once: later changes are news.
    pub(super) drives_known: bool,
}

impl Sidebar {
    /// Every section, in the user's order; sections it doesn't name (newer
    /// ones) keep their default place at the end.
    fn order(&self) -> Vec<SidebarSection> {
        let mut order: Vec<SidebarSection> = self
            .order
            .iter()
            .copied()
            .filter(|section| SidebarSection::ALL.contains(section))
            .collect();
        order.dedup();
        order.extend(
            SidebarSection::ALL
                .into_iter()
                .filter(|section| !self.order.contains(section)),
        );
        order
    }

    /// Moves `section` to where `to` is, as egui's drag onto a header.
    fn move_section(&mut self, section: SidebarSection, to: SidebarSection) {
        let mut order = self.order();
        let (Some(from), Some(at)) = (
            order.iter().position(|s| *s == section),
            order.iter().position(|s| *s == to),
        ) else {
            return;
        };
        let moved = order.remove(from);
        order.insert(at, moved);
        self.order = order;
    }

    /// Puts `path` first among the recent folders.
    pub(super) fn visited(&mut self, path: &str) {
        self.recent.retain(|old| old != path);
        self.recent.insert(0, path.to_owned());
        self.recent.truncate(RECENT_KEPT);
    }

    pub(super) fn is_favorite(&self, path: &str) -> bool {
        self.favorites.iter().any(|favorite| favorite.path == path)
    }

    /// The sidebar of a window whose focused tab is `shown`.
    pub(super) fn view(
        &self,
        shown: Option<&Tab>,
        settings: &Settings,
        saved: &[SavedSearch],
    ) -> Vec<SidebarSectionView> {
        let running = shown.and_then(|tab| tab.search.as_ref()?.saved_from);
        let active_path = shown.and_then(|tab| tab.path.as_deref());
        let in_trash = shown.is_some_and(|tab| tab.trash);
        let item = |title: &str, path: &str, icon: SidebarIcon| SidebarItem {
            title: title.to_owned(),
            path: path.to_owned(),
            trash: false,
            icon,
            active: active_path == Some(path),
            favorite: self.is_favorite(path),
            detail: path.to_owned(),
            used_permille: None,
            eject: None,
            saved_search: None,
        };
        let favorites = self
            .favorites
            .iter()
            .map(|favorite| item(&favorite.name, &favorite.path, SidebarIcon::Folder))
            .collect();
        let recent = self
            .recent
            .iter()
            .filter(|path| settings.show_recent && Some(path.as_str()) != active_path)
            .take(RECENT_SHOWN)
            .map(|path| item(&title_of(path), path, SidebarIcon::Recent))
            .collect();
        let mut locations: Vec<SidebarItem> = self
            .user_folders
            .iter()
            .map(|folder| item(&folder.name, &folder.path, user_folder_icon(&folder.name)))
            .collect();
        if settings.show_trash {
            locations.push(SidebarItem {
                trash: true,
                active: in_trash,
                detail: String::new(),
                ..item(crate::current_path::TRASH_LABEL, "", SidebarIcon::Trash)
            });
        }
        let drives = self
            .drives
            .iter()
            .map(|drive| {
                let mount = drive.mount.to_string_lossy();
                SidebarItem {
                    detail: drive_detail(drive),
                    used_permille: drive
                        .used_fraction()
                        .filter(|_| settings.show_drive_capacity)
                        .map(|used| (used * 1000.0).round() as u16),
                    eject: drive.ejectable().then(|| drive.eject_label().to_owned()),
                    ..item(
                        &drive.display_name(),
                        &mount,
                        SidebarIcon::Drive(drive.kind),
                    )
                }
            })
            .collect();
        let saved_searches = saved
            .iter()
            .map(|search| SidebarItem {
                detail: search.summary(),
                active: running == Some(search.id),
                saved_search: Some(search.id),
                favorite: false,
                ..item(&search.name, "", SidebarIcon::SavedSearch)
            })
            .collect();
        let mut sections = [
            (SidebarSection::Favorites, favorites),
            (SidebarSection::SavedSearches, saved_searches),
            (SidebarSection::Recent, recent),
            (SidebarSection::Locations, locations),
            (SidebarSection::Drives, drives),
        ];
        let order = self.order();
        sections.sort_by_key(|(section, _)| order.iter().position(|s| s == section));
        sections
            .into_iter()
            .filter(|(_, items)| !items.is_empty())
            .map(|(section, items)| SidebarSectionView {
                section,
                title: section.title().to_owned(),
                collapsed: self.collapsed.contains(&section),
                items,
            })
            .collect()
    }
}

fn user_folder_icon(name: &str) -> SidebarIcon {
    match name {
        "User" => SidebarIcon::Home,
        "Documents" => SidebarIcon::Documents,
        "Desktop" => SidebarIcon::Desktop,
        "Downloads" => SidebarIcon::Downloads,
        "Pictures" => SidebarIcon::Pictures,
        "Music" => SidebarIcon::Music,
        _ => SidebarIcon::Folder,
    }
}

/// "16 GB free of 64 GB", or the mount point when the capacity is unknown.
fn drive_detail(drive: &Drive) -> String {
    if drive.total == 0 {
        return drive.mount.to_string_lossy().into_owned();
    }
    let size = |bytes: u64| {
        crate::app::format::size(&crate::listing::DirEntryMetaData {
            size: bytes,
            ..crate::listing::DirEntryMetaData::from(crate::listing::EntryType::File)
        })
    };
    format!("{} free of {}", size(drive.available), size(drive.total))
}

impl DirFleet {
    /// Asks for the user's folders and starts watching the drives.
    pub(super) fn start_sidebar() -> Command<Effect, Event> {
        Command::request_from_shell(UserFolders)
            .then_send(|folders| Event::UserFoldersFound { folders })
            .and(
                Command::stream_from_shell(WatchDrives)
                    .then_send(|drives| Event::DrivesChanged { drives }),
            )
    }

    /// The events that change the sidebar.
    pub(super) fn sidebar_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        match event {
            // As egui's: a drive with operations running asks first.
            Event::EjectDrive { mount }
                if !model.files.running_on(Path::new(&mount)).is_empty() =>
            {
                model.sidebar.confirm_eject = Some(mount);
                return render();
            }
            Event::ConfirmEject => {
                let Some(mount) = model.sidebar.confirm_eject.take() else {
                    return Command::done();
                };
                let cancels: Vec<Command<Effect, Event>> = model
                    .files
                    .running_on(Path::new(&mount))
                    .into_iter()
                    .map(|id| Command::notify_shell(super::CancelFileOp { id }).into())
                    .collect();
                return Command::all(cancels).and(Self::eject(mount)).and(render());
            }
            Event::CancelEject => {
                model.sidebar.confirm_eject = None;
                return render();
            }
            _ => {}
        }
        let sidebar = &mut model.sidebar;
        match event {
            Event::AddFavorite { path } => {
                if !path.is_empty() && !sidebar.is_favorite(&path) {
                    sidebar.favorites.push(Bookmark {
                        name: title_of(&path),
                        path,
                    });
                }
            }
            Event::RemoveFavorite { path } => sidebar.favorites.retain(|f| f.path != path),
            Event::MoveFavorite { from, to } => {
                let (from, to) = (from as usize, to as usize);
                if from < sidebar.favorites.len() && to < sidebar.favorites.len() {
                    let moved = sidebar.favorites.remove(from);
                    sidebar.favorites.insert(to, moved);
                }
            }
            Event::MoveSidebarSection { section, to } => sidebar.move_section(section, to),
            Event::SetSectionCollapsed { section, collapsed } => {
                sidebar.collapsed.retain(|old| *old != section);
                if collapsed {
                    sidebar.collapsed.push(section);
                }
            }
            Event::UserFoldersFound { folders } => sidebar.user_folders = folders,
            Event::DrivesChanged { drives } => return Self::drives_changed(model, drives.0),
            Event::EjectDrive { mount } => return Self::eject(mount),
            Event::Ejected { mount, done } => return Self::ejected(model, &mount, done),
            _ => return Command::done(),
        }
        render()
    }

    fn eject(mount: String) -> Command<Effect, Event> {
        Command::request_from_shell(Eject {
            mount: mount.clone(),
        })
        .then_send(move |done| Event::Ejected { mount, done })
    }

    /// The question before ejecting a busy drive.
    pub(super) fn eject_confirm(model: &Model) -> Option<EjectConfirmView> {
        let mount = model.sidebar.confirm_eject.as_ref()?;
        let drive = model
            .sidebar
            .drives
            .iter()
            .find(|drive| drive.mount.to_string_lossy() == mount.as_str())?;
        let message = match model.files.running_on(Path::new(mount)).len() {
            0 => "The file operations on this drive have finished.".to_owned(),
            1 => "A file operation is still running on this drive. Ejecting cancels it.".to_owned(),
            n => format!(
                "{n} file operations are still running on this drive. Ejecting cancels them."
            ),
        };
        Some(EjectConfirmView {
            title: format!("{} {}?", drive.eject_label(), drive.display_name()),
            message,
            button: drive.eject_label().to_owned(),
        })
    }

    /// The drive `folder` is on, for the status line: its free space and
    /// details, and whether it can be ejected.
    pub(super) fn drive_status(model: &Model, folder: &str) -> Option<DriveStatusView> {
        let path = Path::new(folder);
        if !path.is_absolute() {
            return None;
        }
        let drive = model
            .sidebar
            .drives
            .iter()
            .filter(|drive| path.starts_with(&drive.mount))
            .max_by_key(|drive| drive.mount.as_os_str().len())?;
        if drive.total == 0 && !drive.ejectable() {
            return None;
        }
        let mount = drive.mount.to_string_lossy().into_owned();
        let size = |bytes: u64| {
            super::format::size(&crate::listing::DirEntryMetaData {
                size: bytes,
                ..crate::listing::DirEntryMetaData::from(crate::listing::EntryType::File)
            })
        };
        let mut detail = mount.clone();
        if drive.total > 0 {
            detail = format!(
                "{detail}\n{} free of {}",
                size(drive.available),
                size(drive.total)
            );
        }
        if !drive.file_system.is_empty() {
            detail = format!("{detail} · {}", drive.file_system);
        }
        Some(DriveStatusView {
            free: if drive.total > 0 {
                format!("{} available", size(drive.available))
            } else {
                String::new()
            },
            detail,
            eject: drive
                .ejectable()
                .then(|| format!("{} {}", drive.eject_label(), drive.display_name())),
            mount,
        })
    }

    /// Tabs on an ejected drive go home, as in egui; a refusal says why.
    fn ejected(model: &mut Model, mount: &str, done: EjectDone) -> Command<Effect, Event> {
        if let Err(error) = done.0 {
            return Self::toast(model, ToastKind::Error, error);
        }
        let name = model
            .sidebar
            .drives
            .iter()
            .find(|drive| drive.mount == Path::new(mount))
            .map_or_else(|| title_of(mount), Drive::display_name);
        model
            .sidebar
            .drives
            .retain(|drive| drive.mount != Path::new(mount));
        let home = model.home.clone();
        let moves: Vec<_> = model
            .tabs
            .iter()
            .filter(|(_, tab)| {
                tab.path
                    .as_deref()
                    .is_some_and(|path| Path::new(path).starts_with(mount))
            })
            .map(|(id, _)| {
                Command::event(Event::Navigate {
                    tab: id.number(),
                    path: home.clone(),
                })
            })
            .collect();
        let ejected = Self::toast(model, ToastKind::Success, format!("Ejected {name}"));
        ejected.and(Command::all(moves))
    }

    /// Takes the new drive list, saying which came and went. The first list
    /// says nothing: those drives were there already.
    fn drives_changed(model: &mut Model, drives: Vec<Drive>) -> Command<Effect, Event> {
        let first = !model.sidebar.drives_known;
        model.sidebar.drives_known = true;
        let old = std::mem::replace(&mut model.sidebar.drives, drives);
        if first || !model.settings.notify_drive_changes {
            return render();
        }
        let added: Vec<String> = model
            .sidebar
            .drives
            .iter()
            .filter(|drive| !old.iter().any(|o| o.mount == drive.mount))
            .map(|drive| format!("{} connected", drive.display_name()))
            .collect();
        let removed: Vec<String> = old
            .iter()
            .filter(|drive| !model.sidebar.drives.iter().any(|n| n.mount == drive.mount))
            .map(|drive| format!("{} was disconnected", drive.display_name()))
            .collect();
        let mut command = render();
        for text in added {
            command = command.and(Self::toast(model, ToastKind::Info, text));
        }
        for text in removed {
            command = command.and(Self::toast(model, ToastKind::Warning, text));
        }
        command
    }
}
