use crate::data::files::{DirContent, DirEntry};
use bincode::config;
use directories::ProjectDirs;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::LazyLock,
    thread,
};

pub static SLED_DIRS: LazyLock<sled::Db> = LazyLock::new(|| {
    let path = ProjectDirs::from("com", "Crayen", "Files2").expect("");
    if !path.data_dir().exists() {
        std::fs::create_dir_all(path.data_dir()).expect("Failed to create data directory");
    }
    let dir = path.data_dir().join("dirs");
    sled::open(&dir).unwrap_or_else(|_| panic!("Failed to open database at {}", dir.display()))
});

fn cache_key(dir: &Path) -> Vec<u8> {
    dir.as_os_str().as_encoded_bytes().to_vec()
}

pub fn invalidate_dir(dir: &Path) {
    let path = cache_key(dir);
    if let Err(err) = SLED_DIRS.remove(path) {
        log::warn!("Failed to invalidate cache for {}: {err}", dir.display());
        return;
    }
    log::info!("Invalidated cache for {}", dir.display());
    _ = SLED_DIRS.flush_async();
}

pub fn invalidate_dirs(paths: impl IntoIterator<Item = PathBuf>) {
    let unique_paths: BTreeSet<PathBuf> = paths.into_iter().collect();
    for path in unique_paths {
        invalidate_dir(&path);
    }
}

pub fn read_dir(dir: &Path, entries: &mut Vec<DirEntry>) {
    #[cfg(feature = "profiling")]
    puffin::profile_scope!("lwa_fm::dir_handling::db_read");
    let config = config::standard();
    let path = cache_key(dir);
    if let Ok(Some(data)) = SLED_DIRS.get(&path) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::db_read::deserialize");
        if let Ok((meta, _)) = bincode::decode_from_slice::<DirContent, _>(&data[..], config) {
            #[cfg(feature = "profiling")]
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
