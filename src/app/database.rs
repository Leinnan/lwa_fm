use crate::data::files::{
    DirContent, DirEntry, DirEntryData, DirEntryMetaData, EntryType, SortKey,
};
use bincode::config;
use directories::ProjectDirs;
use lru::LruCache;
use std::{
    collections::BTreeSet,
    collections::HashMap,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
    sync::mpsc,
    sync::{LazyLock, Mutex},
    thread,
};

const MAX_CACHE_SIZE_BYTES: u64 = 100 * 1024 * 1024; // 100 MB
const EVICT_TARGET_BYTES: u64 = 50 * 1024 * 1024; // 50 MB target after eviction
const EVICT_CHECK_INTERVAL: u64 = 1000;
const EVICT_BATCH_SIZE: usize = 500;
const IN_MEMORY_CACHE_CAPACITY: usize = 500;

/// Hot in-memory cache: avoids the mtime stat + sled deserialization for
/// recently accessed directories. The tuple is `(mtime_nanos, DirContent)`.
static IN_MEMORY_CACHE: LazyLock<Mutex<LruCache<Vec<u8>, (u128, Arc<DirContent>)>>> =
    LazyLock::new(|| {
        Mutex::new(LruCache::new(
            NonZeroUsize::new(IN_MEMORY_CACHE_CAPACITY).expect("capacity must be > 0"),
        ))
    });

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
    ApplyChanges {
        dir: PathBuf,
        changes: Vec<crate::watcher::FileChange>,
    },
}

