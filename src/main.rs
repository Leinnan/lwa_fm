#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use anyhow::Context;
// hide console window on Windows in release
use consts::APP_NAME;
use eframe::egui;

mod app;
mod consts;
pub mod data;
mod helper;
mod locations;
#[cfg(feature = "profiling")]
mod profiler;
mod watcher;
mod widgets;
#[cfg(windows)]
mod windows_tools;

fn main() -> anyhow::Result<()> {
    #[cfg(feature = "profiling")]
    profiler::start_puffin_server(); // NOTE: you may only want to call this if the users specifies some flag or clicks a button!

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 300.0])
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../static/base_icon.png"))
                    .unwrap_or_default(),
            )
            .with_min_inner_size([300.0, 220.0]),
        ..Default::default()
    };

    eframe::run_native(
        APP_NAME,
        native_options,
        Box::new(|cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(app::App::new(cc)))
        }),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))
    .context("Failed to run native")
}
