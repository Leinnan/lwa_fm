use crate::data::files::{Listing, ListingChange};
use foldhash::{HashMap, HashMapExt};
use lru::LruCache;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
    sync::mpsc,
    sync::{LazyLock, Mutex},
    thread,
    time::Duration,
};

const MAX_CACHE_SIZE_BYTES: u64 = 100 * 1024 * 1024; // 100 MB
const EVICT_TARGET_BYTES: u64 = 50 * 1024 * 1024; // 50 MB target after eviction
const EVICT_CHECK_INTERVAL: u64 = 1000;
const EVICT_BATCH_SIZE: usize = 500;
/// Byte budget for the hot in-memory listing cache (it used to be bounded
/// by entry count only, so 500 huge folders could pin gigabytes).
const IN_MEMORY_CACHE_BYTES: usize = 256 * 1024 * 1024;
/// Watcher-driven persistent cache writes are coalesced per directory for
/// this long, so a hot file costs one decode/encode cycle, not one per batch.
const APPLY_DEBOUNCE: Duration = Duration::from_millis(500);

struct CachedListing {
    mtime_nanos: u128,
    listing: Arc<Listing>,
    bytes: usize,
}

/// Hot in-memory cache: avoids the sled read and decode for recently visited
/// directories. A hit is an `Arc` clone of the shared listing.
struct DirectoryCache {
    entries: LruCache<Vec<u8>, CachedListing>,
    bytes: usize,
}

impl DirectoryCache {
    fn get(&mut self, key: &[u8], mtime_nanos: u128) -> Option<Arc<Listing>> {
        let cached = self.entries.get(key)?;
        if cached.mtime_nanos == mtime_nanos {
            return Some(Arc::clone(&cached.listing));
        }
        self.remove(key);
        None
    }

    fn put(&mut self, key: Vec<u8>, mtime_nanos: u128, listing: Arc<Listing>) {
        let bytes = listing.approx_bytes();
        if let Some(previous) = self.entries.put(
            key,
            CachedListing {
                mtime_nanos,
                listing,
                bytes,
            },
        ) {
            self.bytes = self.bytes.saturating_sub(previous.bytes);
        }
        self.bytes += bytes;
        while self.bytes > IN_MEMORY_CACHE_BYTES {
            let Some((_, evicted)) = self.entries.pop_lru() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(evicted.bytes);
        }
    }

    fn remove(&mut self, key: &[u8]) -> Option<CachedListing> {
        let removed = self.entries.pop(key)?;
        self.bytes = self.bytes.saturating_sub(removed.bytes);
        Some(removed)
    }
}

static IN_MEMORY_CACHE: LazyLock<Mutex<DirectoryCache>> = LazyLock::new(|| {
    Mutex::new(DirectoryCache {
        entries: LruCache::unbounded(),
        bytes: 0,
    })
});

static CACHE_INSERT_COUNT: AtomicU64 = AtomicU64::new(0);

pub static SLED_DIRS: LazyLock<sled::Db> = LazyLock::new(|| {
    // Tests never touch the real database.
    if cfg!(test) {
        return sled::Config::new()
            .temporary(true)
            .open()
            .expect("Failed to open a temporary directory cache");
    }
    let path = crate::paths::project_dirs().expect("");
    if !path.data_dir.exists() {
        std::fs::create_dir_all(&path.data_dir).expect("Failed to create data directory");
    }
    let dir = path.data_dir.join("dirs");
    // Another running copy of the app holds the database's lock. This copy
    // then caches listings in memory only, for as long as it runs.
    sled::open(&dir).unwrap_or_else(|error| {
        log::warn!(
            "Directory cache at {} unavailable ({error}); using a temporary one",
            dir.display()
        );
        sled::Config::new()
            .temporary(true)
            .open()
            .expect("Failed to open a temporary directory cache")
    })
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
        changes: Vec<crate::watcher::ResolvedChange>,
    },
}

type PendingChanges = std::collections::BTreeMap<PathBuf, Vec<crate::watcher::ResolvedChange>>;

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
    let _ = thread::spawn(move || cache_writer(&rx));
    *guard = Some(tx.clone());
    tx
}

fn cache_writer(rx: &mpsc::Receiver<CacheWrite>) {
    // Watcher batches are coalesced per directory until the debounce window
    // passes without new messages.
    let mut pending = PendingChanges::new();
    loop {
        let message = if pending.is_empty() {
            match rx.recv() {
                Ok(message) => Some(message),
                Err(_) => return,
            }
        } else {
            match rx.recv_timeout(APPLY_DEBOUNCE) {
                Ok(message) => Some(message),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    flush_pending(&mut pending);
                    return;
                }
            }
        };
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::database::cache_writer");
        match message {
            None => flush_pending(&mut pending),
            Some(CacheWrite::Insert {
                path,
                generation,
                data,
                mtime_nanos,
            }) => {
                if current_generation(&path) != generation {
                    log::info!("Skipped stale cache write");
                    continue;
                }
                if let Some(mtime) = mtime_nanos {
                    _ = SLED_DIRS.insert(mtime_key(&path), &mtime.to_ne_bytes()[..]);
                }
                _ = SLED_DIRS.insert(path, data);
                maybe_evict_cache();
            }
            Some(CacheWrite::ApplyChanges { dir, changes }) => {
                pending.entry(dir).or_default().extend(changes);
            }
        }
    }
}

fn flush_pending(pending: &mut PendingChanges) {
    for (dir, changes) in std::mem::take(pending) {
        apply_persistent_file_changes(&dir, &changes);
    }
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
    *CACHE_GENERATIONS
        .lock()
        .expect("cache generation mutex poisoned")
        .entry(path.to_vec())
        .or_default() += 1;
}

