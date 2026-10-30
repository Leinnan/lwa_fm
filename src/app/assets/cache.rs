//! Disk storage with bounded reads, atomic publication, and recurring eviction.
#[cfg(not(test))]
use directories::ProjectDirs;
use lru::LruCache;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

static RECENCY: LazyLock<Mutex<LruCache<PathBuf, SystemTime>>> = LazyLock::new(|| {
    Mutex::new(LruCache::new(
        std::num::NonZero::new(8192).expect("recency capacity"),
    ))
});
static WRITTEN_BYTES: AtomicU64 = AtomicU64::new(0);
static MAINTENANCE_RUNNING: AtomicBool = AtomicBool::new(false);
static LAST_MAINTENANCE: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));

/// How often a read refreshes the file's timestamp on disk.
const PERSIST_RECENCY_EVERY: Duration = Duration::from_hours(1);
/// How often the cache folder is swept when little was written.
const MAINTENANCE_INTERVAL: Duration = Duration::from_mins(10);
/// Written bytes that trigger a sweep before the interval is up.
const MAINTENANCE_AFTER_BYTES: u64 = 16 * 1024 * 1024;

/// Records that a cache entry was used. Recency lives in memory, and is
/// also written to the file's modified time at most once an hour, so the
/// eviction order survives a restart.
pub(super) fn touch(path: &Path) {
    let now = SystemTime::now();
    let previous = RECENCY
        .lock()
        .expect("cache recency mutex")
        .put(path.to_path_buf(), now);
    let stale = previous.is_none_or(|seen| {
        now.duration_since(seen)
            .is_ok_and(|age| age >= PERSIST_RECENCY_EVERY)
    });
    if stale
        && fs::metadata(path)
            .and_then(|meta| meta.modified())
            .is_ok_and(|modified| {
                now.duration_since(modified)
                    .is_ok_and(|age| age >= PERSIST_RECENCY_EVERY)
            })
        && let Ok(file) = fs::File::options().write(true).open(path)
    {
        let _ = file.set_modified(now);
    }
}

pub(super) fn read_bounded(path: &Path, limit: usize) -> std::io::Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    if file.metadata()?.len() > limit as u64 {
        return Err(std::io::Error::other("Cache entry exceeds byte limit"));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(std::io::Error::other("Cache entry exceeds byte limit"));
    }
    touch(path);
    Ok(bytes)
}

pub(super) fn read_overview_first(path: &Path, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    if file.metadata()?.len() > limit as u64 {
        return Err(std::io::Error::other("Overview exceeds limit"));
    }
    let mut bytes = vec![0; 76];
    file.read_exact(&mut bytes)?;
    let length = u32::from_le_bytes(bytes[64..68].try_into().expect("frame length")) as usize;
    let total = 76usize
        .checked_add(length)
        .filter(|total| *total <= limit)
        .ok_or_else(|| std::io::Error::other("Overview frame exceeds limit"))?;
    bytes.resize(total, 0);
    file.read_exact(&mut bytes[76..])?;
    touch(path);
    Ok(bytes)
}

fn note_write(path: &Path) {
    if let Ok(metadata) = fs::metadata(path) {
        WRITTEN_BYTES.fetch_add(metadata.len(), Ordering::Relaxed);
    }
    touch(path);
    maybe_maintain();
}

pub(super) fn atomic_save_bytes(bytes: &[u8], path: &Path) -> bool {
    let tmp = atomic_temp_path(path, "bin");
    if fs::write(&tmp, bytes).is_ok() && fs::rename(&tmp, path).is_ok() {
        note_write(path);
        true
    } else {
        let _ = fs::remove_file(tmp);
        false
    }
}

