use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet, hash_map::DefaultHasher};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::io::Read;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Condvar, LazyLock, Mutex, OnceLock};
use std::thread;
use std::time::Instant;

use directories::ProjectDirs;
use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use ffmpeg_sidecar::command::FfmpegCommand;
use image::codecs::gif::{GifEncoder, Repeat};
use image::{Delay, Frame, RgbaImage};
use lru::LruCache;

use crate::data::files::DirEntry;
use crate::helper::PathHelper;

const FOLDER_ICON_KEY: &str = "icon_folder";
const ICON_EXT_PREFIX: &str = "icon_";
const NO_EXT_ICON_KEY: &str = "icon_no_ext";
const TEXTURE_CAPACITY: usize = 512;
const GIF_BYTES_CAPACITY: usize = 128;
const VIDEO_GIF_FRAMES: u32 = 15;
const VIDEO_GIF_FRAME_DELAY_MS: u16 = 600;
const MAX_TEXTURES_PER_FRAME: usize = 10;

// Failure backoff: first retry after `ICON_RETRY_BASE_SECS`, doubling on each
// consecutive failure. After `ICON_RETRY_MAX_TRIES` the file is treated as
// permanently failed and never re-attempted (stops repeat ffmpeg spawns on
// corrupt/unsupported sources).
const ICON_RETRY_BASE_SECS: u64 = 30;
const ICON_RETRY_MAX_TRIES: u32 = 3;
const ICON_BACKOFF_SHIFT_CAP: u32 = 8;
const DURATION_CACHE_CAPACITY: usize = 512;

const VIDEO_EXTS: &[&str] = &[
    "mp4", "mov", "mkv", "avi", "webm", "wmv", "flv", "m4v", "3gp", "ogv",
];

static FFMPEG_READY: OnceLock<Result<(), String>> = OnceLock::new();

/// Cross-worker cache of probed video durations, keyed by path + mtime so a
/// re-encoded file (new mtime) is re-probed automatically. Evicted by LRU.
static DURATION_CACHE: LazyLock<Mutex<LruCache<String, f64>>> = LazyLock::new(|| {
    Mutex::new(LruCache::new(
        std::num::NonZero::new(DURATION_CACHE_CAPACITY).expect("DURATION_CACHE_CAPACITY > 0"),
    ))
});

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum IconSize {
    Small,
    #[default]
    Medium,
    Large,
    ExtraLarge,
}

impl IconSize {
    pub const fn system_icon_px(self) -> i32 {
        match self {
            Self::Small => 32,
            Self::Medium => 48,
            Self::Large => 64,
            Self::ExtraLarge => 96,
        }
    }

    pub const fn render_px(self) -> f32 {
        match self {
            Self::Small => 18.0,
            Self::Medium => 24.0,
            Self::Large => 40.0,
            Self::ExtraLarge => 60.0,
        }
    }

    pub const fn decode_px(self) -> u32 {
        match self {
            Self::Small => 64,
            Self::Medium => 96,
            Self::Large => 160,
            Self::ExtraLarge => 360,
        }
    }

    pub const fn cache_suffix(self) -> &'static str {
        match self {
            Self::Small => "_s",
            Self::Medium => "_m",
            Self::Large => "_l",
            Self::ExtraLarge => "_xl",
        }
    }

    pub const fn row_height_multiplier(self) -> f32 {
        match self {
            Self::Small => 1.1,
            Self::Medium => 1.25,
            Self::Large => 1.4,
            Self::ExtraLarge => 1.75,
        }
    }

    pub const fn tile_width(self) -> f32 {
        match self {
            Self::Small => 120.0,
            Self::Medium => 168.0,
            Self::Large => 220.0,
            Self::ExtraLarge => 330.0,
        }
    }

    pub const fn tile_height(self) -> f32 {
        match self {
            Self::Small => 106.0,
            Self::Medium => 148.0,
            Self::Large => 196.0,
            Self::ExtraLarge => 294.0,
        }
    }
}

