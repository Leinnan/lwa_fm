//! Finding plugins: the ones shipped with the app and the folders in the
//! user's plugins directory, which are watched so edits load right away.

use super::manifest::{self, Diagnostic};
use super::{PluginCode, Source};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Plugins shipped inside the app: (folder, [(relative path, source)]).
const BUNDLED: &[(&str, &[(&str, &str)])] = &[
    (
        "io.github.leinnan.git",
        &[
            (
                "plugin.toml",
                include_str!("../../plugins/io.github.leinnan.git/plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../plugins/io.github.leinnan.git/main.lua"),
            ),
        ],
    ),
    (
        "io.github.leinnan.video-convert",
        &[
            (
                "plugin.toml",
                include_str!("../../plugins/io.github.leinnan.video-convert/plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../plugins/io.github.leinnan.video-convert/main.lua"),
            ),
            (
                "lib/args.lua",
                include_str!("../../plugins/io.github.leinnan.video-convert/lib/args.lua"),
            ),
        ],
    ),
    (
        "io.github.leinnan.heic-convert",
        &[
            (
                "plugin.toml",
                include_str!("../../plugins/io.github.leinnan.heic-convert/plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../plugins/io.github.leinnan.heic-convert/main.lua"),
            ),
            (
                "lib/args.lua",
                include_str!("../../plugins/io.github.leinnan.heic-convert/lib/args.lua"),
            ),
        ],
    ),
    (
        "io.github.leinnan.folder-usage",
        &[
            (
                "plugin.toml",
                include_str!("../../plugins/io.github.leinnan.folder-usage/plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../plugins/io.github.leinnan.folder-usage/main.lua"),
            ),
            (
                "lib/scan.lua",
                include_str!("../../plugins/io.github.leinnan.folder-usage/lib/scan.lua"),
            ),
        ],
    ),
    (
        "io.github.leinnan.rust-clean",
        &[
            (
                "plugin.toml",
                include_str!("../../plugins/io.github.leinnan.rust-clean/plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../plugins/io.github.leinnan.rust-clean/main.lua"),
            ),
            (
                "lib/cargo.lua",
                include_str!("../../plugins/io.github.leinnan.rust-clean/lib/cargo.lua"),
            ),
        ],
    ),
];

/// The `LuaLS` definitions of the `dirfleet` API.
pub const TYPES: &str = include_str!("../../plugins/types/dirfleet.d.lua");

const LUARC: &str = r#"{
  "$schema": "https://raw.githubusercontent.com/LuaLS/vscode-lua/master/setting/schema.json",
  "runtime.version": "Lua 5.4",
  "workspace.library": ["types"],
  "diagnostics.globals": ["dirfleet"]
}
"#;

/// Where users put plugins: one folder per plugin.
pub fn plugins_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("DIRFLEET_PLUGINS_DIR") {
        return Some(PathBuf::from(dir));
    }
    crate::paths::project_dirs().map(|dirs| dirs.config_dir.join("plugins"))
}

/// Where plugins' `storage` lives.
pub fn data_dir() -> Option<PathBuf> {
    crate::paths::project_dirs().map(|dirs| dirs.data_dir.join("plugin-data"))
}

/// A folder that has a `plugin.toml` but couldn't be loaded.
#[derive(Debug, Clone)]
pub struct Broken {
    pub folder: PathBuf,
    pub diagnostic: Diagnostic,
}

pub fn bundled() -> Vec<Arc<PluginCode>> {
    BUNDLED
        .iter()
        .filter_map(|(folder, files)| {
            let files: BTreeMap<String, String> = files
                .iter()
                .map(|(path, source)| ((*path).to_owned(), (*source).to_owned()))
                .collect();
            match from_files(files, Source::Bundled) {
                Ok(code) => Some(Arc::new(code)),
                Err(error) => {
                    log::error!("Bundled plugin {folder} is broken: {error}");
                    None
                }
            }
        })
        .collect()
}

fn from_files(
    mut files: BTreeMap<String, String>,
    source: Source,
) -> Result<PluginCode, Diagnostic> {
    let manifest_text = files
        .remove(manifest::MANIFEST_FILE)
        .ok_or_else(|| Diagnostic {
            file: manifest::MANIFEST_FILE.into(),
            line: None,
            message: "missing".into(),
        })?;
    let manifest = manifest::parse(&manifest_text)?;
    if !files.contains_key(manifest::MAIN_FILE) {
        return Err(Diagnostic {
            file: manifest::MAIN_FILE.into(),
            line: None,
            message: "missing".into(),
        });
    }
    Ok(PluginCode {
        manifest,
        files,
        source,
    })
}

