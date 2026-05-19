use crate::data::files::{DirContent, DirEntry};
use bincode::config;
use directories::ProjectDirs;
use std::{
    collections::BTreeSet,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    sync::mpsc,
    sync::{LazyLock, Mutex},
    thread,
};

const MAX_CACHE_SIZE_BYTES: u64 = 100 * 1024 * 1024; // 100 MB
const EVICT_TARGET_BYTES: u64 = 50 * 1024 * 1024; // 50 MB target after eviction
const EVICT_CHECK_INTERVAL: u64 = 1000;
const EVICT_BATCH_SIZE: usize = 500;

static CACHE_INSERT_COUNT: AtomicU64 = AtomicU64::new(0);

pub static SLED_DIRS: LazyLock<sled::Db> = LazyLock::new(|| {
    let path = ProjectDirs::from("com", "Crayen", "Files2").expect("");
    if !path.data_dir().exists() {
        std::fs::create_dir_all(path.data_dir()).expect("Failed to create data directory");
    }
    let dir = path.data_dir().join("dirs");
    sled::open(&dir).unwrap_or_else(|_| panic!("Failed to open database at {}", dir.display()))
});

static CACHE_GENERATIONS: LazyLock<Mutex<HashMap<Vec<u8>, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

enum CacheWrite {
    Insert {
        path: Vec<u8>,
        generation: u64,
        data: Vec<u8>,
        mtime_nanos: Option<u128>,
    },
}

/// Dedicated single-threaded cache writer. Processes sled writes sequentially
/// to avoid spawning a thread per cache miss.
fn cache_writer_sender() -> mpsc::Sender<CacheWrite> {
    static WRITER: LazyLock<Mutex<Option<mpsc::Sender<CacheWrite>>>> = LazyLock::new(|| Mutex::new(None));
    let mut guard = WRITER.lock().expect("cache writer mutex poisoned");
    if let Some(ref sender) = *guard {
        return sender.clone();
    }
    let (tx, rx) = mpsc::channel::<CacheWrite>();
    let _ = thread::spawn(move || {
        for msg in rx {
            #[cfg(feature = "profiling")]
            puffin::profile_scope!("lwa_fm::database::cache_writer");
            match msg {
                CacheWrite::Insert {
                    path,
                    generation,
                    data,
                    mtime_nanos,
                } => {
                    if current_generation(&path) != generation {
                        log::info!("Skipped stale cache write");
                        continue;
                    }
                    if let Some(mtime) = mtime_nanos {
                        #[cfg(feature = "profiling")]
                        puffin::profile_scope!("lwa_fm::database::cache_writer::mtime");
                        _ = SLED_DIRS.insert(
                            mtime_key(&path),
                            &mtime.to_ne_bytes()[..],
                        );
                    }
                    _ = SLED_DIRS.insert(path, data);
                    log::info!("Data saved");
                    maybe_evict_cache();
                }
            }
        }
    });
    *guard = Some(tx.clone());
    tx
}

// All callers pass already-normalized paths (from user navigation or file system watchers).
fn cache_key(dir: &Path) -> Vec<u8> {
    #[cfg(windows)]
    {
        // Windows paths are case-insensitive; lowercase the string representation
        // so "C:\Foo" and "c:\foo" map to the same cache entry.
        dir.to_string_lossy().to_ascii_lowercase().into_bytes()
    }
    #[cfg(not(windows))]
    {
        dir.as_os_str().as_encoded_bytes().to_vec()
    }
}

fn current_generation(path: &[u8]) -> u64 {
    CACHE_GENERATIONS
        .lock()
        .expect("cache generation mutex poisoned")
        .get(path)
        .copied()
        .unwrap_or_default()
}

fn bump_generation(path: &[u8]) {
    let mut generations = CACHE_GENERATIONS
        .lock()
        .expect("cache generation mutex poisoned");
    let generation = generations.entry(path.to_vec()).or_default();
    *generation += 1;
}

