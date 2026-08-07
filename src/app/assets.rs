use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet, hash_map::DefaultHasher};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
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
// Maximum GPU texture uploads (via `ctx.load_texture`, which uploads during the
// render pass) processed from the job queue per frame. Each upload is ~0.6 ms,
// so during a bulk thumbnail load (e.g. opening a large image folder) this cap
// spreads the work across frames to avoid single-frame UI stalls. The queue
// is drained over subsequent frames because `poll_results` calls
// `ctx.request_repaint()` when it hits the limit.
const MAX_TEXTURES_PER_FRAME: usize = 4;
const MAX_RESULTS_PER_FRAME: usize = 64;
const MAX_ASSET_JOBS: usize = 512;

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
static CACHE_MAINTENANCE_STARTED: OnceLock<()> = OnceLock::new();

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
        request_key: String,
        kind: ThumbnailKind,
        icon_size: IconSize,
        source_revision: u128,
        source_size: u64,
        navigation_generation: u64,
    },
    SystemIcon {
        request_key: String,
        lookup_arg: String,
        icon_size: IconSize,
        navigation_generation: u64,
    },
    VideoGif {
        source_path: PathBuf,
        request_key: String,
        navigation_generation: u64,
    },
}

impl AssetJob {
    fn request_key(&self) -> &str {
        match self {
            Self::Thumbnail { request_key, .. }
            | Self::SystemIcon { request_key, .. }
            | Self::VideoGif { request_key, .. } => request_key,
        }
    }
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
    class: AssetJobClass,
    directory: Option<String>,
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
    fn enqueue(
        &self,
        job: AssetJob,
        class: AssetJobClass,
        directory: Option<String>,
    ) -> Vec<String> {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::assets::scheduler::enqueue");
        let mut evicted = Vec::new();
        {
            let mut state = self.state.lock().expect("job scheduler mutex poisoned");
            let order = state.next_order;
            state.next_order = state.next_order.saturating_add(1);
            let rank = job_rank(
                class,
                directory.as_deref(),
                state.active_directory.as_deref(),
            );
            if class == AssetJobClass::HoverPreview {
                let mut entries = state.queue.drain().collect::<Vec<_>>();
                entries.retain(|entry| {
                    if entry.class == AssetJobClass::HoverPreview {
                        evicted.push(entry.job.request_key().to_owned());
                        false
                    } else {
                        true
                    }
                });
                state.queue.extend(entries);
            }
            if state.queue.len() >= MAX_ASSET_JOBS {
                // Prefer current work over an old backlog. Rebuilding after
                // dropping the lowest-priority newest item is cheap at this cap.
                let mut entries = state.queue.drain().collect::<Vec<_>>();
                entries.sort_by(|a, b| a.priority.cmp(&b.priority));
                if !entries.is_empty() {
                    evicted.push(entries.remove(0).job.request_key().to_owned());
                }
                state.queue.extend(entries);
            }
            state.queue.push(HeapEntry {
                priority: JobPriority(rank, order),
                job,
                class,
                directory,
            });
        }
        self.has_jobs.notify_one();
        evicted
    }

