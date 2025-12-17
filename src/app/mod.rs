use crate::app::commands::{COMMANDS_QUEUE, TabAction, TabTarget};
use crate::app::directory_path_info::DirectoryPathInfo;
use crate::app::directory_view_settings::DirectoryViewSettings;
use crate::app::dock::CurrentPath;
use crate::data::files::DirEntry;
use crate::helper::{DataHolder, KeyWithCommandPressed};
use crate::locations::Locations;
use crate::watcher::DirectoryWatchers;
use crate::{app::settings::ApplicationSettings, locations::Location};
use command_palette::CommandPalette;
use commands::{ActionToPerform, ModalWindow};
use egui::TextBuffer;
use mlua::Lua;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::cell::RefCell;
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::str::FromStr;
use std::{fs, path::PathBuf};

mod central_panel;
pub mod command_palette;
pub mod commands;
pub mod database;
pub mod dir_handling;
pub mod directory_path_info;
mod directory_view_settings;
pub mod dock;
mod settings;
mod side_panel;
mod top_bottom;

thread_local! {
    pub static LUA_INSTANCE: RefCell<Lua> = RefCell::new({
        Lua::new()
    });
}
// fn print_from_lua() {
//     LUA_INSTANCE.with_borrow(|lua| match lua.load("print(\"Hello!\")").exec() {
//         Ok(_) => println!("Lua print executed successfully"),
//         Err(err) => println!("Lua print failed: {}", err),
//     });
// }

pub static TOASTS: std::sync::LazyLock<egui::mutex::RwLock<egui_notify::Toasts>> =
    std::sync::LazyLock::new(|| {
        egui::mutex::RwLock::new(
            egui_notify::Toasts::new().with_anchor(egui_notify::Anchor::TopRight),
        )
    });
#[macro_export]
macro_rules! toast{
        (Basic, $($format:expr),+) => {
            $crate::app::TOASTS.write().basic(format!($($format),+));
        };
        (Info, $($format:expr),+) => {
            $crate::app::TOASTS.write().info(format!($($format),+));
        };
        (Warning, $($format:expr),+) => {
            $crate::app::TOASTS.write().warning(format!($($format),+));
        };
        (Error, $($format:expr),+) => {
            $crate::app::TOASTS.write().error(format!($($format),+));
        };
        (Success, $($format:expr),+) => {
            $crate::app::TOASTS.write().success(format!($($format),+));
        };
    }

#[derive(Deserialize, Serialize)]
#[serde(default)] // if we add new fields, give them default values when deserializing old state
pub struct App {
    user_locations: Locations,
    #[cfg(not(target_os = "macos"))]
    drives_locations: Locations,
    #[serde(skip)]
    tabs: crate::app::dock::MyTabs,
    pub settings: ApplicationSettings,
    #[serde(skip)]
    display_modal: Option<ModalWindow>,
    #[serde(skip)]
    command_palette: CommandPalette,
    #[serde(skip, default)]
    pub watchers: DirectoryWatchers,
}

impl App {}

#[derive(Deserialize, Serialize, Default, PartialEq, Eq, Debug, Clone, Copy)]
pub enum Sort {
    #[default]
    Name,
    Modified,
    Created,
    Size,
    Random,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
pub struct Search {
    pub value: String,
    pub depth: usize,
    pub case_sensitive: bool,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum DataSource {
    Settings,
    Local,
    #[default]
    Generated,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
pub struct Data<T> {
    pub data: T,
    pub source: DataSource,
}

#[allow(dead_code)]
impl<T> Data<T> {
    /// Returns true if the data is from the local source.
    pub const fn is_local(&self) -> bool {
        matches!(self.source, DataSource::Local)
    }
    /// Returns true if the data is from the global source.
    pub const fn is_global(&self) -> bool {
        matches!(self.source, DataSource::Settings)
    }
    /// Creates a new data instance with the local source.
    pub const fn from_local(data: T) -> Self {
        Self {
            data,
            source: DataSource::Local,
        }
    }
    /// Creates a new data instance with the global source.
    pub const fn from_settings(data: T) -> Self {
        Self {
            data,
            source: DataSource::Settings,
        }
    }
    /// Creates a new data instance with the generated source.
    pub const fn generated(data: T) -> Self {
        Self {
            data,
            source: DataSource::Generated,
        }
    }
}

impl<T> Deref for Data<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}
impl<T> DerefMut for Data<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}

impl Default for App {
    fn default() -> Self {
        #[cfg(not(target_os = "macos"))]
        let drives_locations = Locations::get_drives();

        let command_palette = CommandPalette::default();
        Self {
            #[cfg(not(target_os = "macos"))]
            drives_locations,
            user_locations: Locations::get_user_dirs(),
            tabs: crate::app::dock::MyTabs::new(&get_starting_path()),
            settings: ApplicationSettings::default(),
            display_modal: None,
            command_palette,
            watchers: DirectoryWatchers::default(),
        }
    }
}

impl App {
    fn load_locations(&mut self) {
        #[cfg(not(target_os = "macos"))]
        {
            self.drives_locations = Locations::get_drives();
        }
        self.user_locations = Locations::get_user_dirs();
    }
    /// Called once before the first frame.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_custom_fonts(&cc.egui_ctx);
        // This is also where you can customize the look and feel of egui using
        // `cc.egui_ctx.set_visuals` and `cc.egui_ctx.set_fonts`.