#[derive(Debug, Clone)]
struct DecodedImage {
    name: String,
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

#[derive(Debug, Clone)]
enum AssetJob {
    Thumbnail {
        source_path: PathBuf,
        cache_path: PathBuf,
        request_key: String,
        kind: ThumbnailKind,
        icon_size: IconSize,
    },
    SystemIcon {
        request_key: String,
        lookup_arg: String,
        icon_size: IconSize,
    },
    VideoGif {
        source_path: PathBuf,
        request_key: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssetJobClass {
    EntryVisual,
    SidebarIcon,
    HoverPreview,
}

/// Priority for the binary heap: lower rank = higher priority,
/// lower order within same rank = older job processed first.
#[derive(Debug, Eq, PartialEq)]
struct JobPriority(u8, u64);

impl Ord for JobPriority {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse: BinaryHeap is a max-heap, so Reverse gives us min-behavior
        Reverse((self.0, self.1)).cmp(&Reverse((other.0, other.1)))
    }
}

impl PartialOrd for JobPriority {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug)]
struct HeapEntry {
    priority: JobPriority,
    job: AssetJob,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority
    }
}

impl Eq for HeapEntry {}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.priority.cmp(&other.priority)
    }
}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Default)]
struct JobSchedulerState {
    active_directory: Option<String>,
    queue: BinaryHeap<HeapEntry>,
    next_order: u64,
}

#[derive(Debug, Default)]
struct JobScheduler {
    state: Mutex<JobSchedulerState>,
    has_jobs: Condvar,
}

impl JobScheduler {
    fn enqueue(&self, job: AssetJob, class: AssetJobClass, directory: Option<String>) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::assets::scheduler::enqueue");
        {
            let mut state = self.state.lock().expect("job scheduler mutex poisoned");
            let order = state.next_order;
            state.next_order = state.next_order.saturating_add(1);
            let rank = job_rank(class, directory.as_deref(), state.active_directory.as_deref());
            state.queue.push(HeapEntry {
                priority: JobPriority(rank, order),
                job,
            });
        }
        self.has_jobs.notify_one();
    }

    fn set_active_directory(&self, directory: Option<String>) {
        let mut changed = false;
        {
            let mut state = self.state.lock().expect("job scheduler mutex poisoned");
            if state.active_directory != directory {
                state.active_directory = directory;
                changed = true;
            }
        }
        if changed {
            self.has_jobs.notify_all();
        }
    }

    fn recv(&self) -> AssetJob {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::assets::scheduler::recv");
        let mut state = self.state.lock().expect("job scheduler mutex poisoned");
        loop {
            if let Some(entry) = state.queue.pop() {
                return entry.job;
            }
            state = self
                .has_jobs
                .wait(state)
                .expect("job scheduler mutex poisoned while waiting");
        }
    }
}

fn job_rank(
    class: AssetJobClass,
    directory: Option<&str>,
    active_directory: Option<&str>,
) -> u8 {
    let is_active_directory = directory
        .zip(active_directory)
        .is_some_and(|(dir, active)| dir == active);
    match (class, is_active_directory) {
        (AssetJobClass::EntryVisual, true) => 0,
        (AssetJobClass::EntryVisual, false) => 1,
        (AssetJobClass::SidebarIcon, _) => 2,
        (AssetJobClass::HoverPreview, _) => 3,
    }
}

#[derive(Debug, Clone)]
enum AssetJobResult {
    Ready {
        request_key: String,
        image: DecodedImage,
    },
    Failed {
        request_key: String,
    },
    GifReady {
        source_path: PathBuf,
        request_key: String,
        gif_bytes: Arc<[u8]>,
    },
}

#[derive(Debug, Clone, Copy)]
enum ThumbnailKind {
    Image,
    Video,
}

/// Recorded failure of a single asset request. `tries` drives exponential
/// backoff and eventually permanent-failure semantics.
#[derive(Debug, Clone, Copy)]
struct FailureRecord {
    last_attempt: Instant,
    tries: u32,
}

pub struct AssetManager {
    textures: LruCache<String, TextureHandle>,
    gif_bytes: LruCache<String, Arc<[u8]>>,
    pending: HashSet<String>,
    failed: HashMap<String, FailureRecord>,
    scheduler: Arc<JobScheduler>,
    receiver: Receiver<AssetJobResult>,
    icon_size: IconSize,
    per_dir_icon_size: HashMap<String, IconSize>,
    gif_store: Option<PersistentGifStore>,
}

pub enum HoverPreview {
    ImageUri(String),
    GifBytes {
        uri: Cow<'static, str>,
        bytes: Arc<[u8]>,
    },
    Loading,
    Fallback,
}

struct PersistentGifStore {
    tree: sled::Tree,
}

