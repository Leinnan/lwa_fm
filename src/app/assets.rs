use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet, hash_map::DefaultHasher};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Condvar, LazyLock, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use directories::ProjectDirs;
use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use ffmpeg_sidecar::command::FfmpegCommand;
use image::AnimationDecoder as _;
use image::codecs::gif::{GifDecoder, GifEncoder, Repeat};
use image::{Delay, Frame, RgbaImage};
use lru::LruCache;

use crate::data::files::DirEntry;
use crate::helper::PathHelper;

const FOLDER_ICON_KEY: &str = "icon_folder";
const ICON_EXT_PREFIX: &str = "icon_";
const NO_EXT_ICON_KEY: &str = "icon_no_ext";
const TEXTURE_CAPACITY: usize = 512;
const ANIMATION_CAPACITY: usize = 64;
const MAX_STATIC_TEXTURE_BYTES: usize = 128 * 1024 * 1024;
const MAX_ANIMATION_BYTES: usize = 64 * 1024 * 1024;
const VIDEO_PREVIEW_FRAMES: u32 = 12;
const VIDEO_PREVIEW_FRAME_DELAY: Duration = Duration::from_millis(250);
const VIDEO_PREVIEW_MAX_EDGE: u32 = 240;
// Maximum GPU texture uploads (via `ctx.load_texture`, which uploads during the
// render pass) processed from the job queue per frame. Each upload is ~0.6 ms,
// so during a bulk thumbnail load (e.g. opening a large image folder) this cap
// spreads the work across frames to avoid single-frame UI stalls. The queue
// is drained over subsequent frames because `poll_results` calls
// `ctx.request_repaint()` when it hits the limit.
const MAX_TEXTURES_PER_FRAME: usize = 4;
const MAX_STATIC_TEXTURES_PER_FRAME: usize = 3;
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

static CACHE_MAINTENANCE_STARTED: OnceLock<()> = OnceLock::new();
type FfmpegState = Option<(Instant, Result<(), String>)>;
static FFMPEG_STATE: LazyLock<Mutex<FfmpegState>> = LazyLock::new(|| Mutex::new(None));

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

impl DecodedImage {
    const fn byte_len(&self) -> usize {
        self.rgba.len()
    }
}

#[derive(Debug, Clone)]
struct DecodedAnimation {
    frames: Vec<DecodedImage>,
    frame_delays: Vec<Duration>,
}

impl DecodedAnimation {
    fn byte_len(&self) -> usize {
        self.frames.iter().map(DecodedImage::byte_len).sum()
    }
}

struct AnimationFrame {
    image: DecodedImage,
    texture: Option<TextureHandle>,
}

struct AnimatedPreview {
    frames: Vec<AnimationFrame>,
    frame_delays: Vec<Duration>,
    started_at: Instant,
    last_used: Instant,
    byte_len: usize,
}

impl AnimatedPreview {
    fn new(decoded: DecodedAnimation) -> Self {
        let now = Instant::now();
        let byte_len = decoded.byte_len();
        Self {
            frames: decoded
                .frames
                .into_iter()
                .map(|image| AnimationFrame {
                    image,
                    texture: None,
                })
                .collect(),
            frame_delays: decoded.frame_delays,
            started_at: now,
            last_used: now,
            byte_len,
        }
    }