pub(super) fn maybe_maintain() {
    let mut last = LAST_MAINTENANCE.lock().expect("cache maintenance mutex");
    if last.is_some_and(|time| time.elapsed() < MAINTENANCE_INTERVAL)
        && WRITTEN_BYTES.load(Ordering::Relaxed) < MAINTENANCE_AFTER_BYTES
    {
        return;
    }
    if MAINTENANCE_RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    *last = Some(Instant::now());
    WRITTEN_BYTES.store(0, Ordering::Relaxed);
    drop(last);
    std::thread::spawn(|| {
        maintain(&thumbnail_cache_dir(), 512 * 1024 * 1024, 384 * 1024 * 1024);
        MAINTENANCE_RUNNING.store(false, Ordering::Release);
    });
}

pub(super) fn maintain(root: &Path, max_bytes: u64, target_bytes: u64) {
    let mut files = Vec::new();
    let mut groups = std::collections::HashMap::<String, (SystemTime, u64, Vec<PathBuf>)>::new();
    for entry in walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let path = entry.into_path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("tmp"))
        {
            if modified
                .elapsed()
                .is_ok_and(|age| age > Duration::from_secs(3600))
            {
                let _ = fs::remove_file(path);
            }
            continue;
        }
        if name.ends_with("_owner.json")
            || name.contains("_v2.")
            || name.contains("_v3.")
            || name.contains("_v4.")
            || name.contains("_v5.")
            || name.contains("_v6.")
            || name.contains("_overview_v7.")
        {
            let _ = fs::remove_file(path);
            continue;
        }
        let recency = RECENCY
            .lock()
            .expect("cache recency mutex")
            .peek(&path)
            .copied()
            .unwrap_or(modified);
        let group = name
            .split_once('_')
            .filter(|(prefix, _)| {
                prefix.len() == 16 && prefix.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            .map_or_else(
                || path.to_string_lossy().into_owned(),
                |(prefix, _)| prefix.to_owned(),
            );
        let group = groups.entry(group).or_insert((recency, 0, Vec::new()));
        group.0 = group.0.max(recency);
        group.1 += metadata.len();
        group.2.push(path);
    }
    files.extend(groups.into_values());
    let mut total: u64 = files.iter().map(|(_, size, _)| *size).sum();
    if total <= max_bytes {
        return;
    }
    files.sort_by_key(|(time, _, _)| *time);
    for (_, _, paths) in files {
        if total <= target_bytes {
            break;
        }
        for path in paths {
            let size = fs::metadata(&path).map_or(0, |metadata| metadata.len());
            if fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(size);
                RECENCY.lock().expect("cache recency mutex").pop(&path);
            }
        }
    }
}

/// Create a cache shard directory at most once per process instead of a
/// `create_dir_all` per thumbnail request.
pub(super) fn ensure_shard(prefix: &str) -> PathBuf {
    static CREATED: LazyLock<Mutex<std::collections::HashSet<String>>> =
        LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));
    let shard_name = &prefix[..2];
    let shard = thumbnail_cache_dir().join(shard_name);
    let mut created = CREATED.lock().expect("cache shard mutex");
    // Only success is remembered, so a cache folder deleted while the app
    // runs is created again on the next write.
    if !created.contains(shard_name) && fs::create_dir_all(&shard).is_ok() {
        created.insert(shard_name.to_owned());
    }
    shard
}

pub(super) fn source_prefix(path: &Path) -> String {
    let mut hash = stable_hasher();
    crate::helper::canonical_string(path).hash(&mut hash);
    format!("{:016x}", hash.finish())
}

/// A hasher whose output doesn't change with the Rust toolchain, so cache
/// file names and checksums stay valid across upgrades (`DefaultHasher`
/// makes no such promise).
pub(super) fn stable_hasher() -> impl Hasher {
    use std::hash::BuildHasher;
    foldhash::fast::FixedState::default().build_hasher()
}

