//! `tests/fixtures/app_v0.ron` holds the state this build saves: the `app`
//! value and the domain data kept in egui's persisted memory (favorites,
//! saved searches, view settings, columns). Later builds must still read it.
//!
//! egui keys those entries by a hash of each type's `TypeId`, which changes
//! with every dependency change, so they are read through
//! [`crate::legacy_import`], never by type. The fixture was written by the
//! build before the Crux migration and must not be regenerated.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::app::assets::IconSize;
use crate::app::directory_view_settings::{DirectoryShowHidden, DirectoryViewSettings};
use crate::app::dock::{Column, ListColumns};
use crate::app::dock::{CurrentPath, TabData, Workspace};
use crate::app::search::SavedScope;
use crate::app::settings::SidebarSection;
use crate::app::{App, DisplayType, SavedSearch, SavedSearches, Search, Sort};
use crate::helper::DataHolder;
use crate::locations::{Location, Locations};

const FIXTURE: &str = "tests/fixtures/app_v0.ron";
const PROJECTS: &str = "/fixture/Projects";
const PHOTOS: &str = "/fixture/Photos";

/// eframe's storage, in memory.
#[derive(Default)]
struct Memory(HashMap<String, String>);
impl eframe::Storage for Memory {
    fn get_string(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
    fn set_string(&mut self, key: &str, value: String) {
        self.0.insert(key.to_owned(), value);
    }
    fn remove_string(&mut self, key: &str) {
        self.0.remove(key);
    }
    fn flush(&mut self) {}
}

fn favorites() -> Locations {
    Locations {
        locations: vec![
            Location::from_path(PROJECTS, "Projects"),
            Location::from_path(PHOTOS, "Photos"),
        ],
    }
}

fn saved_searches() -> SavedSearches {
    SavedSearches {
        searches: vec![
            SavedSearch {
                id: 1,
                name: "Invoices".into(),
                query: Search {
                    chips: crate::app::search::query::parse("invoice ext:pdf").chips,
                    ..Default::default()
                },
                scope: SavedScope::Favorites,
            },
            SavedSearch {
                id: 2,
                name: "Raw photos".into(),
                query: Search {
                    chips: crate::app::search::query::parse("ext:dng").chips,
                    ..Default::default()
                },
                scope: SavedScope::Folders(vec![PathBuf::from(PHOTOS)]),
            },
        ],
    }
}

fn global_view() -> DirectoryViewSettings {
    DirectoryViewSettings {
        sorting: Sort::Modified,
        display_type: DisplayType::List,
        invert_sort: true,
    }
}

fn photos_view() -> DirectoryViewSettings {
    DirectoryViewSettings {
        sorting: Sort::Size,
        display_type: DisplayType::Icons,
        invert_sort: false,
    }
}

fn columns() -> ListColumns {
    ListColumns {
        shown: vec![Column::Size, Column::Kind, Column::Modified],
        widths: vec![(Column::Kind, 140.0)],
    }
}

fn tab(folder: &str) -> TabData {
    let mut tab = TabData::from_path(Path::new(folder));
    tab.current_path = CurrentPath::One(folder.into());
    tab
}

fn fixture_app() -> App {
    let tabs = vec![tab(PROJECTS), tab(PHOTOS), tab("/fixture/Music")];
    let music = tabs[2].id;
    let mut workspace = Workspace::from_tabs(tabs);
    workspace.new_window(vec![music], None);
    let mut app = App {
        tabs: workspace,
        recent_folders: vec![PROJECTS.into(), PHOTOS.into()],
        // Read from the machine at launch; kept out so the fixture is portable.
        user_locations: Locations::default(),
        ..Default::default()
    };
    app.settings.icon_size = IconSize::Large;
    app.settings.sidebar_visible = false;
    app.settings.confirm_trash = false;
    app.settings.terminal_path = "Ghostty".into();
    app.settings.sidebar_order = vec![
        SidebarSection::Locations,
        SidebarSection::Favorites,
        SidebarSection::SavedSearches,
        SidebarSection::Recent,
        SidebarSection::Drives,
    ];
    app.settings.collapsed_sections = vec![SidebarSection::Recent];
    app
}

fn fixture_memory() -> egui::Context {
    let ctx = egui::Context::default();
    ctx.data_set_persisted(favorites());
    ctx.data_set_persisted(saved_searches());
    ctx.data_set_persisted(global_view());
    ctx.data_set_persisted(DirectoryShowHidden(false));
    ctx.data_set_persisted(columns());
    let photos = CurrentPath::One(PHOTOS.into());
    ctx.data_set_path(&photos, photos_view());
    ctx.data_set_path(&photos, DirectoryShowHidden(true));
    ctx
}

#[test]
#[ignore = "writes tests/fixtures/app_v0.ron; run after changing the fixture"]
fn write_app_v0_fixture() {
    let _guard = egui_mev_test::snapshot_lock();
    let mut storage = Memory::default();
    let mut app = fixture_app();
    eframe::App::save(&mut app, &mut storage);
    fixture_memory().memory(|memory| eframe::set_value(&mut storage, "egui", memory));
    // As eframe's file storage writes it.
    let text = ron::ser::to_string_pretty(&storage.0, ron::ser::PrettyConfig::default())
        .expect("serialize fixture");
    std::fs::create_dir_all("tests/fixtures").expect("create fixtures folder");
    std::fs::write(FIXTURE, text).expect("write fixture");
}

#[test]
fn app_v0_fixture_loads() {
    let _guard = egui_mev_test::snapshot_lock();
    let text = std::fs::read_to_string(FIXTURE).expect("read fixture");
    let storage = Memory(ron::from_str(&text).expect("fixture is eframe storage"));

    let app: App = eframe::get_value(&storage, eframe::APP_KEY).expect("app value");
    assert_eq!(app.settings.icon_size, IconSize::Large);
    assert!(!app.settings.sidebar_visible);
    assert!(!app.settings.confirm_trash);
    assert_eq!(app.settings.terminal_path, "Ghostty");
    assert_eq!(
        app.settings.collapsed_sections,
        vec![SidebarSection::Recent]
    );
    assert_eq!(app.recent_folders.len(), 2);
    assert_eq!(app.windows.len(), 2, "both windows are saved");

    let memory = eframe::Storage::get_string(&storage, "egui").expect("egui memory");
    let memory = crate::legacy_import::LegacyMemory::parse(&memory).expect("egui memory parses");
    let global = memory.global();
    let favorites = global.favorites.expect("favorites");
    assert_eq!(
        favorites.paths(),
        vec![PathBuf::from(PROJECTS), PathBuf::from(PHOTOS)]
    );
    let searches = global.saved_searches.expect("saved searches");
    assert_eq!(
        searches
            .searches
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["Invoices", "Raw photos"]
    );
    let view = global.view_settings.expect("global view settings");
    assert_eq!(view.sorting, Sort::Modified);
    assert!(view.invert_sort);
    assert!(!global.show_hidden.expect("global hidden files").0);
    let columns = global.columns.expect("columns");
    assert_eq!(
        columns.shown,
        vec![Column::Size, Column::Kind, Column::Modified]
    );

    let photos = memory.folder(&CurrentPath::One(PHOTOS.into()));
    let local = photos.view_settings.expect("per-folder view settings");
    assert_eq!(local.display_type, DisplayType::Icons);
    assert!(photos.show_hidden.expect("per-folder hidden files").0);
    let projects = memory.folder(&CurrentPath::One(PROJECTS.into()));
    assert!(
        projects.view_settings.is_none(),
        "a folder without its own settings gets none"
    );
}

/// What `App::new` does with saved memory: values saved under other `TypeId`s
/// end up readable by this build.
#[test]
fn v0_values_are_imported_into_this_build() {
    let _guard = egui_mev_test::snapshot_lock();
    let text = std::fs::read_to_string(FIXTURE).expect("read fixture");
    let storage = Memory(ron::from_str(&text).expect("fixture is eframe storage"));
    // The process-wide memory is set once; the fixture's folders appear in no other test.
    crate::legacy_import::remember(eframe::Storage::get_string(&storage, "egui").as_deref());

    let ctx = egui::Context::default();
    crate::legacy_import::import_globals(&ctx);
    assert_eq!(
        ctx.data_get_persisted::<Locations>()
            .expect("favorites imported")
            .paths()
            .len(),
        2
    );
    assert!(ctx.data_get_persisted::<SavedSearches>().is_some());

    let photos = CurrentPath::One(PHOTOS.into());
    let view = ctx.data_get_path_or_persisted::<DirectoryViewSettings>(&photos);
    assert!(view.is_local(), "the folder's own settings are found");
    assert_eq!(view.display_type, DisplayType::Icons);
    let projects = CurrentPath::One(PROJECTS.into());
    let view = ctx.data_get_path_or_persisted::<DirectoryViewSettings>(&projects);
    assert!(
        view.is_global(),
        "other folders fall back to the imported default"
    );
    assert_eq!(view.sorting, Sort::Modified);
}
