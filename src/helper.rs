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