impl PersistentGifStore {
    const TREE_NAME: &'static [u8] = b"video_gifs_v1";
    const FALLBACK_TREE_NAME: &'static [u8] = b"video_gifs_tmp_v1";
    const MAX_ENTRY_BYTES: usize = 4 * 1024 * 1024;

    fn open() -> Self {
        let db_path = thumbnail_cache_base_dir().join("asset_store");
        let _ = fs::create_dir_all(&db_path);
        let db = sled::open(&db_path).unwrap_or_else(|err| {
            log::warn!(
                "failed to open gif cache database at {}: {err}",
                db_path.display()
            );
            sled::Config::new()
                .temporary(true)
                .open()
                .expect("temporary sled database should open")
        });
        let tree = db
            .open_tree(Self::TREE_NAME)
            .or_else(|err| {
                log::warn!("failed to open gif cache tree: {err}");
                db.open_tree(Self::FALLBACK_TREE_NAME)
            })
            .expect("gif cache tree should open");
        Self { tree }
    }

    fn get(&self, path: &Path) -> Option<Arc<[u8]>> {
        self.tree
            .get(gif_store_key(path))
            .ok()?
            .map(|bytes| Arc::<[u8]>::from(bytes.as_ref()))
    }

    fn persist(&self, path: &Path, gif_bytes: &Arc<[u8]>) {
        if gif_bytes.len() > Self::MAX_ENTRY_BYTES {
            return;
        }
        if let Err(err) = self.tree.insert(gif_store_key(path), gif_bytes.as_ref()) {
            log::warn!(
                "failed to persist gif preview for {}: {err}",
                path.display()
            );
        }
    }
}

impl AssetManager {
    pub fn new() -> Self {
        let (result_tx, result_rx) = mpsc::channel::<AssetJobResult>();
        let scheduler = Arc::new(JobScheduler::default());

        // Scale decode/resize workers with core count. ffmpeg invocations
        // are pinned to `-threads 1` (see generate_video_thumbnail /
        // generate_video_gif) so this parallelises image work across cores
        // without oversubscribing on video jobs.
        let worker_count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 8);
        for _ in 0..worker_count {
            let worker_scheduler = Arc::clone(&scheduler);
            let worker_tx = result_tx.clone();
            thread::spawn(move || {
                loop {
                    let job = worker_scheduler.recv();

                    let result = match job {
                        AssetJob::Thumbnail {
                            source_path,
                            cache_path,
                            request_key,
                            kind,
                            icon_size,
                        } => match load_or_generate_thumbnail(
                            &source_path,
                            &cache_path,
                            kind,
                            icon_size,
                        ) {
                            Some(image) => AssetJobResult::Ready { image, request_key },
                            None => AssetJobResult::Failed { request_key },
                        },
                        AssetJob::SystemIcon {
                            request_key,
                            lookup_arg,
                            icon_size,
                        } => match load_system_icon_image(&lookup_arg, icon_size) {
                            Some(image) => AssetJobResult::Ready { image, request_key },
                            None => AssetJobResult::Failed { request_key },
                        },
                        AssetJob::VideoGif {
                            source_path,
                            request_key,
                        } => match generate_video_gif(&source_path) {
                            Some(gif_bytes) => AssetJobResult::GifReady {
                                source_path,
                                request_key,
                                gif_bytes,
                            },
                            None => AssetJobResult::Failed { request_key },
                        },
                    };

                    if worker_tx.send(result).is_err() {
                        return;
                    }
                }
            });
        }