/// Reads a plugin folder: the manifest and every `.lua` file in it.
pub fn load_folder(folder: &Path) -> Result<PluginCode, Diagnostic> {
    let mut files = BTreeMap::new();
    let manifest_path = folder.join(manifest::MANIFEST_FILE);
    let text = std::fs::read_to_string(&manifest_path).map_err(|error| Diagnostic {
        file: manifest::MANIFEST_FILE.into(),
        line: None,
        message: error.to_string(),
    })?;
    files.insert(manifest::MANIFEST_FILE.to_owned(), text);
    for entry in walkdir::WalkDir::new(folder)
        .max_depth(4)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "lua") {
            continue;
        }
        let Ok(relative) = path.strip_prefix(folder) else {
            continue;
        };
        let relative = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let source = std::fs::read_to_string(path).map_err(|error| Diagnostic {
            file: relative.clone(),
            line: None,
            message: error.to_string(),
        })?;
        if files.len() > 256 {
            return Err(Diagnostic {
                file: relative,
                line: None,
                message: "a plugin can have at most 256 Lua files".into(),
            });
        }
        files.insert(relative, source);
    }
    from_files(files, Source::User(folder.to_path_buf()))
}

/// Every plugin folder in the plugins directory.
pub fn scan(dir: &Path) -> (Vec<Arc<PluginCode>>, Vec<Broken>) {
    let mut loaded = Vec::new();
    let mut broken = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (loaded, broken);
    };
    let mut folders: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join(manifest::MANIFEST_FILE).is_file())
        .collect();
    folders.sort();
    for folder in folders {
        match load_folder(&folder) {
            Ok(code) => loaded.push(Arc::new(code)),
            Err(diagnostic) => broken.push(Broken { folder, diagnostic }),
        }
    }
    (loaded, broken)
}

/// Creates the plugins directory and keeps the editor support files in it
/// current, so `LuaLS` autocompletes the `dirfleet` API.
pub fn prepare_dir(dir: &Path) {
    let types = dir.join("types");
    if let Err(error) = std::fs::create_dir_all(&types) {
        log::warn!("Could not create {}: {error}", types.display());
        return;
    }
    for (path, content) in [
        (types.join("dirfleet.d.lua"), TYPES),
        (dir.join(".luarc.json"), LUARC),
    ] {
        if std::fs::read_to_string(&path).ok().as_deref() != Some(content)
            && let Err(error) = std::fs::write(&path, content)
        {
            log::warn!("Could not write {}: {error}", path.display());
        }
    }
}

/// The plugin folder a changed path belongs to.
pub fn folder_of(dir: &Path, changed: &Path) -> Option<PathBuf> {
    let relative = changed.strip_prefix(dir).ok()?;
    let first = relative.components().next()?;
    let folder = dir.join(first.as_os_str());
    (folder.file_name()? != "types").then_some(folder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_plugins_load_and_compile() {
        let plugins = bundled();
        assert_eq!(plugins.len(), BUNDLED.len(), "a bundled manifest is broken");
        for code in plugins {
            let permissions = crate::plugins::permissions::PermissionSet::from_manifest(
                &code.manifest.permissions,
            );
            crate::plugins::sandbox::create(&code, permissions, Arc::default(), None)
                .unwrap_or_else(|error| panic!("{} failed to load: {error}", code.id()));
        }
    }

    #[test]
    fn scans_folders_and_reports_broken_ones() {
        let dir = std::env::temp_dir().join(format!("dirfleet_scan_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let good = dir.join("good");
        std::fs::create_dir_all(good.join("lib")).expect("dir");
        std::fs::write(
            good.join("plugin.toml"),
            "api_version = 1\nid = \"io.example.good\"\nname = \"Good\"\nversion = \"1.0.0\"\nauthor = \"Me\"\n",
        )
        .expect("write");
        std::fs::write(good.join("main.lua"), "return {}").expect("write");
        std::fs::write(good.join("lib").join("x.lua"), "return 1").expect("write");
        let bad = dir.join("bad");
        std::fs::create_dir_all(&bad).expect("dir");
        std::fs::write(bad.join("plugin.toml"), "api_version = 1\n").expect("write");
        let (loaded, broken) = scan(&dir);
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].files.contains_key("lib/x.lua"));
        assert_eq!(broken.len(), 1);
        assert_eq!(broken[0].folder, bad);
        assert_eq!(
            folder_of(&dir, &good.join("lib").join("x.lua")),
            Some(good.clone())
        );
        assert_eq!(folder_of(&dir, &dir.join("types").join("a.lua")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
