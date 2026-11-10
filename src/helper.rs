#![allow(clippy::cast_possible_wrap)]
use std::any::Any;
use std::io;
use std::process::{Child, Command, Stdio};

use egui::util::id_type_map::{SerializableAny, TypeId};
use egui::{Context, Id, InputState, Ui};

use crate::app::Data;
use crate::app::dock::CurrentPath;

pub use dirfleet_effects::paths::{canonical_form, canonical_string, normalize_path};

#[allow(dead_code)]
pub trait DataHolder {
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

fn path_hash<T: 'static>(path: &CurrentPath) -> Id {
    Id::unique(path).with(TypeId::of::<T>())
}

/// A folder's value, or the one an earlier build saved for it, which is then
/// stored again under this build's key.
fn get_path_or_import<T: 'static + Clone + Default + Any + SerializableAny + Send + Sync>(
    data: &mut egui::util::IdTypeMap,
    path: &CurrentPath,
) -> Option<T> {
    let key = path_hash::<T>(path);
    if let Some(value) = data.get_persisted::<T>(key) {
        return Some(value);
    }
    let value = crate::legacy_import::folder_value::<T>(path)?;
    data.insert_persisted(key, value.clone());
    Some(value)
}

impl DataHolder for Context {
    fn data_get_persisted<T: 'static + Clone + Default + Any + Send + Sync + SerializableAny>(
        &self,
    ) -> Option<T> {
        self.data_mut(|data| data.get_persisted::<T>(Id::unique(TypeId::of::<T>())))
    }

    fn data_set_persisted<T: 'static + Clone + Default + Any + Send + Sync + SerializableAny>(
        &self,
        value: T,
    ) {
        self.data_mut(|data| data.insert_persisted(Id::unique(TypeId::of::<T>()), value));
    }

    fn data_get_path<T: 'static + Clone + Default + Any + SerializableAny + Send + Sync>(
        &self,
        path: &CurrentPath,
    ) -> Option<T> {
        self.data_mut(|data| get_path_or_import::<T>(data, path))
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
            if let Some(data) = get_path_or_import::<T>(data, path) {
                return Data::from_local(data);
            }
            if let Some(data) = data.get_persisted::<T>(Id::unique(TypeId::of::<T>())) {
                return Data::from_settings(data);
            }
            Data::default()
        })
    }
}
impl DataHolder for Ui {
    fn data_get_persisted<T: Clone + Default + Any + Send + Sync + SerializableAny>(
        &self,
    ) -> Option<T> {
        self.data_mut(|data| data.get_persisted::<T>(Id::unique(TypeId::of::<T>())))
    }
    fn data_set_persisted<T: Clone + Default + Any + Send + Sync + SerializableAny>(
        &self,
        value: T,
    ) {
        self.data_mut(|data| data.insert_persisted(Id::unique(TypeId::of::<T>()), value));
    }

    fn data_get_path<T: 'static + Clone + Default + Any + SerializableAny + Send + Sync>(
        &self,
        path: &CurrentPath,
    ) -> Option<T> {
        self.data_mut(|data| get_path_or_import::<T>(data, path))
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
            if let Some(data) = get_path_or_import::<T>(data, path) {
                return Data::from_local(data);
            }
            if let Some(data) = data.get_persisted::<T>(Id::unique(TypeId::of::<T>())) {
                return Data::from_settings(data);
            }
            Data::default()
        })
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
    fn command_pressed(&self) -> bool;
}

impl KeyWithCommandPressed for InputState {
    fn command_pressed(&self) -> bool {
        self.modifiers.command
    }
}

impl KeyWithCommandPressed for Context {
    fn command_pressed(&self) -> bool {
        self.input(|i| i.modifiers.command)
    }
}

impl KeyWithCommandPressed for Ui {
    fn command_pressed(&self) -> bool {
        self.input(|i| i.modifiers.command)
    }
}

#[cfg(test)]
mod path_tests {
    use super::{canonical_form, normalize_path};

    #[test]
    fn canonical_form_resolves_dots_without_syscalls() {
        let base = normalize_path(&std::env::temp_dir());
        let messy = base.join("a").join(".").join("b").join("..").join("c");
        assert_eq!(canonical_form(&messy), base.join("a").join("c"));
        let mut above_root = base;
        for _ in 0..32 {
            above_root.push("..");
        }
        assert!(canonical_form(&above_root).has_root());
    }

    #[test]
    fn canonical_form_matches_normalize_below_a_normalized_root() {
        let root = normalize_path(&std::env::temp_dir());
        let child = root.join("not_existing_lwa_child.txt");
        assert_eq!(canonical_form(&child), normalize_path(&child));
        assert_eq!(canonical_form(&root), root);
    }

    #[cfg(windows)]
    #[test]
    fn canonical_form_strips_verbatim_and_uppercases_drive() {
        use std::path::Path;
        assert_eq!(
            canonical_form(Path::new(r"\\?\c:\Users\x")),
            Path::new(r"C:\Users\x")
        );
        assert_eq!(
            canonical_form(Path::new("c:/Users/x/")),
            Path::new(r"C:\Users\x")
        );
        assert_eq!(
            canonical_form(Path::new(r"\\?\UNC\srv\share\dir")),
            Path::new(r"\\srv\share\dir")
        );
    }
}
