#![allow(clippy::cast_possible_wrap)]
use std::any::Any;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use egui::util::id_type_map::{SerializableAny, TypeId};
use egui::{Context, Id, InputState, Ui};

use crate::app::Data;
use crate::app::dock::CurrentPath;

pub trait PathHelper {
    fn to_full_path_string(&self) -> String;
}

/// Resolve `path` through the filesystem (symlinks, on-disk casing) and return
/// it in [`canonical_form`]. This costs a `canonicalize` syscall, so it is
/// reserved for roots: navigation targets and watch roots. Paths derived from
/// an already-normalised root (directory entries, watcher events) only need
/// the syscall-free [`canonical_form`].
#[must_use]
pub fn normalize_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).map_or_else(|_| canonical_form(path), |p| canonical_form(&p))
}

/// Lexical normalisation without syscalls: absolutises against the current
/// directory, resolves `.`/`..`, unifies separators, strips the Windows
/// verbatim (`\\?\`) prefix and uppercases the drive letter. For a path below
/// a root produced by [`normalize_path`] this yields the same result as
/// [`normalize_path`] itself.
#[must_use]
pub fn canonical_form(path: &Path) -> PathBuf {
    use std::path::Component;
    let absolute;
    let path = if path.is_absolute() {
        path
    } else {
        absolute =
            std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path));
        &absolute
    };
    let mut out = PathBuf::with_capacity(path.as_os_str().len());
    let mut depth = 0usize;
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(canonical_prefix(prefix)),
            Component::RootDir => out.push(std::path::MAIN_SEPARATOR_STR),
            Component::CurDir => {}
            Component::ParentDir => {
                // Never pop past the root.
                if depth > 0 {
                    out.pop();
                    depth -= 1;
                }
            }
            Component::Normal(part) => {
                out.push(part);
                depth += 1;
            }
        }
    }
    out
}

#[cfg(windows)]
fn canonical_prefix(prefix: std::path::PrefixComponent<'_>) -> std::ffi::OsString {
    use std::path::Prefix;
    match prefix.kind() {
        Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
            format!("{}:", char::from(letter).to_ascii_uppercase()).into()
        }
        Prefix::VerbatimUNC(server, share) => {
            let mut unc = std::ffi::OsString::from(r"\\");
            unc.push(server);
            unc.push(r"\");
            unc.push(share);
            unc
        }
        _ => prefix.as_os_str().to_os_string(),
    }
}

#[cfg(not(windows))]
fn canonical_prefix(prefix: std::path::PrefixComponent<'_>) -> std::ffi::OsString {
    prefix.as_os_str().to_os_string()
}

#[must_use]
pub fn normalize_path_string(path: &Path) -> String {
    normalize_path(path).to_string_lossy().into_owned()
}

/// Syscall-free [`canonical_form`] as a `String`.
#[must_use]
pub fn canonical_string(path: &Path) -> String {
    canonical_form(path).to_string_lossy().into_owned()
}

impl<T: AsRef<Path>> PathHelper for T {
    fn to_full_path_string(&self) -> String {
        normalize_path_string(self.as_ref())
    }
}

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