pub fn invalidate_dir(dir: &Path) {
    let path = cache_key(dir);
    bump_generation(&path);
    if let Ok(mut mem_cache) = IN_MEMORY_CACHE.lock() {
        mem_cache.remove(&path);
    }
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

/// Apply a watcher batch to the cached listing of `dir`. The in-memory copy is
/// updated straight away (one new listing, no re-read); the persistent copy is
/// rewritten by the cache-writer thread after a short debounce. Metadata was
/// already read on the watcher thread.
pub fn apply_file_changes(dir: PathBuf, changes: Vec<crate::watcher::ResolvedChange>) {
    if changes.is_empty() {
        return;
    }
    let key = cache_key(&dir);
    let cached = IN_MEMORY_CACHE
        .lock()
        .ok()
        .and_then(|mut mem_cache| mem_cache.remove(&key));
    if let Some(cached) = cached {
        let new_mtime = current_mtime_nanos(&dir);
        let listing = cached
            .listing
            .with_changes(&listing_changes(&changes))
            .map_or(cached.listing, Arc::new);
        if let Ok(mut mem_cache) = IN_MEMORY_CACHE.lock() {
            mem_cache.put(key, new_mtime.unwrap_or(cached.mtime_nanos), listing);
        }
    }
    _ = cache_writer_sender().send(CacheWrite::ApplyChanges { dir, changes });
}

fn listing_changes(changes: &[crate::watcher::ResolvedChange]) -> Vec<ListingChange<'_>> {
    changes
        .iter()
        .filter_map(crate::watcher::ResolvedChange::listing_change)
        .collect()
}

/// Rewrite the sled record for `dir`. When the content changes, the stored
/// mtime guard is refreshed so a later read doesn't discard the surgical
/// update as stale (creating/deleting a file changes the dir mtime).
fn apply_persistent_file_changes(dir: &Path, changes: &[crate::watcher::ResolvedChange]) {
    let key = cache_key(dir);
    let Ok(Some(data)) = SLED_DIRS.get(&key) else {
        return;
    };
    let Some(listing) = Listing::decode(&data) else {
        return;
    };
    let Some(next) = listing.with_changes(&listing_changes(changes)) else {
        return;
    };
    if let Some(data) = next.encode() {
        _ = SLED_DIRS.insert(&key, data);
    }
    if let Some(mtime) = current_mtime_nanos(dir) {
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

/// Listing for `dir`: from the in-memory cache, the persistent cache, or a
/// fresh read (in that order). The directory mtime guards both cache tiers.
pub fn read_dir(dir: &Path) -> Option<Arc<Listing>> {
    #[cfg(feature = "profiling")]
    puffin::profile_scope!("lwa_fm::dir_handling::db_read");
    let path = cache_key(dir);
    let generation = current_generation(&path);
    // Stat outside of any lock: the mutex is shared with other readers.
    let current_nanos = current_mtime_nanos(dir);
    // A folder's metadata stays readable after macOS takes away access to its
    // contents: check the contents can still be read before serving a cached
    // copy. (This is also what makes macOS ask for access the first time.)
    #[cfg(target_os = "macos")]
    if std::fs::read_dir(dir)
        .is_err_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
    {
        return None;
    }

    // ── Tier 1: in-memory hot cache ──
    if let Some(mtime) = current_nanos
        && let Ok(mut mem_cache) = IN_MEMORY_CACHE.lock()
        && let Some(listing) = mem_cache.get(&path, mtime)
    {
        return Some(listing);
    }

    // ── Tier 2: sled on-disk cache with mtime guard ──
    if let Some(current_nanos) = current_nanos
        && let Ok(Some(stored)) = SLED_DIRS.get(mtime_key(&path))
        && stored.len() == 16
    {
        let stored_nanos = u128::from_ne_bytes(stored.as_ref().try_into().unwrap_or([0; 16]));
        if stored_nanos != current_nanos {
            log::info!(
                "Directory mtime changed for {}, invalidating cache",
                dir.display()
            );
            bump_generation(&path);
            _ = SLED_DIRS.remove(&path);
        }
    }

    let mut from_disk: Option<Arc<Listing>> = None;
    if let Ok(Some(data)) = SLED_DIRS.get(&path) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::dir_handling::db_read::deserialize");
        match Listing::decode(&data) {
            Some(listing) if current_generation(&path) == generation => {
                from_disk = Some(Arc::new(listing));
            }
            Some(_) => {
                log::info!("Stale cache entry for {}, removing", dir.display());
                _ = SLED_DIRS.remove(&path);
            }
            None => {
                // Corrupt or older-format entry; remove it.
                log::info!("Corrupt cache entry for {}, removing", dir.display());
                _ = SLED_DIRS.remove(&path);
            }
        }
    }

    let listing = if let Some(listing) = from_disk {
        listing
    } else {
        let listing = Arc::new(crate::data::files::read_listing(dir)?);
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::database::read_dir::cache_write");
        if let Some(data) = listing.encode() {
            _ = cache_writer_sender().send(CacheWrite::Insert {
                path: path.clone(),
                generation,
                data,
                mtime_nanos: current_nanos,
            });
        }
        listing
    };

    if let Some(mtime) = current_nanos
        && let Ok(mut mem_cache) = IN_MEMORY_CACHE.lock()
    {
        mem_cache.put(path, mtime, Arc::clone(&listing));
    }
    Some(listing)
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
