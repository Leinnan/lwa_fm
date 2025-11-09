use crate::data::files::{DirContent, DirEntry};
use bincode::config;
use directories::ProjectDirs;
use std::{os::unix::ffi::OsStrExt, path::Path, sync::LazyLock, thread};

pub static SLED_DIRS: LazyLock<sled::Db> = LazyLock::new(|| {
    let path = ProjectDirs::from("com", "Crayen", "Files").expect("");
    if !path.data_dir().exists() {
        std::fs::create_dir_all(path.data_dir()).expect("Failed to create data directory");
    }
    let dir = path.data_dir().join("dirs");
    sled::open(&dir).expect(&format!("Failed to open database at {}", dir.display()))
});

pub fn read_dir(dir: &Path, entries: &mut Vec<DirEntry>) {
    #[cfg(feature = "profiling")]
    puffin::profile_scope!("lwa_fm::dir_handling::db_read");
    let config = config::standard();
    let path = dir.as_os_str().as_bytes().to_vec();
    if let Ok(Some(data)) = SLED_DIRS.get(&path) {
        puffin::profile_scope!("lwa_fm::dir_handling::db_read::deserialize");
        if let Ok((meta, _)) = bincode::decode_from_slice::<DirContent, _>(&data[..], config) {
            puffin::profile_scope!("lwa_fm::dir_handling::db_read::deserialize::extend");
            meta.populate(entries);
            return;
        }
    }
    let Some(dir_content) = DirContent::read(dir) else {
        return;
    };
    dir_content.populate(entries);
    _ = thread::spawn(move || {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::db_read::save_data");
        let Ok(data) = bincode::encode_to_vec(&dir_content, config) else {
            log::info!("Data not saved");
            return;
        };
        _ = SLED_DIRS.insert(path, data);
        log::info!("Data saved");
    });
}
