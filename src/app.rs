use crate::{
    consts::*,
    locations::{Location, Locations},
};
use egui::{
    ahash::{HashMap, HashMapExt},
    Layout, RichText,
};
use egui_extras::{Column, TableBuilder};
use std::{fs, path::PathBuf};
use walkdir::WalkDir;

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(default)] // if we add new fields, give them default values when deserializing old state
pub struct App {
    show_search_bar: bool,
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
}

#[derive(serde::Deserialize, serde::Serialize, Default, PartialEq, Debug, Clone, Copy)]
pub enum Sort {
    #[default]
    Name,
    Modified,
    Created,
}

#[derive(serde::Deserialize, serde::Serialize, Default)]
pub struct Search {
    pub favorites: bool,
    pub value: String,
    pub depth: usize,
    pub case_sensitive: bool,
}

impl Default for App {
    fn default() -> Self {
        let mut locations = HashMap::new();
        locations.insert("User".into(), Locations::get_user_dirs());
        locations.insert("Drives".into(), Locations::get_drives());
        locations.insert("Favorites".into(), Locations(vec![], true));
        let mut p = Self {
            show_search_bar: false,
            show_hidden: false,
            cur_path: get_starting_path(),
            locations,
            sorting: Sort::Created,
            list: vec![],
            search: Search {
                case_sensitive: false,
                depth: 3,
                favorites: false,
                value: String::new(),
            },
            invert_sort: false,
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
                value
                    .locations
                    .insert("Favorites".into(), Locations(vec![], true));
            }
            if let Some(user) = value.locations.get_mut("User") {
                *user = Locations::get_user_dirs();
            }
            if let Some(drive) = value.locations.get_mut("Drives") {
                *drive = Locations::get_drives();
            }
            return value;
        }

        Default::default()
    }
}

impl App {
    fn change_current_dir(&mut self, new_path: PathBuf) {
        self.cur_path = new_path;
        self.refresh_list();
    }
    fn refresh_list(&mut self) {
        self.list = self.read_dir();
    }

    fn read_dir(&self) -> Vec<walkdir::DirEntry> {
        let search = &self.search.value;
        let use_search = !self.search.value.is_empty();
        let directories = if use_search && self.search.favorites {
            self.locations
                .get("Favorites")
                .unwrap()
                .0
                .iter()
                .map(|location| &location.path)
                .collect()
        } else {
            [&self.cur_path].to_vec()
        };

        let depth = if use_search { self.search.depth } else { 1 };
        let mut dir_entries: Vec<walkdir::DirEntry> = directories
            .iter()
            .flat_map(|d| {
                WalkDir::new(d)
                    .follow_links(true)
                    .max_depth(depth)
                    .into_iter()
                    .flatten()
                    .skip(1)
                    .filter(|e| {
                        let s = e.file_name().to_string_lossy();
                        if !self.show_hidden && (s.starts_with('.') || s.starts_with('$')) {
                            return false;
                        }
                        if self.search.case_sensitive {
                            s.contains(search)
                        } else {
                            s.to_ascii_lowercase()
                                .contains(&search.to_ascii_lowercase())
                        }
                    })
                    .collect::<Vec<walkdir::DirEntry>>()
            })
            .collect();

        dir_entries.sort_by(|a, b| {
            a.file_type()
                .is_file()
                .cmp(&b.file_type().is_file())
                .then(match &self.sorting {
                    Sort::Name => a
                        .file_name()
                        .to_ascii_lowercase()
                        .cmp(&b.file_name().to_ascii_lowercase()),
                    Sort::Modified => a
                        .metadata()
                        .unwrap()
                        .modified()
                        .unwrap()
                        .cmp(&b.metadata().unwrap().modified().unwrap()),
                    Sort::Created => a
                        .metadata()
                        .unwrap()
                        .created()
                        .unwrap()
                        .cmp(&b.metadata().unwrap().created().unwrap()),
                })
        });
        if self.invert_sort {
            dir_entries.reverse();
        }
        dir_entries
    }
}