        Self {
            textures: LruCache::new(
                std::num::NonZero::new(TEXTURE_CAPACITY).expect("TEXTURE_CAPACITY must be > 0"),
            ),
            gif_bytes: LruCache::new(
                std::num::NonZero::new(GIF_BYTES_CAPACITY).expect("GIF_BYTES_CAPACITY must be > 0"),
            ),
            pending: HashSet::new(),
            failed: HashMap::new(),
            scheduler,
            receiver: result_rx,
            icon_size: IconSize::default(),
            per_dir_icon_size: HashMap::new(),
            gif_store: None,
        }
    }

    pub fn set_active_directory(&self, path: Option<&Path>) {
        let directory = path.map(|path| path.to_full_path_string());
        self.scheduler.set_active_directory(directory);
    }

    pub fn poll_results(&mut self, ctx: &Context) {
        let mut received_any = false;
        let mut processed = 0usize;
        loop {
            // Limit GPU texture uploads per frame to avoid UI thread stalls
            if processed >= MAX_TEXTURES_PER_FRAME {
                ctx.request_repaint();
                return;
            }
            match self.receiver.try_recv() {
                Ok(AssetJobResult::Ready { request_key, image }) => {
                    self.pending.remove(&request_key);
                    self.failed.remove(&request_key);
                    let texture = ctx.load_texture(
                        image.name.clone(),
                        ColorImage::from_rgba_unmultiplied(
                            [image.width, image.height],
                            &image.rgba,
                        ),
                        TextureOptions::LINEAR,
                    );
                    self.textures.put(request_key, texture);
                    received_any = true;
                    processed += 1;
                }
                Ok(AssetJobResult::Failed { request_key }) => {
                    self.pending.remove(&request_key);
                    self.failed
                        .entry(request_key)
                        .and_modify(|record| {
                            record.last_attempt = Instant::now();
                            record.tries = record.tries.saturating_add(1);
                        })
                        .or_insert_with(|| FailureRecord {
                            last_attempt: Instant::now(),
                            tries: 1,
                        });
                    received_any = true;
                }
                Ok(AssetJobResult::GifReady {
                    source_path,
                    request_key,
                    gif_bytes,
                }) => {
                    self.pending.remove(&request_key);
                    self.failed.remove(&request_key);
                    self.gif_store().persist(&source_path, &gif_bytes);
                    self.gif_bytes.put(request_key, gif_bytes);
                    received_any = true;
                    processed += 1;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        if received_any {
            ctx.request_repaint();
        }
    }

    pub fn set_icon_size(&mut self, size: IconSize) {
        if self.icon_size != size {
            self.icon_size = size;
            self.textures.clear();
            self.gif_bytes.clear();
            self.pending.clear();
            self.failed.clear();
        }
    }

    pub const fn icon_size(&self) -> IconSize {
        self.icon_size
    }

    #[expect(dead_code, reason = "public API for future per-directory icon size UI")]
    pub fn set_icon_size_for_dir(&mut self, dir_path: &str, size: IconSize) {
        self.per_dir_icon_size.insert(dir_path.to_string(), size);
    }

    #[expect(dead_code, reason = "public API for future per-directory icon size UI")]
    pub fn icon_size_for_dir(&self, dir_path: &str) -> IconSize {
        self.per_dir_icon_size
            .get(dir_path)
            .copied()
            .unwrap_or(self.icon_size)
    }

    pub fn request_entry_texture(&mut self, entry: &DirEntry) -> Option<TextureHandle> {
        let path = entry.get_path();
        let size = self.effective_icon_size(entry);

        if let Some(kind) = thumbnail_kind(&path) {
            let path_string = path.to_full_path_string();
            let cache_key = format!("{}{}", path_string, size.cache_suffix());
            if let Some(texture) = self.textures.get(&cache_key) {
                return Some(texture.clone());
            }
            if !self.is_pending_or_failed(&cache_key) {
                let cache_path = thumbnail_cache_path(&path, size);
                self.scheduler.enqueue(
                    AssetJob::Thumbnail {
                        source_path: path.to_path_buf(),
                        cache_path,
                        request_key: cache_key.clone(),
                        kind,
                        icon_size: size,
                    },
                    AssetJobClass::EntryVisual,
                    path.parent().map(|path| path.to_full_path_string()),
                );
                self.pending.insert(cache_key);
            }
        }

        self.request_file_icon_texture(&path, entry.is_file(), size, AssetJobClass::EntryVisual)
    }

    pub fn request_sidebar_texture(&mut self, path: &Path) -> Option<TextureHandle> {
        let is_dir = path.is_dir();
        let size = self.icon_size;
        let cache_key = if is_dir {
            format!(
                "sidebar:{}{}",
                directory_icon_key(path),
                size.cache_suffix()
            )
        } else {
            format!("sidebar:{}{}", icon_key(path, false), size.cache_suffix())
        };

        if let Some(texture) = self.textures.get(&cache_key) {
            return Some(texture.clone());
        }
        if self.is_pending_or_failed(&cache_key) {
            return None;
        }

        let lookup_arg = if is_dir {
            directory_lookup_arg(path)
        } else {
            path.to_string_lossy().to_string()
        };
        self.scheduler.enqueue(
            AssetJob::SystemIcon {
                request_key: cache_key.clone(),
                lookup_arg,
                icon_size: size,
            },
            AssetJobClass::SidebarIcon,
            path.parent().map(|path| path.to_full_path_string()),
        );
        self.pending.insert(cache_key);
        None
    }

    pub fn request_hover_preview(&mut self, entry: &DirEntry) -> HoverPreview {
        let Some(ext) = entry
            .get_path()
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .map(str::to_ascii_lowercase)
        else {
            return HoverPreview::Fallback;
        };

        match ext.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tiff" | "tif" | "ico" | "avif"
            | "tga" => HoverPreview::ImageUri(format!("file://{}", entry.to_full_path_string())),
            ext_str if VIDEO_EXTS.contains(&ext_str) => {
                self.request_video_hover_preview(&entry.get_path())
            }
            _ => HoverPreview::Fallback,
        }
    }

    pub fn invalidate_directories(&mut self, directories: impl IntoIterator<Item = PathBuf>) {
        let directories: Vec<PathBuf> = directories.into_iter().collect();
        if directories.is_empty() {
            return;
        }

        let matches_dir = |key: &str| -> bool {
            let entry_path = key.strip_prefix("sidebar:").unwrap_or(key);
            let entry_path = Path::new(entry_path);
            directories.iter().any(|dir| {
                crate::helper::path_starts_with_dir(entry_path, dir)
                    || entry_path
                        .parent()
                        .is_some_and(|parent| crate::helper::path_starts_with_dir(parent, dir))
            })
        };

        let keys_to_remove: Vec<String> = self
            .textures
            .iter()
            .filter(|(key, _)| matches_dir(key))
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys_to_remove {
            self.textures.pop(&key);
        }
        let gif_keys_to_remove: Vec<String> = self
            .gif_bytes
            .iter()
            .filter(|(key, _)| matches_dir(key))
            .map(|(key, _)| key.clone())
            .collect();
        for key in gif_keys_to_remove {
            self.gif_bytes.pop(&key);
        }
        self.pending.retain(|key| !matches_dir(key));
        self.failed.retain(|key, _| !matches_dir(key));
    }

    fn effective_icon_size(&self, entry: &DirEntry) -> IconSize {
        let (dir, _) = entry.get_splitted_path();
        self.per_dir_icon_size
            .get(dir)
            .copied()
            .unwrap_or(self.icon_size)
    }

    fn request_video_hover_preview(&mut self, path: &Path) -> HoverPreview {
        let request_key = video_gif_request_key(path);
        if let Some(bytes) = self.gif_bytes.get(&request_key) {
            return HoverPreview::GifBytes {
                uri: Cow::Owned(format!("bytes://{request_key}.gif")),
                bytes: bytes.clone(),
            };
        }
        if let Some(bytes) = self.gif_store().get(path) {
            self.gif_bytes.put(request_key.clone(), bytes.clone());
            return HoverPreview::GifBytes {
                uri: Cow::Owned(format!("bytes://{request_key}.gif")),
                bytes,
            };
        }
        if !self.is_pending_or_failed(&request_key) {
            self.scheduler.enqueue(
                AssetJob::VideoGif {
                    source_path: path.to_path_buf(),
                    request_key: request_key.clone(),
                },
                AssetJobClass::HoverPreview,
                path.parent().map(|path| path.to_full_path_string()),
            );
            self.pending.insert(request_key);
        }
        HoverPreview::Loading
    }

    fn gif_store(&mut self) -> &mut PersistentGifStore {
        self.gif_store.get_or_insert_with(PersistentGifStore::open)
    }

    fn request_file_icon_texture(
        &mut self,
        path: &Path,
        is_file: bool,
        size: IconSize,
        class: AssetJobClass,
    ) -> Option<TextureHandle> {
        let key = if is_file {
            format!("{}{}", icon_key(path, false), size.cache_suffix())
        } else {
            format!("{}{}", directory_icon_key(path), size.cache_suffix())
        };

        if let Some(texture) = self.textures.get(&key) {
            return Some(texture.clone());
        }
        if self.is_pending_or_failed(&key) {
            return None;
        }

        let lookup_arg = if is_file {
            path.to_string_lossy().to_string()
        } else {
            directory_lookup_arg(path)
        };

        self.scheduler.enqueue(
            AssetJob::SystemIcon {
                request_key: key.clone(),
                lookup_arg,
                icon_size: size,
            },
            class,
            path.parent().map(|path| path.to_full_path_string()),
        );
        self.pending.insert(key);
        None
    }

    fn is_pending_or_failed(&self, key: &str) -> bool {
        if self.pending.contains(key) {
            return true;
        }
        if let Some(record) = self.failed.get(key) {
            // Exponential backoff (30s, 60s, 120s); once we hit the max tries
            // the entry is considered permanently failed and never retried.
            if record.tries >= ICON_RETRY_MAX_TRIES {
                return true;
            }
            let shift = (record.tries - 1).min(ICON_BACKOFF_SHIFT_CAP);
            let backoff = ICON_RETRY_BASE_SECS * (1u64 << shift);
            return record.last_attempt.elapsed().as_secs() < backoff;
        }
        false
    }
}

impl Default for AssetManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AssetManager {
    pub const fn render_size(&self) -> f32 {
        self.icon_size.render_px()
    }

    pub fn render_size_for(&self, entry: &DirEntry) -> f32 {
        self.effective_icon_size(entry).render_px()
    }

    pub const fn row_height_multiplier(&self) -> f32 {
        self.icon_size.row_height_multiplier()
    }
}

fn directory_icon_key(path: &Path) -> String {
    let mut h = DefaultHasher::new();
    path.to_string_lossy().to_lowercase().hash(&mut h);
    let hash = h.finish();
    format!("{FOLDER_ICON_KEY}_{hash:016x}")
}

fn icon_key(path: &Path, is_dir: bool) -> String {
    if is_dir {
        return FOLDER_ICON_KEY.to_string();
    }
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .map_or_else(
            || NO_EXT_ICON_KEY.to_string(),
            |ext| format!("{ICON_EXT_PREFIX}{}", ext.to_lowercase()),
        )
}

#[cfg(target_os = "macos")]
fn directory_lookup_arg(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[cfg(not(target_os = "macos"))]
fn directory_lookup_arg(_path: &Path) -> String {
    "folder".to_string()
}

pub fn entry_has_animated_preview(entry: &DirEntry) -> bool {
    let Some(ext) = entry
        .get_path()
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase)
    else {
        return false;
    };
    ext == "gif" || VIDEO_EXTS.contains(&ext.as_str())
}

fn thumbnail_kind(path: &Path) -> Option<ThumbnailKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tiff" | "tif" | "ico" | "avif"
        | "tga" => Some(ThumbnailKind::Image),
        "mp4" | "mov" | "mkv" | "avi" | "webm" | "wmv" | "flv" | "m4v" | "3gp" | "ogv" => {
            Some(ThumbnailKind::Video)
        }
        _ => None,
    }
}