/// Where the thumbnail of `path` at `target_edge` is cached. The `.jpg` name
/// is the default; images with transparency are stored next to it as `.png`
/// (see [`cached_variant`] and [`atomic_save_image`]).
pub(super) fn thumbnail_cache_path(
    path: &Path,
    target_edge: u32,
    source_revision: u128,
    source_size: u64,
) -> PathBuf {
    let mut hasher = stable_hasher();
    crate::helper::canonical_string(path).hash(&mut hasher);
    if super::kinds::media_kind(path) == Some(super::kinds::MediaKind::Video) {
        super::media::backend_revision().hash(&mut hasher);
    }
    source_revision.hash(&mut hasher);
    source_size.hash(&mut hasher);
    target_edge.hash(&mut hasher);
    let hash = format!("{:016x}", hasher.finish());
    let prefix = source_prefix(path);
    ensure_shard(&prefix).join(format!("{prefix}_{hash}_v7.jpg"))
}

/// The cached thumbnail file for a path from [`thumbnail_cache_path`], in
/// whichever format it was saved.
pub(super) fn cached_variant(path: &Path) -> Option<PathBuf> {
    [path.to_path_buf(), path.with_extension("png")]
        .into_iter()
        .find(|candidate| candidate.is_file())
}

pub(super) fn thumbnail_cache_dir() -> PathBuf {
    static CACHE_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
        #[cfg(not(test))]
        let path = thumbnail_cache_base_dir().join("thumbnails");
        #[cfg(test)]
        let path = std::env::temp_dir().join(format!("lwa_fm_assets_tests_{}", std::process::id()));
        let _ = fs::create_dir_all(&path);
        path
    });
    CACHE_DIR.clone()
}

#[cfg(not(test))]
pub(super) fn thumbnail_cache_base_dir() -> PathBuf {
    ProjectDirs::from("io", "github.leinnan", "dirfleet").map_or_else(
        || std::env::temp_dir().join("dirfleet"),
        |dirs| dirs.cache_dir().to_path_buf(),
    )
}

/// Whether any pixel is not fully opaque.
pub(super) fn has_transparency(image: &image::DynamicImage) -> bool {
    image.color().has_alpha() && image.to_rgba8().pixels().any(|pixel| pixel.0[3] < u8::MAX)
}

/// Encodes a thumbnail for the cache: JPEG when fully opaque (small, fast),
/// PNG when it has transparency. Returns the bytes and the extension.
pub(super) fn encode_thumbnail(image: &image::DynamicImage) -> Option<(Vec<u8>, &'static str)> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    let ext = if has_transparency(image) {
        image.write_to(&mut bytes, image::ImageFormat::Png).ok()?;
        "png"
    } else {
        image::DynamicImage::ImageRgb8(image.to_rgb8())
            .write_to(&mut bytes, image::ImageFormat::Jpeg)
            .ok()?;
        "jpg"
    };
    Some((bytes.into_inner(), ext))
}

/// Writes bytes from [`encode_thumbnail`] next to `cache_path` (which names
/// the `.jpg` variant), replacing the other variant if there was one.
pub(super) fn atomic_write_thumbnail(cache_path: &Path, bytes: &[u8], ext: &str) {
    let target = cache_path.with_extension(ext);
    let tmp = atomic_temp_path(&target, ext);
    if fs::write(&tmp, bytes).is_ok() && fs::rename(&tmp, &target).is_ok() {
        note_write(&target);
        let other = if ext == "png" { "jpg" } else { "png" };
        let _ = fs::remove_file(cache_path.with_extension(other));
    } else {
        log::debug!("could not write thumbnail cache {}", target.display());
        let _ = fs::remove_file(&tmp);
    }
}

/// Encodes and writes a thumbnail; see [`encode_thumbnail`].
#[cfg(test)]
pub(super) fn atomic_save_image(image: &image::DynamicImage, cache_path: &Path) {
    if let Some((bytes, ext)) = encode_thumbnail(image) {
        atomic_write_thumbnail(cache_path, &bytes, ext);
    }
}