        // Load previous app state (if any).
        // Note that you must enable the `persistence` feature for this to work.
        if let Some(storage) = cc.storage {
            let mut value: Self = eframe::get_value(storage, eframe::APP_KEY).unwrap_or_default();

            value.load_locations();
            value.tabs = crate::app::dock::MyTabs::new(&get_starting_path());
            return value;
        }

        Self::default()
    }

    #[allow(clippy::too_many_lines)]
    fn handle_action(&mut self, ctx: &egui::Context, action: ActionToPerform) {
        #[cfg(feature = "profiling")]
        puffin::profile_function!("lwa_fm::handle_action");

        match action {
            ActionToPerform::TabAction(target, action) => {
                if target == TabTarget::AllTabs {
                    let tabs_ids = self.tabs.get_tab_ids();
                    for id in tabs_ids {
                        ActionToPerform::TabAction(TabTarget::TabWithId(id), action.clone())
                            .schedule();
                    }
                    return;
                }
                let Some(tab) = self.tabs.try_get_tab_by_target(target) else {
                    return;
                };
                match action {
                    commands::TabAction::ChangePaths(path) => {
                        #[cfg(feature = "profiling")]
                        puffin::profile_scope!(
                            "lwa_fm::handle_action::ChangePaths: {}",
                            path.get_name_from_path()
                        );
                        if let Some(old_path) = tab.current_path.get_path() {
                            self.watchers.stop(&old_path);
                        }

                        path.print_from_lua();
                        let new_path = tab.set_path(path);
                        if let Some(new_path) = new_path.get_path() {
                            _ = self.watchers.start(new_path);
                        }
                        if let Some(data) = ctx.data_get_tab::<DirectoryPathInfo>(tab.id) {
                            let new_data = match tab.current_path.single_path() {
                                Some(p) => {
                                    if Path::new(&data.text_input).eq(p.as_path()) {
                                        Some(DirectoryPathInfo::build(p.as_path(), false))
                                    } else {
                                        None
                                    }
                                }
                                None => None,
                            };
                            match new_data {
                                Some(s) => ctx.data_set_tab(tab.id, s),
                                None => ctx.data_remove_tab::<DirectoryPathInfo>(tab.id),
                            }
                        }
                        self.handle_action(
                            ctx,
                            ActionToPerform::TabAction(target, TabAction::RequestFilesRefresh),
                        );
                    }
                    commands::TabAction::FilterChanged => {
                        #[cfg(feature = "profiling")]
                        puffin::profile_scope!("lwa_fm::handle_action::FilterChanged");
                        tab.update_settings(ctx);
                        tab.update_visible_entries();
                    }
                    commands::TabAction::RequestFilesRefresh => {
                        #[cfg(feature = "profiling")]
                        puffin::profile_scope!("lwa_fm::handle_action::RefreshFiles");
                        tab.update_settings(ctx);
                        tab.refresh_list();
                        dock::populate_time_pool(
                            tab.list.iter().map(|e| e.meta.since_modified),
                            ctx,
                        );
                        dock::populate_sizes_pool(tab.list.iter().map(|e| e.meta.size), ctx);

                        // _ = thread::spawn(move || {
                        //     thread::sleep(Duration::from_secs_f32(0.2));
                        //     COMMANDS_QUEUE
                        //         .push(ActionToPerform::TabAction(target, TabAction::FilesSort));
                        // });
                        self.handle_action(
                            ctx,
                            ActionToPerform::TabAction(target, TabAction::FilesSort),
                        );
                    }
                    commands::TabAction::FilesSort => {
                        #[cfg(feature = "profiling")]
                        puffin::profile_scope!("lwa_fm::handle_action::FilesSort");

                        let settings = ctx
                            .data_get_path_or_persisted::<DirectoryViewSettings>(&tab.current_path);
                        tab.sort_entries(&settings.data);
                        tab.update_visible_entries();
                    }
                    commands::TabAction::SearchInFavorites(start) => {
                        let favorites = ctx.data_get_persisted::<Locations>().unwrap_or_default();
                        if favorites.locations.is_empty() {
                            return;
                        }
                        if start {
                            self.handle_action(
                                ctx,
                                ActionToPerform::TabAction(
                                    target,
                                    TabAction::ChangePaths(CurrentPath::Multiple(
                                        favorites.paths(),
                                    )),
                                ),
                            );
                        } else {
                            if !tab.can_undo() {
                                return;
                            }

                            let Some(previous_path_action) = tab.undo() else {
                                return;
                            };
                            self.handle_action(ctx, previous_path_action);
                        }
                    }
                }
            }
            ActionToPerform::NewTab(path) => self.tabs.open_in_new_tab(&path),
            ActionToPerform::OpenInTerminal(path_buf) => {
                match self.settings.open_in_terminal(&path_buf) {
                    Ok(_) => {
                        toast!(Success, "Open in terminal");
                    }
                    Err(_) => {
                        toast!(Error, "Failed to open directory");
                    }
                }
            }
            ActionToPerform::CloseActiveModalWindow => {
                self.display_modal = None;
                TabAction::RequestFilesRefresh.schedule_active_tab();
            }
            ActionToPerform::ViewSettingsChanged(_) => {
                TabAction::FilesSort.schedule_active_tab();
            }
            ActionToPerform::ToggleModalWindow(modal_window) => {
                if let Some(modal) = &self.display_modal {
                    if modal.eq(&modal_window) {
                        self.display_modal = None;
                    } else {
                        self.display_modal = Some(modal_window);
                    }
                } else {
                    self.display_modal = Some(modal_window);
                }
            }
            ActionToPerform::ToggleTopEdit => {
                let current_path = self.tabs.get_current_path();
                let index = self.tabs.get_current_index().unwrap_or_default();

                match ctx.data_get_tab::<DirectoryPathInfo>(index) {
                    Some(_) => ctx.data_remove_tab::<DirectoryPathInfo>(index),
                    None => {
                        if let Some(path) = current_path {
                            ctx.data_set_tab(
                                index,
                                DirectoryPathInfo::build(path.as_path(), false),
                            );
                        }
                    }
                }
            }
            ActionToPerform::AddToFavorites(path) => {
                let mut favorites = ctx.data_get_persisted::<Locations>().unwrap_or_default();
                if favorites
                    .locations
                    .iter()
                    .any(|location| location.path == path)
                {
                    return;
                }
                let path_buf = PathBuf::from_str(&path).unwrap();
                let Some(name) = path_buf.iter().next_back() else {
                    toast!(Error, "Could not get name of file");
                    return;
                };
                favorites.locations.push(Location {
                    name: Cow::Owned(name.to_string_lossy().to_string()),
                    path,
                });
                ctx.data_set_persisted(favorites);
            }
            ActionToPerform::RemoveFromFavorites(path_buf) => {
                let mut favorites = ctx.data_get_persisted::<Locations>().unwrap_or_default();
                favorites.locations.retain(|s| s.path != path_buf);
                ctx.data_set_persisted(favorites);
            }
            ActionToPerform::SystemOpen(cow) => {
                let _ = open::that_detached(cow.as_str());
            }
        }
    }
}