/// On-disk thumbnail extension chosen per source type. Photo-like and video
/// sources use JPEG (faster encode/decode, far smaller); formats that can
/// carry meaningful alpha keep PNG so transparency isn't lost on the tile.
fn thumbnail_cache_ext(source_path: &Path) -> &'static str {
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

fn thumbnail_cache_path(path: &Path, size: IconSize) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    if let Ok(metadata) = fs::metadata(path)
        && let Ok(modified) = metadata.modified()
    {
        modified.hash(&mut hasher);
    }
    let ext = thumbnail_cache_ext(path);
    // `_v2` invalidates caches written by older format versions.
    thumbnail_cache_dir().join(format!(
        "{:x}{}_v2.{}",
        hasher.finish(),
        size.cache_suffix(),
        ext
    ))
}

fn thumbnail_cache_dir() -> PathBuf {
    let base = thumbnail_cache_base_dir();
    let path = base.join("thumbnails");
    let _ = fs::create_dir_all(&path);
    path
}

fn thumbnail_cache_base_dir() -> PathBuf {
    ProjectDirs::from("io", "github.leinnan", "dirfleet").map_or_else(
        || PathBuf::from(".cache"),
        |dirs| dirs.cache_dir().to_path_buf(),
    )
}

fn load_or_generate_thumbnail(
    source_path: &Path,
    cache_path: &Path,
    kind: ThumbnailKind,
    icon_size: IconSize,
) -> Option<DecodedImage> {
    if cache_path.exists() {
        return decode_image_file(cache_path, source_path.to_full_path_string());
    }

    let decode_px = icon_size.decode_px();
    let image = match kind {
        ThumbnailKind::Image => image::open(source_path)
            .ok()?
            .thumbnail(decode_px, decode_px),
        ThumbnailKind::Video => generate_video_thumbnail(source_path, cache_path, icon_size)?,
    };

    if !cache_path.exists() {
        let _ = image.save(cache_path);
    }

    Some(decoded_from_dynamic(
        &image,
        source_path.to_full_path_string(),
    ))
}

