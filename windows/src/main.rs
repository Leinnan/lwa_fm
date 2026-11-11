//! `DirFleet`'s native Windows shell: `WinUI` 3 through `windows-reactor`, on the
//! shared core (`dirfleet_core`) and its Rust effect handlers
//! (`dirfleet_effects`). The shell only draws, collects input and answers the
//! few effects that need a window.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod commands;
#[cfg(windows)]
mod core_link;
#[cfg(windows)]
mod dialogs;
#[cfg(windows)]
mod files;
#[cfg(windows)]
mod hwnd;
#[cfg(windows)]
mod icons;
#[cfg(windows)]
mod inspector;
#[cfg(windows)]
mod launch;
#[cfg(windows)]
mod menus;
#[cfg(windows)]
mod notices;
#[cfg(windows)]
mod pane;
#[cfg(windows)]
mod pathbar;
#[cfg(all(windows, test))]
mod perf;
#[cfg(windows)]
mod picker;
#[cfg(windows)]
mod plugins;
#[cfg(windows)]
mod popup;
#[cfg(windows)]
mod preview;
#[cfg(windows)]
mod saved;
#[cfg(windows)]
mod search;
#[cfg(windows)]
mod sheets;
#[cfg(windows)]
mod shortcuts;
#[cfg(windows)]
mod sidebar;
#[cfg(windows)]
mod store;
#[cfg(windows)]
mod theme;
#[cfg(windows)]
mod trace;
#[cfg(windows)]
mod ui_thread;
#[cfg(windows)]
mod window;

#[cfg(windows)]
fn main() {
    trace::start();
    let launch = launch::Launch::from_args(std::env::args().skip(1));
    if let Err(error) = windows_reactor::App::run_application::<app::DirFleetApp>(launch) {
        eprintln!("DirFleet could not start: {error:?}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("This is DirFleet's Windows shell; it only runs on Windows.");
}
