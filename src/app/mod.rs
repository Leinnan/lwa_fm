use crate::app::dock::CurrentPath;
use crate::helper::KeyWithCommandPressed;
use crate::locations::Locations;
use crate::{app::settings::ApplicationSettings, locations::Location};
use command_palette::CommandPalette;
use commands::{ActionToPerform, ModalWindow};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::str::FromStr;
use std::{fs, path::PathBuf};

mod central_panel;
pub mod command_palette;
pub mod commands;
mod dir_handling;
pub mod directory_path_info;
mod directory_view_settings;
mod dock;
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
    locations: BTreeMap<Cow<'static, str>, Locations>,
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

#[derive(Deserialize, Serialize, Default, Debug)]
pub struct Search {
    pub visible: bool,
    pub value: String,
    pub depth: usize,
    pub case_sensitive: bool,
}

impl Default for App {
    fn default() -> Self {
        let mut locations = BTreeMap::new();
        locations.insert("User".into(), Locations::get_user_dirs());
        #[cfg(not(target_os = "macos"))]
        locations.insert("Drives".into(), Locations::get_drives());
        locations.insert(
            "Favorites".into(),
            Locations {
                editable: true,
                ..Default::default()
            },
        );
        let command_palette = CommandPalette::default();
        Self {
            locations,
            tabs: crate::app::dock::MyTabs::new(&get_starting_path()),
            settings: ApplicationSettings::default(),
            display_modal: None,
            command_palette,
        }
    }
}

impl App {
    /// Called once before the first frame.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_custom_fonts(&cc.egui_ctx);
        // This is also where you can customize the look and feel of egui using
        // `cc.egui_ctx.set_visuals` and `cc.egui_ctx.set_fonts`.

        // Load previous app state (if any).
        // Note that you must enable the `persistence` feature for this to work.
        if let Some(storage) = cc.storage {
            let mut value: Self = eframe::get_value(storage, eframe::APP_KEY).unwrap_or_default();
            {
                if !value.locations.contains_key("Favorites") {
                    value.locations.insert(
                        "Favorites".into(),
                        Locations {
                            editable: true,
                            ..Default::default()
                        },
                    );
                }
                if let Some(user) = value.locations.get_mut("User") {
                    *user = Locations::get_user_dirs();
                }
                #[cfg(not(target_os = "macos"))]
                if let Some(drive) = value.locations.get_mut("Drives") {
                    *drive = Locations::get_drives();
                }
            }
            value.tabs = crate::app::dock::MyTabs::new(&get_starting_path());
            return value;
        }

        Self::default()
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
        if let Some(current_path) = self.tabs.get_current_path() {
            if ctx.key_with_command_pressed(egui::Key::R) {
                command_request = Some(ActionToPerform::ToggleModalWindow(ModalWindow::Commands));
                self.command_palette.build_for_path(&current_path);
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
        match action {
            ActionToPerform::ChangePath(path) => {
                self.tabs.update_active_tab(path);
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
            ActionToPerform::CloseActiveModalWindow => self.display_modal = None,
            ActionToPerform::RequestFilesRefresh => self.tabs.refresh_list(),
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
                let Some(tab) = self.tabs.get_current_tab() else {
                    return;
                };
                tab.toggle_top_edit();
            }
            ActionToPerform::AddToFavorites(path) => {
                let path_buf = PathBuf::from_str(&path).unwrap();
                if let Some(name) = path_buf.iter().next_back() {
                    if let Some(fav) = self.locations.get_mut("Favorites") {
                        fav.locations.push(Location {
                            name: name.to_string_lossy().to_string(),
                            path,
                        });
                    } else {
                        self.locations.insert(
                            "Favorites".into(),
                            Locations {
                                editable: true,
                                ..Default::default()
                            },
                        );
                        if let Some(fav) = self.locations.get_mut("Favorites") {
                            fav.locations.push(Location {
                                name: name.to_string_lossy().to_string(),
                                path,
                            });
                        } else {
                            toast!(Error, "Failed to add favorite");
                        }
                    }
                } else {
                    toast!(Error, "Could not get name of file");
                }
                ctx.data_mut(|d| {
                    d.insert_persisted(
                        "FavoritesPaths".into(),
                        self.locations
                            .get("Favorites")
                            .unwrap()
                            .locations
                            .iter()
                            .map(|s| s.path.clone())
                            .collect::<Vec<Cow<'static, str>>>(),
                    );
                });
            }
            ActionToPerform::RemoveFromFavorites(path_buf) => {
                self.locations
                    .get_mut("Favorites")
                    .unwrap()
                    .locations
                    .retain(|s| s.path != path_buf);
                ctx.data_mut(|d| {
                    d.insert_persisted(
                        "FavoritesPaths".into(),
                        self.locations
                            .get("Favorites")
                            .unwrap()
                            .locations
                            .iter()
                            .map(|s| s.path.clone())
                            .collect::<Vec<Cow<'static, str>>>(),
                    );
                });
            }
            ActionToPerform::SearchInFavorites(start) => {
                let favorites_paths: Vec<Cow<'static, str>> = ctx
                    .data_mut(|d| d.get_persisted("FavoritesPaths".into()))
                    .unwrap_or_default();
                if favorites_paths.is_empty() {
                    return;
                }
                let path = if start {
                    if let Some(old_path) = self.tabs.get_current_path() {
                        ctx.data_mut(|d| d.insert_temp("Previous".into(), old_path));
                    }
                    CurrentPath::Multiple(
                        favorites_paths
                            .iter()
                            .filter_map(|s| PathBuf::from_str(s).ok())
                            .collect(),
                    )
                } else if let Some(old_path) =
                    ctx.data::<Option<PathBuf>>(|d| d.get_temp("Previous".into()))
                {
                    CurrentPath::One(old_path)
                } else {
                    CurrentPath::One(PathBuf::from_str("/").unwrap())
                };
                self.tabs.update_active_tab(path);
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
