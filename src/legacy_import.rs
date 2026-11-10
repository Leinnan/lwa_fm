//! Reads favorites, saved searches, view settings and list columns saved in
//! egui's persisted memory, whichever build saved them.
//!
//! egui keys each entry by its id XOR a hash of the stored type's `TypeId`.
//! A `TypeId` changes whenever the crate or its dependencies change, so an
//! entry saved by another build is never found by type. It is found here by
//! recomputing the key from the type hash the entry itself stores, and its
//! type is told apart by the fields of its value.
use std::sync::OnceLock;

use serde::Deserialize;

use crate::app::directory_view_settings::{DirectoryShowHidden, DirectoryViewSettings};
use crate::app::dock::{CurrentPath, ListColumns};
use crate::app::search::SavedSearches;
use crate::locations::Locations;

/// egui memory as saved: only the persisted data is read.
#[derive(Deserialize)]
struct MemoryRon {
    #[serde(default)]
    data: DataRon,
}

#[derive(Deserialize, Default)]
struct DataRon(Vec<(u64, ElementRon)>);

#[derive(Deserialize)]
struct ElementRon {
    type_id: TypeHashRon,
    ron: String,
    #[serde(default)]
    generation: usize,
}

#[derive(Deserialize)]
struct TypeHashRon(u64);

/// Which persisted type an entry holds, from the fields of its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Favorites,
    SavedSearches,
    ViewSettings,
    ShowHidden,
    Columns,
}

impl Kind {
    fn of(ron: &str) -> Option<Self> {
        let inner = ron.trim().strip_prefix('(')?.trim_start();
        if inner.starts_with("true") || inner.starts_with("false") {
            return Some(Self::ShowHidden);
        }
        let field: String = inner
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        match field.as_str() {
            "locations" => Some(Self::Favorites),
            "searches" => Some(Self::SavedSearches),
            "sorting" | "display_type" | "invert_sort" => Some(Self::ViewSettings),
            "shown" | "widths" => Some(Self::Columns),
            _ => None,
        }
    }
}

struct Entry {
    key: u64,
    type_hash: u64,
    kind: Kind,
    ron: String,
    generation: usize,
}

/// The persisted entries of one saved egui memory.
pub struct LegacyMemory {
    entries: Vec<Entry>,
}

/// Values saved app-wide, not per folder.
#[derive(Default)]
pub struct GlobalValues {
    pub favorites: Option<Locations>,
    pub saved_searches: Option<SavedSearches>,
    pub view_settings: Option<DirectoryViewSettings>,
    pub show_hidden: Option<DirectoryShowHidden>,
    pub columns: Option<ListColumns>,
}

/// Values saved for one folder.
#[derive(Default)]
pub struct FolderValues {
    pub view_settings: Option<DirectoryViewSettings>,
    pub show_hidden: Option<DirectoryShowHidden>,
    pub columns: Option<ListColumns>,
}

/// The key of an app-wide entry (`Id::unique(TypeId::of::<T>())` in `helper.rs`).
fn global_key(type_hash: u64) -> u64 {
    type_hash ^ egui::Id::unique(type_hash).value()
}

/// The key of a folder's entry (`path_hash` in `helper.rs`).
fn folder_key(path: &CurrentPath, type_hash: u64) -> u64 {
    type_hash ^ egui::Id::unique(path).with(type_hash).value()
}

impl LegacyMemory {
    /// Reads the value eframe stores under its `egui` key.
    pub fn parse(egui_memory: &str) -> Option<Self> {
        let memory: MemoryRon = ron::from_str(egui_memory).ok()?;
        let entries = memory
            .data
            .0
            .into_iter()
            .filter_map(|(key, element)| {
                Some(Entry {
                    key,
                    type_hash: element.type_id.0,
                    kind: Kind::of(&element.ron)?,
                    ron: element.ron,
                    generation: element.generation,
                })
            })
            .collect();
        Some(Self { entries })
    }

    /// The most recently read entry of `kind` whose key matches.
    fn find<T: serde::de::DeserializeOwned>(
        &self,
        kind: Kind,
        key_of: impl Fn(u64) -> u64,
    ) -> Option<T> {
        self.entries
            .iter()
            .filter(|entry| entry.kind == kind && entry.key == key_of(entry.type_hash))
            .min_by_key(|entry| entry.generation)
            .and_then(|entry| ron::from_str(&entry.ron).ok())
    }

    pub fn global(&self) -> GlobalValues {
        GlobalValues {
            favorites: self.find(Kind::Favorites, global_key),
            saved_searches: self.find(Kind::SavedSearches, global_key),
            view_settings: self.find(Kind::ViewSettings, global_key),
            show_hidden: self.find(Kind::ShowHidden, global_key),
            columns: self.find(Kind::Columns, global_key),
        }
    }

    pub fn folder(&self, path: &CurrentPath) -> FolderValues {
        let key = |type_hash| folder_key(path, type_hash);
        FolderValues {
            view_settings: self.find(Kind::ViewSettings, key),
            show_hidden: self.find(Kind::ShowHidden, key),
            columns: self.find(Kind::Columns, key),
        }
    }
}

static SAVED: OnceLock<Option<LegacyMemory>> = OnceLock::new();

/// Keeps the memory eframe loaded at launch, for [`saved`].
pub fn remember(egui_memory: Option<&str>) {
    let _ = SAVED.set(egui_memory.and_then(LegacyMemory::parse));
}

/// The memory loaded at launch, if [`remember`] was given one.
pub fn saved() -> Option<&'static LegacyMemory> {
    SAVED.get().and_then(Option::as_ref)
}

/// `T`'s value for `path` in the memory loaded at launch.
pub fn folder_value<T: std::any::Any>(path: &CurrentPath) -> Option<T> {
    let values = saved()?.folder(path);
    let candidates: [Option<Box<dyn std::any::Any>>; 3] = [
        values
            .view_settings
            .map(|v| Box::new(v) as Box<dyn std::any::Any>),
        values
            .show_hidden
            .map(|v| Box::new(v) as Box<dyn std::any::Any>),
        values
            .columns
            .map(|v| Box::new(v) as Box<dyn std::any::Any>),
    ];
    candidates
        .into_iter()
        .flatten()
        .find_map(|value| value.downcast::<T>().ok().map(|value| *value))
}

/// Stores `value` unless this build already has one of its type.
fn fill<T>(ctx: &egui::Context, value: Option<T>)
where
    T: 'static + Clone + Default + Send + Sync + egui::util::id_type_map::SerializableAny,
{
    use crate::helper::DataHolder;
    if let Some(value) = value
        && ctx.data_get_persisted::<T>().is_none()
    {
        ctx.data_set_persisted(value);
    }
}

/// Stores the app-wide values an earlier build saved, where this build has none.
pub fn import_globals(ctx: &egui::Context) {
    let Some(saved) = saved() else {
        return;
    };
    let values = saved.global();
    fill(ctx, values.favorites);
    fill(ctx, values.saved_searches);
    fill(ctx, values.view_settings);
    fill(ctx, values.show_hidden);
    fill(ctx, values.columns);
}