impl eframe::App for App {
    /// Called by the frame work to save state before shutdown.
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, self);
    }

    /// Called each time the UI needs repainting, which may be many times per second.
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(feature = "profiling")]
        puffin::profile_function!("my_update");
        self.top_panel(ctx);
        self.bottom_panel(ctx);
        self.left_side_panel(ctx);
        self.central_panel(ctx);

        if ctx.key_with_command_pressed(egui::Key::P) {
            ActionToPerform::ToggleModalWindow(ModalWindow::Settings).schedule();
        }

        if ctx.key_with_command_pressed(egui::Key::L) {
            ActionToPerform::ToggleTopEdit.schedule();
        }
        if let Some(current_path) = self.tabs.get_current_path() {
            let favorites = ctx.data_get_persisted::<Locations>().unwrap_or_default();
            if ctx.key_with_command_pressed(egui::Key::R) {
                ActionToPerform::ToggleModalWindow(ModalWindow::Commands).schedule();
                self.command_palette.build_for_path(
                    &CurrentPath::One(current_path.clone()),
                    &current_path,
                    &favorites,
                );
            }
        }

        if let Some(modal) = &self.display_modal {
            match modal {
                ModalWindow::Settings => {
                    self.settings.display(ctx);
                }
                ModalWindow::Commands => {
                    self.command_palette.ui(ctx);
                } // ModalWindow::NewDirectory => todo!(),
                ModalWindow::Rename => {
                    let modal_response =
                        egui::Modal::new(egui::Id::new(ModalWindow::Rename)).show(ctx, |ui| {
                            ui.label("Old name");
                            let (old, mut name) = ui.data_mut(|d| {
                                let old =
                                    d.get_temp::<DirEntry>(egui::Id::new(ModalWindow::Rename));
                                let new = d
                                    .get_temp::<String>(
                                        egui::Id::new(ModalWindow::Rename).with("new"),
                                    )
                                    .unwrap_or_else(|| {
                                        old.as_ref()
                                            .map(|d| d.get_splitted_path().1.to_string())
                                            .unwrap_or_default()
                                    });
                                (old, new)
                            });
                            let Some(old) = old else {
                                return;
                            };
                            let mut old_file_name = old.get_splitted_path().1.to_string();
                            ui.add_enabled(false, egui::TextEdit::singleline(&mut old_file_name));
                            ui.label("New name");
                            ui.text_edit_singleline(&mut name);
                            let valid = !Path::new(&name).try_exists().is_ok_and(|f| f);
                            if ui.add_enabled(valid, egui::Button::new("Rename")).clicked() {
                                let _ = fs::rename(
                                    old.get_path(),
                                    Path::new(old.get_splitted_path().0).join(name),
                                );
                                ui.data_mut(|w| {
                                    w.remove_temp::<String>(
                                        egui::Id::new(ModalWindow::Rename).with("new"),
                                    )
                                });
                                ui.close();
                            } else {
                                ui.data_mut(|w| {
                                    w.insert_temp(
                                        egui::Id::new(ModalWindow::Rename).with("new"),
                                        name.clone(),
                                    );
                                });
                            }
                        });

                    if modal_response.should_close() {
                        ActionToPerform::CloseActiveModalWindow.schedule();
                    }
                }
            }
        }

        TOASTS.write().show(ctx);
        while let Some(action) = COMMANDS_QUEUE.pop() {
            self.handle_action(ctx, action);
        }
        {
            #[cfg(feature = "profiling")]
            puffin::profile_scope!("lwa_fm::MyTabs::ui::check_for_new_watchers");
            self.watchers.check_for_new_watchers();
        }
        {
            #[cfg(feature = "profiling")]
            puffin::profile_scope!("lwa_fm::MyTabs::ui::check_for_file_system_events");
            if self.watchers.check_for_file_system_events() {
                self.handle_action(
                    ctx,
                    ActionToPerform::TabAction(
                        commands::TabTarget::AllTabs,
                        TabAction::RequestFilesRefresh,
                    ),
                );
            }
        }
    }
}

