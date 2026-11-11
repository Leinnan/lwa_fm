//! The effects the core asks for. Every one of them is handled in Rust by
//! `dirfleet_effects`, except `Render`, [`PickFolder`] and [`OpenWindow`],
//! which each shell handles.
use std::sync::Arc;

use crux_core::capability::Operation;
use facet::Facet;
use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;

use super::layout::FlatLayout;
use crate::columns::ListColumns;
use crate::listing::Listing;
use crate::search::Search;
use crate::{DisplayType, Sort};

/// List one folder, sorted and filtered for display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListDir {
    pub path: String,
    pub view: ListView,
}

/// How a tab's listing is shown: its order, whether hidden items are, and
/// the search that filters it.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ListView {
    pub sort: Sort,
    /// Reverse the order (folders stay first).
    pub invert: bool,
    pub show_hidden: bool,
    /// Only entries matching this search are shown.
    pub filter: Option<Arc<Search>>,
}

/// Opaque to facet: it only travels inside the core, never to a shell.
#[derive(Clone, Debug, Facet)]
#[facet(opaque)]
#[repr(C)]
pub enum ListDirOutput {
    /// The folder's entries, and the display order of those shown with `view`.
    Listed {
        listing: Arc<Listing>,
        order: Vec<u32>,
        view: ListView,
        /// `(times opened, last opened)` per entry from the open history;
        /// empty when nothing was ever opened.
        opened: Arc<Vec<(u32, u32)>>,
    },
    Failed(String),
    /// The folder exists but the app may not read it.
    Denied(String),
    /// The archive lists nothing without its password; `wrong` when the
    /// one given did not fit.
    NeedsPassword {
        wrong: bool,
    },
    /// The folder is on a server that is not connected: the connection's id.
    NeedsConnection {
        connection: String,
    },
}

/// Listings compare by identity: they are shared, never rebuilt equal.
impl PartialEq for ListDirOutput {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Listed {
                    listing,
                    order,
                    view,
                    ..
                },
                Self::Listed {
                    listing: other_listing,
                    order: other_order,
                    view: other_view,
                    ..
                },
            ) => Arc::ptr_eq(listing, other_listing) && order == other_order && view == other_view,
            (Self::Failed(a), Self::Failed(b)) | (Self::Denied(a), Self::Denied(b)) => a == b,
            (Self::NeedsPassword { wrong: a }, Self::NeedsPassword { wrong: b }) => a == b,
            _ => false,
        }
    }
}

impl Eq for ListDirOutput {}

impl Operation for ListDir {
    type Output = ListDirOutput;
}

/// How one folder is shown. Folders without their own use the defaults.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct FolderView {
    pub sort: Sort,
    /// Reverse the order (folders stay first).
    pub invert: bool,
    pub display: DisplayType,
    pub show_hidden: bool,
}

impl FolderView {
    /// What listing the folder needs: everything but how it is drawn.
    /// How a listing is shown with this view and no search.
    pub const fn list_view(self) -> ListView {
        ListView {
            sort: self.sort,
            invert: self.invert,
            show_hidden: self.show_hidden,
            filter: None,
        }
    }

    /// Sort by `sort`; choosing the current sort again reverses it.
    pub fn sort_by(&mut self, sort: Sort) {
        if self.sort == sort {
            self.invert = !self.invert;
        } else {
            self.sort = sort;
        }
    }
}

/// Order an already read listing for another view, without reading it again.
#[derive(Clone, Debug)]
pub struct OrderListing {
    pub listing: Arc<Listing>,
    pub view: ListView,
}

/// A listing's new display order. Opaque to facet: it never leaves the core.
#[derive(Clone, Debug, Facet)]
#[facet(opaque)]
pub struct Ordered {
    pub listing: Arc<Listing>,
    pub order: Vec<u32>,
    /// The view the order is for.
    pub view: ListView,
}

/// Orders compare by the listing they belong to, which is shared, never rebuilt equal.
impl PartialEq for Ordered {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.listing, &other.listing)
            && self.order == other.order
            && self.view == other.view
    }
}

impl Eq for Ordered {}

impl Operation for OrderListing {
    type Output = Ordered;
}

