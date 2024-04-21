#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release
use consts::{APP_NAME, VERSION};
use eframe::egui;

mod app;
mod consts;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 300.0])
            .with_min_inner_size([300.0, 220.0]),
        ..Default::default()
    };
    eframe::run_native(
        &format!("{} v {}", APP_NAME, VERSION),
        native_options,
        Box::new(|cc| Box::new(app::App::new(cc))),
    )
}
