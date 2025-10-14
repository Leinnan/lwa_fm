#![allow(clippy::cast_possible_wrap)]
use std::any::Any;
use std::hash::Hash;
use std::io;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use egui::util::id_type_map::{SerializableAny, TypeId};
use egui::{Context, Id, InputState, Ui};

use crate::app::Data;
use crate::app::dock::CurrentPath;

#[allow(dead_code)]
pub trait DataHolder {
    fn data_get_tab<T: 'static + Clone + Default + Any + Send + Sync>(
        &self,
        index: u32,
    ) -> Option<T>;
    fn data_has_tab<T: 'static + Clone + Default + Any + Send + Sync>(&self, index: u32) -> bool {
        self.data_get_tab::<T>(index).is_some()
    }
    fn data_set_tab<T: 'static + Clone + Default + Any + Send + Sync>(&self, index: u32, value: T);
    fn data_remove_tab<T: 'static + Clone + Default + Any + Send + Sync>(&self, index: u32);
    fn data_get_persisted<T: 'static + Clone + Default + Any + Send + Sync + SerializableAny>(
        &self,
    ) -> Option<T>;
    fn data_set_persisted<T: 'static + Clone + Default + Any + Send + Sync + SerializableAny>(
        &self,
        value: T,
    );
    fn data_get_path<T: 'static + Clone + Default + Any + SerializableAny + Send + Sync>(
        &self,
        path: &CurrentPath,
    ) -> Option<T>;
    fn data_set_path<T: 'static + Clone + Default + Any + SerializableAny + Send + Sync>(
        &self,
        path: &CurrentPath,
        value: T,
    );

    fn data_get_path_or_persisted<
        T: 'static + Clone + Default + Any + SerializableAny + Send + Sync,
    >(
        &self,
        path: &CurrentPath,
    ) -> Data<T>;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TabData {
    pub type_id: TypeId,
    pub index: u32,
}

impl TabData {
    pub fn id<T: 'static + Clone + Default + Any + Send + Sync>(index: u32) -> Id {
        Id::new(Self {
            type_id: TypeId::of::<T>(),
            index,
        })
    }
}

fn path_hash<T: 'static>(path: &CurrentPath) -> Id {
    Id::new(path).with(TypeId::of::<T>())
}

impl DataHolder for Context {
    fn data_get_tab<T: 'static + Clone + Default + Any + Send + Sync>(
        &self,
        index: u32,
    ) -> Option<T> {
        self.data(|data| data.get_temp::<T>(TabData::id::<T>(index)))
    }

    fn data_set_tab<T: 'static + Clone + Default + Any + Send + Sync>(&self, index: u32, value: T) {
        self.data_mut(|data| {
            data.insert_temp::<T>(TabData::id::<T>(index), value);
        });
    }

    fn data_remove_tab<T: 'static + Clone + Default + Any + Send + Sync>(&self, index: u32) {
        self.data_mut(|data| data.remove::<T>(TabData::id::<T>(index)));
    }

    fn data_get_persisted<T: 'static + Clone + Default + Any + Send + Sync + SerializableAny>(
        &self,
    ) -> Option<T> {
        self.data_mut(|data| data.get_persisted::<T>(Id::new(TypeId::of::<T>())))
    }

    fn data_set_persisted<T: 'static + Clone + Default + Any + Send + Sync + SerializableAny>(
        &self,
        value: T,
    ) {
        self.data_mut(|data| data.insert_persisted(Id::new(TypeId::of::<T>()), value));
    }

    fn data_get_path<T: 'static + Clone + Default + Any + SerializableAny + Send + Sync>(
        &self,
        path: &CurrentPath,
    ) -> Option<T> {
        self.data_mut(|data| data.get_persisted::<T>(path_hash::<T>(path)))
    }

    fn data_set_path<T: Clone + Default + Any + SerializableAny + Send + Sync>(
        &self,
        path: &CurrentPath,
        value: T,
    ) {
        self.data_mut(|data| data.insert_persisted::<T>(path_hash::<T>(path), value));
    }

    fn data_get_path_or_persisted<
        T: 'static + Clone + Default + Any + SerializableAny + Send + Sync,
    >(
        &self,
        path: &CurrentPath,
    ) -> Data<T> {
        self.data_mut(|data| {
            if let Some(data) = data.get_persisted::<T>(path_hash::<T>(path)) {
                return Data::from_local(data);
            }
            if let Some(data) = data.get_persisted::<T>(Id::new(TypeId::of::<T>())) {
                return Data::from_settings(data);
            }
            Data::default()
        })
    }
}
impl DataHolder for Ui {
    fn data_get_tab<T: Clone + Default + Any + Send + Sync>(&self, index: u32) -> Option<T> {
        self.data(|data| data.get_temp::<T>(TabData::id::<T>(index)))
    }

    fn data_set_tab<T: Clone + Default + Any + Send + Sync>(&self, index: u32, value: T) {
        self.data_mut(|data| {
            data.insert_temp::<T>(TabData::id::<T>(index), value);
        });
    }

    fn data_remove_tab<T: Clone + Default + Any + Send + Sync>(&self, index: u32) {
        self.data_mut(|data| data.remove::<T>(TabData::id::<T>(index)));
    }

    fn data_get_persisted<T: Clone + Default + Any + Send + Sync + SerializableAny>(
        &self,
    ) -> Option<T> {
        self.data_mut(|data| data.get_persisted::<T>(Id::new(TypeId::of::<T>())))
    }
    fn data_set_persisted<T: Clone + Default + Any + Send + Sync + SerializableAny>(
        &self,
        value: T,
    ) {
        self.data_mut(|data| data.insert_persisted(Id::new(TypeId::of::<T>()), value));
    }

    fn data_get_path<T: 'static + Clone + Default + Any + SerializableAny + Send + Sync>(
        &self,
        path: &CurrentPath,
    ) -> Option<T> {
        self.data_mut(|data| data.get_persisted::<T>(path_hash::<T>(path)))
    }

    fn data_set_path<T: Clone + Default + Any + SerializableAny + Send + Sync>(
        &self,
        path: &CurrentPath,
        value: T,
    ) {
        self.data_mut(|data| data.insert_persisted::<T>(path_hash::<T>(path), value));
    }

    fn data_get_path_or_persisted<
        T: 'static + Clone + Default + Any + SerializableAny + Send + Sync,
    >(
        &self,
        path: &CurrentPath,
    ) -> Data<T> {
        self.data_mut(|data| {
            if let Some(data) = data.get_persisted::<T>(path_hash::<T>(path)) {
                return Data::from_local(data);
            }
            if let Some(data) = data.get_persisted::<T>(Id::new(TypeId::of::<T>())) {
                return Data::from_settings(data);
            }
            Data::default()
        })
    }
}

pub trait PathFixer {
    fn to_fixed_string(&self) -> String;
}

impl PathFixer for std::path::PathBuf {
    fn to_fixed_string(&self) -> String {
        self.display().to_string().replace("\\\\?\\", "")
    }
}
impl PathFixer for Path {
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
