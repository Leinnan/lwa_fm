use std::cmp::Reverse;
#[cfg(target_os = "macos")]
use std::collections::hash_map::DefaultHasher;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::fs;
#[cfg(target_os = "macos")]
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use egui::{ColorImage, Context, TextureHandle, TextureOptions};
#[cfg(test)]
use image::codecs::gif::{GifEncoder, Repeat};
#[cfg(test)]
use image::{Delay, Frame, RgbaImage};
use lru::LruCache;

use crate::data::files::DirEntry;
use crate::helper::PathHelper;

mod cache;
mod media;
mod process;
use cache::{atomic_save_image, thumbnail_cache_path};
#[cfg(test)]
use media::load_or_generate_animated_preview;
use media::{PreviewSpec, PreviewStep, SourceKey, VideoTask};
use process::{ErrorKind, MediaError};

const FOLDER_ICON_KEY: &str = "icon_folder";
const ICON_EXT_PREFIX: &str = "icon_";
const NO_EXT_ICON_KEY: &str = "icon_no_ext";
const TEXTURE_CAPACITY: usize = 512;
const ANIMATION_CAPACITY: usize = 64;
const MAX_STATIC_TEXTURE_BYTES: usize = 128 * 1024 * 1024;
const MAX_ANIMATION_BYTES: usize = 32 * 1024 * 1024;
const MAX_ANIMATION_GPU_BYTES: usize = 16 * 1024 * 1024;
const PREVIEW_DWELL: Duration = Duration::from_millis(250);
const VIDEO_PREVIEW_FRAMES: u32 = 12;
const VIDEO_PREVIEW_FRAME_DELAY: Duration = Duration::from_millis(250);
#[cfg(test)]
const VIDEO_PREVIEW_MAX_EDGE: u32 = 240;
// Bound texture preparation per UI frame. The renderer performs the GPU upload
// later; the elapsed budget here measures copying and enqueueing texture data.
const MAX_TEXTURES_PER_FRAME: usize = 4;
const MAX_STATIC_TEXTURES_PER_FRAME: usize = 3;
const MAX_RESULTS_PER_FRAME: usize = 64;
const MAX_ASSET_JOBS: usize = 512;
const MAX_RESULT_BYTES: usize = 16 * 1024 * 1024;
static QUEUE_WAIT_MICROS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

// Failure backoff: first retry after `ICON_RETRY_BASE_SECS`, doubling on each
// consecutive failure. Only invalid/unsupported media and fixed resource-limit failures
// become permanent after three attempts. Missing tools and timeouts can recover.
const ICON_RETRY_BASE_SECS: u64 = 30;
const ICON_RETRY_MAX_TRIES: u32 = 3;
const ICON_BACKOFF_SHIFT_CAP: u32 = 8;
const VIDEO_EXTS: &[&str] = &[
    "mp4", "mov", "mkv", "avi", "webm", "wmv", "flv", "m4v", "3gp", "ogv",
];

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
    rgba: Arc<[u8]>,
}

impl DecodedImage {
    fn byte_len(&self) -> usize {
        self.rgba.len()
    }
}

#[derive(Debug, Clone)]
struct DecodedAnimation {
    frames: Vec<DecodedImage>,
    frame_delays: Vec<Duration>,
    source_timestamps: Vec<Duration>,
}

impl DecodedAnimation {
    fn byte_len(&self) -> usize {
        self.frames.iter().map(DecodedImage::byte_len).sum()
    }
}

struct AnimatedPreview {
    frames: Vec<DecodedImage>,
    frame_delays: Vec<Duration>,
    started_at: Instant,
    last_used: Instant,
    byte_len: usize,
    texture: Option<TextureHandle>,
    texture_frame: Option<usize>,
    complete: bool,
}

impl AnimatedPreview {
    fn new(decoded: DecodedAnimation) -> Self {
        let now = Instant::now();
        let byte_len = decoded.byte_len();
        Self {
            frames: decoded.frames,
            frame_delays: decoded.frame_delays,
            started_at: now,
            last_used: now,
            byte_len,
            texture: None,
            texture_frame: None,
            complete: true,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    fn step_preview(
        &self,
        source: &SourceKey,
        spec: PreviewSpec,
        kind: AnimatedSourceKind,
        cancel: &AtomicBool,
        _task: &mut Option<Box<VideoTask>>,
    ) -> Result<PreviewStep, MediaError> {
        let _ = spec;
        self.load_animated_preview(&source.path, source.revision, source.size, kind, cancel)
            .map(PreviewStep::Complete)
            .map_err(|error| MediaError::new(ErrorKind::InvalidMedia, error))
    }
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
        media::load_or_generate_animated_preview(
            path,
            source_revision,
            source_size,
            source_kind,
            cancel,
        )
    }
    fn step_preview(
        &self,
        source: &SourceKey,
        spec: PreviewSpec,
        kind: AnimatedSourceKind,
        cancel: &AtomicBool,
        task: &mut Option<Box<VideoTask>>,
    ) -> Result<PreviewStep, MediaError> {
        task.get_or_insert_with(|| Box::new(VideoTask::new(source.clone(), spec, kind)))
            .step(cancel)
    }
}

#[derive(Debug)]
enum AssetJob {
    Thumbnail {
        source_path: PathBuf,
        request_key: String,
        kind: ThumbnailKind,
        icon_size: IconSize,
        source_revision: u128,
        source_size: u64,
        request_id: u64,
        target_edge: u32,
        cache_checked: bool,
        cancel: Arc<AtomicBool>,
    },
    SystemIcon {
        request_key: String,
        lookup_arg: String,
        icon_size: IconSize,
        request_id: u64,
        cancel: Arc<AtomicBool>,
    },
    AnimatedPreview {
        source_path: PathBuf,
        request_key: String,
        source_revision: u128,
        source_size: u64,
        source_kind: AnimatedSourceKind,
        request_id: u64,
        cancel: Arc<AtomicBool>,
        spec: PreviewSpec,
        task: Option<Box<VideoTask>>,
    },
}

impl AssetJob {
    const fn request_id(&self) -> u64 {
        match self {
            Self::Thumbnail { request_id, .. }
            | Self::SystemIcon { request_id, .. }
            | Self::AnimatedPreview { request_id, .. } => *request_id,
        }
    }
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
    #[cfg_attr(not(test), allow(dead_code))]
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
    closed: bool,
    running: HashMap<(String, u64), AssetJobClass>,
    classes: HashMap<String, (u64, AssetJobClass)>,
    priority_rebuilds: u64,
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
            if state.closed {
                result.evicted.push(job.request_key().to_owned());
                return result;
            }
            let order = state.next_order;
            state.next_order = state.next_order.saturating_add(1);
            let rank = job_rank(
                class,
                directory.as_deref(),
                state.active_directory.as_deref(),
            );
            if state.queue.len() + state.running.len() >= MAX_ASSET_JOBS {
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
                    state.classes.remove(&worst_key);
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
                } else {
                    result.evicted.push(job.request_key().to_owned());
                    return result;
                }
            }
            state
                .classes
                .insert(job.request_key().to_owned(), (job.request_id(), class));
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
        let evicted = Vec::new();
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
            drop(state);
        }
        if changed {
            self.has_jobs.notify_all();
        }
        evicted
    }

    fn reprioritize(&self, request_key: &str, class: AssetJobClass) {
        let mut state = self.state.lock().expect("job scheduler mutex poisoned");
        let Some((_, previous)) = state.classes.get_mut(request_key) else {
            return;
        };
        if *previous == class {
            return;
        }
        *previous = class;
        state.priority_rebuilds += 1;
        for ((key, _), running_class) in &mut state.running {
            if key == request_key {
                *running_class = class;
            }
        }
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
        for key in request_keys {
            state.classes.remove(key);
        }
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

    fn recv_entry(&self) -> Option<HeapEntry> {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::assets::scheduler::recv");
        let mut state = self.state.lock().expect("job scheduler mutex poisoned");
        let entry = loop {
            if state.closed {
                return None;
            }
            if let Some(entry) = state.queue.pop() {
                break entry;
            }
            state = self
                .has_jobs
                .wait(state)
                .expect("job scheduler mutex poisoned while waiting");
        };
        state.running.insert(
            (entry.job.request_key().to_owned(), entry.job.request_id()),
            entry.class,
        );
        drop(state);
        QUEUE_WAIT_MICROS.fetch_add(
            entry.enqueued_at.elapsed().as_micros() as u64,
            AtomicOrdering::Relaxed,
        );
        #[cfg(not(feature = "profiling"))]
        let _queue_delay = entry.enqueued_at.elapsed();
        #[cfg(feature = "profiling")]
        puffin::profile_scope!(
            "lwa_fm::assets::scheduler::queue_delay",
            &format!("{} us", entry.enqueued_at.elapsed().as_micros())
        );
        Some(entry)
    }
    fn finish(&self, identity: &(String, u64)) -> Option<AssetJobClass> {
        let mut state = self.state.lock().expect("scheduler state");
        if state
            .classes
            .get(&identity.0)
            .is_some_and(|(id, _)| *id == identity.1)
        {
            state.classes.remove(&identity.0);
        }
        state.running.remove(identity)
    }
    fn transfer(&self, mut entry: HeapEntry) -> bool {
        let mut state = self.state.lock().expect("scheduler transfer");
        if state.closed || state.queue.len() + state.running.len() >= MAX_ASSET_JOBS {
            return false;
        }
        entry.priority = JobPriority(
            job_rank(
                entry.class,
                entry.directory.as_deref(),
                state.active_directory.as_deref(),
            ),
            state.next_order,
        );
        state.next_order = state.next_order.wrapping_add(1);
        entry.enqueued_at = Instant::now();
        state.classes.insert(
            entry.job.request_key().to_owned(),
            (entry.job.request_id(), entry.class),
        );
        state.queue.push(entry);
        drop(state);
        self.has_jobs.notify_one();
        true
    }
    fn resume(&self, mut entry: HeapEntry) {
        let identity = (entry.job.request_key().to_owned(), entry.job.request_id());
        let mut state = self.state.lock().expect("scheduler state");
        entry.class = state.running.remove(&identity).unwrap_or(entry.class);
        if state.closed {
            return;
        }
        entry.priority.0 = job_rank(
            entry.class,
            entry.directory.as_deref(),
            state.active_directory.as_deref(),
        );
        if matches!(&entry.job, AssetJob::AnimatedPreview { task: Some(task), .. } if task.is_refinement())
        {
            entry.priority.0 = 4; // Initial feedback stays urgent; refinement follows visible posters.
        }
        entry.priority.1 = state.next_order;
        state.next_order = state.next_order.wrapping_add(1);
        entry.enqueued_at = Instant::now();
        // Running continuations reserve their queue slot while a sample is decoded.
        state.queue.push(entry);
        drop(state);
        self.has_jobs.notify_one();
    }
    #[cfg(test)]
    fn recv(&self) -> AssetJob {
        self.recv_entry().expect("open scheduler").job
    }
    fn shutdown(&self) {
        let mut state = self.state.lock().expect("scheduler state");
        state.closed = true;
        state.queue.clear();
        state.classes.clear();
        drop(state);
        self.has_jobs.notify_all();
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
        (AssetJobClass::PrefetchThumbnail, true) => 5,
        (AssetJobClass::PrefetchThumbnail, false) => 6,
        (AssetJobClass::SidebarIcon, _) => 7,
    }
}