fn decode_image_file(path: &Path, name: String) -> Option<DecodedImage> {
    let image = image::open(path).ok()?;
    Some(decoded_from_dynamic(&image, name))
}

fn decoded_from_dynamic(image: &image::DynamicImage, name: String) -> DecodedImage {
    let rgba = image.to_rgba8();
    let width = rgba.width() as usize;
    let height = rgba.height() as usize;
    DecodedImage {
        name,
        width,
        height,
        rgba: rgba.into_raw(),
    }
}

fn load_system_icon_image(lookup_arg: &str, icon_size: IconSize) -> Option<DecodedImage> {
    let px = icon_size.system_icon_px();
    let bytes = match systemicons::get_icon(lookup_arg, px) {
        Ok(b) => b,
        Err(e) => {
            log::warn!("systemicons::get_icon failed for {lookup_arg:?}: {e:?}");
            return None;
        }
    };
    let image = match image::load_from_memory(&bytes) {
        Ok(img) => img,
        Err(e) => {
            log::warn!("image::load_from_memory failed for {lookup_arg:?}: {e}");
            return None;
        }
    };
    Some(decoded_from_dynamic(&image, lookup_arg.to_string()))
}

fn generate_video_thumbnail(
    source_path: &Path,
    cache_path: &Path,
    icon_size: IconSize,
) -> Option<image::DynamicImage> {
    if let Err(err) = ensure_ffmpeg() {
        log::warn!(
            "ffmpeg unavailable; cannot generate video thumbnail for {}: {err}",
            source_path.display()
        );
        return None;
    }
    let decode_px = icon_size.decode_px();
    // Capture the PNG straight from ffmpeg's stdout, then persist it in the
    // cache format implied by `cache_path`'s extension. A fixed ~1s seek avoids
    // the extra ffprobe round-trip the old 15% seek needed, a plain `scale`
    // (no `thumbnail=n=24`) decodes a single frame instead of buffering 24, and
    // piping stdout removes the write-then-read-back disk round-trip.
    // `-threads 1` keeps concurrent workers from oversubscribing the CPU.
    let mut ffmpeg = match FfmpegCommand::new()
        .args(["-threads", "1"])
        .seek("1")
        .input(source_path.as_os_str().to_string_lossy())
        .frames(1)
        .args(["-vf", &format!("scale='min({decode_px}\\,iw)':-1")])
        .format("image2pipe")
        .codec_video("png")
        .pipe_stdout()
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            log::warn!(
                "failed to spawn ffmpeg thumbnail process for {}: {err}",
                source_path.display()
            );
            return None;
        }
    };
    let Some(mut stdout) = ffmpeg.take_stdout() else {
        log::warn!(
            "ffmpeg thumbnail process did not expose stdout for {}",
            source_path.display()
        );
        return None;
    };
    let mut png_bytes = Vec::new();
    if let Err(err) = stdout.read_to_end(&mut png_bytes) {
        log::warn!(
            "failed to read ffmpeg thumbnail output for {}: {err}",
            source_path.display()
        );
        return None;
    }
    drop(stdout);
    if let Err(err) = ffmpeg.wait() {
        log::warn!(
            "failed to wait for ffmpeg thumbnail process for {}: {err}",
            source_path.display()
        );
        return None;
    }
    let image = match image::load_from_memory(&png_bytes) {
        Ok(image) => image,
        Err(err) => {
            log::warn!(
                "failed to decode ffmpeg thumbnail output for {}: {err}",
                source_path.display()
            );
            return None;
        }
    };
    // `save` infers the format from `cache_path`'s extension (JPEG/PNG), so no
    // separate read-back of the just-written file is needed on this path.
    let _ = image.save(cache_path);
    Some(image)
}

