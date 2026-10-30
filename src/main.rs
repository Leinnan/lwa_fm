#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use anyhow::Context;
// hide console window on Windows in release
use consts::APP_NAME;
use eframe::egui;

mod app;
mod child_process;
mod clipboard;
mod consts;
pub mod data;
mod drives;
mod fileops;
mod fs_enum;
mod helper;
mod locations;
#[cfg(target_os = "macos")]
mod macos_tools;
mod native_drag;
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

fn parse_present_mode() -> wgpu::PresentMode {
    let mut args = std::env::args();
    while let Some(arg) = args.next() {
        if arg == "--present-mode"
            && let Some(val) = args.next()
        {
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
            .with_inner_size([1100.0, 720.0])
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../static/base_icon.png"))
                    .unwrap_or_default(),
            )
            .with_min_inner_size([640.0, 420.0]),
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            surface: eframe::egui_wgpu::SurfaceConfig {
                present_mode,
                ..eframe::egui_wgpu::SurfaceConfig::HIGH_THROUGHPUT
            },
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
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))
    .context("Failed to run native")
}