    fn current_frame(&mut self) -> (usize, Duration) {
        let now = Instant::now();
        if now.duration_since(self.last_used) > Duration::from_secs(1) {
            self.started_at = now;
        }
        self.last_used = now;
        let total = self
            .frame_delays
            .iter()
            .copied()
            .sum::<Duration>()
            .max(Duration::from_millis(1));
        let mut position = now.duration_since(self.started_at).as_millis() % total.as_millis();
        for (index, delay) in self.frame_delays.iter().copied().enumerate() {
            let delay_ms = delay.as_millis().max(1);
            if position < delay_ms {
                return (
                    index.min(self.frames.len().saturating_sub(1)),
                    Duration::from_millis((delay_ms - position) as u64),
                );
            }
            position -= delay_ms;
        }
        (
            0,
            self.frame_delays
                .first()
                .copied()
                .unwrap_or(VIDEO_PREVIEW_FRAME_DELAY),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnimatedSourceKind {
    Gif,
    Video,
}

trait MediaBackend: Send + Sync {
    fn load_animated_preview(
        &self,
        path: &Path,
        source_revision: u128,
        source_size: u64,
        source_kind: AnimatedSourceKind,
        cancel: &AtomicBool,
    ) -> Result<DecodedAnimation, String>;
}

struct SystemMediaBackend;

impl MediaBackend for SystemMediaBackend {
    fn load_animated_preview(
        &self,
        path: &Path,
        source_revision: u128,
        source_size: u64,
        source_kind: AnimatedSourceKind,
        cancel: &AtomicBool,
    ) -> Result<DecodedAnimation, String> {
        load_or_generate_animated_preview(path, source_revision, source_size, source_kind, cancel)
    }
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
    AnimatedPreview {
        source_path: PathBuf,
        request_key: String,
        source_revision: u128,
        source_size: u64,
        source_kind: AnimatedSourceKind,
        navigation_generation: u64,
        cancel: Arc<AtomicBool>,
    },
}

impl AssetJob {
    fn request_key(&self) -> &str {
        match self {
            Self::Thumbnail { request_key, .. }
            | Self::SystemIcon { request_key, .. }
            | Self::AnimatedPreview { request_key, .. } => request_key,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssetJobClass {
    VisibleThumbnail,
    PrefetchThumbnail,
    SidebarIcon,
    HoveredPreview,
    SelectedPreview,
}

/// Priority for the binary heap: lower rank = higher priority,
/// lower order within same rank = older job processed first.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
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
    enqueued_at: Instant,
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
    ) -> EnqueueResult {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::assets::scheduler::enqueue");
        let mut result = EnqueueResult::default();
        {
            let mut state = self.state.lock().expect("job scheduler mutex poisoned");
            let order = state.next_order;
            state.next_order = state.next_order.saturating_add(1);
            let rank = job_rank(
                class,
                directory.as_deref(),
                state.active_directory.as_deref(),
            );
            if state.queue.len() >= MAX_ASSET_JOBS {
                let incoming_priority = JobPriority(rank, order);
                let worst = state
                    .queue
                    .iter()
                    .min_by(|a, b| a.priority.cmp(&b.priority))
                    .map(|entry| (entry.priority, entry.job.request_key().to_owned()));
                if let Some((worst_priority, worst_key)) = worst {
                    if incoming_priority <= worst_priority {
                        result.evicted.push(job.request_key().to_owned());
                        drop(state);
                        return result;
                    }
                    let mut removed = false;
                    let entries = state.queue.drain().filter(|entry| {
                        if !removed && entry.job.request_key() == worst_key {
                            removed = true;
                            false
                        } else {
                            true
                        }
                    });
                    state.queue = entries.collect();
                    result.evicted.push(worst_key);
                }
            }
            state.queue.push(HeapEntry {
                priority: JobPriority(rank, order),
                enqueued_at: Instant::now(),
                job,
                class,
                directory,
            });
            result.accepted = true;
            drop(state);
        }
        self.has_jobs.notify_one();
        result
    }

    fn set_active_directory(&self, directory: Option<String>) -> Vec<String> {
        let mut changed = false;
        let mut evicted = Vec::new();
        {
            let mut state = self.state.lock().expect("job scheduler mutex poisoned");
            if state.active_directory != directory {
                state.active_directory = directory;
                let active = state.active_directory.clone();
                let mut entries = state.queue.drain().collect::<Vec<_>>();
                entries.retain(|entry| {
                    let stale_visible = matches!(
                        entry.class,
                        AssetJobClass::VisibleThumbnail | AssetJobClass::PrefetchThumbnail
                    ) && entry.directory.is_some()
                        && entry.directory != active;
                    if stale_visible {
                        evicted.push(entry.job.request_key().to_owned());
                    }
                    !stale_visible
                });
                for entry in &mut entries {
                    entry.priority.0 =
                        job_rank(entry.class, entry.directory.as_deref(), active.as_deref());
                }
                state.queue.extend(entries);
                changed = true;
            }
            drop(state);
        }
        if changed {
            self.has_jobs.notify_all();
        }
        evicted
    }

    fn reprioritize(&self, request_key: &str, class: AssetJobClass) {
        let mut state = self.state.lock().expect("job scheduler mutex poisoned");
        let active = state.active_directory.clone();
        let mut entries = state.queue.drain().collect::<Vec<_>>();
        for entry in &mut entries {
            if entry.job.request_key() == request_key {
                entry.class = class;
                entry.priority.0 = job_rank(class, entry.directory.as_deref(), active.as_deref());
            }
        }
        state.queue.extend(entries);
    }

    fn cancel(&self, request_keys: &HashSet<String>) -> Vec<String> {
        if request_keys.is_empty() {
            return Vec::new();
        }
        let mut state = self.state.lock().expect("job scheduler mutex poisoned");
        let mut removed = Vec::new();
        let entries = state.queue.drain().filter(|entry| {
            if request_keys.contains(entry.job.request_key()) {
                removed.push(entry.job.request_key().to_owned());
                false
            } else {
                true
            }
        });
        state.queue = entries.collect();
        removed
    }

    fn recv(&self) -> AssetJob {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::assets::scheduler::recv");
        let mut state = self.state.lock().expect("job scheduler mutex poisoned");
        let entry = loop {
            if let Some(entry) = state.queue.pop() {
                break entry;
            }
            state = self
                .has_jobs
                .wait(state)
                .expect("job scheduler mutex poisoned while waiting");
        };
        drop(state);
        #[cfg(not(feature = "profiling"))]
        let _queue_delay = entry.enqueued_at.elapsed();
        #[cfg(feature = "profiling")]
        puffin::profile_scope!(
            "lwa_fm::assets::scheduler::queue_delay",
            &format!("{} us", entry.enqueued_at.elapsed().as_micros())
        );
        entry.job
    }
}

#[derive(Debug, Default)]
struct EnqueueResult {
    accepted: bool,
    evicted: Vec<String>,
}

fn job_rank(class: AssetJobClass, directory: Option<&str>, active_directory: Option<&str>) -> u8 {
    let is_active_directory = directory
        .zip(active_directory)
        .is_some_and(|(dir, active)| dir == active);
    match (class, is_active_directory) {
        (AssetJobClass::HoveredPreview, _) => 0,
        (AssetJobClass::SelectedPreview, _) => 1,
        (AssetJobClass::VisibleThumbnail, true) => 2,
        (AssetJobClass::VisibleThumbnail, false) => 3,
        (AssetJobClass::PrefetchThumbnail, true) => 4,
        (AssetJobClass::PrefetchThumbnail, false) => 5,
        (AssetJobClass::SidebarIcon, _) => 6,
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
        reason: String,
        navigation_generation: u64,
    },
    AnimationReady {
        request_key: String,
        animation: DecodedAnimation,
        navigation_generation: u64,
    },
    Cancelled {
        request_key: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThumbnailKind {
    Image,
    Video,
}

/// Recorded failure of a single asset request. `tries` drives exponential
/// backoff and eventually permanent-failure semantics.
#[derive(Debug, Clone)]
struct FailureRecord {
    last_attempt: Instant,
    tries: u32,
    reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PreviewIntent {
    Selected,
    Hovered,
}

impl PreviewIntent {
    const fn job_class(self) -> AssetJobClass {
        match self {
            Self::Selected => AssetJobClass::SelectedPreview,
            Self::Hovered => AssetJobClass::HoveredPreview,
        }
    }
}

#[derive(Debug)]
struct PreviewJobControl {
    intent: PreviewIntent,
    cancel: Arc<AtomicBool>,
}

pub struct AssetManager {
    textures: LruCache<String, TextureHandle>,
    texture_bytes: usize,
    animations: LruCache<String, AnimatedPreview>,
    animation_bytes: usize,
    pending: HashSet<String>,
    failed: HashMap<String, FailureRecord>,
    scheduler: Arc<JobScheduler>,
    video_thumbnail_scheduler: Arc<JobScheduler>,
    preview_scheduler: Arc<JobScheduler>,
    preview_jobs: HashMap<String, PreviewJobControl>,
    desired_previews: HashMap<String, PreviewIntent>,
    receiver: Receiver<AssetJobResult>,
    icon_size: IconSize,
    per_dir_icon_size: HashMap<String, IconSize>,
    repaint_ctx: Arc<Mutex<Option<Context>>>,
    active_directory: Option<String>,
    navigation_generation: u64,
    frame_uploads: usize,
    frame_upload_started: Instant,
    last_asset_activity: Instant,
    cache_maintenance_started: bool,
}

pub enum HoverPreview {
    Ready(TextureHandle),
    Pending,
    Unavailable {
        reason: String,
        retry_after: Option<Duration>,
    },
    Fallback,
}

impl AssetManager {
    pub fn new() -> Self {
        Self::with_media_backend(Arc::new(SystemMediaBackend))
    }

    fn with_media_backend(media_backend: Arc<dyn MediaBackend>) -> Self {
        let (result_tx, result_rx) = mpsc::sync_channel::<AssetJobResult>(64);
        let scheduler = Arc::new(JobScheduler::default());
        let video_thumbnail_scheduler = Arc::new(JobScheduler::default());
        let preview_scheduler = Arc::new(JobScheduler::default());
        let repaint_ctx = Arc::new(Mutex::new(None::<Context>));

        // Scale decode/resize workers with core count. ffmpeg invocations
        // are pinned to `-threads 1` (see generate_video_thumbnail /
        // generate_video_gif) so this parallelises image work across cores
        // without oversubscribing on video jobs.
        let worker_count = std::thread::available_parallelism()
            .map_or(4, std::num::NonZero::get)
            .saturating_sub(1)
            .clamp(1, 4);
        for _ in 0..worker_count {
            let worker_scheduler = Arc::clone(&scheduler);
            let worker_tx = result_tx.clone();
            let worker_repaint = Arc::clone(&repaint_ctx);
            let worker_backend = Arc::clone(&media_backend);
            thread::spawn(move || {
                loop {
                    let job = worker_scheduler.recv();

                    let result = process_asset_job(job, worker_backend.as_ref());
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
        let video_worker_count = std::thread::available_parallelism()
            .map_or(4, std::num::NonZero::get)
            .saturating_sub(2)
            .clamp(1, 2);
        for _ in 0..video_worker_count {
            let worker_scheduler = Arc::clone(&video_thumbnail_scheduler);
            let worker_tx = result_tx.clone();
            let worker_repaint = Arc::clone(&repaint_ctx);
            let worker_backend = Arc::clone(&media_backend);
            thread::spawn(move || {
                loop {
                    let result =
                        process_asset_job(worker_scheduler.recv(), worker_backend.as_ref());
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
            let worker_scheduler = Arc::clone(&preview_scheduler);
            let worker_tx = result_tx;
            let worker_repaint = Arc::clone(&repaint_ctx);
            let worker_backend = Arc::clone(&media_backend);
            thread::spawn(move || {
                loop {
                    let result =
                        process_asset_job(worker_scheduler.recv(), worker_backend.as_ref());
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
            texture_bytes: 0,
            animations: LruCache::new(
                std::num::NonZero::new(ANIMATION_CAPACITY).expect("ANIMATION_CAPACITY must be > 0"),
            ),
            animation_bytes: 0,
            pending: HashSet::new(),
            failed: HashMap::new(),
            scheduler,
            video_thumbnail_scheduler,
            preview_scheduler,
            preview_jobs: HashMap::new(),
            desired_previews: HashMap::new(),
            receiver: result_rx,
            icon_size: IconSize::default(),
            per_dir_icon_size: HashMap::new(),
            repaint_ctx,
            active_directory: None,
            navigation_generation: 0,
            frame_uploads: 0,
            frame_upload_started: Instant::now(),
            last_asset_activity: Instant::now(),
            cache_maintenance_started: false,
        }
    }

    pub fn begin_frame(&mut self) {
        self.desired_previews.clear();
        self.frame_uploads = 0;
        self.frame_upload_started = Instant::now();
    }

    pub fn end_frame(&mut self) {
        let obsolete: HashSet<String> = self
            .preview_jobs
            .keys()
            .filter(|key| !self.desired_previews.contains_key(*key))
            .cloned()
            .collect();
        for key in &obsolete {
            if let Some(control) = self.preview_jobs.get(key) {
                control.cancel.store(true, AtomicOrdering::Release);
            }
        }
        let removed = self.preview_scheduler.cancel(&obsolete);
        for key in removed {
            self.preview_jobs.remove(&key);
            self.pending.remove(&key);
        }

        if !self.cache_maintenance_started
            && self.pending.is_empty()
            && self.last_asset_activity.elapsed() >= Duration::from_secs(2)
        {
            self.cache_maintenance_started = true;
            CACHE_MAINTENANCE_STARTED.get_or_init(|| {
                thread::spawn(maintain_visual_cache);
            });
        }
    }

    pub fn set_active_directory(&mut self, path: Option<&Path>) {
        let directory = path.map(|path| path.to_full_path_string());
        if self.active_directory != directory {
            self.active_directory.clone_from(&directory);
            self.navigation_generation = self.navigation_generation.wrapping_add(1);
        }
        let mut evicted = self.scheduler.set_active_directory(directory.clone());
        evicted.extend(
            self.video_thumbnail_scheduler
                .set_active_directory(directory.clone()),
        );
        evicted.extend(self.preview_scheduler.set_active_directory(directory));
        self.clear_evicted_requests(evicted);
    }

    pub fn poll_results(&mut self, ctx: &Context) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::assets::poll_results");
        if let Ok(mut repaint_ctx) = self.repaint_ctx.lock()
            && repaint_ctx.is_none()
        {
            *repaint_ctx = Some(ctx.clone());
        }
        let mut received_any = false;
        let mut processed = 0usize;
        let mut received = 0usize;
        loop {
            // Limit GPU texture uploads per frame to avoid UI thread stalls
            if processed >= MAX_STATIC_TEXTURES_PER_FRAME
                || received >= MAX_RESULTS_PER_FRAME
                || (self.frame_uploads > 0
                    && self.frame_upload_started.elapsed() >= Duration::from_millis(2))
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
                    #[cfg(feature = "profiling")]
                    puffin::profile_scope!("lwa_fm::assets::texture_upload::static");
                    let texture = ctx.load_texture(
                        image.name.clone(),
                        ColorImage::from_rgba_unmultiplied(
                            [image.width, image.height],
                            &image.rgba,
                        ),
                        TextureOptions::LINEAR,
                    );
                    self.frame_uploads += 1;
                    self.put_texture(request_key, texture);
                    received_any = true;
                    processed += 1;
                }
                Ok(AssetJobResult::Failed {
                    request_key,
                    reason,
                    navigation_generation,
                }) => {
                    received += 1;
                    self.pending.remove(&request_key);
                    self.preview_jobs.remove(&request_key);
                    if navigation_generation != self.navigation_generation {
                        received_any = true;
                        continue;
                    }
                    self.failed
                        .entry(request_key)
                        .and_modify(|record| {
                            record.last_attempt = Instant::now();
                            record.tries = record.tries.saturating_add(1);
                            record.reason.clone_from(&reason);
                        })
                        .or_insert_with(|| FailureRecord {
                            last_attempt: Instant::now(),
                            tries: 1,
                            reason,
                        });
                    received_any = true;
                }
                Ok(AssetJobResult::AnimationReady {
                    request_key,
                    animation,
                    navigation_generation,
                }) => {
                    received += 1;
                    self.pending.remove(&request_key);
                    self.preview_jobs.remove(&request_key);
                    if navigation_generation != self.navigation_generation {
                        received_any = true;
                        continue;
                    }
                    self.failed.remove(&request_key);
                    self.put_animation(request_key, AnimatedPreview::new(animation));
                    received_any = true;
                }
                Ok(AssetJobResult::Cancelled { request_key }) => {
                    received += 1;
                    self.pending.remove(&request_key);
                    self.preview_jobs.remove(&request_key);
                    received_any = true;
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
            self.begin_frame();
            self.poll_results(ctx);
            thread::sleep(std::time::Duration::from_millis(1));
        }
        self.begin_frame();
        self.poll_results(ctx);
    }

    fn put_texture(&mut self, key: String, texture: TextureHandle) {
        let bytes = texture.size()[0]
            .saturating_mul(texture.size()[1])
            .saturating_mul(4);
        if let Some(previous) = self.textures.put(key, texture) {
            self.texture_bytes = self.texture_bytes.saturating_sub(
                previous.size()[0]
                    .saturating_mul(previous.size()[1])
                    .saturating_mul(4),
            );
        }
        self.texture_bytes = self.texture_bytes.saturating_add(bytes);
        while self.texture_bytes > MAX_STATIC_TEXTURE_BYTES {
            let Some((_key, evicted)) = self.textures.pop_lru() else {
                break;
            };
            self.texture_bytes = self.texture_bytes.saturating_sub(
                evicted.size()[0]
                    .saturating_mul(evicted.size()[1])
                    .saturating_mul(4),
            );
        }
        #[cfg(feature = "profiling")]
        puffin::profile_scope!(
            "lwa_fm::assets::cache_bytes::static",
            &format!("{} bytes", self.texture_bytes)
        );
    }

    fn put_animation(&mut self, key: String, animation: AnimatedPreview) {
        let bytes = animation.byte_len;
        if let Some(previous) = self.animations.put(key, animation) {
            self.animation_bytes = self.animation_bytes.saturating_sub(previous.byte_len);
        }
        self.animation_bytes = self.animation_bytes.saturating_add(bytes);
        while self.animation_bytes > MAX_ANIMATION_BYTES {
            let Some((_key, evicted)) = self.animations.pop_lru() else {
                break;
            };
            self.animation_bytes = self.animation_bytes.saturating_sub(evicted.byte_len);
        }
        #[cfg(feature = "profiling")]
        puffin::profile_scope!(
            "lwa_fm::assets::cache_bytes::animated",
            &format!("{} bytes", self.animation_bytes)
        );
    }

    pub fn set_icon_size(&mut self, size: IconSize) {
        if self.icon_size != size {
            self.icon_size = size;
            self.textures.clear();
            self.texture_bytes = 0;
            self.animations.clear();
            self.animation_bytes = 0;
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
        self.request_entry_texture_at_size(entry, size, AssetJobClass::VisibleThumbnail)
    }

    pub fn prefetch_entry_texture(&mut self, entry: &DirEntry) {
        let size = self.effective_icon_size(entry);
        let _ = self.request_entry_texture_at_size(entry, size, AssetJobClass::PrefetchThumbnail);
    }

    fn request_entry_texture_at_size(
        &mut self,
        entry: &DirEntry,
        size: IconSize,
        class: AssetJobClass,
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
            let scheduler = if kind == ThumbnailKind::Video {
                &self.video_thumbnail_scheduler
            } else {
                &self.scheduler
            };
            if self.pending.contains(&cache_key) && class == AssetJobClass::VisibleThumbnail {
                scheduler.reprioritize(&cache_key, class);
            }
            if !self.is_pending_or_failed(&cache_key) {
                let job = AssetJob::Thumbnail {
                    source_path: path.clone(),
                    request_key: cache_key.clone(),
                    kind,
                    icon_size: size,
                    source_revision: entry.meta.source_revision,
                    source_size: entry.meta.size,
                    navigation_generation: self.navigation_generation,
                };
                let enqueue = scheduler.enqueue(
                    job,
                    class,
                    path.parent().map(|path| path.to_full_path_string()),
                );
                self.clear_evicted_requests(enqueue.evicted);
                if enqueue.accepted {
                    self.pending.insert(cache_key);
                    self.last_asset_activity = Instant::now();
                }
            }
        }

        self.request_file_icon_texture(&path, entry.is_file(), size, class)
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
        let enqueue = self.scheduler.enqueue(
            AssetJob::SystemIcon {
                request_key: cache_key.clone(),
                lookup_arg,
                icon_size: size,
                navigation_generation: self.navigation_generation,
            },
            AssetJobClass::SidebarIcon,
            path.parent().map(|path| path.to_full_path_string()),
        );
        self.clear_evicted_requests(enqueue.evicted);
        if enqueue.accepted {
            self.pending.insert(cache_key);
            self.last_asset_activity = Instant::now();
        }
        None
    }

    pub fn request_hover_preview(
        &mut self,
        ctx: &Context,
        entry: &DirEntry,
        intent: PreviewIntent,
    ) -> HoverPreview {
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
                self.request_entry_texture_at_size(
                    entry,
                    IconSize::ExtraLarge,
                    AssetJobClass::VisibleThumbnail,
                )
                .map_or(HoverPreview::Pending, HoverPreview::Ready)
            }
            "gif" => self.request_animated_preview(ctx, entry, intent, AnimatedSourceKind::Gif),
            ext_str if VIDEO_EXTS.contains(&ext_str) => {
                self.request_animated_preview(ctx, entry, intent, AnimatedSourceKind::Video)
            }
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
        self.texture_bytes = self
            .textures
            .iter()
            .map(|(_, texture)| texture.size()[0] * texture.size()[1] * 4)
            .sum();
        let animation_keys_to_remove: Vec<String> = self
            .animations
            .iter()
            .filter(|(key, _)| matches_file(key))
            .map(|(key, _)| key.clone())
            .collect();
        for key in animation_keys_to_remove {
            self.animations.pop(&key);
        }
        self.animation_bytes = self
            .animations
            .iter()
            .map(|(_, value)| value.byte_len)
            .sum();
        let preview_keys: HashSet<String> = self
            .preview_jobs
            .keys()
            .filter(|key| matches_file(key))
            .cloned()
            .collect();
        for key in &preview_keys {
            if let Some(control) = self.preview_jobs.get(key) {
                control.cancel.store(true, AtomicOrdering::Release);
            }
        }
        for key in self.preview_scheduler.cancel(&preview_keys) {
            self.preview_jobs.remove(&key);
            self.pending.remove(&key);
        }
        let running_preview_keys: HashSet<String> = self.preview_jobs.keys().cloned().collect();
        self.pending
            .retain(|key| !matches_file(key) || running_preview_keys.contains(key));
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
        self.texture_bytes = self
            .textures
            .iter()
            .map(|(_, texture)| texture.size()[0] * texture.size()[1] * 4)
            .sum();
        let animation_keys_to_remove: Vec<String> = self
            .animations
            .iter()
            .filter(|(key, _)| matches_dir(key))
            .map(|(key, _)| key.clone())
            .collect();
        for key in animation_keys_to_remove {
            self.animations.pop(&key);
        }
        self.animation_bytes = self
            .animations
            .iter()
            .map(|(_, value)| value.byte_len)
            .sum();
        let preview_keys: HashSet<String> = self
            .preview_jobs
            .keys()
            .filter(|key| matches_dir(key))
            .cloned()
            .collect();
        for key in &preview_keys {
            if let Some(control) = self.preview_jobs.get(key) {
                control.cancel.store(true, AtomicOrdering::Release);
            }
        }
        for key in self.preview_scheduler.cancel(&preview_keys) {
            self.preview_jobs.remove(&key);
            self.pending.remove(&key);
        }
        let running_preview_keys: HashSet<String> = self.preview_jobs.keys().cloned().collect();
        self.pending
            .retain(|key| !matches_dir(key) || running_preview_keys.contains(key));
        self.failed.retain(|key, _| !matches_dir(key));
    }

    fn effective_icon_size(&self, entry: &DirEntry) -> IconSize {
        let (dir, _) = entry.get_splitted_path();
        self.per_dir_icon_size
            .get(dir)
            .copied()
            .unwrap_or(self.icon_size)
    }

    fn request_animated_preview(
        &mut self,
        ctx: &Context,
        entry: &DirEntry,
        intent: PreviewIntent,
        source_kind: AnimatedSourceKind,
    ) -> HoverPreview {
        let path = entry.get_path();
        let request_key = format!(
            "{}#{}:{}#anim_v4",
            path.to_full_path_string(),
            entry.meta.source_revision,
            entry.meta.size
        );
        self.desired_previews
            .entry(request_key.clone())
            .and_modify(|current| *current = (*current).max(intent))
            .or_insert(intent);

        if self.animations.contains(&request_key) {
            return self.render_animated_preview(ctx, &request_key);
        }

        if let Some(state) = self.preview_failure_state(&request_key) {
            return state;
        }

        if self.pending.contains(&request_key) {
            if let Some(control) = self.preview_jobs.get_mut(&request_key)
                && intent > control.intent
            {
                control.intent = intent;
                self.preview_scheduler
                    .reprioritize(&request_key, intent.job_class());
            }
            return HoverPreview::Pending;
        }

        let cancel = Arc::new(AtomicBool::new(false));
        let enqueue = self.preview_scheduler.enqueue(
            AssetJob::AnimatedPreview {
                source_path: path.clone(),
                request_key: request_key.clone(),
                source_revision: entry.meta.source_revision,
                source_size: entry.meta.size,
                source_kind,
                navigation_generation: self.navigation_generation,
                cancel: Arc::clone(&cancel),
            },
            intent.job_class(),
            path.parent().map(|path| path.to_full_path_string()),
        );
        self.clear_evicted_requests(enqueue.evicted);
        if enqueue.accepted {
            self.pending.insert(request_key.clone());
            self.preview_jobs
                .insert(request_key, PreviewJobControl { intent, cancel });
            self.last_asset_activity = Instant::now();
        }
        HoverPreview::Pending
    }

    fn render_animated_preview(&mut self, ctx: &Context, request_key: &str) -> HoverPreview {
        let can_upload = self.frame_uploads < MAX_TEXTURES_PER_FRAME
            && (self.frame_uploads == 0
                || self.frame_upload_started.elapsed() < Duration::from_millis(2));
        let Some(animation) = self.animations.get_mut(request_key) else {
            return HoverPreview::Pending;
        };
        let (frame_index, next_frame) = animation.current_frame();
        ctx.request_repaint_after(next_frame);
        let Some(frame) = animation.frames.get_mut(frame_index) else {
            return HoverPreview::Unavailable {
                reason: "Animated preview contains no frames".to_owned(),
                retry_after: None,
            };
        };
        if let Some(texture) = &frame.texture {
            return HoverPreview::Ready(texture.clone());
        }
        if !can_upload {
            ctx.request_repaint();
            return HoverPreview::Pending;
        }
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::assets::texture_upload::animated");
        let texture = ctx.load_texture(
            format!("{request_key}#{frame_index}"),
            ColorImage::from_rgba_unmultiplied(
                [frame.image.width, frame.image.height],
                &frame.image.rgba,
            ),
            TextureOptions::LINEAR,
        );
        frame.texture = Some(texture.clone());
        self.frame_uploads += 1;
        HoverPreview::Ready(texture)
    }

    fn preview_failure_state(&self, key: &str) -> Option<HoverPreview> {
        let record = self.failed.get(key)?;
        if record.tries >= ICON_RETRY_MAX_TRIES {
            return Some(HoverPreview::Unavailable {
                reason: record.reason.clone(),
                retry_after: None,
            });
        }
        let shift = (record.tries - 1).min(ICON_BACKOFF_SHIFT_CAP);
        let backoff = Duration::from_secs(ICON_RETRY_BASE_SECS * (1u64 << shift));
        let elapsed = record.last_attempt.elapsed();
        (elapsed < backoff).then(|| HoverPreview::Unavailable {
            reason: record.reason.clone(),
            retry_after: Some(backoff.saturating_sub(elapsed)),
        })
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
        if self.pending.contains(&key) && class == AssetJobClass::VisibleThumbnail {
            self.scheduler.reprioritize(&key, class);
        }
        if self.is_pending_or_failed(&key) {
            return None;
        }

        let lookup_arg = if is_file {
            path.to_string_lossy().to_string()
        } else {
            directory_lookup_arg(path)
        };

        let enqueue = self.scheduler.enqueue(
            AssetJob::SystemIcon {
                request_key: key.clone(),
                lookup_arg,
                icon_size: size,
                navigation_generation: self.navigation_generation,
            },
            class,
            path.parent().map(|path| path.to_full_path_string()),
        );
        self.clear_evicted_requests(enqueue.evicted);
        if enqueue.accepted {
            self.pending.insert(key);
            self.last_asset_activity = Instant::now();
        }
        None
    }

    fn clear_evicted_requests(&mut self, request_keys: Vec<String>) {
        for key in request_keys {
            self.pending.remove(&key);
            if let Some(control) = self.preview_jobs.remove(&key) {
                control.cancel.store(true, AtomicOrdering::Release);
            }
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
                .is_some_and(|name| name.contains("_v2.") || name.contains("_anim_v3.gif"))
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

fn process_asset_job(job: AssetJob, media_backend: &dyn MediaBackend) -> AssetJobResult {
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
                reason: format!("Could not create thumbnail for {}", source_path.display()),
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
                reason: format!("Could not load the system icon for {lookup_arg}"),
                navigation_generation,
            },
        },
        AssetJob::AnimatedPreview {
            source_path,
            request_key,
            source_revision,
            source_size,
            source_kind,
            navigation_generation,
            cancel,
        } => {
            if cancel.load(AtomicOrdering::Acquire) {
                return AssetJobResult::Cancelled { request_key };
            }
            match media_backend.load_animated_preview(
                &source_path,
                source_revision,
                source_size,
                source_kind,
                &cancel,
            ) {
                Ok(animation) if !cancel.load(AtomicOrdering::Acquire) => {
                    AssetJobResult::AnimationReady {
                        request_key,
                        animation,
                        navigation_generation,
                    }
                }
                Ok(_) => AssetJobResult::Cancelled { request_key },
                Err(reason) if cancel.load(AtomicOrdering::Acquire) => {
                    let _ = reason;
                    AssetJobResult::Cancelled { request_key }
                }
                Err(reason) => AssetJobResult::Failed {
                    request_key,
                    reason,
                    navigation_generation,
                },
            }
        }
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
    #[cfg(feature = "profiling")]
    puffin::profile_scope!("lwa_fm::assets::thumbnail::load_or_generate");
    let cache_path = thumbnail_cache_path(source_path, icon_size, source_revision, source_size);
    if let Some(image) = decode_image_file(&cache_path, source_path.to_full_path_string()) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::assets::thumbnail::cache_hit");
        return Some(image);
    }
    #[cfg(feature = "profiling")]
    puffin::profile_scope!("lwa_fm::assets::thumbnail::cache_miss");
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
    static TEMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
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
    if !wait_ffmpeg_timeout(&mut ffmpeg, Duration::from_secs(20), None) {
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
    let mut state = FFMPEG_STATE.lock().map_err(|err| err.to_string())?;
    if let Some((attempted_at, result)) = state.as_ref()
        && (result.is_ok() || attempted_at.elapsed() < Duration::from_secs(30))
    {
        return result.clone();
    }
    let result = ffmpeg_sidecar::download::auto_download().map_err(|err| err.to_string());
    *state = Some((Instant::now(), result.clone()));
    result
}

fn video_probe_cache_key(path: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    if let Ok(metadata) = fs::metadata(path)
        && let Ok(modified) = metadata.modified()
    {
        modified.hash(&mut hasher);
    }
    format!("{}#{:x}", path.to_full_path_string(), hasher.finish())
}

fn animated_preview_cache_path(path: &Path, source_revision: u128, source_size: u64) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    source_revision.hash(&mut hasher);
    source_size.hash(&mut hasher);
    VIDEO_PREVIEW_FRAMES.hash(&mut hasher);
    VIDEO_PREVIEW_MAX_EDGE.hash(&mut hasher);
    VIDEO_PREVIEW_FRAME_DELAY.hash(&mut hasher);
    5_u8.hash(&mut hasher);
    90_u8.hash(&mut hasher);
    let hash = format!("{:016x}", hasher.finish());
    let shard = thumbnail_cache_dir().join(&hash[..2]);
    let _ = fs::create_dir_all(&shard);
    shard.join(format!("{hash}_anim_v4.gif"))
}

fn load_or_generate_animated_preview(
    path: &Path,
    source_revision: u128,
    source_size: u64,
    source_kind: AnimatedSourceKind,
    cancel: &AtomicBool,
) -> Result<DecodedAnimation, String> {
    #[cfg(feature = "profiling")]
    puffin::profile_scope!("lwa_fm::assets::animation::load_or_generate");
    const MAX_ANIMATED_CACHE_ENTRY_BYTES: usize = 4 * 1024 * 1024;
    if cancel.load(AtomicOrdering::Acquire) {
        return Err("Preview request was cancelled".to_owned());
    }
    let cache_path = animated_preview_cache_path(path, source_revision, source_size);
    if let Ok(bytes) = fs::read(&cache_path) {
        match decode_gif_preview(&bytes, source_kind) {
            Ok(animation) => {
                #[cfg(feature = "profiling")]
                puffin::profile_scope!("lwa_fm::assets::animation::cache_hit");
                return Ok(animation);
            }
            Err(err) => {
                log::warn!("discarding corrupt animated preview cache: {err}");
                let _ = fs::remove_file(&cache_path);
            }
        }
    }
    #[cfg(feature = "profiling")]
    puffin::profile_scope!("lwa_fm::assets::animation::cache_miss");

    if source_kind == AnimatedSourceKind::Gif {
        let bytes =
            fs::read(path).map_err(|err| format!("Could not read animated image: {err}"))?;
        let animation = decode_gif_preview(&bytes, source_kind)?;
        if let Ok(compact_bytes) = encode_animation_gif(&animation)
            && compact_bytes.len() <= MAX_ANIMATED_CACHE_ENTRY_BYTES
        {
            atomic_save_bytes(&compact_bytes, &cache_path);
        }
        return Ok(animation);
    }

    let bytes = generate_video_preview_gif(path, cancel)?;
    if bytes.len() <= MAX_ANIMATED_CACHE_ENTRY_BYTES {
        atomic_save_bytes(&bytes, &cache_path);
    }
    decode_gif_preview(&bytes, source_kind)
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

fn generate_video_preview_gif(path: &Path, cancel: &AtomicBool) -> Result<Vec<u8>, String> {
    #[cfg(feature = "profiling")]
    puffin::profile_scope!("lwa_fm::assets::animation::ffmpeg");
    ensure_ffmpeg().map_err(|err| format!("FFmpeg is unavailable: {err}"))?;
    let duration_secs = probe_video_duration(path).unwrap_or(30.0).max(1.0);
    let start_pct = 0.05_f64;
    let end_pct = 0.90_f64;
    let span = (end_pct - start_pct) * duration_secs;
    let fps = f64::from(VIDEO_PREVIEW_FRAMES) / span;
    let filter = format!(
        "fps={fps:.4},scale={VIDEO_PREVIEW_MAX_EDGE}:{VIDEO_PREVIEW_MAX_EDGE}:force_original_aspect_ratio=decrease:force_divisible_by=2:flags=lanczos"
    );

    let mut ffmpeg = match FfmpegCommand::new()
        .args(["-loglevel", "error"])
        .args(["-threads", "1"])
        .seek(format!("{:.3}", start_pct * duration_secs))
        .input(path.to_string_lossy())
        .duration(format!("{span:.3}"))
        .args(["-vf", &filter])
        .frames(VIDEO_PREVIEW_FRAMES)
        .format("gif")
        .pipe_stdout()
        .spawn()
    {
        Ok(child) => child,
        Err(err) => return Err(format!("Could not start FFmpeg: {err}")),
    };
    let mut stdout = ffmpeg
        .take_stdout()
        .ok_or_else(|| "FFmpeg did not expose preview output".to_owned())?;
    let stdout_thread = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr_thread = ffmpeg.take_stderr().map(|mut stderr| {
        thread::spawn(move || {
            let mut message = String::new();
            let _ = stderr.read_to_string(&mut message);
            message
        })
    });
    let succeeded = wait_ffmpeg_timeout(&mut ffmpeg, Duration::from_mins(1), Some(cancel));
    let stderr = stderr_thread
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    let bytes = stdout_thread
        .join()
        .map_err(|_| "FFmpeg output reader panicked".to_owned())?
        .map_err(|err| format!("Could not read FFmpeg preview output: {err}"))?;
    if cancel.load(AtomicOrdering::Acquire) {
        return Err("Preview request was cancelled".to_owned());
    }
    if !succeeded || bytes.is_empty() {
        return Err(if stderr.trim().is_empty() {
            "FFmpeg could not decode this video".to_owned()
        } else {
            format!("FFmpeg could not decode this video: {}", stderr.trim())
        });
    }
    Ok(bytes)
}

fn decode_gif_preview(
    bytes: &[u8],
    source_kind: AnimatedSourceKind,
) -> Result<DecodedAnimation, String> {
    #[cfg(feature = "profiling")]
    puffin::profile_scope!("lwa_fm::assets::animation::decode");
    let decoder = GifDecoder::new(Cursor::new(bytes))
        .map_err(|err| format!("Could not decode animated preview: {err}"))?;
    let source_frames = decoder
        .into_frames()
        .take(120)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("Could not decode animated preview frame: {err}"))?;
    if source_frames.is_empty() {
        return Err("Animated preview contains no frames".to_owned());
    }
    let selected_count = source_frames.len().min(VIDEO_PREVIEW_FRAMES as usize);
    let selected_indices: Vec<usize> = if selected_count == 1 {
        vec![0]
    } else {
        (0..selected_count)
            .map(|index| index * (source_frames.len() - 1) / (selected_count - 1))
            .collect()
    };
    let mut frames = Vec::with_capacity(selected_count);
    let mut frame_delays = Vec::with_capacity(selected_count);
    for (source_index, frame) in source_frames.into_iter().enumerate() {
        if !selected_indices.contains(&source_index) {
            continue;
        }
        let delay: Duration = frame.delay().into();
        let image = image::DynamicImage::ImageRgba8(frame.into_buffer())
            .thumbnail(VIDEO_PREVIEW_MAX_EDGE, VIDEO_PREVIEW_MAX_EDGE);
        frames.push(decoded_from_dynamic(
            &image,
            format!("animated-frame-{source_index}"),
        ));
        frame_delays.push(if source_kind == AnimatedSourceKind::Video {
            VIDEO_PREVIEW_FRAME_DELAY
        } else {
            delay.clamp(Duration::from_millis(20), Duration::from_secs(2))
        });
    }
    Ok(DecodedAnimation {
        frames,
        frame_delays,
    })
}

fn encode_animation_gif(animation: &DecodedAnimation) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    {
        let mut encoder = GifEncoder::new(&mut bytes);
        encoder
            .set_repeat(Repeat::Infinite)
            .map_err(|err| format!("Could not configure animated preview cache: {err}"))?;
        for (frame, delay) in animation.frames.iter().zip(&animation.frame_delays) {
            let width = u32::try_from(frame.width)
                .map_err(|_| "Animated preview frame is too wide".to_owned())?;
            let height = u32::try_from(frame.height)
                .map_err(|_| "Animated preview frame is too tall".to_owned())?;
            let image = RgbaImage::from_raw(width, height, frame.rgba.clone())
                .ok_or_else(|| "Animated preview frame has invalid pixel data".to_owned())?;
            let delay_ms = u32::try_from(delay.as_millis()).unwrap_or(u32::MAX);
            encoder
                .encode_frame(Frame::from_parts(
                    image,
                    0,
                    0,
                    Delay::from_numer_denom_ms(delay_ms, 1),
                ))
                .map_err(|err| format!("Could not encode animated preview cache: {err}"))?;
        }
    }
    Ok(bytes)
}

fn wait_ffmpeg_timeout(
    child: &mut ffmpeg_sidecar::child::FfmpegChild,
    timeout: Duration,
    cancel: Option<&AtomicBool>,
) -> bool {
    let started = Instant::now();
    loop {
        match child.as_inner_mut().try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None)
                if started.elapsed() < timeout
                    && !cancel.is_some_and(|value| value.load(AtomicOrdering::Acquire)) =>
            {
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
    let cache_key = video_probe_cache_key(path);
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

    struct MockMediaBackend {
        result: Mutex<Result<DecodedAnimation, String>>,
    }

    impl MediaBackend for MockMediaBackend {
        fn load_animated_preview(
            &self,
            _path: &Path,
            _source_revision: u128,
            _source_size: u64,
            _source_kind: AnimatedSourceKind,
            _cancel: &AtomicBool,
        ) -> Result<DecodedAnimation, String> {
            self.result.lock().expect("mock backend mutex").clone()
        }
    }

    fn wait_for_preview_result(assets: &mut AssetManager, ctx: &Context) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !assets.pending.is_empty() && Instant::now() < deadline {
            assets.begin_frame();
            assets.poll_results(ctx);
            thread::yield_now();
        }
        assets.begin_frame();
        assets.poll_results(ctx);
        assert!(assets.pending.is_empty(), "mock preview job timed out");
    }

    fn icon_job(key: &str) -> AssetJob {
        AssetJob::SystemIcon {
            request_key: key.to_owned(),
            lookup_arg: "txt".to_owned(),
            icon_size: IconSize::Small,
            navigation_generation: 0,
        }
    }

    #[test]
    fn mock_media_backend_drives_ready_and_unavailable_states() {
        let ctx = Context::default();
        let entry = DirEntry::test_new("C:/media/mock.mp4");
        let ready_backend = Arc::new(MockMediaBackend {
            result: Mutex::new(Ok(DecodedAnimation {
                frames: vec![DecodedImage {
                    name: "mock-frame".to_owned(),
                    width: 2,
                    height: 2,
                    rgba: vec![255; 16],
                }],
                frame_delays: vec![VIDEO_PREVIEW_FRAME_DELAY],
            })),
        });
        let mut ready_assets = AssetManager::with_media_backend(ready_backend);
        ready_assets.begin_frame();
        assert!(matches!(
            ready_assets.request_hover_preview(&ctx, &entry, PreviewIntent::Hovered),
            HoverPreview::Pending
        ));
        ready_assets.end_frame();
        wait_for_preview_result(&mut ready_assets, &ctx);
        assert!(matches!(
            ready_assets.request_hover_preview(&ctx, &entry, PreviewIntent::Hovered),
            HoverPreview::Ready(_)
        ));

        let failed_backend = Arc::new(MockMediaBackend {
            result: Mutex::new(Err("FFmpeg executable was not found".to_owned())),
        });
        let mut failed_assets = AssetManager::with_media_backend(failed_backend);
        failed_assets.begin_frame();
        assert!(matches!(
            failed_assets.request_hover_preview(&ctx, &entry, PreviewIntent::Hovered),
            HoverPreview::Pending
        ));
        failed_assets.end_frame();
        wait_for_preview_result(&mut failed_assets, &ctx);
        assert!(matches!(
            failed_assets.request_hover_preview(&ctx, &entry, PreviewIntent::Hovered),
            HoverPreview::Unavailable {
                retry_after: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn active_directory_change_reprioritizes_existing_backlog() {
        let scheduler = JobScheduler::default();
        scheduler.set_active_directory(Some("old".to_owned()));
        scheduler.enqueue(
            icon_job("old"),
            AssetJobClass::VisibleThumbnail,
            Some("old".to_owned()),
        );
        scheduler.set_active_directory(Some("new".to_owned()));
        scheduler.enqueue(
            icon_job("new"),
            AssetJobClass::VisibleThumbnail,
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
            AssetJobClass::VisibleThumbnail,
            Some("active".to_owned()),
        );
        let AssetJob::SystemIcon { request_key, .. } = scheduler.recv() else {
            panic!("expected icon job");
        };
        assert_eq!(request_key, "active");
    }

    #[test]
    fn selected_previews_coexist_and_hover_wins() {
        let scheduler = JobScheduler::default();
        for index in 0..4 {
            assert!(
                scheduler
                    .enqueue(
                        icon_job(&format!("selected-{index}")),
                        AssetJobClass::SelectedPreview,
                        None,
                    )
                    .accepted
            );
        }
        assert!(
            scheduler
                .enqueue(icon_job("hovered"), AssetJobClass::HoveredPreview, None,)
                .accepted
        );
        let AssetJob::SystemIcon { request_key, .. } = scheduler.recv() else {
            panic!("expected icon job");
        };
        assert_eq!(request_key, "hovered");
        assert_eq!(
            scheduler.state.lock().expect("scheduler state").queue.len(),
            4
        );
    }

    #[test]
    fn hovered_preview_precedes_thumbnail_backlog() {
        let scheduler = JobScheduler::default();
        for index in 0..32 {
            scheduler.enqueue(
                icon_job(&format!("thumbnail-{index}")),
                AssetJobClass::VisibleThumbnail,
                Some("active".to_owned()),
            );
        }
        scheduler.enqueue(
            icon_job("hovered"),
            AssetJobClass::HoveredPreview,
            Some("active".to_owned()),
        );
        let AssetJob::SystemIcon { request_key, .. } = scheduler.recv() else {
            panic!("expected icon job");
        };
        assert_eq!(request_key, "hovered");
    }

    #[test]
    fn navigation_drops_stale_visible_work() {
        let scheduler = JobScheduler::default();
        scheduler.set_active_directory(Some("old".to_owned()));
        scheduler.enqueue(
            icon_job("old"),
            AssetJobClass::VisibleThumbnail,
            Some("old".to_owned()),
        );
        let evicted = scheduler.set_active_directory(Some("new".to_owned()));
        assert_eq!(evicted, vec!["old"]);
        assert!(
            scheduler
                .state
                .lock()
                .expect("scheduler state")
                .queue
                .is_empty()
        );
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
    fn animated_cache_key_tracks_revision_and_settings() {
        let path = Path::new("example.mp4");
        let first = animated_preview_cache_path(path, 1, 10);
        let second = animated_preview_cache_path(path, 2, 10);
        assert_ne!(first, second);
        assert!(first.to_string_lossy().contains("_anim_v4.gif"));
    }

    #[test]
    fn gif_preview_decodes_and_resamples_off_thread_format() {
        let mut bytes = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut bytes);
            encoder.set_repeat(Repeat::Infinite).expect("repeat");
            for index in 0..20 {
                let image =
                    image::RgbaImage::from_pixel(480, 100, image::Rgba([index, 40, 80, 255]));
                encoder
                    .encode_frame(Frame::from_parts(
                        image,
                        0,
                        0,
                        Delay::from_numer_denom_ms(100, 1),
                    ))
                    .expect("encode frame");
            }
        }
        let decoded = decode_gif_preview(&bytes, AnimatedSourceKind::Gif)
            .expect("animated GIF should decode");
        assert_eq!(decoded.frames.len(), VIDEO_PREVIEW_FRAMES as usize);
        assert_eq!(decoded.frame_delays.len(), decoded.frames.len());
        assert!(
            decoded
                .frames
                .iter()
                .all(|frame| { frame.width.max(frame.height) <= VIDEO_PREVIEW_MAX_EDGE as usize })
        );
        assert!(
            decoded
                .frame_delays
                .iter()
                .all(|delay| *delay == Duration::from_millis(100))
        );
    }

    #[test]
    fn direct_gif_cache_recovers_corruption_without_ffmpeg() {
        let unique = format!(
            "lwa_fm_preview_test_{}_{}.gif",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        );
        let source = std::env::temp_dir().join(unique);
        let animation = DecodedAnimation {
            frames: (0..3)
                .map(|index| DecodedImage {
                    name: format!("frame-{index}"),
                    width: 32,
                    height: 16,
                    rgba: image::RgbaImage::from_pixel(
                        32,
                        16,
                        image::Rgba([index * 40, 80, 160, 255]),
                    )
                    .into_raw(),
                })
                .collect(),
            frame_delays: vec![Duration::from_millis(80); 3],
        };
        fs::write(
            &source,
            encode_animation_gif(&animation).expect("encode source GIF"),
        )
        .expect("write source GIF");
        let metadata = fs::metadata(&source).expect("source metadata");
        let meta: crate::data::files::DirEntryMetaData = metadata.into();
        let cache_path = animated_preview_cache_path(&source, meta.source_revision, meta.size);
        let _ = fs::remove_file(&cache_path);
        let cancel = AtomicBool::new(false);

        let decoded = load_or_generate_animated_preview(
            &source,
            meta.source_revision,
            meta.size,
            AnimatedSourceKind::Gif,
            &cancel,
        )
        .expect("direct GIF should decode and cache");
        assert_eq!(decoded.frames.len(), 3);
        assert!(cache_path.exists());

        fs::write(&cache_path, b"truncated").expect("corrupt animation cache");
        let recovered = load_or_generate_animated_preview(
            &source,
            meta.source_revision,
            meta.size,
            AnimatedSourceKind::Gif,
            &cancel,
        )
        .expect("corrupt animation cache should regenerate from the GIF source");
        assert_eq!(recovered.frames.len(), 3);
        let cached = fs::read(&cache_path).expect("read regenerated animation cache");
        assert!(decode_gif_preview(&cached, AnimatedSourceKind::Gif).is_ok());

        let _ = fs::remove_file(source);
        let _ = fs::remove_file(cache_path);
    }

    #[test]
    fn permanent_preview_failure_is_not_reported_as_pending() {
        let mut assets = AssetManager::new();
        assets.failed.insert(
            "broken".to_owned(),
            FailureRecord {
                last_attempt: Instant::now(),
                tries: ICON_RETRY_MAX_TRIES,
                reason: "Unsupported codec".to_owned(),
            },
        );
        assert!(matches!(
            assets.preview_failure_state("broken"),
            Some(HoverPreview::Unavailable {
                retry_after: None,
                ..
            })
        ));
    }

    #[test]
    fn preview_failure_backoff_expires_and_allows_retry() {
        let mut assets = AssetManager::new();
        assets.failed.insert(
            "retryable".to_owned(),
            FailureRecord {
                last_attempt: Instant::now(),
                tries: 1,
                reason: "Temporary FFmpeg failure".to_owned(),
            },
        );
        assert!(matches!(
            assets.preview_failure_state("retryable"),
            Some(HoverPreview::Unavailable {
                retry_after: Some(_),
                ..
            })
        ));
        assets
            .failed
            .get_mut("retryable")
            .expect("failure record")
            .last_attempt = Instant::now()
            .checked_sub(Duration::from_secs(ICON_RETRY_BASE_SECS + 1))
            .expect("test duration must fit before now");
        assert!(assets.preview_failure_state("retryable").is_none());
    }

    #[test]
    fn invalidation_cancels_running_preview_without_losing_pending_state() {
        let mut assets = AssetManager::new();
        let source = PathBuf::from("C:/media/clip.mp4");
        let key = format!("{}#1:10#anim_v4", source.to_full_path_string());
        let cancel = Arc::new(AtomicBool::new(false));
        assets.pending.insert(key.clone());
        assets.preview_jobs.insert(
            key.clone(),
            PreviewJobControl {
                intent: PreviewIntent::Selected,
                cancel: Arc::clone(&cancel),
            },
        );

        assets.invalidate_files([source]);

        assert!(cancel.load(AtomicOrdering::Acquire));
        assert!(assets.pending.contains(&key));
        assert!(assets.preview_jobs.contains_key(&key));
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
        let workers = [(), ()].map(|()| {
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
        });
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
