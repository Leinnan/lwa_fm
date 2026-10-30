//! macOS packages: directories (`.app`, `.framework`, `.rtfd`, ...) that
//! Finder shows and opens as single items.

use std::{
    collections::HashMap,
    path::Path,
    sync::{Mutex, OnceLock},
};

use objc2_app_kit::NSWorkspace;
use objc2_foundation::NSString;

/// Extensions known without asking `LaunchServices`. Whether an extension is a
/// package is declared by its type, so one answer per extension holds for
/// every directory carrying it.
const KNOWN: &[(&str, bool)] = &[
    ("app", true),
    ("appex", true),
    ("bundle", true),
    ("framework", true),
    ("kext", true),
    ("mpkg", true),
    ("photoslibrary", true),
    ("pkg", true),
    ("plugin", true),
    ("rtfd", true),
    ("xcodeproj", true),
    ("xcworkspace", true),
    ("xpc", true),
    ("d", false),
    ("git", false),
    ("local", false),
];

fn cache() -> &'static Mutex<HashMap<String, bool>> {
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(
            KNOWN
                .iter()
                .map(|&(ext, package)| (ext.to_owned(), package))
                .collect(),
        )
    })
}

/// Whether the directory at `path` is a package. Directories without an
/// extension never are; for the rest the answer is cached per extension.
#[must_use]
pub fn is_package(path: &Path) -> bool {
    // `.config` and other dot directories have no extension either.
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    if let Some(&package) = cache().lock().expect("package cache").get(&ext) {
        return package;
    }
    let package = NSWorkspace::sharedWorkspace()
        .isFilePackageAtPath(&NSString::from_str(&path.to_string_lossy()));
    cache().lock().expect("package cache").insert(ext, package);
    package
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_extension() {
        let root = std::env::temp_dir().join(format!("lwa_pkg_{}", std::process::id()));
        for dir in ["Foo.app/Contents", "Notes.rtfd", "plain", "v1.2", ".hidden"] {
            std::fs::create_dir_all(root.join(dir)).expect("create test dir");
        }
        assert!(is_package(&root.join("Foo.app")));
        assert!(is_package(&root.join("Notes.rtfd")));
        assert!(!is_package(&root.join("plain")));
        assert!(!is_package(&root.join("v1.2")));
        assert!(!is_package(&root.join(".hidden")));
        std::fs::remove_dir_all(&root).expect("cleanup");
    }
}
