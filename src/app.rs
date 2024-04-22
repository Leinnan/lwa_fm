use crate::consts::*;
use egui::{Layout, RichText};
use egui_extras::{Column, TableBuilder};
use std::{fs, path::PathBuf};

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(default)] // if we add new fields, give them default values when deserializing old state
pub struct App {
    // Example stuff:
    label: String,

    #[serde(skip)] // This how you opt-out of serialization of a field
    value: f32,
    #[serde(skip)]
    cur_path: PathBuf,
    #[serde(skip)]
    drives: sysinfo::Disks,
}

impl Default for App {
    fn default() -> Self {
        let mut drives = sysinfo::Disks::new_with_refreshed_list();
        drives.sort_by(|a, b| a.mount_point().cmp(b.mount_point()));
        Self {
            // Example stuff:
            label: "Hello World!".to_owned(),
            value: 2.7,
            cur_path: get_starting_path(),
            drives,
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
            return eframe::get_value(storage, eframe::APP_KEY).unwrap_or_default();
        }

        Default::default()
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

        egui::TopBottomPanel::top("top_panel")
            .frame(egui::Frame::canvas(&ctx.style()))
            .show(ctx, |ui| {
                ui.add_space(TOP_SIDE_MARGIN);
                ui.heading(format!(" {}", &self.cur_path.display()));
                ui.add_space(TOP_SIDE_MARGIN);
            });

        egui::SidePanel::left("leftPanel")
            .frame(egui::Frame::canvas(&ctx.style()))
            .show(ctx, |ui| {
                ui.with_layout(Layout::top_down(eframe::emath::Align::Min), |ui| {
                    egui::CollapsingHeader::new("Drives")
                        .default_open(true)
                        .show(ui, |ui| {
                            for p in self.drives.iter() {
                                if ui
                                    .button(format!(
                                        "{} ({})",
                                        p.name().to_str().unwrap(),
                                        p.mount_point().display()
                                    ))
                                    .clicked()
                                {
                                    self.cur_path = p.mount_point().to_path_buf();
                                    return;
                                }
                            }
                        });
                });
            });

        egui::TopBottomPanel::bottom("bottomPanel")
            .frame(egui::Frame::canvas(&ctx.style()))
            .show(ctx, |ui| {
                ui.with_layout(Layout::right_to_left(eframe::emath::Align::Center), |ui| {
                    egui::widgets::global_dark_light_mode_switch(ui);
                    ui.hyperlink_to(
                        format!("{} v {}", egui::special_emojis::GITHUB, VERSION),
                        HOMEPAGE,
                    );
                    egui::warn_if_debug_build(ui);
                });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            let text_height = egui::TextStyle::Body.resolve(ui.style()).size * 2.0;
            let ss = fs::read_dir(&self.cur_path);
            if let Some(parent) = self.cur_path.parent() {
                if ui
                    .button("⬆")
                    .on_hover_text("Go to parent directory")
                    .clicked()
                {
                    self.cur_path = parent.into();
                    return;
                }
                ui.separator();
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                if let Ok(readed_dir) = ss {
                    let mut dir_entries: Vec<fs::DirEntry> =
                        readed_dir.into_iter().filter_map(|e| e.ok()).collect();
                    dir_entries.sort_by(|a, b| {
                        a.file_type()
                            .unwrap()
                            .is_file()
                            .cmp(&b.file_type().unwrap().is_file())
                            .then(
                                a.file_name()
                                    .to_ascii_lowercase()
                                    .cmp(&b.file_name().to_ascii_lowercase()),
                            )
                    });
                    let table = TableBuilder::new(ui)
                        .striped(true)
                        .vscroll(false)
                        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                        .column(Column::remainder().at_least(260.0))
                        .resizable(false);
                    table.body(|body| {
                        body.rows(text_height, dir_entries.len(), |mut row| {
                            let val = &dir_entries[row.index()];
                            let meta = val.metadata().unwrap();
                            row.col(|ui| {
                                ui.add_space(VERTICAL_SPACING);
                                let text = val.file_name().to_str().unwrap().to_string();
                                let text = if meta.is_dir() {
                                    RichText::new(text)
                                } else {
                                    RichText::strong(text.into())
                                };
                                if ui.button(text).clicked() {
                                    if meta.is_file() {
                                        let _ = open::that_detached(val.path());
                                    } else {
                                        self.cur_path = val.path();
                                    }
                                }
                            });
                        });
                    });
                }
            });
        });
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
