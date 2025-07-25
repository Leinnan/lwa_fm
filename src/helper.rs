#![allow(clippy::cast_possible_wrap)]
use std::io;
use std::process::{Child, Command, Stdio};

use egui::{Context, InputState, Ui};

pub trait PathFixer {
    fn to_fixed_string(&self) -> String;
}

impl PathFixer for std::path::PathBuf {
    fn to_fixed_string(&self) -> String {
        self.display().to_string().replace("\\\\?\\", "")
    }
}
#[allow(dead_code)]
pub trait DetachedSpawn {
    fn spawn_detached(&mut self) -> io::Result<Child>;
}

impl DetachedSpawn for Command {
    #[allow(unused_mut)]
    fn spawn_detached(&mut self) -> io::Result<Child> {
        let mut command = self;
        #[cfg(windows)]
        {
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            use std::os::windows::process::CommandExt;
            command = command.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
        }

        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    }
}

pub trait KeyWithCommandPressed {
    fn key_with_command_pressed(&self, key: egui::Key) -> bool;
    fn command_pressed(&self) -> bool;
    fn shift_pressed(&self) -> bool;
}

impl KeyWithCommandPressed for InputState {
    fn key_with_command_pressed(&self, key: egui::Key) -> bool {
        self.modifiers.command_only() && self.key_pressed(key)
    }
    fn command_pressed(&self) -> bool {
        self.modifiers.command
    }
    fn shift_pressed(&self) -> bool {
        self.modifiers.shift
    }
}

impl KeyWithCommandPressed for Context {
    fn key_with_command_pressed(&self, key: egui::Key) -> bool {
        self.input(|i| i.key_with_command_pressed(key))
    }
    fn command_pressed(&self) -> bool {
        self.input(|i| i.modifiers.command)
    }
    fn shift_pressed(&self) -> bool {
        self.input(|i| i.modifiers.shift)
    }
}

impl KeyWithCommandPressed for Ui {
    fn key_with_command_pressed(&self, key: egui::Key) -> bool {
        self.input(|i| i.key_with_command_pressed(key))
    }
    fn command_pressed(&self) -> bool {
        self.input(|i| i.modifiers.command)
    }
    fn shift_pressed(&self) -> bool {
        self.input(|i| i.modifiers.shift)
    }
}

/// Converts bytes (as usize) to a human-readable format
/// Returns a tuple of (value, unit) for flexible formatting
pub fn format_bytes_detailed(bytes: u64) -> (f64, &'static str) {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB", "PB"];
    const THRESHOLD: f64 = 1024.0;

    if bytes == 0 {
        return (0.0, "B");
    }

    let bytes_f64 = bytes as f64;
    let unit_index = bytes_f64.log(THRESHOLD).floor() as usize;
    let unit_index = unit_index.min(UNITS.len() - 1);

    let value = bytes_f64 / THRESHOLD.powi(unit_index as i32);
    (value, UNITS[unit_index])
}

/// Converts bytes (as usize) to a simplified human-readable string
/// Examples: "1.5 GB", "256 MB", "1.2 TB"
pub fn format_bytes_simple(bytes: u64) -> String {
    let (value, unit) = format_bytes_detailed(bytes);

    if value >= 100.0 {
        format!("{value:.0} {unit}")
    } else if value >= 10.0 {
        format!("{value:.1} {unit}")
    } else {
        format!("{value:.2} {unit}")
    }
}

#[allow(dead_code)]
/// Converts bytes to a compact format (shorter strings)
/// Examples: "1.5G", "256M", "1.2T"
pub fn format_bytes_compact(bytes: u64) -> String {
    let (value, unit) = format_bytes_detailed(bytes);
    let short_unit = match unit {
        "B" => "B",
        "KB" => "K",
        "MB" => "M",
        "GB" => "G",
        "TB" => "T",
        "PB" => "P",
        _ => unit,
    };

    if value >= 100.0 {
        format!("{value:.0}{short_unit}")
    } else if value >= 10.0 {
        format!("{value:.1}{short_unit}")
    } else {
        format!("{value:.2}{short_unit}")
    }
}