    fn set_active_directory(&self, directory: Option<String>) {
        let mut changed = false;
        {
            let mut state = self.state.lock().expect("job scheduler mutex poisoned");
            if state.active_directory != directory {
                state.active_directory = directory;
                let active = state.active_directory.clone();
                let mut entries = state.queue.drain().collect::<Vec<_>>();
                for entry in &mut entries {
                    entry.priority.0 =
                        job_rank(entry.class, entry.directory.as_deref(), active.as_deref());
                }
                state.queue.extend(entries);
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

fn job_rank(class: AssetJobClass, directory: Option<&str>, active_directory: Option<&str>) -> u8 {
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
        navigation_generation: u64,
    },
    Failed {
        request_key: String,
        navigation_generation: u64,
    },
    GifReady {
        request_key: String,
        gif_bytes: Arc<[u8]>,
        navigation_generation: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    video_scheduler: Arc<JobScheduler>,
    receiver: Receiver<AssetJobResult>,
    icon_size: IconSize,
    per_dir_icon_size: HashMap<String, IconSize>,
    repaint_ctx: Arc<Mutex<Option<Context>>>,
    active_directory: Option<String>,
    navigation_generation: u64,
}

pub enum HoverPreview {
    Texture(TextureHandle),
    GifBytes {
        uri: Cow<'static, str>,
        bytes: Arc<[u8]>,
    },
    Loading,
    Fallback,
}

impl AssetManager {
    pub fn new() -> Self {
        CACHE_MAINTENANCE_STARTED.get_or_init(|| {
            thread::spawn(maintain_visual_cache);
        });
        let (result_tx, result_rx) = mpsc::sync_channel::<AssetJobResult>(64);
        let scheduler = Arc::new(JobScheduler::default());
        let video_scheduler = Arc::new(JobScheduler::default());
        let repaint_ctx = Arc::new(Mutex::new(None::<Context>));

        // Scale decode/resize workers with core count. ffmpeg invocations
        // are pinned to `-threads 1` (see generate_video_thumbnail /
        // generate_video_gif) so this parallelises image work across cores
        // without oversubscribing on video jobs.
        let worker_count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .saturating_sub(1)
            .clamp(1, 4);
        for _ in 0..worker_count {
            let worker_scheduler = Arc::clone(&scheduler);
            let worker_tx = result_tx.clone();
            let worker_repaint = Arc::clone(&repaint_ctx);
            thread::spawn(move || {
                loop {
                    let job = worker_scheduler.recv();

                    let result = process_asset_job(job);
                    if let Ok(guard) = worker_repaint.lock()
                        && let Some(ctx) = guard.as_ref()
                    {
                        ctx.request_repaint();
                    }

                    if worker_tx.send(result).is_err() {
                        return;
                    }
                }
            });
        }
        {
            let worker_scheduler = Arc::clone(&video_scheduler);
            let worker_tx = result_tx;
            let worker_repaint = Arc::clone(&repaint_ctx);
            thread::spawn(move || {
                loop {
                    let result = process_asset_job(worker_scheduler.recv());
                    if let Ok(guard) = worker_repaint.lock()
                        && let Some(ctx) = guard.as_ref()
                    {
                        ctx.request_repaint();
                    }
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
            video_scheduler,
            receiver: result_rx,
            icon_size: IconSize::default(),
            per_dir_icon_size: HashMap::new(),
            repaint_ctx,
            active_directory: None,
            navigation_generation: 0,
        }
    }

    pub fn set_active_directory(&mut self, path: Option<&Path>) {
        let directory = path.map(|path| path.to_full_path_string());
        if self.active_directory != directory {
            self.active_directory.clone_from(&directory);
            self.navigation_generation = self.navigation_generation.wrapping_add(1);
        }
        self.scheduler.set_active_directory(directory.clone());
        self.video_scheduler.set_active_directory(directory);
    }

    pub fn poll_results(&mut self, ctx: &Context) {
        if let Ok(mut repaint_ctx) = self.repaint_ctx.lock()
            && repaint_ctx.is_none()
        {
            *repaint_ctx = Some(ctx.clone());
        }
        let mut received_any = false;
        let mut processed = 0usize;
        let mut received = 0usize;
        let upload_started = Instant::now();
        loop {
            // Limit GPU texture uploads per frame to avoid UI thread stalls
            if processed >= MAX_TEXTURES_PER_FRAME
                || received >= MAX_RESULTS_PER_FRAME
                || (processed > 0
                    && upload_started.elapsed() >= std::time::Duration::from_millis(2))
            {
                ctx.request_repaint();
                return;
            }
            match self.receiver.try_recv() {
                Ok(AssetJobResult::Ready {
                    request_key,
                    image,
                    navigation_generation,
                }) => {
                    received += 1;
                    self.pending.remove(&request_key);
                    if navigation_generation != self.navigation_generation {
                        received_any = true;
                        continue;
                    }
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
                Ok(AssetJobResult::Failed {
                    request_key,
                    navigation_generation,
                }) => {
                    received += 1;
                    self.pending.remove(&request_key);
                    if navigation_generation != self.navigation_generation {
                        received_any = true;
                        continue;
                    }
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
                    request_key,
                    gif_bytes,
                    navigation_generation,
                }) => {
                    received += 1;
                    self.pending.remove(&request_key);
                    if navigation_generation != self.navigation_generation {
                        received_any = true;
                        continue;
                    }
                    self.failed.remove(&request_key);
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

    #[cfg(test)]
    pub fn wait_for_idle(&mut self, ctx: &Context) {
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        while !self.pending.is_empty() && Instant::now() < deadline {
            self.poll_results(ctx);
            thread::sleep(std::time::Duration::from_millis(1));
        }
        self.poll_results(ctx);
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
        let size = self.effective_icon_size(entry);
        self.request_entry_texture_at_size(entry, size)
    }

    fn request_entry_texture_at_size(
        &mut self,
        entry: &DirEntry,
        size: IconSize,
    ) -> Option<TextureHandle> {
        let path = entry.get_path();

        if let Some(kind) = thumbnail_kind(&path) {
            let path_string = path.to_full_path_string();
            let cache_key = format!(
                "{}{}#{}:{}",
                path_string,
                size.cache_suffix(),
                entry.meta.source_revision,
                entry.meta.size
            );
            if let Some(texture) = self.textures.get(&cache_key) {
                return Some(texture.clone());
            }
            if !self.is_pending_or_failed(&cache_key) {
                let job = AssetJob::Thumbnail {
                    source_path: path.to_path_buf(),
                    request_key: cache_key.clone(),
                    kind,
                    icon_size: size,
                    source_revision: entry.meta.source_revision,
                    source_size: entry.meta.size,
                    navigation_generation: self.navigation_generation,
                };
                let scheduler = if kind == ThumbnailKind::Video {
                    &self.video_scheduler
                } else {
                    &self.scheduler
                };
                let evicted = scheduler.enqueue(
                    job,
                    AssetJobClass::EntryVisual,
                    path.parent().map(|path| path.to_full_path_string()),
                );
                self.clear_evicted_requests(evicted);
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
        let evicted = self.scheduler.enqueue(
            AssetJob::SystemIcon {
                request_key: cache_key.clone(),
                lookup_arg,
                icon_size: size,
                navigation_generation: self.navigation_generation,
            },
            AssetJobClass::SidebarIcon,
            path.parent().map(|path| path.to_full_path_string()),
        );
        self.clear_evicted_requests(evicted);
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
            "png" | "jpg" | "jpeg" | "bmp" | "webp" | "tiff" | "tif" | "ico" | "avif" | "tga" => {
                self.request_entry_texture_at_size(entry, IconSize::ExtraLarge)
                    .map_or(HoverPreview::Loading, HoverPreview::Texture)
            }
            "gif" => self.request_video_hover_preview(entry),
            ext_str if VIDEO_EXTS.contains(&ext_str) => self.request_video_hover_preview(entry),
            _ => HoverPreview::Fallback,
        }
    }

    pub fn invalidate_files(&mut self, files: impl IntoIterator<Item = PathBuf>) {
        let files: Vec<PathBuf> = files.into_iter().collect();
        if files.is_empty() {
            return;
        }

        let file_prefixes: Vec<String> = files
            .iter()
            .map(|file| file.to_full_path_string())
            .collect();
        let matches_file = |key: &str| -> bool {
            let entry_key = key.strip_prefix("sidebar:").unwrap_or(key);
            file_prefixes.iter().any(|file| entry_key.starts_with(file))
        };

        let keys_to_remove: Vec<String> = self
            .textures
            .iter()
            .filter(|(key, _)| matches_file(key))
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys_to_remove {
            self.textures.pop(&key);
        }
        let gif_keys_to_remove: Vec<String> = self
            .gif_bytes
            .iter()
            .filter(|(key, _)| matches_file(key))
            .map(|(key, _)| key.clone())
            .collect();
        for key in gif_keys_to_remove {
            self.gif_bytes.pop(&key);
        }
        self.pending.retain(|key| !matches_file(key));
        self.failed.retain(|key, _| !matches_file(key));
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

    fn request_video_hover_preview(&mut self, entry: &DirEntry) -> HoverPreview {
        let path = entry.get_path();
        let request_key = format!(
            "{}#{}:{}#anim_v3",
            path.to_full_path_string(),
            entry.meta.source_revision,
            entry.meta.size
        );
        if let Some(bytes) = self.gif_bytes.get(&request_key) {
            return HoverPreview::GifBytes {
                uri: Cow::Owned(format!("bytes://{request_key}.gif")),
                bytes: bytes.clone(),
            };
        }
        if !self.is_pending_or_failed(&request_key) {
            let evicted = self.video_scheduler.enqueue(
                AssetJob::VideoGif {
                    source_path: path.to_path_buf(),
                    request_key: request_key.clone(),
                    navigation_generation: self.navigation_generation,
                },
                AssetJobClass::HoverPreview,
                path.parent().map(|path| path.to_full_path_string()),
            );
            self.clear_evicted_requests(evicted);
            self.pending.insert(request_key);
        }
        HoverPreview::Loading
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

        let evicted = self.scheduler.enqueue(
            AssetJob::SystemIcon {
                request_key: key.clone(),
                lookup_arg,
                icon_size: size,
                navigation_generation: self.navigation_generation,
            },
            class,
            path.parent().map(|path| path.to_full_path_string()),
        );
        self.clear_evicted_requests(evicted);
        self.pending.insert(key);
        None
    }

    fn clear_evicted_requests(&mut self, request_keys: Vec<String>) {
        for key in request_keys {
            self.pending.remove(&key);
        }
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
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        return FOLDER_ICON_KEY.to_string();
    }
    #[cfg(target_os = "macos")]
    {
        let mut h = DefaultHasher::new();
        path.to_string_lossy().to_lowercase().hash(&mut h);
        let hash = h.finish();
        format!("{FOLDER_ICON_KEY}_{hash:016x}")
    }
}

fn maintain_visual_cache() {
    const MAX_BYTES: u64 = 512 * 1024 * 1024;
    const TARGET_BYTES: u64 = 384 * 1024 * 1024;
    let legacy_asset_store = thumbnail_cache_base_dir().join("asset_store");
    if legacy_asset_store.exists() {
        let _ = fs::remove_dir_all(legacy_asset_store);
    }
    let root = thumbnail_cache_dir();
    let mut files = walkdir::WalkDir::new(&root)
        .min_depth(1)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            let path = entry.into_path();
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("_v2."))
            {
                let _ = fs::remove_file(path);
                return None;
            }
            Some((
                metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                metadata.len(),
                path,
            ))
        })
        .collect::<Vec<_>>();
    let mut total = files.iter().map(|(_, len, _)| *len).sum::<u64>();
    if total <= MAX_BYTES {
        return;
    }
    files.sort_by_key(|(modified, _, _)| *modified);
    for (_, len, path) in files {
        if total <= TARGET_BYTES {
            break;
        }
        if fs::remove_file(path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

fn process_asset_job(job: AssetJob) -> AssetJobResult {
    match job {
        AssetJob::Thumbnail {
            source_path,
            request_key,
            kind,
            icon_size,
            source_revision,
            source_size,
            navigation_generation,
        } => match load_or_generate_thumbnail(
            &source_path,
            kind,
            icon_size,
            source_revision,
            source_size,
        ) {
            Some(image) => AssetJobResult::Ready {
                image,
                request_key,
                navigation_generation,
            },
            None => AssetJobResult::Failed {
                request_key,
                navigation_generation,
            },
        },
        AssetJob::SystemIcon {
            request_key,
            lookup_arg,
            icon_size,
            navigation_generation,
        } => match load_system_icon_image(&lookup_arg, icon_size) {
            Some(image) => AssetJobResult::Ready {
                image,
                request_key,
                navigation_generation,
            },
            None => AssetJobResult::Failed {
                request_key,
                navigation_generation,
            },
        },
        AssetJob::VideoGif {
            source_path,
            request_key,
            navigation_generation,
        } => match load_or_generate_video_gif(&source_path) {
            Some(gif_bytes) => AssetJobResult::GifReady {
                request_key,
                gif_bytes,
                navigation_generation,
            },
            None => AssetJobResult::Failed {
                request_key,
                navigation_generation,
            },
        },
    }
}

fn icon_key(path: &Path, is_dir: bool) -> String {
    if is_dir {
        return FOLDER_ICON_KEY.to_string();
    }
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .map_or_else(
            || NO_EXT_ICON_KEY.to_string(),
            // Single allocation: build `icon_<ext>` and ASCII-lowercase in
            // place (the `icon_` prefix is already lowercase, so this matches
            // the previous `format!("{ICON_EXT_PREFIX}{}", ext.to_lowercase())`
            // for the ascii extensions we actually cache).
            |ext| {
                let mut s = String::with_capacity(ICON_EXT_PREFIX.len() + ext.len());
                s.push_str(ICON_EXT_PREFIX);
                s.push_str(ext);
                s.make_ascii_lowercase();
                s
            },
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
    const IMAGE: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "bmp", "webp", "tiff", "tif", "ico", "avif", "tga",
    ];
    const VIDEO: &[&str] = &[
        "mp4", "mov", "mkv", "avi", "webm", "wmv", "flv", "m4v", "3gp", "ogv",
    ];
    // Match case-insensitively WITHOUT allocating a lowercased string (this is
    // called for every visible row every frame).
    let ext = path.extension()?.to_str()?;
    if IMAGE.iter().any(|c| ext.eq_ignore_ascii_case(c)) {
        Some(ThumbnailKind::Image)
    } else if VIDEO.iter().any(|c| ext.eq_ignore_ascii_case(c)) {
        Some(ThumbnailKind::Video)
    } else {
        None
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

fn thumbnail_cache_path(
    path: &Path,
    size: IconSize,
    source_revision: u128,
    source_size: u64,
) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    source_revision.hash(&mut hasher);
    source_size.hash(&mut hasher);
    let ext = thumbnail_cache_ext(path);
    let hash = format!("{:016x}", hasher.finish());
    let shard = thumbnail_cache_dir().join(&hash[..2]);
    let _ = fs::create_dir_all(&shard);
    shard.join(format!("{}{}_v3.{}", hash, size.cache_suffix(), ext))
}

fn thumbnail_cache_dir() -> PathBuf {
    static CACHE_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
        let path = thumbnail_cache_base_dir().join("thumbnails");
        let _ = fs::create_dir_all(&path);
        path
    });
    CACHE_DIR.clone()
}

fn thumbnail_cache_base_dir() -> PathBuf {
    ProjectDirs::from("io", "github.leinnan", "dirfleet").map_or_else(
        || PathBuf::from(".cache"),
        |dirs| dirs.cache_dir().to_path_buf(),
    )
}

fn load_or_generate_thumbnail(
    source_path: &Path,
    kind: ThumbnailKind,
    icon_size: IconSize,
    source_revision: u128,
    source_size: u64,
) -> Option<DecodedImage> {
    let cache_path = thumbnail_cache_path(source_path, icon_size, source_revision, source_size);
    if let Some(image) = decode_image_file(&cache_path, source_path.to_full_path_string()) {
        return Some(image);
    }
    if cache_path.exists() {
        let _ = fs::remove_file(&cache_path);
    }

    let decode_px = icon_size.decode_px();
    let image = match kind {
        ThumbnailKind::Image => image::open(source_path)
            .ok()?
            .thumbnail(decode_px, decode_px),
        ThumbnailKind::Video => generate_video_thumbnail(source_path, &cache_path, icon_size)?,
    };

    if !cache_path.exists() {
        atomic_save_image(&image, &cache_path);
    }

    Some(decoded_from_dynamic(
        &image,
        source_path.to_full_path_string(),
    ))
}

fn atomic_save_image(image: &image::DynamicImage, cache_path: &Path) {
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
    if image.save_with_format(&tmp, format).is_ok() {
        if fs::rename(&tmp, cache_path).is_err() {
            let _ = fs::remove_file(&tmp);
        }
    }
}

fn atomic_temp_path(cache_path: &Path, extension: &str) -> PathBuf {
    static TEMP_COUNTER: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);
    let sequence = TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    cache_path.with_extension(format!(
        "{extension}.{}.{}.tmp",
        std::process::id(),
        sequence
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
    #[cfg(test)]
    if Path::new(lookup_arg)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_none_or(|ext| ext.eq_ignore_ascii_case("txt"))
        && lookup_arg != "folder"
    {
        return Some(deterministic_test_document_icon(lookup_arg, icon_size));
    }

    // The Windows shell/GDI extraction used by `systemicons` is not reliably
    // re-entrant. Serializing this short call prevents corrupt/alternate
    // generic icons when several quick workers request extensions together.
    static SYSTEM_ICON_EXTRACTION: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
    let _guard = SYSTEM_ICON_EXTRACTION
        .lock()
        .expect("system icon extraction mutex poisoned");
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

#[cfg(test)]
fn deterministic_test_document_icon(lookup_arg: &str, icon_size: IconSize) -> DecodedImage {
    let size = icon_size.system_icon_px().max(16) as u32;
    let mut image = image::RgbaImage::new(size, size);
    let left = size / 5;
    let right = size - left;
    let top = size / 20;
    let bottom = size - top;
    let fold = size / 4;
    for y in top..bottom {
        for x in left..right {
            if x > right - fold && y < top + fold && x - (right - fold) > y - top {
                continue;
            }
            let border = x == left || x + 1 == right || y == top || y + 1 == bottom;
            image.put_pixel(
                x,
                y,
                if border {
                    image::Rgba([145, 145, 145, 255])
                } else {
                    image::Rgba([245, 245, 245, 255])
                },
            );
        }
    }
    for offset in 1..fold {
        let x = right - fold + offset;
        let y = top + offset;
        image.put_pixel(x, y, image::Rgba([145, 145, 145, 255]));
    }
    for line in [2u32, 3, 4, 5] {
        let y = top + fold + line * size / 10;
        for x in (left + size / 10)..(right - size / 10) {
            image.put_pixel(x, y, image::Rgba([170, 170, 170, 255]));
        }
    }
    DecodedImage {
        name: lookup_arg.to_owned(),
        width: size as usize,
        height: size as usize,
        rgba: image.into_raw(),
    }
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
        .args(["-loglevel", "error"])
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
    let stdout_thread = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr_thread = ffmpeg
        .take_stderr()
        .map(|mut stderr| thread::spawn(move || std::io::copy(&mut stderr, &mut std::io::sink())));
    if !wait_ffmpeg_timeout(&mut ffmpeg, std::time::Duration::from_secs(20)) {
        log::warn!("ffmpeg thumbnail timed out for {}", source_path.display());
        return None;
    }
    if let Some(handle) = stderr_thread {
        let _ = handle.join();
    }
    let png_bytes = stdout_thread.join().ok()?.ok()?;
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
    atomic_save_image(&image, cache_path);
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

fn animated_preview_cache_path(path: &Path) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    video_gif_request_key(path).hash(&mut hasher);
    let hash = format!("{:016x}", hasher.finish());
    let shard = thumbnail_cache_dir().join(&hash[..2]);
    let _ = fs::create_dir_all(&shard);
    shard.join(format!("{hash}_anim_v3.gif"))
}

fn load_or_generate_video_gif(path: &Path) -> Option<Arc<[u8]>> {
    const MAX_ANIMATED_CACHE_ENTRY_BYTES: usize = 4 * 1024 * 1024;
    let cache_path = animated_preview_cache_path(path);
    if let Ok(bytes) = fs::read(&cache_path) {
        if image::load_from_memory_with_format(&bytes, image::ImageFormat::Gif).is_ok() {
            return Some(Arc::from(bytes));
        }
        let _ = fs::remove_file(&cache_path);
    }

    let bytes = generate_video_gif(path)?;
    if bytes.len() <= MAX_ANIMATED_CACHE_ENTRY_BYTES {
        atomic_save_bytes(&bytes, &cache_path);
    }
    Some(bytes)
}

fn atomic_save_bytes(bytes: &[u8], cache_path: &Path) {
    let extension = cache_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("bin");
    let tmp = atomic_temp_path(cache_path, extension);
    if fs::write(&tmp, bytes).is_ok() && fs::rename(&tmp, cache_path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
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
        .args(["-loglevel", "error"])
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
    let stderr_thread = ffmpeg
        .take_stderr()
        .map(|mut stderr| thread::spawn(move || std::io::copy(&mut stderr, &mut std::io::sink())));
    if !wait_ffmpeg_timeout(&mut ffmpeg, std::time::Duration::from_secs(60)) {
        log::warn!("ffmpeg animated preview timed out for {}", path.display());
        let _ = fs::remove_dir_all(frame_dir);
        return None;
    }
    if let Some(handle) = stderr_thread {
        let _ = handle.join();
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

fn wait_ffmpeg_timeout(
    child: &mut ffmpeg_sidecar::child::FfmpegChild,
    timeout: std::time::Duration,
) -> bool {
    let started = Instant::now();
    loop {
        match child.as_inner_mut().try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() < timeout => {
                thread::sleep(std::time::Duration::from_millis(20));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn icon_job(key: &str) -> AssetJob {
        AssetJob::SystemIcon {
            request_key: key.to_owned(),
            lookup_arg: "txt".to_owned(),
            icon_size: IconSize::Small,
            navigation_generation: 0,
        }
    }

    #[test]
    fn active_directory_change_reprioritizes_existing_backlog() {
        let scheduler = JobScheduler::default();
        scheduler.set_active_directory(Some("old".to_owned()));
        scheduler.enqueue(
            icon_job("old"),
            AssetJobClass::EntryVisual,
            Some("old".to_owned()),
        );
        scheduler.set_active_directory(Some("new".to_owned()));
        scheduler.enqueue(
            icon_job("new"),
            AssetJobClass::EntryVisual,
            Some("new".to_owned()),
        );
        let AssetJob::SystemIcon { request_key, .. } = scheduler.recv() else {
            panic!("expected icon job");
        };
        assert_eq!(request_key, "new");
    }

    #[test]
    fn scheduler_queue_is_bounded() {
        let scheduler = JobScheduler::default();
        for index in 0..(MAX_ASSET_JOBS + 100) {
            scheduler.enqueue(
                icon_job(&index.to_string()),
                AssetJobClass::SidebarIcon,
                None,
            );
        }
        assert_eq!(
            scheduler.state.lock().expect("scheduler state").queue.len(),
            MAX_ASSET_JOBS
        );
    }

    #[test]
    fn full_queue_keeps_new_active_work() {
        let scheduler = JobScheduler::default();
        for index in 0..MAX_ASSET_JOBS {
            scheduler.enqueue(
                icon_job(&index.to_string()),
                AssetJobClass::SidebarIcon,
                None,
            );
        }
        scheduler.set_active_directory(Some("active".to_owned()));
        scheduler.enqueue(
            icon_job("active"),
            AssetJobClass::EntryVisual,
            Some("active".to_owned()),
        );
        let AssetJob::SystemIcon { request_key, .. } = scheduler.recv() else {
            panic!("expected icon job");
        };
        assert_eq!(request_key, "active");
    }

    #[test]
    fn newest_hover_preview_replaces_queued_hover_work() {
        let scheduler = JobScheduler::default();
        assert!(
            scheduler
                .enqueue(icon_job("old"), AssetJobClass::HoverPreview, None)
                .is_empty()
        );
        assert_eq!(
            scheduler.enqueue(icon_job("new"), AssetJobClass::HoverPreview, None),
            vec!["old"]
        );
        let AssetJob::SystemIcon { request_key, .. } = scheduler.recv() else {
            panic!("expected icon job");
        };
        assert_eq!(request_key, "new");
    }

    #[test]
    fn source_revision_changes_thumbnail_cache_key() {
        let path = Path::new("example.jpg");
        let first = thumbnail_cache_path(path, IconSize::Medium, 1, 10);
        let second = thumbnail_cache_path(path, IconSize::Medium, 2, 10);
        assert_ne!(first, second);
        assert!(first.to_string_lossy().contains("_v3.jpg"));
    }

    #[test]
    fn thumbnail_cache_recovers_corruption_and_handles_concurrent_misses() {
        let unique = format!(
            "lwa_fm_thumbnail_test_{}_{}.png",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        );
        let source = std::env::temp_dir().join(unique);
        let mut rgba = image::RgbaImage::new(24, 24);
        for pixel in rgba.pixels_mut() {
            *pixel = image::Rgba([20, 80, 160, 0]);
        }
        rgba.save(&source).expect("save source image");
        let metadata = fs::metadata(&source).expect("source metadata");
        let meta: crate::data::files::DirEntryMetaData = metadata.into();
        let cache_path =
            thumbnail_cache_path(&source, IconSize::Small, meta.source_revision, meta.size);
        let _ = fs::remove_file(&cache_path);

        let first = load_or_generate_thumbnail(
            &source,
            ThumbnailKind::Image,
            IconSize::Small,
            meta.source_revision,
            meta.size,
        )
        .expect("thumbnail miss should generate");
        assert_eq!(first.rgba[3], 0, "PNG alpha must be preserved");
        assert!(cache_path.exists());

        fs::write(&cache_path, b"truncated").expect("corrupt cache");
        let recovered = load_or_generate_thumbnail(
            &source,
            ThumbnailKind::Image,
            IconSize::Small,
            meta.source_revision,
            meta.size,
        )
        .expect("corrupt cache should regenerate immediately");
        assert_eq!(recovered.rgba[3], 0);

        let _ = fs::remove_file(&cache_path);
        let workers = (0..2)
            .map(|_| {
                let source = source.clone();
                thread::spawn(move || {
                    load_or_generate_thumbnail(
                        &source,
                        ThumbnailKind::Image,
                        IconSize::Small,
                        meta.source_revision,
                        meta.size,
                    )
                })
            })
            .collect::<Vec<_>>();
        assert!(workers.into_iter().all(|worker| {
            worker
                .join()
                .expect("thumbnail worker should not panic")
                .is_some()
        }));
        assert!(decode_image_file(&cache_path, "cached".to_owned()).is_some());

        let _ = fs::remove_file(source);
        let _ = fs::remove_file(cache_path);
    }
}