/// Dedicated single-threaded cache writer. Processes sled writes sequentially
/// to avoid spawning a thread per cache miss.
fn cache_writer_sender() -> mpsc::Sender<CacheWrite> {
    static WRITER: LazyLock<Mutex<Option<mpsc::Sender<CacheWrite>>>> =
        LazyLock::new(|| Mutex::new(None));
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
                        _ = SLED_DIRS.insert(mtime_key(&path), &mtime.to_ne_bytes()[..]);
                    }
                    _ = SLED_DIRS.insert(path, data);
                    log::info!("Data saved");
                    maybe_evict_cache();
                }
                CacheWrite::ApplyChanges { dir, changes } => {
                    apply_cached_file_changes(&dir, &changes);
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

/// Queue a watcher batch for the dedicated cache writer. This keeps sled reads,
/// serialization, and persistent writes off the UI thread.
pub fn apply_file_changes(dir: PathBuf, changes: Vec<crate::watcher::FileChange>) {
    if changes.is_empty() {
        return;
    }
    _ = cache_writer_sender().send(CacheWrite::ApplyChanges { dir, changes });
}

fn apply_cached_file_changes(dir: &Path, changes: &[crate::watcher::FileChange]) {
    enum Mutation {
        Upsert(DirEntryData),
        Update(String, DirEntryMetaData),
        Remove(String),
    }

    let mutations = changes
        .iter()
        .filter_map(|change| {
            let path = match change {
                crate::watcher::FileChange::Metadata(path)
                | crate::watcher::FileChange::Created(path)
                | crate::watcher::FileChange::Removed(path) => path,
            };
            let file_name = path.file_name()?.to_str()?.to_owned();
            match change {
                crate::watcher::FileChange::Metadata(_) => std::fs::metadata(path)
                    .ok()
                    .map(DirEntryMetaData::from)
                    .map(|meta| Mutation::Update(file_name, meta)),
                crate::watcher::FileChange::Created(_) => std::fs::metadata(path)
                    .ok()
                    .map(|metadata| Mutation::Upsert(build_entry_data(metadata, file_name))),
                crate::watcher::FileChange::Removed(_) => Some(Mutation::Remove(file_name)),
            }
        })
        .collect::<Vec<_>>();

    mutate_cached_entries(dir, |entries| {
        let mut changed = false;
        for mutation in &mutations {
            match mutation {
                Mutation::Upsert(entry) => {
                    upsert_entry(entries, entry.clone());
                    changed = true;
                }
                Mutation::Update(file_name, meta) => {
                    if let Some(entry) = entries
                        .iter_mut()
                        .find(|entry| entry.file_name == *file_name)
                    {
                        entry.meta = *meta;
                        changed = true;
                    }
                }
                Mutation::Remove(file_name) => {
                    changed |= remove_entry(entries, file_name);
                }
            }
        }
        changed
    });
}

pub fn update_file_metadata(path: &Path) {
    let Some(dir) = path.parent() else {
        return;
    };
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    let meta: DirEntryMetaData = metadata.into();
    let config = config::standard();
    let key = cache_key(dir);

    if let Ok(mut mem_cache) = IN_MEMORY_CACHE.lock()
        && let Some((cached_mtime, content)) = mem_cache.pop(&key)
    {
        let mut content = (*content).clone();
        if let Some(entry) = content
            .entries
            .iter_mut()
            .find(|entry| entry.file_name == file_name)
        {
            entry.meta = meta;
        }
        mem_cache.put(key.clone(), (cached_mtime, Arc::new(content)));
    }

    if let Ok(Some(data)) = SLED_DIRS.get(&key)
        && let Ok((mut content, _)) = bincode::decode_from_slice::<DirContent, _>(&data[..], config)
        && let Some(entry) = content
            .entries
            .iter_mut()
            .find(|entry| entry.file_name == file_name)
    {
        entry.meta = meta;
        if let Ok(data) = bincode::encode_to_vec(&content, config) {
            _ = SLED_DIRS.insert(key, data);
        }
    }
}

/// Surgically insert (or replace) a single entry in the cached `DirContent` for
/// `path`'s parent directory, without re-reading the whole directory. Mirrors
/// [`update_file_metadata`] but for newly-created files. The directory's stored
/// mtime guard is refreshed so the cache stays consistent (creating a file
/// changes the parent dir's mtime).
pub fn insert_file_entry(path: &Path) {
    let Some(dir) = path.parent() else {
        return;
    };
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    let Ok(metadata) = std::fs::metadata(path) else {
        // File is already gone (fast create/delete) — nothing to cache.
        return;
    };
    let entry = build_entry_data(metadata, file_name.to_string());
    mutate_cached_entries(dir, |entries| {
        upsert_entry(entries, entry.clone());
        true
    });
}

/// Surgically remove a single entry from the cached `DirContent` for `path`'s
/// parent directory, without re-reading it. Returns silently if the entry (or
/// its cache) is absent.
pub fn remove_file_entry(path: &Path) {
    let Some(dir) = path.parent() else {
        return;
    };
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    mutate_cached_entries(dir, |entries| remove_entry(entries, file_name));
}

/// Build a cacheable [`DirEntryData`] straight from metadata + a file name,
/// avoiding a full directory re-scan.
fn build_entry_data(metadata: std::fs::Metadata, file_name: String) -> DirEntryData {
    let meta: DirEntryMetaData = metadata.into();
    let is_file = matches!(meta.entry_type, EntryType::File);
    DirEntryData {
        sort_key: SortKey::new_path(&file_name, is_file),
        file_name,
        meta,
    }
}

fn upsert_entry(entries: &mut Vec<DirEntryData>, entry: DirEntryData) {
    if let Some(existing) = entries.iter_mut().find(|e| e.file_name == entry.file_name) {
        *existing = entry;
    } else {
        entries.push(entry);
    }
}

fn remove_entry(entries: &mut Vec<DirEntryData>, file_name: &str) -> bool {
    let before = entries.len();
    entries.retain(|e| e.file_name != file_name);
    entries.len() != before
}

/// Apply `mutate` to the cached `DirContent` entries for `dir` in both the
/// in-memory hot cache and the sled store. When the mutation changes content,
/// the stored mtime guard is refreshed so a subsequent read doesn't discard the
/// surgical update as stale (creating/deleting a file changes the dir mtime).
fn mutate_cached_entries<F>(dir: &Path, mutate: F)
where
    F: Fn(&mut Vec<DirEntryData>) -> bool,
{
    let config = config::standard();
    let key = cache_key(dir);
    let new_mtime = current_mtime_nanos(dir);
    let mut changed_mem = false;
    let mut changed_sled = false;

    if let Ok(mut mem_cache) = IN_MEMORY_CACHE.lock()
        && let Some((cached_mtime, content)) = mem_cache.pop(&key)
    {
        let mut content = (*content).clone();
        changed_mem = mutate(&mut content.entries);
        mem_cache.put(
            key.clone(),
            (new_mtime.unwrap_or(cached_mtime), Arc::new(content)),
        );
    }

    if let Ok(Some(data)) = SLED_DIRS.get(&key)
        && let Ok((mut content, _)) = bincode::decode_from_slice::<DirContent, _>(&data[..], config)
    {
        if mutate(&mut content.entries) {
            changed_sled = true;
            if let Ok(data) = bincode::encode_to_vec(&content, config) {
                _ = SLED_DIRS.insert(&key, data);
            }
        }
    }

    if (changed_mem || changed_sled)
        && let Some(mtime) = new_mtime
    {
        _ = SLED_DIRS.insert(mtime_key(&key), &mtime.to_ne_bytes()[..]);
    }
}

fn current_mtime_nanos(dir: &Path) -> Option<u128> {
    let modified = std::fs::metadata(dir).ok()?.modified().ok()?;
    modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_nanos())
}

