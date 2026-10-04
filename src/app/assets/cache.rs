//! Disk storage with bounded reads, atomic publication, and recurring eviction.
use super::IconSize;
#[cfg(not(test))]
use directories::ProjectDirs;
use lru::LruCache;
use std::collections::hash_map::DefaultHasher;
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

pub(super) fn touch(path: &Path) {
    RECENCY
        .lock()
        .expect("cache recency mutex")
        .put(path.to_path_buf(), SystemTime::now());
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
    if last.is_some_and(|time| time.elapsed() < Duration::from_secs(60))
        && WRITTEN_BYTES.load(Ordering::Relaxed) < 16 * 1024 * 1024
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
        if name.contains("_v2.")
            || name.contains("_v3.")
            || name.contains("_anim_v3.")
            || name.contains("_anim_v4.")
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

pub(super) fn thumbnail_cache_ext(source_path: &Path) -> &'static str {
    let ext = source_path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("png" | "gif" | "ico" | "webp" | "tiff" | "tif") => "png",
        // jpg, jpeg, bmp, avif, tga and all video extensions default to JPEG.
        _ => "jpg",
    }
}

pub(super) fn register_source(path: &Path, prefix: &str) {
    let shard = thumbnail_cache_dir().join(&prefix[..2]);
    let _ = fs::create_dir_all(&shard);
    let owner = shard.join(format!("{prefix}_owner.json"));
    if !owner.exists()
        && let Ok(bytes) = serde_json::to_vec(path)
    {
        atomic_save_bytes(&bytes, &owner);
    }
}

pub(super) fn purge_sources(paths: &[PathBuf], directories: bool) {
    let mut prefixes: std::collections::HashSet<String> =
        paths.iter().map(|path| source_prefix(path)).collect();
    if directories {
        prefixes.clear();
        for entry in walkdir::WalkDir::new(thumbnail_cache_dir())
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_type().is_file()
                    && entry.file_name().to_string_lossy().ends_with("_owner.json")
            })
        {
            if let Ok(bytes) = read_bounded(entry.path(), 64 * 1024)
                && let Ok(source) = serde_json::from_slice::<PathBuf>(&bytes)
                && paths.iter().any(|directory| source.starts_with(directory))
                && let Some(prefix) = entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.strip_suffix("_owner.json"))
            {
                prefixes.insert(prefix.to_owned());
            }
        }
    }
    for prefix in prefixes {
        if let Ok(entries) = fs::read_dir(thumbnail_cache_dir().join(&prefix[..2])) {
            for entry in entries.filter_map(Result::ok) {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&format!("{prefix}_"))
                {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }
}

pub(super) fn source_prefix(path: &Path) -> String {
    let mut hash = DefaultHasher::new();
    crate::helper::normalize_path_string(path).hash(&mut hash);
    format!("{:016x}", hash.finish())
}

pub(super) fn thumbnail_cache_path(
    path: &Path,
    _size: IconSize,
    target_edge: u32,
    source_revision: u128,
    source_size: u64,
) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    crate::helper::normalize_path_string(path).hash(&mut hasher);
    if super::thumbnail_kind(path) == Some(super::ThumbnailKind::Video) {
        super::media::backend_revision().hash(&mut hasher);
    }
    source_revision.hash(&mut hasher);
    source_size.hash(&mut hasher);
    target_edge.hash(&mut hasher);
    let ext = thumbnail_cache_ext(path);
    let hash = format!("{:016x}", hasher.finish());
    let prefix = source_prefix(path);
    register_source(
        Path::new(&crate::helper::normalize_path_string(path)),
        &prefix,
    );
    let shard = thumbnail_cache_dir().join(&prefix[..2]);
    let _ = fs::create_dir_all(&shard);
    shard.join(format!("{prefix}_{hash}_v6.{ext}"))
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
        || PathBuf::from(".cache"),
        |dirs| dirs.cache_dir().to_path_buf(),
    )
}

pub(super) fn atomic_save_image(image: &image::DynamicImage, cache_path: &Path) {
    let extension = cache_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("png");
    let format = if extension.eq_ignore_ascii_case("jpg") {
        image::ImageFormat::Jpeg
    } else {
        image::ImageFormat::Png
    };
    let tmp = atomic_temp_path(cache_path, extension);
    let saved = if format == image::ImageFormat::Jpeg {
        image::DynamicImage::ImageRgb8(image.to_rgb8()).save_with_format(&tmp, format)
    } else {
        image.save_with_format(&tmp, format)
    };
    if saved.is_ok() && fs::rename(&tmp, cache_path).is_ok() {
        note_write(cache_path);
    } else {
        let _ = fs::remove_file(&tmp);
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
    fn directory_purge_preserves_other_sources_and_sizes_share_entries() {
        let directory =
            std::env::temp_dir().join(format!("lwa_cache_ownership_{}", std::process::id()));
        let inside = directory.join("nested/movie.mp4");
        let outside = directory.with_extension("other").join("movie.mp4");
        let first = thumbnail_cache_path(&inside, IconSize::Small, 160, 1, 10);
        assert_eq!(
            first,
            thumbnail_cache_path(&inside, IconSize::ExtraLarge, 160, 1, 10)
        );
        let second = thumbnail_cache_path(&outside, IconSize::Small, 160, 1, 10);
        assert!(atomic_save_bytes(b"inside", &first));
        assert!(atomic_save_bytes(b"outside", &second));
        purge_sources(std::slice::from_ref(&directory), true);
        assert!(!first.exists());
        assert!(second.exists());
        purge_sources(&[outside], false);
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
