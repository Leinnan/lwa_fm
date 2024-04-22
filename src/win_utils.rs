use std::path::{Path, PathBuf};

use winsafe::{co, SHGetKnownFolderPath};

pub fn print_recent() {
    let docs_folder =
        SHGetKnownFolderPath(&co::KNOWNFOLDERID::Recent, co::KF::DEFAULT, None).unwrap();
    let path = Path::new(&docs_folder).join("CustomDestinations");
    println!("Recent Folder Path: {}", path.display());

    // Collect each component of the path along with its corresponding full path
    let mut current_path = PathBuf::new();
    let mut parts = Vec::new();

    for component in path.components() {
        current_path.push(component.as_os_str());
        parts.push((
            component.as_os_str().to_string_lossy().to_string(),
            current_path.display().to_string(),
        ));
    }

    // Display each directory and its full path
    for (dir_name, full_path) in parts {
        println!("Directory: {}, Full path: {}", dir_name, full_path);
    }
}