fn ensure_ffmpeg() -> Result<(), String> {
    FFMPEG_READY
        .get_or_init(|| ffmpeg_sidecar::download::auto_download().map_err(|err| err.to_string()))
        .clone()
}

fn video_gif_request_key(path: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    if let Ok(metadata) = fs::metadata(path)
        && let Ok(modified) = metadata.modified()
    {
        modified.hash(&mut hasher);
    }
    format!("{}#{:x}", path.to_full_path_string(), hasher.finish())
}

fn gif_store_key(path: &Path) -> Vec<u8> {
    video_gif_request_key(path).into_bytes()
}

fn video_frame_temp_dir(path: &Path) -> Option<PathBuf> {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "dirfleet_hover_gif_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos()
    ));
    dir.push(path.file_stem()?.to_string_lossy().as_ref());
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn generate_video_gif(path: &Path) -> Option<Arc<[u8]>> {
    if let Err(err) = ensure_ffmpeg() {
        log::warn!(
            "ffmpeg unavailable; cannot generate animated preview for {}: {err}",
            path.display()
        );
        return None;
    }

    let duration_secs = probe_video_duration(path).unwrap_or(30.0).max(1.0);
    let start_pct = 0.05_f64;
    let end_pct = 0.90_f64;
    let span = (end_pct - start_pct) * duration_secs;
    // Sample the desired frame count evenly across the span in a SINGLE ffmpeg
    // pass. The old code spawned one process per frame (~15 process creations
    // and disk round-trips per preview); this writes the whole sequence with
    // one `-ss ... -t ... fps=...` invocation.
    let fps = f64::from(VIDEO_GIF_FRAMES) / span;
    let frame_dir = video_frame_temp_dir(path)?;
    let file_stem = path.file_stem()?.to_string_lossy();
    let pattern = frame_dir.join(format!("{file_stem}_%03d.png"));

    let mut ffmpeg = match FfmpegCommand::new()
        .args(["-threads", "1"])
        .seek(format!("{:.3}", start_pct * duration_secs))
        .input(path.to_string_lossy())
        .duration(format!("{span:.3}"))
        .args(["-vf", &format!("fps={fps:.4},scale='min(320,iw)':-2")])
        .frames(VIDEO_GIF_FRAMES)
        .output(pattern.to_string_lossy())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            log::warn!(
                "failed to spawn ffmpeg animated-preview process for {}: {err}",
                path.display()
            );
            let _ = fs::remove_dir_all(frame_dir);
            return None;
        }
    };
    if let Err(err) = ffmpeg.wait() {
        log::warn!(
            "failed to wait for ffmpeg animated-preview process for {}: {err}",
            path.display()
        );
        let _ = fs::remove_dir_all(frame_dir);
        return None;
    }

    // Collect whatever frames ffmpeg actually wrote (000.png, 001.png, ...).
    // The double `flatten` collapses both the read_dir Result and the per-entry
    // io::Result, yielding DirEntry items (and nothing on read_dir failure).
    let mut frame_paths: Vec<PathBuf> = fs::read_dir(&frame_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "png"))
        .collect();
    frame_paths.sort();

    if frame_paths.is_empty() {
        log::warn!(
            "ffmpeg produced no animated-preview frames for {}",
            path.display()
        );
        let _ = fs::remove_dir_all(frame_dir);
        return None;
    }
    let delay = Delay::from_saturating_duration(std::time::Duration::from_millis(u64::from(
        VIDEO_GIF_FRAME_DELAY_MS,
    )));
    let mut gif_bytes = Vec::new();
    {
        let mut encoder = GifEncoder::new_with_speed(&mut gif_bytes, 10);
        encoder.set_repeat(Repeat::Infinite).ok()?;

        for frame_path in &frame_paths {
            let image = image::open(frame_path).ok()?;
            let rgba: RgbaImage = image.to_rgba8();
            let frame = Frame::from_parts(rgba, 0, 0, delay);
            encoder.encode_frame(frame).ok()?;
        }
    }

    let _ = fs::remove_dir_all(frame_dir);
    Some(Arc::from(gif_bytes))
}

fn probe_video_duration(path: &Path) -> Option<f64> {
    let cache_key = video_gif_request_key(path);
    if let Ok(mut cache) = DURATION_CACHE.lock()
        && let Some(&duration) = cache.get(&cache_key)
    {
        return Some(duration);
    }
    let mut command = std::process::Command::new(ffmpeg_sidecar::ffprobe::ffprobe_path());
    command.args([
        "-v",
        "error",
        "-show_entries",
        "format=duration",
        "-of",
        "default=noprint_wrappers=1:nokey=1",
        path.to_str()?,
    ]);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let output = command.output().ok()?;
    let duration = String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .parse::<f64>()
        .ok()?;
    if let Ok(mut cache) = DURATION_CACHE.lock() {
        cache.put(cache_key, duration);
    }
    Some(duration)
}