pub fn read_dir(dir: &Path, entries: &mut Vec<DirEntry>) {
    #[cfg(feature = "profiling")]
    puffin::profile_scope!("lwa_fm::dir_handling::db_read");
    let config = config::standard();
    let path = cache_key(dir);
    let generation = current_generation(&path);

    // ── Tier 1: In-memory hot cache (avoids mtime stat + sled deserialization) ──
    if let Ok(mut mem_cache) = IN_MEMORY_CACHE.lock() {
        if let Some(&(cached_mtime, ref content)) = mem_cache.get(&path) {
            // Verify generation
            if current_generation(&path) == generation {
                // Fast mtime check via in-memory value (no syscall)
                if let Ok(dir_meta) = std::fs::metadata(dir) {
                    if let Ok(modified) = dir_meta.modified() {
                        if let Ok(nanos) = modified.duration_since(std::time::UNIX_EPOCH) {
                            if nanos.as_nanos() == cached_mtime {
                                content.populate(entries);
                                return;
                            }
                        }
                    }
                }
                // mtime changed — remove from memory cache, fall through
                mem_cache.pop(&path);
            }
        }
    }

    // ── Tier 2: Sled on-disk cache with mtime guard ──
    #[cfg(feature = "profiling")]
    puffin::profile_scope!("lwa_fm::database::read_dir::mtime_check");
    if let Ok(dir_meta) = std::fs::metadata(dir) {
        if let Ok(current_mtime) = dir_meta.modified() {
            if let Ok(current_nanos) = current_mtime.duration_since(std::time::UNIX_EPOCH) {
                let current_nanos = current_nanos.as_nanos();
                if let Ok(Some(stored)) = SLED_DIRS.get(mtime_key(&path)) {
                    if stored.len() == 16 {
                        let stored_nanos =
                            u128::from_ne_bytes(stored.as_ref().try_into().unwrap_or([0; 16]));
                        if stored_nanos != current_nanos {
                            log::info!(
                                "Directory mtime changed for {}, invalidating cache",
                                dir.display()
                            );
                            bump_generation(&path);
                            _ = SLED_DIRS.remove(&path);
                        }
                    }
                }
            }
        }
    }

    let mut from_memory: Option<Arc<DirContent>> = None;
    if let Ok(Some(data)) = SLED_DIRS.get(&path) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::db_read::deserialize");
        if let Ok((meta, _)) = bincode::decode_from_slice::<DirContent, _>(&data[..], config) {
            // Verify generation still matches (in case invalidate_dir failed to remove the entry)
            if current_generation(&path) == generation {
                #[cfg(feature = "profiling")]
                puffin::profile_scope!("lwa_fm::dir_handling::db_read::deserialize::extend");
                from_memory = Some(Arc::new(meta));
            } else {
                // Generation mismatch — cache entry is stale; remove it
                log::info!(
                    "Stale cache entry (generation mismatch) for {}, removing",
                    dir.display()
                );
                _ = SLED_DIRS.remove(&path);
            }
        } else {
            // Corrupt or incompatible cache entry; remove it
            log::info!("Corrupt cache entry for {}, removing", dir.display());
            _ = SLED_DIRS.remove(&path);
        }
    }

    let dir_content = match from_memory {
        Some(content) => content,
        None => {
            let Some(content) = DirContent::read(dir) else {
                return;
            };
            let content = Arc::new(content);
            // Write to sled via background thread
            #[cfg(feature = "profiling")]
            puffin::profile_scope!("lwa_fm::database::read_dir::cache_write");
            let entry_mtime = std::fs::metadata(dir).ok().and_then(|m| m.modified().ok());
            let mtime_nanos = entry_mtime.and_then(|m| {
                m.duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_nanos())
            });
            if let Ok(data) = bincode::encode_to_vec(&content, config) {
                _ = cache_writer_sender().send(CacheWrite::Insert {
                    path: path.clone(),
                    generation,
                    data,
                    mtime_nanos,
                });
            }
            content
        }
    };

    // Populate entries and seed in-memory cache
    dir_content.populate(entries);
    if let Ok(mut mem_cache) = IN_MEMORY_CACHE.lock() {
        if let Ok(dir_meta) = std::fs::metadata(dir) {
            if let Ok(modified) = dir_meta.modified() {
                if let Ok(nanos) = modified.duration_since(std::time::UNIX_EPOCH) {
                    mem_cache.put(path, (nanos.as_nanos(), Arc::clone(&dir_content)));
                }
            }
        }
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
    let Ok(mut current_size) = SLED_DIRS.size_on_disk() else {
        return;
    };
    if current_size <= MAX_CACHE_SIZE_BYTES {
        return;
    }
    log::warn!("Sled cache size {current_size} exceeds {MAX_CACHE_SIZE_BYTES}, evicting");
    let mut removed = 0u64;

    // First pass: scan for data keys, filtering out mtime entries
    let keys: Vec<Vec<u8>> = SLED_DIRS
        .iter()
        .keys()
        .filter_map(Result::ok)
        .filter(|k| !k.starts_with(b"mtime_"))
        .take(EVICT_BATCH_SIZE)
        .map(|ivec| ivec.to_vec())
        .collect();

    for key in &keys {
        current_size = SLED_DIRS.size_on_disk().unwrap_or(current_size);
        if current_size <= EVICT_TARGET_BYTES {
            break;
        }
        if SLED_DIRS.remove(key.as_slice()).is_ok() {
            removed += 1;
        }
    }

    // If first pass found no data keys, scan a larger batch before giving up
    if removed == 0 {
        let more: Vec<Vec<u8>> = SLED_DIRS
            .iter()
            .keys()
            .filter_map(Result::ok)
            .filter(|k| !k.starts_with(b"mtime_"))
            .take(EVICT_BATCH_SIZE * 4)
            .map(|ivec| ivec.to_vec())
            .collect();
        for key in &more {
            current_size = SLED_DIRS.size_on_disk().unwrap_or(current_size);
            if current_size <= EVICT_TARGET_BYTES {
                break;
            }
            _ = SLED_DIRS.remove(key.as_slice());
            removed += 1;
        }
    }

    log::info!("Evicted {removed} entries from sled cache (target: {EVICT_TARGET_BYTES})");
}
