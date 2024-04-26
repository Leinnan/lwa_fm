use crate::{consts::*, locations::Locations};
use egui::{
    ahash::{HashMap, HashMapExt},
    Layout, RichText,
};
use egui_extras::{Column, TableBuilder};
use std::{path::PathBuf};
use walkdir::WalkDir;

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(default)] // if we add new fields, give them default values when deserializing old state
pub struct App {
    show_search_bar: bool,
    show_hidden: bool,
    #[serde(skip)]
    cur_path: PathBuf,
    #[serde(skip)]
    locations: HashMap<String, Locations>,
    #[serde(skip)]
    list: Vec<walkdir::DirEntry>,
    #[serde(skip)]
    search: String,
    search_depth: usize,
}

impl Default for App {
    fn default() -> Self {
        let mut locations = HashMap::new();
        locations.insert("User".into(), Locations::get_user_dirs());
        locations.insert("Drives".into(), Locations::get_drives());
        let mut p = Self {
            show_search_bar: false,
            show_hidden: false,
            cur_path: get_starting_path(),
            locations,
            list: vec![],
            search: String::new(),
            search_depth: 3,
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
            return eframe::get_value(storage, eframe::APP_KEY).unwrap_or_default();
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
        let use_search = !self.search.is_empty();
        let depth = if use_search { self.search_depth } else { 1 };
        let mut dir_entries: Vec<walkdir::DirEntry> = WalkDir::new(&self.cur_path)
            .max_depth(depth)
            .into_iter()
            .flatten()
            .skip(1)
            .filter(|e| {
                let s = e.file_name().to_string_lossy();
                if !self.show_hidden && (s.starts_with('.') || s.starts_with('$')) {
                    return false;
                }
                s.contains(&self.search)
            })
            .collect();

        dir_entries.sort_by(|a, b| {
            a.file_type().is_file().cmp(&b.file_type().is_file()).then(
                a.file_name()
                    .to_ascii_lowercase()
                    .cmp(&b.file_name().to_ascii_lowercase()),
            )
        });
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
                    ui.heading(format!("{}", &self.cur_path.display()));
                });
                ui.add_space(TOP_SIDE_MARGIN);
            });

        egui::SidePanel::left("leftPanel")
            .frame(egui::Frame::canvas(&ctx.style()))
            .show(ctx, |ui| {
                ui.with_layout(Layout::top_down(eframe::emath::Align::Min), |ui| {
                    for (id, collection) in &self.locations {
                        egui::CollapsingHeader::new(id)
                            .default_open(true)
                            .show(ui, |ui| {
                                for location in collection.0.iter() {
                                    if ui.button(&location.name).clicked() {
                                        new_path = Some(location.path.clone());
                                        return;
                                    }
                                }
                            });
                    }
                });
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
                });
                ui.add_space(TOP_SIDE_MARGIN);
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            let text_height = egui::TextStyle::Body.resolve(ui.style()).size * 2.0;
            if self.show_search_bar {
                ui.with_layout(Layout::right_to_left(eframe::emath::Align::Min), |ui| {
                    search_changed = ui
                        .add(egui::Slider::new(&mut self.search_depth, 1..=5).text("Search depth"))
                        .changed();
                    search_changed |= ui.text_edit_singleline(&mut self.search).changed();
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
                            if ui.button(text).clicked() {
                                if meta.is_file() {
                                    let _ = open::that_detached(val.path());
                                } else {
                                    let Ok(path) = std::fs::canonicalize(val.path()) else {
                                        return;
                                    };
                                    new_path = Some(path);
                                }
                            }
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

    // Install my own font (maybe supporting non-latin characters).
    // .ttf and .otf files supported.
    fonts.font_data.insert(
        "regular".to_owned(),
        egui::FontData::from_static(include_bytes!("../static/Inter-Regular.ttf")),
    );
    fonts.font_data.insert(
        "semibold".to_owned(),
        egui::FontData::from_static(include_bytes!("../static/Inter-SemiBold.ttf")),
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

fn get_starting_path() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        return PathBuf::from(args[1].clone());
    }
    std::env::current_dir().unwrap()
}
