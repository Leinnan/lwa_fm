#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release
use consts::{APP_NAME, VERSION};
use eframe::egui;

mod app;
mod consts;
mod locations;
#[cfg(windows)]
mod windows_tools;

fn main() -> eframe::Result<()> {
    const ICON: &[u8] = include_bytes!("../static/icon.png");
    let image = image::load_from_memory(ICON)
        .expect("Failed to open icon path")
        .to_rgba8();
    let (icon_width, icon_height) = image.dimensions();
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 300.0])
            .with_icon(egui::IconData {
                rgba: image.into_raw(),
                width: icon_width,
                height: icon_height,
            })
            .with_min_inner_size([300.0, 220.0]),
        ..Default::default()
    };
    eframe::run_native(
        &format!("{} v {}", APP_NAME, VERSION),
        native_options,
        Box::new(|cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(app::App::new(cc)))
        }),
    )
}