pub(super) fn atomic_temp_path(cache_path: &Path, extension: &str) -> PathBuf {
    static TEMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    cache_path.with_extension(format!(
        "{extension}.{}.{}.tmp",
        std::process::id(),
        sequence
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sizes_share_entries_and_sources_stay_distinct() {
        let directory =
            std::env::temp_dir().join(format!("lwa_cache_ownership_{}", std::process::id()));
        let inside = directory.join("nested/movie.mp4");
        let outside = directory.with_extension("other").join("movie.mp4");
        let first = thumbnail_cache_path(&inside, 160, 1, 10);
        assert_eq!(first, thumbnail_cache_path(&inside, 160, 1, 10));
        let second = thumbnail_cache_path(&outside, 160, 1, 10);
        assert_ne!(first, second);
        assert!(atomic_save_bytes(b"inside", &first));
        assert!(atomic_save_bytes(b"outside", &second));
        let _ = fs::remove_file(first);
        let _ = fs::remove_file(second);
    }

    #[test]
    fn maintenance_repeats_and_uses_read_recency() {
        let root = std::env::temp_dir().join(format!("lwa_fm_prune_{}", std::process::id()));
        fs::create_dir_all(&root).expect("prune fixture");
        let paths = ["a.bin", "b.bin", "c.bin"].map(|name| root.join(name));
        for (index, path) in paths.iter().enumerate() {
            fs::write(path, [1; 4]).expect("cache fixture");
            RECENCY.lock().expect("recency").put(
                path.clone(),
                SystemTime::UNIX_EPOCH + Duration::from_secs(index as u64),
            );
        }
        read_bounded(&paths[0], 4).expect("read refreshes recency");
        let stale = root.join("stale.bin.tmp");
        let fresh = root.join("live.bin.tmp");
        fs::write(&stale, [0; 4]).expect("old temp");
        fs::OpenOptions::new()
            .write(true)
            .open(&stale)
            .expect("temp")
            .set_modified(SystemTime::UNIX_EPOCH)
            .expect("set old time");
        fs::write(&fresh, [0; 4]).expect("live temp");
        let legacy = root.join("old_anim_v4.gif");
        fs::write(&legacy, [0; 4]).expect("legacy");
        maintain(&root, 10, 8);
        assert!(paths[0].exists());
        assert!(!paths[1].exists());
        assert!(paths[2].exists());
        assert!(!stale.exists());
        assert!(!legacy.exists());
        assert!(fresh.exists());
        let added = root.join("d.bin");
        fs::write(&added, [1; 4]).expect("new write");
        touch(&added);
        maintain(&root, 10, 8);
        assert!(!paths[2].exists());
        assert!(paths[0].exists());
        assert!(added.exists());
        fs::remove_dir_all(root).expect("clean fixture");
    }

    #[test]
    fn jpeg_cache_accepts_rgba_and_large_reads_are_rejected() {
        let root = std::env::temp_dir().join(format!("lwa_fm_jpeg_{}", std::process::id()));
        fs::create_dir_all(&root).expect("jpeg fixture");
        let path = root.join("rgba.jpg");
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            4,
            4,
            image::Rgba([40, 80, 120, 255]),
        ));
        atomic_save_image(&image, &path);
        assert!(image::open(&path).is_ok());
        assert!(read_bounded(&path, 1).is_err());
        fs::remove_dir_all(root).expect("clean fixture");
    }
}

#[cfg(test)]
mod hash_tests {
    use super::stable_hasher;
    use std::hash::{Hash, Hasher};

    /// Cache file names and checksums are built from this hasher. If this
    /// value changes, every cached thumbnail is orphaned: bump the cache
    /// version in the file names instead of updating the expected value.
    #[test]
    fn stable_hasher_output_is_fixed() {
        let mut hasher = stable_hasher();
        "C:/photos/holiday.jpg".hash(&mut hasher);
        480_u32.hash(&mut hasher);
        let first = hasher.finish();
        let mut again = stable_hasher();
        "C:/photos/holiday.jpg".hash(&mut again);
        480_u32.hash(&mut again);
        assert_eq!(first, again.finish());
        assert_eq!(first, GOLDEN, "stable hasher output changed: {first:#x}");
    }

    const GOLDEN: u64 = 0x48e7_546a_8dc9_8b12;
}