impl eframe::App for App {
    /// Called by the frame work to save state before shutdown.
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, self);
    }

    /// Called each time the UI needs repainting, which may be many times per second.
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Put your widgets into a `SidePanel`, `TopBottomPanel`, `CentralPanel`, `Window` or `Area`.
        // For inspiration and more examples, go to https://emilk.github.io/egui
        let mut new_path = None;
        let mut search_changed = false;
        egui::TopBottomPanel::top("top_panel")
            .frame(egui::Frame::canvas(&ctx.style()))
            .show(ctx, |ui| {
                ui.add_space(TOP_SIDE_MARGIN);
                ui.with_layout(Layout::left_to_right(eframe::emath::Align::Min), |ui| {
                    ui.add_space(TOP_SIDE_MARGIN);
                    if let Some(parent) = self.cur_path.parent() {
                        if ui
                            .button("⬆")
                            .on_hover_text("Go to parent directory")
                            .clicked()
                        {
                            new_path = Some(parent.into());
                            return;
                        }
                    }
                    ui.add_space(TOP_SIDE_MARGIN);
                    let mut path: String = "".into();

                    #[allow(unused_variables)]
                    for (i, e) in self.cur_path.iter().enumerate() {
                        #[cfg(windows)]
                        {
                            let text = match &i {
                                0 => {
                                    let last_two_chars: String =
                                        e.to_str().unwrap().chars().rev().take(2).collect();
                                    path += &last_two_chars.chars().rev().collect::<String>();
                                    path.push(std::path::MAIN_SEPARATOR);
                                    continue;
                                }
                                1 => &path,
                                _ => {
                                    path += e.to_str().unwrap();
                                    path.push(std::path::MAIN_SEPARATOR);
                                    e.to_str().unwrap()
                                }
                            };
                            if ui.button(text).clicked() {
                                new_path = Some(path.into());
                                return;
                            }
                        }
                        #[cfg(not(windows))]
                        {
                            path += e.to_str().unwrap();
                            if ui.button(e.to_str().unwrap()).clicked() {
                                new_path = Some(path.into());
                                return;
                            }
                        }
                    }
                });
                ui.add_space(TOP_SIDE_MARGIN);
            });

        egui::TopBottomPanel::bottom("bottomPanel")
            .frame(egui::Frame::canvas(&ctx.style()))
            .show(ctx, |ui| {
                ui.add_space(TOP_SIDE_MARGIN);
                ui.with_layout(Layout::right_to_left(eframe::emath::Align::Min), |ui| {
                    egui::widgets::global_dark_light_mode_switch(ui);
                    ui.hyperlink_to(
                        format!("{} v {}", egui::special_emojis::GITHUB, VERSION),
                        HOMEPAGE,
                    );
                    egui::warn_if_debug_build(ui);
                    if ui.toggle_value(&mut self.show_hidden, "Hidden").changed() {
                        self.refresh_list();
                    }
                    ui.toggle_value(&mut self.show_search_bar, "Search");
                    let old_value = self.sorting;

                    search_changed |= ui
                        .toggle_value(&mut self.invert_sort, "Inverted sort")
                        .changed();
                    egui::ComboBox::from_label("Sort by:")
                        .selected_text(format!("{:?}", self.sorting))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.sorting, Sort::Name, "Name");
                            ui.selectable_value(&mut self.sorting, Sort::Created, "Created");
                            ui.selectable_value(&mut self.sorting, Sort::Modified, "Modified");
                        });
                    search_changed |= old_value != self.sorting;
                });
            });

        egui::SidePanel::left("leftPanel")
            .frame(egui::Frame::canvas(&ctx.style()))
            .show(ctx, |ui| {
                ui.with_layout(Layout::top_down(eframe::emath::Align::Min), |ui| {
                    for id in ["Favorites", "User", "Drives"] {
                        let Some(collection) = self.locations.get_mut(id) else {
                            continue;
                        };
                        if collection.0.is_empty() {
                            continue;
                        }
                        egui::CollapsingHeader::new(id)
                            .default_open(true)
                            .show(ui, |ui| {
                                let mut id_to_remove = None;
                                for (i, location) in collection.0.iter().enumerate() {
                                    let button = ui.button(&location.name);
                                    if button.clicked() {
                                        new_path = Some(location.path.clone());
                                        return;
                                    }
                                    if !collection.1 {
                                        continue;
                                    }
                                    button.context_menu(|ui| {
                                        if ui.button("Remove").clicked() {
                                            id_to_remove = Some(i);
                                            ui.close_menu();
                                        }
                                    });
                                }
                                if let Some(id) = id_to_remove {
                                    collection.0.remove(id);
                                }
                            });
                    }
                });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            let text_height = egui::TextStyle::Body.resolve(ui.style()).size * 2.0;
            if self.show_search_bar {
                ui.with_layout(Layout::right_to_left(eframe::emath::Align::Min), |ui| {
                    search_changed |= ui
                        .add(egui::Slider::new(&mut self.search.depth, 1..=5).text("Search depth"))
                        .changed();
                    search_changed |= ui
                        .checkbox(&mut self.search.case_sensitive, "Case sensitive")
                        .changed();
                    search_changed |= ui
                        .checkbox(&mut self.search.favorites, "Search Favorites")
                        .changed();
                    search_changed |= ui.text_edit_singleline(&mut self.search.value).changed();
                });
                ui.add_space(TOP_SIDE_MARGIN);
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                let table = TableBuilder::new(ui)
                    .striped(true)
                    .vscroll(false)
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::remainder().at_least(260.0))
                    .resizable(false);
                table.body(|body| {
                    body.rows(text_height, self.list.len(), |mut row| {
                        let val = &self.list[row.index()];
                        let meta = val.metadata().unwrap();
                        row.col(|ui| {
                            ui.add_space(VERTICAL_SPACING);
                            // ui.allocate_space(egui::vec2(available_width, 20.0));
                            let file_type = meta.file_type();
                            #[cfg(target_os = "windows")]
                            let is_dir = {
                                use std::os::windows::fs::FileTypeExt;
                                file_type.is_dir() || file_type.is_symlink_dir()
                            };
                            #[cfg(not(target_os = "windows"))]
                            let is_dir = file_type.is_dir();
                            let text = val.file_name().to_str().unwrap().to_string();

                            let text = if is_dir {
                                RichText::new(text)
                            } else {
                                RichText::strong(text.into())
                            };
                            let added_button = ui.button(text);

                            if added_button.clicked() {
                                if meta.is_file() {
                                    let _ = open::that_detached(val.path());
                                } else {
                                    let Ok(path) = std::fs::canonicalize(val.path()) else {
                                        return;
                                    };
                                    new_path = Some(path);
                                }
                            }
                            added_button.context_menu(|ui| {
                                if is_dir {
                                    #[cfg(windows)]
                                    if ui.button("Open in explorer").clicked() {
                                        crate::windows_tools::open_in_explorer(val.path(), true);
                                        ui.close_menu();
                                        return;
                                    }
                                    let Ok(path) = std::fs::canonicalize(val.path()) else {
                                        return;
                                    };
                                    let existing_path = self
                                        .locations
                                        .get("Favorites")
                                        .unwrap()
                                        .0
                                        .iter()
                                        .enumerate()
                                        // THIS MAYBE WOULD NEED TO BE CHANGED ON PLATFORMS DIFFERENT THAN WINDOWS
                                        .find(|loc| path.ends_with(&loc.1.path))
                                        .map(|(i, _)| i);
                                    if existing_path.is_none()
                                        && ui.button("Add to favorites").clicked()
                                    {
                                        let name = path
                                            .iter()
                                            .last()
                                            .unwrap()
                                            .to_str()
                                            .unwrap()
                                            .to_owned();
                                        if let Some(fav) = self.locations.get_mut("Favorites") {
                                            fav.0.push(Location { name, path });
                                        }
                                        ui.close_menu();
                                        return;
                                    }
                                    if existing_path.is_some()
                                        && ui.button("Remove from favorites").clicked()
                                    {
                                        if let Some(fav) = self.locations.get_mut("Favorites") {
                                            fav.0.remove(existing_path.unwrap());
                                        }
                                        ui.close_menu();
                                    }
                                } else {
                                    #[cfg(windows)]
                                    if ui.button("Show in explorer").clicked() {
                                        crate::windows_tools::open_in_explorer(val.path(), false);
                                        ui.close_menu();
                                    }
                                }
                                #[cfg(windows)]
                                if ui.button("Properties").clicked() {
                                    crate::windows_tools::open_properties(val.path());
                                    ui.close_menu();
                                }
                            });
                            added_button.on_hover_text(format!(
                                "{:?}",
                                // consider caching here
                                std::fs::canonicalize(val.path())
                                    .unwrap_or(val.path().to_path_buf())
                            ));
                        });
                    });
                });
            });
        });
        if search_changed {
            self.refresh_list();
        }

        if let Some(new) = new_path {
            self.change_current_dir(new);
        }
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
        for (_text_style, font_id) in style.text_styles.iter_mut() {
            font_id.size *= 1.2;
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
    std::env::current_dir().unwrap()
}