#[derive(Debug)]
enum AssetJobResult {
    Ready {
        request_key: String,
        image: DecodedImage,
        request_id: u64,
    },
    Failed {
        request_key: String,
        reason: String,
        kind: ErrorKind,
        request_id: u64,
    },
    AnimationReady {
        request_key: String,
        animation: DecodedAnimation,
        request_id: u64,
    },
    Cancelled {
        request_key: String,
        request_id: u64,
    },
    AnimationProgress {
        request_key: String,
        request_id: u64,
        animation: DecodedAnimation,
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
    kind: ErrorKind,
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
struct RequestControl {
    id: u64,
    source: Option<SourceKey>,
    cancel: Arc<AtomicBool>,
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
    result_bytes: Arc<AtomicUsize>,
    stale_results: u64,
    cancelled_requests: u64,
    total_uploads: u64,
    backend_epoch: u64,
    icon_size: IconSize,
    per_dir_icon_size: HashMap<String, IconSize>,
    repaint_ctx: Arc<Mutex<Option<Context>>>,
    active_directory: Option<String>,
    request_id: u64,
    requests: HashMap<String, RequestControl>,
    desired_assets: HashSet<String>,
    dwell: HashMap<String, Instant>,
    selected_previews_enabled: bool,
    selected_preview_key: Option<String>,
    workers: Vec<thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
    frame_uploads: usize,
    upload_time: Duration,
    last_asset_activity: Instant,
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

    #[allow(
        clippy::needless_pass_by_value,
        reason = "owned backend is shared with worker threads"
    )]
    fn with_media_backend(media_backend: Arc<dyn MediaBackend>) -> Self {
        let (result_tx, result_rx) = mpsc::sync_channel::<AssetJobResult>(4);
        let scheduler = Arc::new(JobScheduler::default());
        let preview_scheduler = Arc::new(JobScheduler::default());
        // One priority queue makes the video budget global and yields between samples.
        let video_thumbnail_scheduler = Arc::clone(&preview_scheduler);
        let repaint_ctx = Arc::new(Mutex::new(None::<Context>));
        let shutdown = Arc::new(AtomicBool::new(false));
        let result_bytes = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::new();
        let worker_count = std::thread::available_parallelism()
            .map_or(2, std::num::NonZero::get)
            .saturating_sub(1)
            .clamp(1, 4);
        for worker_scheduler in std::iter::repeat_n(Arc::clone(&scheduler), worker_count)
            .chain(std::iter::once(Arc::clone(&preview_scheduler)))
        {
            let video_queue = Arc::clone(&preview_scheduler);
            let tx = result_tx.clone();
            let repaint = Arc::clone(&repaint_ctx);
            let backend = Arc::clone(&media_backend);
            let stop = Arc::clone(&shutdown);
            let bytes = Arc::clone(&result_bytes);
            workers.push(thread::spawn(move || {
                asset_worker(
                    &worker_scheduler,
                    &video_queue,
                    &tx,
                    &repaint,
                    backend.as_ref(),
                    &stop,
                    &bytes,
                );
            }));
        }
        #[cfg(not(test))]
        media::prepare_backend_async();

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
            result_bytes,
            stale_results: 0,
            cancelled_requests: 0,
            total_uploads: 0,
            backend_epoch: media::BACKEND_EPOCH.load(AtomicOrdering::Acquire),
            icon_size: IconSize::default(),
            per_dir_icon_size: HashMap::new(),
            repaint_ctx,
            active_directory: None,
            request_id: 0,
            frame_uploads: 0,
            upload_time: Duration::ZERO,
            requests: HashMap::new(),
            desired_assets: HashSet::new(),
            dwell: HashMap::new(),
            selected_previews_enabled: false,
            selected_preview_key: None,
            workers,
            shutdown,
            last_asset_activity: Instant::now(),
        }
    }

    pub fn begin_frame(&mut self) {
        #[cfg(not(test))]
        media::check_backend_async();
        let epoch = media::BACKEND_EPOCH.load(AtomicOrdering::Acquire);
        if epoch != self.backend_epoch {
            self.backend_epoch = epoch;
            let keys = self.requests.keys().cloned().collect();
            self.cancel_requests(&keys);
            self.textures.clear();
            self.animations.clear();
            self.texture_bytes = 0;
            self.animation_bytes = 0;
            self.failed.clear();
        }
        self.desired_previews.clear();
        self.frame_uploads = 0;
        self.upload_time = Duration::ZERO;
        self.desired_assets.clear();
        self.selected_preview_key = None;
    }

    pub fn end_frame(&mut self) {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!(
            "lwa_fm::assets::metrics",
            &format!(
                "processes={} samples={} disk_hits={} stale={} cancelled={} uploads={} result_bytes={} static_bytes={} animation_bytes={} queue_us={} process_us={} permit_us={} pipe_bytes={} writes_dropped={} writes_coalesced={} writes_failed={}",
                process::PROCESS_COUNT.load(AtomicOrdering::Relaxed),
                media::SAMPLES.load(AtomicOrdering::Relaxed),
                media::CACHE_HITS.load(AtomicOrdering::Relaxed),
                self.stale_results,
                self.cancelled_requests,
                self.total_uploads,
                self.result_bytes.load(AtomicOrdering::Relaxed),
                self.texture_bytes,
                self.animation_bytes,
                QUEUE_WAIT_MICROS.load(AtomicOrdering::Relaxed),
                process::PROCESS_MICROS.load(AtomicOrdering::Relaxed),
                process::PERMIT_WAIT_MICROS.load(AtomicOrdering::Relaxed),
                process::OUTPUT_BYTES.load(AtomicOrdering::Relaxed),
                media::WRITES_DROPPED.load(AtomicOrdering::Relaxed),
                media::WRITES_COALESCED.load(AtomicOrdering::Relaxed),
                media::WRITES_FAILED.load(AtomicOrdering::Relaxed)
            )
        );
        let obsolete: HashSet<_> = self
            .requests
            .keys()
            .filter(|key| !self.desired_assets.contains(*key))
            .cloned()
            .collect();
        self.cancel_requests(&obsolete);
        self.dwell
            .retain(|key, _| self.desired_previews.contains_key(key));
        for (_, animation) in &mut self.animations {
            if animation.last_used.elapsed() > Duration::from_secs(2) {
                animation.texture = None;
                animation.texture_frame = None;
            }
        }
        if self.pending.is_empty() && self.last_asset_activity.elapsed() >= Duration::from_secs(2) {
            cache::maybe_maintain();
        }
    }

    fn cancel_requests(&mut self, keys: &HashSet<String>) {
        for key in keys {
            if let Some(control) = self.requests.remove(key) {
                control.cancel.store(true, AtomicOrdering::Release);
                self.cancelled_requests += 1;
            }
            if let Some(control) = self.preview_jobs.remove(key) {
                control.cancel.store(true, AtomicOrdering::Release);
            }
            self.pending.remove(key);
        }
        self.scheduler.cancel(keys);
        self.preview_scheduler.cancel(keys);
    }
    fn start_request(&mut self, key: &str, source: Option<SourceKey>) -> (u64, Arc<AtomicBool>) {
        self.request_id = self.request_id.wrapping_add(1);
        let cancel = Arc::new(AtomicBool::new(false));
        self.requests.insert(
            key.to_owned(),
            RequestControl {
                id: self.request_id,
                source,
                cancel: Arc::clone(&cancel),
            },
        );
        (self.request_id, cancel)
    }
    pub const fn set_selected_previews_enabled(&mut self, enabled: bool) {
        self.selected_previews_enabled = enabled;
    }
    pub const fn selected_previews_enabled(&self) -> bool {
        self.selected_previews_enabled
    }

    pub fn set_active_directory(&mut self, path: Option<&Path>) {
        let directory = path.map(|path| path.to_full_path_string());
        if self.active_directory != directory {
            self.active_directory.clone_from(&directory);
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
        if let Ok(mut repaint) = self.repaint_ctx.lock()
            && repaint.is_none()
        {
            *repaint = Some(ctx.clone());
        }
        let mut uploads = 0;
        for _ in 0..MAX_RESULTS_PER_FRAME {
            if uploads >= MAX_STATIC_TEXTURES_PER_FRAME
                || (self.frame_uploads > 0 && self.upload_time >= Duration::from_millis(2))
            {
                ctx.request_repaint();
                break;
            }
            let result = match self.receiver.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            };
            self.result_bytes
                .fetch_update(AtomicOrdering::AcqRel, AtomicOrdering::Acquire, |bytes| {
                    Some(bytes.saturating_sub(result.byte_len()))
                })
                .expect("release result bytes");
            let (key, id) = result.identity();
            let Some(control) = self.requests.get(key) else {
                self.stale_results += 1;
                continue;
            };
            if control.id != id {
                self.stale_results += 1;
                continue;
            }
            if control.cancel.load(AtomicOrdering::Acquire) {
                let keys = HashSet::from([key.to_owned()]);
                self.cancel_requests(&keys);
                continue;
            }
            if !matches!(&result, AssetJobResult::AnimationProgress { .. }) {
                self.requests.remove(key);
                self.pending.remove(key);
                self.preview_jobs.remove(key);
            }
            match result {
                AssetJobResult::Ready {
                    request_key, image, ..
                } => {
                    self.failed.remove(&request_key);
                    let texture = self.upload_static_texture(ctx, &image);
                    uploads += 1;
                    self.put_texture(request_key, texture);
                }
                AssetJobResult::Failed {
                    request_key,
                    reason,
                    kind,
                    ..
                } => {
                    self.failed
                        .entry(request_key)
                        .and_modify(|record| {
                            record.last_attempt = Instant::now();
                            record.tries = record.tries.saturating_add(1);
                            record.reason.clone_from(&reason);
                            record.kind = kind;
                        })
                        .or_insert_with(|| FailureRecord {
                            last_attempt: Instant::now(),
                            tries: 1,
                            reason,
                            kind,
                        });
                }
                AssetJobResult::AnimationReady {
                    request_key,
                    animation,
                    ..
                } => {
                    self.failed.remove(&request_key);
                    self.put_animation(request_key, AnimatedPreview::new(animation));
                }
                AssetJobResult::AnimationProgress {
                    request_key,
                    animation,
                    ..
                } => {
                    self.failed.remove(&request_key);
                    let mut preview = AnimatedPreview::new(animation);
                    preview.complete = false;
                    self.put_animation(request_key, preview);
                }
                AssetJobResult::Cancelled { .. } => {}
            }
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
        if let Some((_, previous)) = self.textures.push(key, texture) {
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

    fn upload_static_texture(&mut self, ctx: &Context, image: &DecodedImage) -> TextureHandle {
        let started = Instant::now();
        let texture = ctx.load_texture(
            image.name.clone(),
            ColorImage::from_rgba_unmultiplied([image.width, image.height], &image.rgba),
            TextureOptions::LINEAR,
        );
        self.upload_time += started.elapsed();
        self.frame_uploads += 1;
        self.total_uploads += 1;
        texture
    }

    fn put_animation(&mut self, key: String, animation: AnimatedPreview) {
        let bytes = animation.byte_len;
        let mut animation = animation;
        if let Some(previous) = self.animations.peek_mut(&key) {
            animation.texture = previous.texture.take();
            animation.started_at = previous.started_at;
        }
        if let Some((_, previous)) = self.animations.push(key, animation) {
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
            // Cache identities contain resolution. Existing consumers retire at end_frame.
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

    pub fn request_entry_texture_for_size(
        &mut self,
        entry: &DirEntry,
        target: egui::Vec2,
        pixels_per_point: f32,
    ) -> Option<TextureHandle> {
        let size = self.effective_icon_size(entry);
        self.request_entry_texture_at_edge(
            entry,
            size,
            PreviewSpec::for_display(target.max_elem() * pixels_per_point).max_edge,
            AssetJobClass::VisibleThumbnail,
        )
    }
    pub fn prefetch_entry_texture_for_size(
        &mut self,
        entry: &DirEntry,
        target: egui::Vec2,
        pixels_per_point: f32,
    ) {
        let size = self.effective_icon_size(entry);
        let _ = self.request_entry_texture_at_edge(
            entry,
            size,
            PreviewSpec::for_display(target.max_elem() * pixels_per_point).max_edge,
            AssetJobClass::PrefetchThumbnail,
        );
    }
    fn request_entry_texture_at_size(
        &mut self,
        entry: &DirEntry,
        size: IconSize,
        class: AssetJobClass,
    ) -> Option<TextureHandle> {
        self.request_entry_texture_at_edge(entry, size, size.decode_px(), class)
    }
    fn request_entry_texture_at_edge(
        &mut self,
        entry: &DirEntry,
        size: IconSize,
        edge: u32,
        class: AssetJobClass,
    ) -> Option<TextureHandle> {
        let path = entry.get_path();
        if entry.is_file()
            && let Some(kind) = thumbnail_kind(&path)
        {
            let key = format!(
                "{}#{}:{}#thumb_px{}",
                entry.full_path_string(),
                entry.meta.source_revision,
                entry.meta.size,
                edge
            );
            self.desired_assets.insert(key.clone());
            if let Some(texture) = self.textures.get(&key) {
                return Some(texture.clone());
            }
            if self.pending.contains(&key) && class == AssetJobClass::VisibleThumbnail {
                self.scheduler.reprioritize(&key, class);
                if kind == ThumbnailKind::Video {
                    self.preview_scheduler.reprioritize(&key, class);
                }
            }
            if !self.is_pending_or_failed(&key) {
                let (request_id, cancel) = self.start_request(
                    &key,
                    Some(SourceKey::from_normalized(
                        &path,
                        entry.meta.source_revision,
                        entry.meta.size,
                    )),
                );
                let job = AssetJob::Thumbnail {
                    source_path: path.clone(),
                    request_key: key.clone(),
                    kind,
                    icon_size: size,
                    source_revision: entry.meta.source_revision,
                    source_size: entry.meta.size,
                    request_id,
                    target_edge: edge,
                    cache_checked: false,
                    cancel,
                };
                let enqueue = self
                    .scheduler
                    .enqueue(job, class, Some(entry.dir.to_string()));
                self.clear_evicted_requests(enqueue.evicted);
                if enqueue.accepted {
                    self.pending.insert(key);
                    self.last_asset_activity = Instant::now();
                }
            }
        }
        self.request_file_icon_texture(&path, entry.is_file(), size, class)
    }

    // Keep native location-icon requests available alongside the Lucide source list.
    #[allow(dead_code)]
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

        self.desired_assets.insert(cache_key.clone());
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
        let (request_id, cancel) = self.start_request(&cache_key, None);
        let enqueue = self.scheduler.enqueue(
            AssetJob::SystemIcon {
                request_key: cache_key.clone(),
                lookup_arg,
                icon_size: size,
                request_id,
                cancel,
            },
            AssetJobClass::SidebarIcon,
            path.parent()
                .map(|path| path.to_string_lossy().replace("\\\\?\\", "")),
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
        self.request_hover_preview_at_size(ctx, entry, intent, egui::Vec2::splat(300.0))
    }
    pub fn request_hover_preview_at_size(
        &mut self,
        ctx: &Context,
        entry: &DirEntry,
        intent: PreviewIntent,
        target: egui::Vec2,
    ) -> HoverPreview {
        if !entry.is_file() {
            return HoverPreview::Fallback;
        }
        let spec = PreviewSpec::for_display(target.max_elem() * ctx.pixels_per_point());
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
                self.request_entry_texture_at_edge(
                    entry,
                    IconSize::ExtraLarge,
                    spec.max_edge,
                    AssetJobClass::VisibleThumbnail,
                )
                .map_or(HoverPreview::Pending, HoverPreview::Ready)
            }
            "gif" => {
                self.request_animated_preview(ctx, entry, intent, AnimatedSourceKind::Gif, spec)
            }
            ext_str if VIDEO_EXTS.contains(&ext_str) => {
                self.request_animated_preview(ctx, entry, intent, AnimatedSourceKind::Video, spec)
            }
            _ => HoverPreview::Fallback,
        }
    }

    pub fn invalidate_files(&mut self, files: impl IntoIterator<Item = PathBuf>) {
        let files: Vec<PathBuf> = files
            .into_iter()
            .map(|path| PathBuf::from(crate::helper::normalize_path_string(&path)))
            .collect();
        if files.is_empty() {
            return;
        }

        media::invalidate_sources(&files, false);

        let file_prefixes: Vec<String> =
            files.iter().map(PathHelper::to_full_path_string).collect();
        let matches_file = |key: &str| -> bool {
            let entry_key = key.strip_prefix("sidebar:").unwrap_or(key);
            file_prefixes
                .iter()
                .any(|file| asset_key_matches_file(entry_key, file))
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
        let keys: HashSet<_> = self
            .requests
            .iter()
            .filter(|(key, control)| {
                control
                    .source
                    .as_ref()
                    .map_or_else(|| matches_file(key), |source| files.contains(&source.path))
            })
            .map(|(key, _)| key.clone())
            .chain(self.pending.iter().filter(|key| matches_file(key)).cloned())
            .collect();
        self.cancel_requests(&keys);
        self.failed.retain(|key, _| !matches_file(key));
    }

    pub fn invalidate_directories(&mut self, directories: impl IntoIterator<Item = PathBuf>) {
        let directories: Vec<PathBuf> = directories
            .into_iter()
            .map(|path| PathBuf::from(crate::helper::normalize_path_string(&path)))
            .collect();
        if directories.is_empty() {
            return;
        }

        media::invalidate_sources(&directories, true);

        let matches_dir = |key: &str| -> bool {
            let entry_path = source_path_from_asset_key(key)
                .unwrap_or_else(|| key.strip_prefix("sidebar:").unwrap_or(key));
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
        let keys: HashSet<_> = self
            .pending
            .iter()
            .filter(|key| matches_dir(key))
            .cloned()
            .collect();
        self.cancel_requests(&keys);
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
        spec: PreviewSpec,
    ) -> HoverPreview {
        if ctx.input(|input| input.viewport().focused == Some(false)) {
            return HoverPreview::Fallback;
        }
        let path = entry.get_path();
        let key = format!(
            "{}#{}:{}#overview_v7_px{}",
            entry.full_path_string(),
            entry.meta.source_revision,
            entry.meta.size,
            spec.max_edge
        );
        if intent == PreviewIntent::Selected {
            if !self.selected_previews_enabled
                || self
                    .selected_preview_key
                    .as_ref()
                    .is_some_and(|selected| selected != &key)
            {
                return HoverPreview::Fallback;
            }
            self.selected_preview_key = Some(key.clone());
        }
        self.desired_previews
            .entry(key.clone())
            .and_modify(|current| *current = (*current).max(intent))
            .or_insert(intent);
        self.desired_assets.insert(key.clone());
        let dwell = self
            .dwell
            .entry(key.clone())
            .or_insert_with(Instant::now)
            .elapsed();
        if dwell < PREVIEW_DWELL {
            ctx.request_repaint_after(PREVIEW_DWELL.saturating_sub(dwell));
            return HoverPreview::Pending;
        }
        let cached_frame = if self.animations.contains(&key) {
            Some(self.render_animated_preview(ctx, &key))
        } else {
            None
        };
        if self
            .animations
            .peek(&key)
            .is_some_and(|animation| animation.complete)
        {
            return cached_frame.unwrap_or(HoverPreview::Pending);
        }
        if let Some(state) = self.preview_failure_state(&key) {
            if let HoverPreview::Unavailable {
                retry_after: Some(delay),
                ..
            } = &state
            {
                ctx.request_repaint_after(*delay);
            }
            return cached_frame.unwrap_or(state);
        }
        if self.pending.contains(&key) {
            if let Some(control) = self.preview_jobs.get_mut(&key)
                && intent != control.intent
            {
                control.intent = intent;
                self.preview_scheduler
                    .reprioritize(&key, intent.job_class());
            }
            return cached_frame.unwrap_or(HoverPreview::Pending);
        }
        let (request_id, cancel) = self.start_request(
            &key,
            Some(SourceKey::from_normalized(
                &path,
                entry.meta.source_revision,
                entry.meta.size,
            )),
        );
        let enqueue = self.preview_scheduler.enqueue(
            AssetJob::AnimatedPreview {
                source_path: path.clone(),
                request_key: key.clone(),
                source_revision: entry.meta.source_revision,
                source_size: entry.meta.size,
                source_kind,
                request_id,
                cancel: Arc::clone(&cancel),
                spec,
                task: None,
            },
            intent.job_class(),
            path.parent()
                .map(|path| path.to_string_lossy().replace("\\\\?\\", "")),
        );
        self.clear_evicted_requests(enqueue.evicted);
        if enqueue.accepted {
            self.pending.insert(key.clone());
            self.preview_jobs
                .insert(key, PreviewJobControl { intent, cancel });
            self.last_asset_activity = Instant::now();
        }
        cached_frame.unwrap_or(HoverPreview::Pending)
    }

    fn render_animated_preview(&mut self, ctx: &Context, key: &str) -> HoverPreview {
        let can_upload = self.frame_uploads < MAX_TEXTURES_PER_FRAME
            && (self.frame_uploads == 0 || self.upload_time < Duration::from_millis(2));
        let gpu_bytes: usize = self
            .animations
            .iter()
            .filter_map(|(_, animation)| animation.texture.as_ref())
            .map(TextureHandle::byte_size)
            .sum();
        let Some(animation) = self.animations.get_mut(key) else {
            return HoverPreview::Pending;
        };
        let (index, next_frame) = animation.current_frame();
        if animation.frames.len() > 1 {
            ctx.request_repaint_after(next_frame);
        }
        let Some(frame) = animation.frames.get(index) else {
            return HoverPreview::Fallback;
        };
        if animation.texture_frame == Some(index) {
            return animation
                .texture
                .as_ref()
                .map_or(HoverPreview::Pending, |texture| {
                    HoverPreview::Ready(texture.clone())
                });
        }
        let old_bytes = animation
            .texture
            .as_ref()
            .map_or(0, TextureHandle::byte_size);
        if !can_upload
            || gpu_bytes.saturating_sub(old_bytes) + frame.byte_len() > MAX_ANIMATION_GPU_BYTES
        {
            ctx.request_repaint_after(Duration::from_millis(16));
            return animation
                .texture
                .as_ref()
                .map_or(HoverPreview::Pending, |texture| {
                    HoverPreview::Ready(texture.clone())
                });
        }
        let started = Instant::now();
        let image = ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.rgba);
        if let Some(texture) = animation.texture.as_mut() {
            texture.set(image, TextureOptions::LINEAR);
        } else {
            animation.texture = Some(ctx.load_texture(key, image, TextureOptions::LINEAR));
        }
        animation.texture_frame = Some(index);
        self.upload_time += started.elapsed();
        self.frame_uploads += 1;
        self.total_uploads += 1;
        HoverPreview::Ready(animation.texture.as_ref().expect("uploaded frame").clone())
    }

    fn preview_failure_state(&self, key: &str) -> Option<HoverPreview> {
        let record = self.failed.get(key)?;
        if record.tries >= ICON_RETRY_MAX_TRIES
            && matches!(
                record.kind,
                ErrorKind::InvalidMedia | ErrorKind::Unsupported | ErrorKind::Limit
            )
        {
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

        self.desired_assets.insert(key.clone());
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

        let (request_id, cancel) = self.start_request(&key, None);
        let enqueue = self.scheduler.enqueue(
            AssetJob::SystemIcon {
                request_key: key.clone(),
                lookup_arg,
                icon_size: size,
                request_id,
                cancel,
            },
            class,
            path.parent()
                .map(|path| path.to_string_lossy().replace("\\\\?\\", "")),
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
            if let Some(control) = self.requests.remove(&key) {
                control.cancel.store(true, AtomicOrdering::Release);
            }
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
            if record.tries >= ICON_RETRY_MAX_TRIES
                && matches!(
                    record.kind,
                    ErrorKind::InvalidMedia | ErrorKind::Unsupported | ErrorKind::Limit
                )
            {
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
    pub fn render_size_for(&self, entry: &DirEntry) -> f32 {
        self.effective_icon_size(entry).render_px()
    }
}

fn directory_icon_key(path: &Path) -> String {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        FOLDER_ICON_KEY.to_string()
    }
    #[cfg(target_os = "macos")]
    {
        let mut h = DefaultHasher::new();
        path.to_string_lossy().to_lowercase().hash(&mut h);
        let hash = h.finish();
        format!("{FOLDER_ICON_KEY}_{hash:016x}")
    }
}

enum JobStep {
    Transfer(AssetJob),
    Complete(AssetJobResult),
    Continue(AssetJob, Option<AssetJobResult>),
}

impl AssetJobResult {
    fn byte_len(&self) -> usize {
        match self {
            Self::Ready { image, .. } => image.byte_len(),
            Self::AnimationReady { animation, .. } | Self::AnimationProgress { animation, .. } => {
                animation.byte_len()
            }
            Self::Failed { reason, .. } => reason.len(),
            Self::Cancelled { .. } => 0,
        }
    }
    fn identity(&self) -> (&str, u64) {
        match self {
            Self::Ready {
                request_key,
                request_id,
                ..
            }
            | Self::Failed {
                request_key,
                request_id,
                ..
            }
            | Self::AnimationReady {
                request_key,
                request_id,
                ..
            }
            | Self::AnimationProgress {
                request_key,
                request_id,
                ..
            }
            | Self::Cancelled {
                request_key,
                request_id,
            } => (request_key, *request_id),
        }
    }
}

fn publish_result(
    tx: &mpsc::SyncSender<AssetJobResult>,
    repaint: &Mutex<Option<Context>>,
    stop: &AtomicBool,
    mut result: AssetJobResult,
    bytes: &AtomicUsize,
) -> bool {
    if result.byte_len() > MAX_RESULT_BYTES {
        let (key, request_id) = result.identity();
        result = AssetJobResult::Failed {
            request_key: key.to_owned(),
            request_id,
            kind: ErrorKind::Limit,
            reason: "Decoded asset exceeds result memory budget".into(),
        };
    }
    let size = result.byte_len();
    while bytes
        .fetch_update(
            AtomicOrdering::AcqRel,
            AtomicOrdering::Acquire,
            |retained| {
                (retained.saturating_add(size) <= MAX_RESULT_BYTES).then_some(retained + size)
            },
        )
        .is_err()
    {
        if stop.load(AtomicOrdering::Acquire) {
            return false;
        }
        thread::sleep(Duration::from_millis(5));
    }
    loop {
        if stop.load(AtomicOrdering::Acquire) {
            bytes.fetch_sub(size, AtomicOrdering::AcqRel);
            return false;
        }
        match tx.try_send(result) {
            Ok(()) => {
                if let Ok(ctx) = repaint.lock()
                    && let Some(ctx) = ctx.as_ref()
                {
                    ctx.request_repaint();
                }
                return true;
            }
            Err(mpsc::TrySendError::Full(value)) => {
                result = value;
                thread::sleep(Duration::from_millis(5));
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                bytes.fetch_sub(size, AtomicOrdering::AcqRel);
                return false;
            }
        }
    }
}

fn asset_worker(
    scheduler: &JobScheduler,
    video_scheduler: &JobScheduler,
    tx: &mpsc::SyncSender<AssetJobResult>,
    repaint: &Mutex<Option<Context>>,
    backend: &dyn MediaBackend,
    stop: &AtomicBool,
    bytes: &AtomicUsize,
) {
    while let Some(mut entry) = scheduler.recv_entry() {
        if stop.load(AtomicOrdering::Acquire) {
            return;
        }
        let identity = (entry.job.request_key().to_owned(), entry.job.request_id());
        match process_asset_job(entry.job, backend) {
            JobStep::Transfer(job) => {
                scheduler.finish(&identity);
                entry.job = job;
                if !video_scheduler.transfer(entry)
                    && !publish_result(
                        tx,
                        repaint,
                        stop,
                        AssetJobResult::Cancelled {
                            request_key: identity.0,
                            request_id: identity.1,
                        },
                        bytes,
                    )
                {
                    return;
                }
            }
            JobStep::Complete(result) => {
                #[cfg(not(test))]
                if let AssetJobResult::Failed { kind, .. } = &result {
                    media::recover_backend(*kind);
                }
                scheduler.finish(&identity);
                if !publish_result(tx, repaint, stop, result, bytes) {
                    return;
                }
            }
            JobStep::Continue(job, result) => {
                if let Some(result) = result
                    && !publish_result(tx, repaint, stop, result, bytes)
                {
                    return;
                }
                entry.job = job;
                scheduler.resume(entry);
            }
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "exhaustive dispatch for three asset job types"
)]
fn process_asset_job(job: AssetJob, media_backend: &dyn MediaBackend) -> JobStep {
    match job {
        AssetJob::Thumbnail {
            source_path,
            request_key,
            kind,
            icon_size,
            source_revision,
            source_size,
            request_id,
            target_edge,
            cache_checked,
            cancel,
        } => {
            if kind == ThumbnailKind::Video && !cache_checked {
                if cancel.load(AtomicOrdering::Acquire) {
                    return JobStep::Complete(AssetJobResult::Cancelled {
                        request_key,
                        request_id,
                    });
                }
                let source = SourceKey::new(&source_path, source_revision, source_size);
                if let Some(image) = media::cached_poster_image(&source, target_edge, &cancel) {
                    if source_revision != 0 && !source.unchanged() {
                        return JobStep::Complete(AssetJobResult::Cancelled {
                            request_key,
                            request_id,
                        });
                    }
                    return JobStep::Complete(AssetJobResult::Ready {
                        request_key,
                        request_id,
                        image,
                    });
                }
                return JobStep::Transfer(AssetJob::Thumbnail {
                    source_path,
                    request_key,
                    kind,
                    icon_size,
                    source_revision,
                    source_size,
                    request_id,
                    target_edge,
                    cache_checked: true,
                    cancel,
                });
            }
            let result = load_or_generate_thumbnail(
                &source_path,
                kind,
                icon_size,
                target_edge,
                source_revision,
                source_size,
                &cancel,
            );
            JobStep::Complete(
                if cancel.load(AtomicOrdering::Acquire)
                    || (source_revision != 0
                        && !SourceKey::new(&source_path, source_revision, source_size).unchanged())
                {
                    AssetJobResult::Cancelled {
                        request_key,
                        request_id,
                    }
                } else {
                    match result {
                        Ok(image) => AssetJobResult::Ready {
                            image,
                            request_key,
                            request_id,
                        },
                        Err(error) => AssetJobResult::Failed {
                            request_key,
                            reason: error.message,
                            kind: error.kind,
                            request_id,
                        },
                    }
                },
            )
        }
        AssetJob::SystemIcon {
            request_key,
            lookup_arg,
            icon_size,
            request_id,
            cancel,
        } => {
            if cancel.load(AtomicOrdering::Acquire) {
                return JobStep::Complete(AssetJobResult::Cancelled {
                    request_key,
                    request_id,
                });
            }
            JobStep::Complete(match load_system_icon_image(&lookup_arg, icon_size) {
                Some(image) => AssetJobResult::Ready {
                    image,
                    request_key,
                    request_id,
                },
                None => AssetJobResult::Failed {
                    request_key,
                    reason: format!("Could not load system icon for {lookup_arg}"),
                    kind: ErrorKind::InvalidMedia,
                    request_id,
                },
            })
        }
        AssetJob::AnimatedPreview {
            source_path,
            request_key,
            source_revision,
            source_size,
            source_kind,
            request_id,
            cancel,
            spec,
            mut task,
        } => {
            if cancel.load(AtomicOrdering::Acquire) {
                return JobStep::Complete(AssetJobResult::Cancelled {
                    request_key,
                    request_id,
                });
            }
            let source = SourceKey::new(&source_path, source_revision, source_size);
            let step = media_backend.step_preview(&source, spec, source_kind, &cancel, &mut task);
            if source_revision != 0 && !source.unchanged() {
                return JobStep::Complete(AssetJobResult::Cancelled {
                    request_key,
                    request_id,
                });
            }
            match step {
                Ok(PreviewStep::Complete(animation)) if !cancel.load(AtomicOrdering::Acquire) => {
                    JobStep::Complete(AssetJobResult::AnimationReady {
                        request_key,
                        request_id,
                        animation,
                    })
                }
                Ok(PreviewStep::Continue(progress)) if !cancel.load(AtomicOrdering::Acquire) => {
                    let result = progress.map(|animation| AssetJobResult::AnimationProgress {
                        request_key: request_key.clone(),
                        request_id,
                        animation,
                    });
                    JobStep::Continue(
                        AssetJob::AnimatedPreview {
                            source_path,
                            request_key,
                            source_revision,
                            source_size,
                            source_kind,
                            request_id,
                            cancel,
                            spec,
                            task,
                        },
                        result,
                    )
                }
                Ok(_) => JobStep::Complete(AssetJobResult::Cancelled {
                    request_key,
                    request_id,
                }),
                Err(_) if cancel.load(AtomicOrdering::Acquire) => {
                    JobStep::Complete(AssetJobResult::Cancelled {
                        request_key,
                        request_id,
                    })
                }
                Err(error) => JobStep::Complete(AssetJobResult::Failed {
                    request_key,
                    request_id,
                    reason: error.message,
                    kind: error.kind,
                }),
            }
        }
    }
}

fn asset_key_matches_file(key: &str, file: &str) -> bool {
    source_path_from_asset_key(key).unwrap_or(key) == file
}

fn source_path_from_asset_key(key: &str) -> Option<&str> {
    // Parse from the right: '#' and size-like suffixes are valid filename characters.
    let (identity, spec) = key.rsplit_once('#')?;
    let (path, revision) = identity.rsplit_once('#')?;
    let (revision, size) = revision.split_once(':')?;
    revision.parse::<u128>().ok()?;
    size.parse::<u64>().ok()?;
    if spec.starts_with("thumb_px") {
        Some(path)
    } else if spec.starts_with("px") {
        ["_xl", "_l", "_m", "_s"]
            .iter()
            .find_map(|suffix| path.strip_suffix(suffix))
    } else if spec.starts_with("overview_") || spec.starts_with("anim_v") {
        Some(path)
    } else {
        None
    }
}

impl Drop for AssetManager {
    fn drop(&mut self) {
        self.shutdown.store(true, AtomicOrdering::Release);
        for control in self.requests.values() {
            control.cancel.store(true, AtomicOrdering::Release);
        }
        self.scheduler.shutdown();
        self.preview_scheduler.shutdown();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
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
fn load_or_generate_thumbnail(
    source_path: &Path,
    kind: ThumbnailKind,
    icon_size: IconSize,
    target_edge: u32,
    source_revision: u128,
    source_size: u64,
    cancel: &AtomicBool,
) -> Result<DecodedImage, MediaError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    process::check(cancel, deadline)?;
    let path = thumbnail_cache_path(
        source_path,
        icon_size,
        target_edge,
        source_revision,
        source_size,
    );
    let source = SourceKey::new(source_path, source_revision, source_size);
    if kind == ThumbnailKind::Video {
        if let Some(image) = media::cached_poster_image(&source, target_edge, cancel) {
            media::schedule_poster_cache(source, path, target_edge, image.clone());
            return Ok(image);
        }
    } else if let Some(image) = decode_image_file(&path, source_path.to_full_path_string()) {
        cache::touch(&path);
        return Ok(image);
    }
    if path.exists() {
        let _ = fs::remove_file(&path);
    }
    let image = match kind {
        ThumbnailKind::Video => media::poster(&source, target_edge, cancel)?,
        ThumbnailKind::Image => {
            let mut reader = image::ImageReader::open(source_path)
                .map_err(|err| MediaError::new(ErrorKind::InvalidMedia, err.to_string()))?;
            let mut limits = image::Limits::default();
            limits.max_alloc = Some(128 * 1024 * 1024);
            reader.limits(limits);
            let image = reader
                .decode()
                .map_err(|err| MediaError::new(ErrorKind::InvalidMedia, err.to_string()))?
                .thumbnail(target_edge, target_edge);
            decoded_from_dynamic(&image, source_path.to_full_path_string())
        }
    };
    process::check(cancel, deadline)?;
    if kind == ThumbnailKind::Video {
        media::schedule_poster_cache(source, path, target_edge, image.clone());
    } else if source.unchanged()
        && let Some(rgba) =
            image::RgbaImage::from_raw(image.width as u32, image.height as u32, image.rgba.to_vec())
    {
        let _publication = media::PUBLICATION.lock().expect("asset publication");
        if source.unchanged() {
            atomic_save_image(&image::DynamicImage::ImageRgba8(rgba), &path);
        }
    }
    Ok(image)
}

fn decode_image_file(path: &Path, name: String) -> Option<DecodedImage> {
    let bytes = cache::read_bounded(path, 16 * 1024 * 1024).ok()?;
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(16 * 1024 * 1024);
    limits.max_image_width = Some(media::MAX_EDGE);
    limits.max_image_height = Some(media::MAX_EDGE);
    reader.limits(limits);
    Some(decoded_from_dynamic(&reader.decode().ok()?, name))
}

fn decoded_from_dynamic(image: &image::DynamicImage, name: String) -> DecodedImage {
    let rgba = image.to_rgba8();
    let width = rgba.width() as usize;
    let height = rgba.height() as usize;
    DecodedImage {
        name,
        width,
        height,
        rgba: rgba.into_raw().into(),
    }
}

#[allow(
    clippy::items_after_statements,
    reason = "test fixture bypass precedes platform icon extraction"
)]
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
        rgba: image.into_raw().into(),
    }
}

#[cfg(test)]
fn animated_preview_cache_path(path: &Path, source_revision: u128, source_size: u64) -> PathBuf {
    media::preview_cache_path(
        &SourceKey::new(path, source_revision, source_size),
        PreviewSpec::new(240),
        if path.extension().is_some_and(|ext| ext == "gif") {
            AnimatedSourceKind::Gif
        } else {
            AnimatedSourceKind::Video
        },
    )
}
#[cfg(test)]
fn decode_gif_preview(bytes: &[u8], _kind: AnimatedSourceKind) -> Result<DecodedAnimation, String> {
    media::decode_gif(bytes, PreviewSpec::new(240), &AtomicBool::new(false))
        .map_err(|error| error.to_string())
}
#[cfg(test)]
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
            let image = RgbaImage::from_raw(width, height, frame.rgba.to_vec())
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unchanged_priority_does_not_rebuild_the_queue() {
        let scheduler = JobScheduler::default();
        scheduler.enqueue(icon_job("pending"), AssetJobClass::PrefetchThumbnail, None);
        for _ in 0..100 {
            scheduler.reprioritize("pending", AssetJobClass::PrefetchThumbnail);
        }
        assert_eq!(
            scheduler
                .state
                .lock()
                .expect("asset fixture operation")
                .priority_rebuilds,
            0
        );
        scheduler.reprioritize("pending", AssetJobClass::VisibleThumbnail);
        for _ in 0..100 {
            scheduler.reprioritize("pending", AssetJobClass::VisibleThumbnail);
        }
        assert_eq!(
            scheduler
                .state
                .lock()
                .expect("asset fixture operation")
                .priority_rebuilds,
            1
        );
    }

    fn tiny_animation() -> DecodedAnimation {
        DecodedAnimation {
            frames: vec![DecodedImage {
                name: "frame".into(),
                width: 2,
                height: 2,
                rgba: vec![255; 16].into(),
            }],
            frame_delays: vec![VIDEO_PREVIEW_FRAME_DELAY],
            source_timestamps: vec![Duration::ZERO],
        }
    }

    #[test]
    fn invalidation_matches_the_complete_path_even_with_hash_characters() {
        assert!(asset_key_matches_file(
            "/media/clip#scene.mp4_m#1:20#px240",
            "/media/clip#scene.mp4"
        ));
        assert!(!asset_key_matches_file(
            "/media/clip.mp4#scene.mp4#1:20#overview_v7_px240",
            "/media/clip.mp4"
        ));
        assert!(!asset_key_matches_file(
            "/media/clip.mp4-extra_m#1:20#px240",
            "/media/clip.mp4"
        ));
    }

    #[test]
    fn continuation_reserves_capacity_and_applies_running_priority_changes() {
        let scheduler = JobScheduler::default();
        scheduler.enqueue(icon_job("running"), AssetJobClass::SelectedPreview, None);
        let running = scheduler.recv_entry().expect("running entry");
        for index in 0..MAX_ASSET_JOBS - 1 {
            assert!(
                scheduler
                    .enqueue(
                        icon_job(&index.to_string()),
                        AssetJobClass::VisibleThumbnail,
                        None
                    )
                    .accepted
            );
        }
        assert!(
            !scheduler
                .enqueue(icon_job("overflow"), AssetJobClass::PrefetchThumbnail, None)
                .accepted
        );
        scheduler.reprioritize("running", AssetJobClass::HoveredPreview);
        scheduler.resume(running);
        assert_eq!(
            scheduler.state.lock().expect("state").queue.len(),
            MAX_ASSET_JOBS
        );
        assert_eq!(scheduler.recv().request_key(), "running");
    }

    #[test]
    fn result_backpressure_honors_byte_budget_and_shutdown() {
        let (tx, rx) = mpsc::sync_channel(4);
        let bytes = Arc::new(AtomicUsize::new(MAX_RESULT_BYTES));
        let stop = Arc::new(AtomicBool::new(false));
        let retained = Arc::clone(&bytes);
        let signal = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            publish_result(
                &tx,
                &Mutex::new(None),
                &signal,
                AssetJobResult::AnimationReady {
                    request_key: "waiting".into(),
                    request_id: 1,
                    animation: tiny_animation(),
                },
                &retained,
            )
        });
        thread::sleep(Duration::from_millis(20));
        assert!(rx.try_recv().is_err());
        assert_eq!(bytes.load(AtomicOrdering::Acquire), MAX_RESULT_BYTES);
        stop.store(true, AtomicOrdering::Release);
        assert!(!worker.join().expect("backpressure worker"));
        assert_eq!(bytes.load(AtomicOrdering::Acquire), MAX_RESULT_BYTES);
    }

    #[test]
    fn dropping_manager_cancels_and_joins_running_workers() {
        struct WaitingBackend {
            started: AtomicBool,
        }
        impl MediaBackend for WaitingBackend {
            fn load_animated_preview(
                &self,
                _path: &Path,
                _revision: u128,
                _size: u64,
                _kind: AnimatedSourceKind,
                cancel: &AtomicBool,
            ) -> Result<DecodedAnimation, String> {
                self.started.store(true, AtomicOrdering::Release);
                let deadline = Instant::now() + Duration::from_secs(2);
                while !cancel.load(AtomicOrdering::Acquire) && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(1));
                }
                Err("cancelled".into())
            }
        }
        let backend = Arc::new(WaitingBackend {
            started: AtomicBool::new(false),
        });
        let mut assets = AssetManager::with_media_backend(backend.clone());
        let (request_id, cancel) = assets.start_request("running", None);
        assets.pending.insert("running".into());
        assets.preview_scheduler.enqueue(
            AssetJob::AnimatedPreview {
                source_path: PathBuf::from("mock.mp4"),
                request_key: "running".into(),
                source_revision: 0,
                source_size: 0,
                source_kind: AnimatedSourceKind::Video,
                request_id,
                cancel,
                spec: PreviewSpec::new(160),
                task: None,
            },
            AssetJobClass::HoveredPreview,
            None,
        );
        let deadline = Instant::now() + Duration::from_secs(1);
        while !backend.started.load(AtomicOrdering::Acquire) && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(backend.started.load(AtomicOrdering::Acquire));
        let started = Instant::now();
        drop(assets);
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn lru_accounting_matches_retained_textures_and_frames() {
        let ctx = Context::default();
        let mut assets = AssetManager::new();
        for index in 0..TEXTURE_CAPACITY + 3 {
            assets.put_texture(
                index.to_string(),
                ctx.load_texture(
                    index.to_string(),
                    ColorImage::new([2, 2], vec![egui::Color32::WHITE; 4]),
                    TextureOptions::LINEAR,
                ),
            );
        }
        assert_eq!(assets.texture_bytes, TEXTURE_CAPACITY * 16);
        assert_eq!(
            assets.texture_bytes,
            assets
                .textures
                .iter()
                .map(|(_, texture)| texture.byte_size())
                .sum::<usize>()
        );
        for index in 0..ANIMATION_CAPACITY + 3 {
            assets.put_animation(index.to_string(), AnimatedPreview::new(tiny_animation()));
        }
        assert_eq!(assets.animation_bytes, ANIMATION_CAPACITY * 16);
        assets.put_animation("replacement".into(), AnimatedPreview::new(tiny_animation()));
        assets.put_animation("replacement".into(), AnimatedPreview::new(tiny_animation()));
        assert_eq!(
            assets.animation_bytes,
            assets
                .animations
                .iter()
                .map(|(_, animation)| animation.byte_len)
                .sum::<usize>()
        );
    }

    #[test]
    fn stale_result_cannot_retire_a_newer_attempt() {
        let ctx = Context::default();
        let mut assets = AssetManager::new();
        let (tx, rx) = mpsc::sync_channel(4);
        assets.receiver = rx;
        let (old, _) = assets.start_request("same", None);
        assets.cancel_requests(&HashSet::from(["same".into()]));
        let (new, _) = assets.start_request("same", None);
        assets.pending.insert("same".into());
        tx.send(AssetJobResult::Cancelled {
            request_key: "same".into(),
            request_id: old,
        })
        .expect("old result");
        assets.poll_results(&ctx);
        assert!(assets.pending.contains("same"));
        assert_eq!(assets.requests["same"].id, new);
        tx.send(AssetJobResult::AnimationReady {
            request_key: "same".into(),
            request_id: new,
            animation: tiny_animation(),
        })
        .expect("new result");
        assets.poll_results(&ctx);
        assert!(!assets.pending.contains("same"));
        assert!(assets.animations.contains("same"));
    }

    #[test]
    fn source_change_during_generation_discards_the_result() {
        struct MutatingBackend;
        impl MediaBackend for MutatingBackend {
            fn load_animated_preview(
                &self,
                path: &Path,
                _revision: u128,
                _size: u64,
                _kind: AnimatedSourceKind,
                _cancel: &AtomicBool,
            ) -> Result<DecodedAnimation, String> {
                fs::write(path, b"changed source").expect("change source");
                Ok(tiny_animation())
            }
        }
        let path = std::env::temp_dir().join(format!("lwa_fm_mutating_{}.mp4", std::process::id()));
        fs::write(&path, b"old").expect("source fixture");
        let metadata: crate::data::files::DirEntryMetaData =
            fs::metadata(&path).expect("source metadata").into();
        let step = process_asset_job(
            AssetJob::AnimatedPreview {
                source_path: path.clone(),
                request_key: "changing".into(),
                source_revision: metadata.source_revision,
                source_size: metadata.size,
                source_kind: AnimatedSourceKind::Video,
                request_id: 7,
                cancel: Arc::new(AtomicBool::new(false)),
                spec: PreviewSpec::new(160),
                task: None,
            },
            &MutatingBackend,
        );
        assert!(matches!(
            step,
            JobStep::Complete(AssetJobResult::Cancelled { request_id: 7, .. })
        ));
        fs::remove_file(path).expect("clean source fixture");
    }

    #[test]
    fn brief_hover_starts_no_work_and_selected_playback_is_globally_limited() {
        let ctx = Context::default();
        let mut assets = AssetManager::new();
        let one = DirEntry::test_new("C:/media/one.mp4");
        let two = DirEntry::test_new("C:/media/two.mp4");
        assets.begin_frame();
        assert!(matches!(
            assets.request_hover_preview(&ctx, &one, PreviewIntent::Hovered),
            HoverPreview::Pending
        ));
        assert!(assets.pending.is_empty());
        assert!(assets.requests.is_empty());
        assets.end_frame();
        assets.begin_frame();
        assets.end_frame();
        assert!(assets.dwell.is_empty());
        assert!(matches!(
            assets.request_hover_preview(&ctx, &one, PreviewIntent::Selected),
            HoverPreview::Fallback
        ));
        assets.set_selected_previews_enabled(true);
        assert!(matches!(
            assets.request_hover_preview(&ctx, &one, PreviewIntent::Selected),
            HoverPreview::Pending
        ));
        assert!(matches!(
            assets.request_hover_preview(&ctx, &two, PreviewIntent::Selected),
            HoverPreview::Fallback
        ));
        assert!(matches!(
            assets.request_hover_preview(&ctx, &two, PreviewIntent::Hovered),
            HoverPreview::Pending
        ));
    }

    #[test]
    fn deferred_upload_retains_last_frame_and_reuses_texture() {
        let ctx = Context::default();
        let mut assets = AssetManager::new();
        let mut decoded = tiny_animation();
        decoded.frames.push(DecodedImage {
            name: "next".into(),
            width: 2,
            height: 2,
            rgba: vec![100; 16].into(),
        });
        decoded.frame_delays.push(VIDEO_PREVIEW_FRAME_DELAY);
        decoded.source_timestamps.push(Duration::from_secs(1));
        assets.put_animation("preview".into(), AnimatedPreview::new(decoded));
        let HoverPreview::Ready(first) = assets.render_animated_preview(&ctx, "preview") else {
            panic!("first frame");
        };
        assets
            .animations
            .peek_mut("preview")
            .expect("preview")
            .started_at = Instant::now()
            .checked_sub(Duration::from_millis(300))
            .expect("past");
        assets.frame_uploads = MAX_TEXTURES_PER_FRAME;
        let HoverPreview::Ready(deferred) = assets.render_animated_preview(&ctx, "preview") else {
            panic!("must retain visible frame");
        };
        assert_eq!(first.id(), deferred.id());
        assert_eq!(
            assets
                .animations
                .peek("preview")
                .expect("preview")
                .texture_frame,
            Some(0)
        );
        assets.begin_frame();
        let HoverPreview::Ready(next) = assets.render_animated_preview(&ctx, "preview") else {
            panic!("next frame");
        };
        assert_eq!(first.id(), next.id());
        assert_eq!(
            assets
                .animations
                .peek("preview")
                .expect("preview")
                .texture_frame,
            Some(1)
        );
        assets
            .animations
            .peek_mut("preview")
            .expect("preview")
            .last_used = Instant::now()
            .checked_sub(Duration::from_secs(3))
            .expect("past");
        assets.end_frame();
        assert!(
            assets
                .animations
                .peek("preview")
                .expect("preview")
                .texture
                .is_none()
        );
    }

    #[test]
    fn incomplete_preview_is_displayed_while_completion_resumes() {
        let ctx = Context::default();
        let entry = DirEntry::test_new("C:/media/partial.mp4");
        let backend = Arc::new(MockMediaBackend {
            result: Mutex::new(Ok(tiny_animation())),
        });
        let mut assets = AssetManager::with_media_backend(backend);
        let _ = assets.request_hover_preview(&ctx, &entry, PreviewIntent::Hovered);
        let key = assets.dwell.keys().next().expect("dwell key").clone();
        let mut preview = AnimatedPreview::new(tiny_animation());
        preview.complete = false;
        assets.put_animation(key.clone(), preview);
        *assets.dwell.get_mut(&key).expect("dwell") =
            Instant::now().checked_sub(PREVIEW_DWELL).expect("past");
        assert!(matches!(
            assets.request_hover_preview(&ctx, &entry, PreviewIntent::Hovered),
            HoverPreview::Ready(_)
        ));
        assert!(assets.pending.contains(&key));
        wait_for_preview_result(&mut assets, &ctx);
        assert!(
            assets
                .animations
                .peek(&key)
                .expect("completed preview")
                .complete
        );
    }

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
            request_id: 0,
            cancel: Arc::new(AtomicBool::new(false)),
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
                    rgba: vec![255; 16].into(),
                }],
                frame_delays: vec![VIDEO_PREVIEW_FRAME_DELAY],
                source_timestamps: vec![Duration::ZERO],
            })),
        });
        let mut ready_assets = AssetManager::with_media_backend(ready_backend);
        ready_assets.begin_frame();
        assert!(matches!(
            ready_assets.request_hover_preview(&ctx, &entry, PreviewIntent::Hovered),
            HoverPreview::Pending
        ));
        for time in ready_assets.dwell.values_mut() {
            *time = Instant::now()
                .checked_sub(PREVIEW_DWELL)
                .expect("dwell timestamp");
        }
        let _ = ready_assets.request_hover_preview(&ctx, &entry, PreviewIntent::Hovered);
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
        for time in failed_assets.dwell.values_mut() {
            *time = Instant::now()
                .checked_sub(PREVIEW_DWELL)
                .expect("dwell timestamp");
        }
        let _ = failed_assets.request_hover_preview(&ctx, &entry, PreviewIntent::Hovered);
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
    fn focus_change_preserves_other_visible_panes() {
        let scheduler = JobScheduler::default();
        scheduler.set_active_directory(Some("old".to_owned()));
        scheduler.enqueue(
            icon_job("old"),
            AssetJobClass::VisibleThumbnail,
            Some("old".to_owned()),
        );
        let evicted = scheduler.set_active_directory(Some("new".to_owned()));
        assert!(evicted.is_empty());
        assert_eq!(
            scheduler.state.lock().expect("scheduler state").queue.len(),
            1
        );
    }

    #[test]
    fn source_revision_changes_thumbnail_cache_key() {
        let path = Path::new("example.jpg");
        let first = thumbnail_cache_path(path, IconSize::Medium, 96, 1, 10);
        let second = thumbnail_cache_path(path, IconSize::Medium, 96, 2, 10);
        assert_ne!(first, second);
        assert!(first.to_string_lossy().contains("_v6.jpg"));
    }

    #[test]
    fn animated_cache_key_tracks_revision_and_settings() {
        let path = Path::new("example.mp4");
        let first = animated_preview_cache_path(path, 1, 10);
        let second = animated_preview_cache_path(path, 2, 10);
        assert_ne!(first, second);
        assert!(first.to_string_lossy().contains("_overview_v7.bin"));
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
        assert_eq!(
            decoded.frame_delays.iter().copied().sum::<Duration>(),
            Duration::from_secs(2)
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
                    .into_raw()
                    .into(),
                })
                .collect(),
            frame_delays: vec![Duration::from_millis(80); 3],
            source_timestamps: vec![
                Duration::ZERO,
                Duration::from_millis(80),
                Duration::from_millis(160),
            ],
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
        assert!(
            media::decode_bundle(
                &cached,
                &SourceKey::new(&source, meta.source_revision, meta.size),
                PreviewSpec::new(240),
                &cancel
            )
            .is_ok()
        );

        let _ = fs::remove_file(source);
        let _ = fs::remove_file(cache_path);
    }

    #[test]
    fn permanent_preview_failure_is_not_reported_as_pending() {
        let mut assets = AssetManager::new();
        for kind in [
            ErrorKind::InvalidMedia,
            ErrorKind::Unsupported,
            ErrorKind::Limit,
        ] {
            assets.failed.insert(
                "broken".to_owned(),
                FailureRecord {
                    last_attempt: Instant::now(),
                    tries: ICON_RETRY_MAX_TRIES,
                    reason: "Unsupported codec".to_owned(),
                    kind,
                },
            );
            assert!(matches!(
                assets.preview_failure_state("broken"),
                Some(HoverPreview::Unavailable {
                    retry_after: None,
                    ..
                })
            ));
            assert!(assets.is_pending_or_failed("broken"));
        }
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
                kind: ErrorKind::Timeout,
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
    fn invalidation_cancels_running_preview_and_retires_attempt() {
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
        assert!(!assets.pending.contains(&key));
        assert!(!assets.preview_jobs.contains_key(&key));
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
        let cache_path = thumbnail_cache_path(
            &source,
            IconSize::Small,
            64,
            meta.source_revision,
            meta.size,
        );
        let _ = fs::remove_file(&cache_path);

        let first = load_or_generate_thumbnail(
            &source,
            ThumbnailKind::Image,
            IconSize::Small,
            64,
            meta.source_revision,
            meta.size,
            &AtomicBool::new(false),
        )
        .expect("thumbnail miss should generate");
        assert_eq!(first.rgba[3], 0, "PNG alpha must be preserved");
        assert!(cache_path.exists());

        fs::write(&cache_path, b"truncated").expect("corrupt cache");
        let recovered = load_or_generate_thumbnail(
            &source,
            ThumbnailKind::Image,
            IconSize::Small,
            64,
            meta.source_revision,
            meta.size,
            &AtomicBool::new(false),
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
                    64,
                    meta.source_revision,
                    meta.size,
                    &AtomicBool::new(false),
                )
            })
        });
        assert!(workers.into_iter().all(|worker| {
            worker
                .join()
                .expect("thumbnail worker should not panic")
                .is_ok()
        }));
        assert!(decode_image_file(&cache_path, "cached".to_owned()).is_some());

        let _ = fs::remove_file(source);
        let _ = fs::remove_file(cache_path);
    }
}