fn setup_custom_fonts(ctx: &egui::Context) {
    // Start with the default fonts (we will be adding to them rather than replacing them).
    let mut fonts = egui::FontDefinitions::default();
    if let Ok((regular, semibold)) = get_fonts() {
        fonts.font_data.insert(
            "regular".to_owned(),
            egui::FontData::from_owned(regular).into(),
        );
        fonts.font_data.insert(
            "semibold".to_owned(),
            egui::FontData::from_owned(semibold).into(),
        );

        // Put my font first (highest priority) for proportional text:
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "regular".to_owned());
        fonts
            .families
            .entry(egui::FontFamily::Name("semibold".into()))
            .or_default()
            .insert(0, "semibold".to_owned());

        // Put my font as last fallback for monospace:
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .push("regular".to_owned());

        // Tell egui to use these fonts:
        ctx.set_fonts(fonts);
    }

    ctx.all_styles_mut(|style| {
        for font_id in style.text_styles.values_mut() {
            font_id.size *= 1.4;
        }
    });
}

#[cfg(not(windows))]
fn get_fonts() -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let font_path = std::path::Path::new("/System/Library/Fonts");

    let regular = fs::read(font_path.join("SFNSRounded.ttf"))?;
    let semibold = fs::read(font_path.join("SFCompact.ttf"))?;

    Ok((regular, semibold))
}

#[cfg(windows)]
fn get_fonts() -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let app_data = std::env::var("APPDATA")?;
    let font_path = std::path::Path::new(&app_data);

    let regular = fs::read(font_path.join("../Local/Microsoft/Windows/Fonts/aptos.ttf"))?;
    let semibold = fs::read(font_path.join("../Local/Microsoft/Windows/Fonts/aptos-semibold.ttf"))?;

    Ok((regular, semibold))
}

fn get_starting_path() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        return PathBuf::from(args[1].clone());
    }
    std::env::current_dir().expect("Could not get current_dir")
}