pub fn invalidate_dir(dir: &Path) {
    let path = cache_key(dir);
    bump_generation(&path);
    _ = SLED_DIRS.remove(mtime_key(&path));
    if let Err(err) = SLED_DIRS.remove(&path) {
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
    let generation = current_generation(&path);

    // Safety net: check directory modification time. If the directory's mtime
    // changed since it was cached, invalidate the stale entry.
    #[cfg(feature = "profiling")]
    puffin::profile_scope!("lwa_fm::database::read_dir::mtime_check");
    if let Ok(dir_meta) = std::fs::metadata(dir) {
        if let Ok(current_mtime) = dir_meta.modified() {
            if let Ok(current_nanos) = current_mtime.duration_since(std::time::UNIX_EPOCH) {
                let current_nanos = current_nanos.as_nanos();
                if let Ok(Some(stored)) = SLED_DIRS.get(mtime_key(&path)) {
                    if stored.len() == 16 {
                        let stored_nanos = u128::from_ne_bytes(
                            stored.as_ref().try_into().unwrap_or([0; 16]),
                        );
                        if stored_nanos != current_nanos {
                            log::info!("Directory mtime changed for {}, invalidating cache", dir.display());
                            bump_generation(&path);
                            _ = SLED_DIRS.remove(&path);
                        }
                    }
                }
            }
        }
    }

    if let Ok(Some(data)) = SLED_DIRS.get(&path) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::db_read::deserialize");
        if let Ok((meta, _)) = bincode::decode_from_slice::<DirContent, _>(&data[..], config) {
            // Verify generation still matches (in case invalidate_dir failed to remove the entry)
            if current_generation(&path) == generation {
                #[cfg(feature = "profiling")]
                puffin::profile_scope!("lwa_fm::dir_handling::db_read::deserialize::extend");
                meta.populate(entries);
                return;
            }
            // Generation mismatch — cache entry is stale; remove it
            log::info!("Stale cache entry (generation mismatch) for {}, removing", dir.display());
            _ = SLED_DIRS.remove(&path);
        } else {
            // Corrupt or incompatible cache entry; remove it
            log::info!("Corrupt cache entry for {}, removing", dir.display());
            _ = SLED_DIRS.remove(&path);
        }
    }
    let Some(dir_content) = DirContent::read(dir) else {
        return;
    };
    dir_content.populate(entries);
    #[cfg(feature = "profiling")]
    puffin::profile_scope!("lwa_fm::database::read_dir::cache_write");
    let entry_mtime = std::fs::metadata(dir).ok()
        .and_then(|m| m.modified().ok());
    let mtime_nanos = entry_mtime.and_then(|m| {
        m.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_nanos())
    });
    if let Ok(data) = bincode::encode_to_vec(&dir_content, config) {
        _ = cache_writer_sender().send(CacheWrite::Insert {
            path,
            generation,
            data,
            mtime_nanos,
        });
    }
}

fn mtime_key(path: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(path.len() + 6);
    key.extend_from_slice(b"mtime_");
    key.extend_from_slice(path);
    key
}

fn maybe_evict_cache() {
    #[cfg(feature = "profiling")]
    puffin::profile_scope!("lwa_fm::database::maybe_evict_cache");
    let count = CACHE_INSERT_COUNT.fetch_add(1, Ordering::Relaxed);
    if !count.is_multiple_of(EVICT_CHECK_INTERVAL) {
        return;
    }
    let Ok(size) = SLED_DIRS.size_on_disk() else {
        return;
    };
    if size <= MAX_CACHE_SIZE_BYTES {
        return;
    }
    log::warn!("Sled cache size {size} exceeds {MAX_CACHE_SIZE_BYTES}, evicting");
    let mut removed = 0u64;
    let keys: Vec<Vec<u8>> = SLED_DIRS
        .iter()
        .keys()
        .take(EVICT_BATCH_SIZE)
        .filter_map(Result::ok)
        .map(|ivec| ivec.to_vec())
        .collect();
    for key in &keys {
        if SLED_DIRS.size_on_disk().unwrap_or(size) <= EVICT_TARGET_BYTES {
            break;
        }
        // Skip mtime entries when evicting directory listings
        if key.starts_with(b"mtime_") {
            continue;
        }
        if SLED_DIRS.remove(key.as_slice()).is_ok() {
            removed += 1;
        }
    }
    if removed == 0 {
        // Fallback: if individual removal doesn't help, clear everything
        log::warn!("Sled cache eviction removed 0 entries, falling back to full clear");
        if let Err(e) = SLED_DIRS.clear() {
            log::error!("Failed to clear sled cache: {e}");
            return;
        }
        if let Ok(mut generations) = CACHE_GENERATIONS.lock() {
            generations.clear();
        }
    }
    log::info!("Evicted {removed} entries from sled cache (target: {EVICT_TARGET_BYTES})");
}
