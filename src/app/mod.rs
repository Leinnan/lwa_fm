use crate::app::settings::ApplicationSettings;
use crate::locations::Locations;
use egui::ahash::{HashMap, HashMapExt};
use serde::{Deserialize, Serialize};
use std::{cell::RefCell, collections::BTreeSet, fs, path::PathBuf, rc::Rc};

mod central_panel;
mod dir_handling;
mod directory_view_settings;
mod dock;
mod settings;
mod side_panel;
mod top_bottom;

pub static TOASTS: once_cell::sync::Lazy<egui::mutex::RwLock<egui_notify::Toasts>> =
    once_cell::sync::Lazy::new(|| {
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
    locations: Rc<RefCell<HashMap<String, Locations>>>,
    #[serde(skip)]
    tabs: crate::app::dock::MyTabs,
    #[serde(skip)]
    display_edit_top: bool,
    top_edit: String,
    possible_options: BTreeSet<String>,
    pub settings: ApplicationSettings,
    #[serde(skip)]
    display_settings: bool,
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
    pub favorites: bool,
    pub value: String,
    pub depth: usize,
    pub case_sensitive: bool,
}

impl Default for App {
    fn default() -> Self {
        let mut locations = HashMap::new();
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
        let locations = Rc::new(RefCell::new(locations));
        let clone = Rc::clone(&locations);
        Self {
            locations,
            tabs: crate::app::dock::MyTabs::new(&get_starting_path(), clone),
            display_edit_top: false,
            possible_options: BTreeSet::new(),
            top_edit: String::new(),
            settings: ApplicationSettings::default(),
            display_settings: true,
        }
    }
}

#[derive(Debug)]
pub struct NewPathRequest {
    pub new_tab: bool,
    pub path: PathBuf,
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
                let mut locations = value.locations.borrow_mut();
                if !locations.contains_key("Favorites") {
                    locations.insert(
                        "Favorites".into(),
                        Locations {
                            editable: true,
                            ..Default::default()
                        },
                    );
                }
                if let Some(user) = locations.get_mut("User") {
                    *user = Locations::get_user_dirs();
                }
                #[cfg(not(target_os = "macos"))]
                if let Some(drive) = locations.get_mut("Drives") {
                    *drive = Locations::get_drives();
                }
            }
            value.tabs =
                crate::app::dock::MyTabs::new(&get_starting_path(), Rc::clone(&value.locations));
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
        let mut new_path = None;
        let mut search_changed = false;
        self.top_panel(ctx, &mut new_path);
        self.bottom_panel(ctx, &mut search_changed);

        self.left_side_panel(ctx, &mut new_path);

        self.central_panel(ctx);
        if search_changed {
            self.tabs.refresh_list();
        }

        if let Some(new) = &new_path {
            if new.new_tab {
                self.tabs.open_in_new_tab(&new.path);
            } else {
                use crate::helper::PathFixer;
                self.top_edit = new.path.to_fixed_string();
                self.tabs.update_active_tab(&new.path);
            }
        }
        if ctx.input(|i| i.key_pressed(egui::Key::P)) {
            self.display_settings = true;
        }

        if self.display_settings {
            self.display_settings = !self.settings.display(ctx);
        }

        TOASTS.write().show(ctx);
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