/// Report changes in a folder until [`Unwatch`] with the same id.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Watch {
    pub id: u64,
    pub path: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum WatchEvent {
    /// The OS watcher runs. Changes made before this may have been missed.
    Ready,
    /// Something in the folder changed; bursts arrive as one.
    Changed,
    Failed(String),
}

impl Operation for Watch {
    type Output = WatchEvent;
}

/// Stop the [`Watch`] with this id. Aborting a stream command does not reach
/// the handler (crux issue #622), so the core says so explicitly.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Unwatch {
    pub id: u64,
}

impl Operation for Unwatch {
    type Output = ();
}

/// Ask the user to choose a folder with the platform's own picker. The only
/// operation here a shell handles, since pickers need the UI thread.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PickFolder {
    /// What the picker asks for, e.g. "Open folder".
    pub title: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum PickFolderOutput {
    Picked(String),
    Cancelled,
}

impl Operation for PickFolder {
    type Output = PickFolderOutput;
}

/// Show a window for the core's window `window`. Handled by the shell; the
/// shell reports closing it with `Event::WindowClosed`.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct OpenWindow {
    pub window: u32,
}

impl Operation for OpenWindow {
    type Output = ();
}

/// The version of [`PersistedState`] this build writes.
pub const STATE_VERSION: u32 = 1;

/// What the app keeps between launches. Each feature adds its fields with
/// `#[serde(default)]`, so state written by an older build still loads.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct PersistedState {
    pub version: u32,
    #[serde(default)]
    pub windows: Vec<PersistedWindow>,
    /// Each folder's own view, by path.
    #[serde(default)]
    pub folder_views: BTreeMap<String, FolderView>,
    /// The view of folders without their own.
    #[serde(default)]
    pub default_view: FolderView,
    /// The list view's columns, shared by every folder.
    #[serde(default)]
    pub columns: ListColumns,
    #[serde(default)]
    pub favorites: Vec<super::Bookmark>,
    /// Recent folders, newest first.
    #[serde(default)]
    pub recent: Vec<String>,
    #[serde(default)]
    pub collapsed_sections: Vec<super::SidebarSection>,
    /// The sidebar's sections in the order the user put them; empty for
    /// the default order.
    #[serde(default)]
    pub section_order: Vec<super::SidebarSection>,
    #[serde(default)]
    pub inspector_shown: bool,
    #[serde(default)]
    pub settings: super::Settings,
    /// Opaque to facet: the state is only ever stored, never sent to a shell.
    #[serde(default)]
    #[facet(opaque)]
    pub saved_searches: crate::search::saved::SavedSearches,
    /// The servers the user saved, without their passwords.
    #[serde(default)]
    pub connections: Vec<super::Connection>,
    /// What the user chose for each plugin. Opaque to facet, as above.
    #[serde(default)]
    #[facet(opaque)]
    pub plugins: crate::plugins::SettingsMap,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct PersistedWindow {
    /// Each tab's folder, pane by pane.
    pub tabs: Vec<String>,
    /// The position of the focused tab in `tabs`.
    pub active: u32,
    /// The panes and splits, naming tabs by their position in `tabs`;
    /// absent (one pane of every tab) in state written before splits.
    #[serde(default)]
    pub layout: Option<FlatLayout>,
    /// The Mac's native tab group the window was in.
    #[serde(default)]
    pub group: Option<u32>,
    #[serde(default)]
    pub frame: Option<WindowFrame>,
}

/// A window's place on screen, in the shell's points: the origin is the
/// bottom-left corner on the Mac.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct WindowFrame {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Read the state the previous run saved.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LoadState {
    /// Unused; operations need a value to carry.
    pub version: u32,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
// Read once at start: its size doesn't matter.
#[allow(clippy::large_enum_variant)]
pub enum LoadedState {
    Found(PersistedState),
    /// Nothing saved yet.
    Missing,
    Failed(String),
}

impl Operation for LoadState {
    type Output = LoadedState;
}

/// Replace the saved state. Writes may be coalesced: only the latest counts.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SaveState {
    pub state: PersistedState,
}

impl Operation for SaveState {
    type Output = ();
}

/// Show these files in the system's Quick Look panel, starting at `index`.
/// Handled by the shell.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct QuickLook {
    /// The tab asking, so only its window shows the panel.
    pub tab: u32,
    pub paths: Vec<String>,
    pub index: u32,
}

impl Operation for QuickLook {
    type Output = ();
}
