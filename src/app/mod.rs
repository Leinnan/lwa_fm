use crate::locations::Locations;
use egui::ahash::{HashMap, HashMapExt};
use std::{fs, path::PathBuf};

mod central_panel;
mod dir_handling;
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

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(default)] // if we add new fields, give them default values when deserializing old state
pub struct App {
    show_hidden: bool,
    #[serde(skip)]
    cur_path: PathBuf,
    sorting: Sort,
    invert_sort: bool,
    locations: HashMap<String, Locations>,
    #[serde(skip)]
    list: Vec<walkdir::DirEntry>,
    #[serde(skip)]
    search: Search,
    #[serde(skip)]
    dir_has_cargo: bool,
}

#[derive(serde::Deserialize, serde::Serialize, Default, PartialEq, Eq, Debug, Clone, Copy)]
pub enum Sort {
    #[default]
    Name,
    Modified,
    Created,
    Size,
    Random,
}

#[derive(serde::Deserialize, serde::Serialize, Default)]
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
        let mut p = Self {
            show_hidden: false,
            cur_path: get_starting_path(),
            locations,
            sorting: Sort::Created,
            list: vec![],
            search: Search {
                visible: false,
                case_sensitive: false,
                depth: 3,
                favorites: false,
                value: String::new(),
            },
            invert_sort: false,
            dir_has_cargo: false,
        };
        p.refresh_list();
        p
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
    #[allow(clippy::too_many_lines)] // todo: refactor
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Put your widgets into a `SidePanel`, `TopBottomPanel`, `CentralPanel`, `Window` or `Area`.
        // For inspiration and more examples, go to https://emilk.github.io/egui
        let mut new_path = None;
        let mut search_changed = false;
        self.top_panel(ctx, &mut new_path);
        self.bottom_panel(ctx, &mut search_changed);

        self.left_side_panel(ctx, &mut new_path);

        self.central_panel(ctx, &mut search_changed, &mut new_path);
        if search_changed {
            self.refresh_list();
        }

        if let Some(new) = new_path {
            self.change_current_dir(new);
        }

        TOASTS.write().show(ctx);
    }
}

fn setup_custom_fonts(ctx: &egui::Context) {
    // Start with the default fonts (we will be adding to them rather than replacing them).
    let mut fonts = egui::FontDefinitions::default();
    if let Some((regular, semibold)) = get_fonts() {
        fonts
            .font_data
            .insert("regular".to_owned(), egui::FontData::from_owned(regular));
        fonts
            .font_data
            .insert("semibold".to_owned(), egui::FontData::from_owned(semibold));

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

fn get_fonts() -> Option<(Vec<u8>, Vec<u8>)> {
    let Ok(app_data) = std::env::var("APPDATA") else {
        return None;
    };
    let font_path = std::path::Path::new(&app_data);

    let Ok(regular) = fs::read(font_path.join("../Local/Microsoft/Windows/Fonts/aptos.ttf")) else {
        return None;
    };
    let Ok(semibold) =
        fs::read(font_path.join("../Local/Microsoft/Windows/Fonts/aptos-semibold.ttf"))
    else {
        return None;
    };

    Some((regular, semibold))
}

fn get_starting_path() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        return PathBuf::from(args[1].clone());
    }
    std::env::current_dir().expect("Could not get current_dir")
}
