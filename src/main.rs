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

fn parse_present_mode() -> wgpu::PresentMode {
    let mut args = std::env::args();
    while let Some(arg) = args.next() {
        if arg == "--present-mode" && let Some(val) = args.next() {
            return match val.to_lowercase().as_str() {
                "fifo" => wgpu::PresentMode::Fifo,
                "mailbox" => wgpu::PresentMode::Mailbox,
                "immediate" => wgpu::PresentMode::Immediate,
                "auto-no-vsync" => wgpu::PresentMode::AutoNoVsync,
                _ => {
                    eprintln!("Unknown present mode '{val}', using AutoVsync");
                    wgpu::PresentMode::AutoVsync
                }
            };
        }
    }
    if let Ok(val) = std::env::var("LWA_FM_PRESENT_MODE") {
        return match val.to_lowercase().as_str() {
            "fifo" => wgpu::PresentMode::Fifo,
            "mailbox" => wgpu::PresentMode::Mailbox,
            "immediate" => wgpu::PresentMode::Immediate,
            "auto-no-vsync" => wgpu::PresentMode::AutoNoVsync,
            _ => wgpu::PresentMode::AutoVsync,
        };
    }
    wgpu::PresentMode::AutoVsync
}

fn main() -> anyhow::Result<()> {
    #[cfg(feature = "profiling")]
    profiler::enable_profiling();

    #[cfg(feature = "profiling")]
    profiler::start_puffin_server();

    let present_mode = parse_present_mode();
    if present_mode != wgpu::PresentMode::AutoVsync {
        log::info!("Using present mode: {present_mode:?}");
    }

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 300.0])
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../static/base_icon.png"))
                    .unwrap_or_default(),
            )
            .with_min_inner_size([300.0, 220.0]),
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            present_mode,
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
