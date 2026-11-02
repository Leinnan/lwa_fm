//! Where the app keeps its files: every folder is named after [`APP_ID`].
use std::path::{Path, PathBuf};

use directories::{BaseDirs, ProjectDirs};

use crate::consts::{APP_ID, APP_NAME};

pub fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from_path(PathBuf::from(APP_ID))
}

/// The file eframe saves the settings to.
pub fn settings_file() -> Option<PathBuf> {
    project_dirs().map(|dirs| dirs.data_dir().join("app.ron"))
}

/// Moves files from the folders older versions used. Must run before anything
/// opens them. Whatever can't be moved stays where it was, and the app starts
/// without it.
pub fn migrate_legacy_paths() {
    let Some(dirs) = project_dirs() else {
        return;
    };
    let mut moves = Vec::new();
    if let Some(old) = eframe::storage_dir(APP_NAME) {
        moves.push((old.join("app.ron"), dirs.data_dir().join("app.ron")));
    }
    if let Some(old) = ProjectDirs::from("com", "Crayen", "Files2") {
        moves.push((old.data_dir().join("dirs"), dirs.data_dir().join("dirs")));
    }
    if let Some(old) = ProjectDirs::from("io", "github.leinnan", "dirfleet") {
        moves.extend([
            (
                old.config_dir().join("plugins"),
                dirs.config_dir().join("plugins"),
            ),
            (
                old.data_dir().join("plugin-data"),
                dirs.data_dir().join("plugin-data"),
            ),
            (
                old.data_local_dir().join("ffmpeg"),
                dirs.data_local_dir().join("ffmpeg"),
            ),
            (
                old.cache_dir().join("thumbnails"),
                dirs.cache_dir().join("thumbnails"),
            ),
        ]);
    }
    let keep: Vec<PathBuf> = BaseDirs::new().map_or_else(Vec::new, |base| {
        vec![
            base.cache_dir().to_path_buf(),
            base.config_dir().to_path_buf(),
            base.data_dir().to_path_buf(),
            base.data_local_dir().to_path_buf(),
        ]
    });
    move_all(&moves, &keep);
}

/// Renames each `(old, new)` unless `new` already exists, then removes the
/// folders that left empty, going up until one of `keep`.
fn move_all(moves: &[(PathBuf, PathBuf)], keep: &[PathBuf]) {
    for (old, new) in moves {
        if old == new || !old.exists() || new.exists() {
            continue;
        }
        let result = new
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::rename(old, new));
        match result {
            Ok(()) => remove_empty_parents(old, keep),
            Err(error) => log::warn!(
                "Could not move {} to {}: {error}",
                old.display(),
                new.display()
            ),
        }
    }
}

fn remove_empty_parents(path: &Path, keep: &[PathBuf]) {
    for parent in path.ancestors().skip(1) {
        if keep.iter().any(|kept| kept == parent) || std::fs::remove_dir(parent).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("lwa_fm_paths_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp root");
        root
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("folders");
        std::fs::write(path, text).expect("write");
    }

    #[test]
    fn moves_files_and_removes_emptied_folders() {
        let root = temp_root("moves");
        let old = root.join("Org/App/data/app.ron");
        let new = root.join("new/data/app.ron");
        write(&old, "settings");
        move_all(&[(old, new.clone())], std::slice::from_ref(&root));
        assert_eq!(std::fs::read_to_string(&new).expect("moved"), "settings");
        assert!(!root.join("Org").exists());
        assert!(root.exists());
        std::fs::remove_dir_all(root).expect("clean up");
    }

    #[test]
    fn moves_whole_folders() {
        let root = temp_root("folders");
        write(&root.join("old/dirs/db"), "database");
        move_all(
            &[(root.join("old/dirs"), root.join("new/dirs"))],
            std::slice::from_ref(&root),
        );
        let moved = std::fs::read_to_string(root.join("new/dirs/db")).expect("moved");
        assert_eq!(moved, "database");
        std::fs::remove_dir_all(root).expect("clean up");
    }

    #[test]
    fn never_overwrites_and_keeps_folders_with_other_files() {
        let root = temp_root("overwrite");
        let old = root.join("old/app.ron");
        let new = root.join("new/app.ron");
        write(&old, "old");
        write(&new, "new");
        move_all(&[(old.clone(), new.clone())], std::slice::from_ref(&root));
        assert_eq!(std::fs::read_to_string(&new).expect("kept"), "new");
        assert_eq!(std::fs::read_to_string(&old).expect("kept"), "old");

        let moved = root.join("shared/moved");
        write(&moved, "moved");
        write(&root.join("shared/other"), "other");
        move_all(
            &[(moved, root.join("new/moved"))],
            std::slice::from_ref(&root),
        );
        assert!(root.join("shared/other").exists());
        std::fs::remove_dir_all(root).expect("clean up");
    }

    #[test]
    fn ignores_missing_sources_and_paths_that_stay_put() {
        let root = temp_root("noop");
        let same = root.join("same/plugins");
        write(&same.join("plugin.toml"), "id");
        move_all(
            &[
                (root.join("missing"), root.join("new/missing")),
                (same.clone(), same.clone()),
            ],
            std::slice::from_ref(&root),
        );
        assert!(!root.join("new").exists());
        assert!(same.join("plugin.toml").exists());
        std::fs::remove_dir_all(root).expect("clean up");
    }

    #[test]
    fn packaging_uses_the_same_id() {
        assert!(include_str!("../static/Info.plist").contains(APP_ID));
        assert!(include_str!("../Cargo.toml").contains(&format!("identifier = \"{APP_ID}\"")));
    }

    #[test]
    fn every_folder_is_named_after_the_id() {
        let dirs = project_dirs().expect("home folder");
        assert_eq!(dirs.project_path(), Path::new(APP_ID));
        let file = settings_file().expect("settings file");
        assert!(file.starts_with(dirs.data_dir()));
        assert!(file.ends_with("app.ron"));
    }
}
