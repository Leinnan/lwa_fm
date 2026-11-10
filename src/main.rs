#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use anyhow::Context;
// hide console window on Windows in release
use consts::APP_NAME;
use eframe::egui;

use dirfleet_effects::access_time;
mod app;
mod clipboard;
mod consts;
pub mod data;
mod drives;
mod fileops;
mod fs_enum;
mod helper;
mod legacy_import;
mod locations;
#[cfg(target_os = "macos")]
mod macos_tools;
mod native_drag;
mod paths;
mod plugins;
#[cfg(feature = "profiling")]
mod profiler;
mod watcher;
mod widgets;
#[cfg(windows)]
mod windows_tools;

/// The system file manager integration: Explorer on Windows, Finder on macOS.
#[cfg(target_os = "macos")]
use macos_tools as shell;
#[cfg(windows)]
use windows_tools as shell;

// Gives the bare binary (`cargo run`) the bundle's identity and privacy
// strings; the built `.app` reads the same file from `Contents/Info.plist`.
#[cfg(target_os = "macos")]
embed_plist::embed_info_plist!("../static/Info.plist");

// The listing, asset and layout paths are allocation heavy; mimalloc is
// markedly faster than the platform allocators for that workload.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> anyhow::Result<()> {
    paths::migrate_legacy_paths();
    // Before any listing is read: names sort in the device's language.
    dirfleet_effects::listing_io::init_locale();

    #[cfg(feature = "profiling")]
    profiler::enable_profiling();

    #[cfg(feature = "profiling")]
    profiler::start_puffin_server();

    let mut native_options = eframe::NativeOptions {
        persistence_path: paths::settings_file(),
        viewport: egui::ViewportBuilder::default()
            .with_app_id(consts::APP_ID)
            .with_inner_size([1100.0, 720.0])
            .with_icon(
                eframe::icon_data::from_png_bytes(consts::WINDOW_ICON_PNG).unwrap_or_default(),
            )
            .with_min_inner_size([640.0, 420.0]),
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            on_surface_status: std::sync::Arc::new(|status| {
                #[cfg(feature = "profiling")]
                puffin::profile_scope!("lwa_fm::surface::error", &format!("{status:?}"));
                match status {
                    wgpu::CurrentSurfaceTexture::Outdated
                    | wgpu::CurrentSurfaceTexture::Occluded
                    | wgpu::CurrentSurfaceTexture::Timeout => {
                        eframe::egui_wgpu::SurfaceErrorAction::SkipFrame
                    }
                    _ => eframe::egui_wgpu::SurfaceErrorAction::RecreateSurface,
                }
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    egui_mev_app::gpu::apply(
        &mut native_options,
        egui_mev_app::gpu::present_mode("LWA_FM_PRESENT_MODE"),
    );

    eframe::run_native(
        APP_NAME,
        native_options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))
    .context("Failed to run native")
}
