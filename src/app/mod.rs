use crate::app::directory_path_info::DirectoryPathInfo;
use crate::app::directory_view_settings::DirectoryViewSettings;
use crate::app::dock::CurrentPath;
use crate::helper::{DataHolder, KeyWithCommandPressed};
use crate::locations::Locations;
use crate::{app::settings::ApplicationSettings, locations::Location};
use command_palette::CommandPalette;
use commands::{ActionToPerform, ModalWindow};
use egui::TextBuffer;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::str::FromStr;
use std::{fs, path::PathBuf};

mod central_panel;
pub mod command_palette;
pub mod commands;
mod dir_handling;
pub mod directory_path_info;
mod directory_view_settings;
pub mod dock;
mod settings;
mod side_panel;
mod top_bottom;

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
}

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
        // let action_name = format!("{:?}", action);
        // eprintln!("start {}", &action_name);
        // let start_time = std::time::Instant::now();
        match action {
            ActionToPerform::ChangePaths(path) => {
                let Some(tab) = self.tabs.get_current_tab() else {
                    return;
                };
                tab.set_path(path);
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
                self.handle_action(ctx, ActionToPerform::RequestFilesRefresh);
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
                self.handle_action(ctx, ActionToPerform::RequestFilesRefresh);
            }
            ActionToPerform::RequestFilesRefresh => {
                let Some(tab) = self.tabs.get_current_tab() else {
                    return;
                };
                tab.update_settings(ctx);
                tab.refresh_list();
                self.handle_action(ctx, ActionToPerform::FilesSort);
            }
            ActionToPerform::FilesSort | ActionToPerform::ViewSettingsChanged(_) => {
                let Some(tab) = self.tabs.get_current_tab() else {
                    return;
                };
                let settings =
                    ctx.data_get_path_or_persisted::<DirectoryViewSettings>(&tab.current_path);
                tab.sort_entries(&settings.data);
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
                let path_buf = PathBuf::from_str(&path).unwrap();
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
            ActionToPerform::SearchInFavorites(start) => {
                let favorites = ctx.data_get_persisted::<Locations>().unwrap_or_default();
                if favorites.locations.is_empty() {
                    return;
                }
                let path = if start {
                    ActionToPerform::ChangePaths(CurrentPath::Multiple(favorites.paths()))
                } else {
                    let Some(tab) = self.tabs.get_current_tab() else {
                        return;
                    };
                    if !tab.can_undo() {
                        return;
                    }

                    let Some(previous_path_action) = tab.undo() else {
                        return;
                    };
                    previous_path_action
                };
                self.handle_action(ctx, path);
            }
            ActionToPerform::FilterChanged => {
                self.handle_action(ctx, ActionToPerform::RequestFilesRefresh);
            }
            // ActionToPerform::ViewSettingsChanged(_) => {
            //     self.handle_action(ctx, ActionToPerform::FilesSort);
            // }
            ActionToPerform::SystemOpen(cow) => {
                let _ = open::that_detached(cow.as_str());
            }
        }
        // let duration = start_time.elapsed();
        // eprintln!("handle_action {} took: {:?}", action_name, duration);
    }
}

impl eframe::App for App {
    /// Called by the frame work to save state before shutdown.
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, self);
    }

    /// Called each time the UI needs repainting, which may be many times per second.
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut command_request = self.top_panel(ctx);
        if command_request.is_none() {
            command_request = self.bottom_panel(ctx);
        }
        if command_request.is_none() {
            command_request = self.left_side_panel(ctx);
        }
        if command_request.is_none() {
            command_request = self.central_panel(ctx);
        }

        if ctx.key_with_command_pressed(egui::Key::P) {
            command_request = Some(ActionToPerform::ToggleModalWindow(ModalWindow::Settings));
        }

        if ctx.key_with_command_pressed(egui::Key::L) {
            command_request = Some(ActionToPerform::ToggleTopEdit);
        }
        if let Some(current_tab) = self.tabs.get_current_tab() {
            if command_request.is_none() && current_tab.action_to_perform.is_some() {
                command_request.clone_from(&current_tab.action_to_perform);
            }
        }
        if let Some(current_path) = self.tabs.get_current_path() {
            let favorites = ctx.data_get_persisted::<Locations>().unwrap_or_default();
            if ctx.key_with_command_pressed(egui::Key::R) {
                command_request = Some(ActionToPerform::ToggleModalWindow(ModalWindow::Commands));
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
                    command_request = self.settings.display(ctx);
                }
                ModalWindow::Commands => {
                    command_request = self.command_palette.ui(ctx);
                    if command_request.is_some() {
                        self.display_modal = None;
                    }
                } // ModalWindow::NewDirectory => todo!(),
            }
        }

        TOASTS.write().show(ctx);
        let Some(action) = command_request else {
            return;
        };
        self.handle_action(ctx, action);
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

    ctx.style_mut(|style| {
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
