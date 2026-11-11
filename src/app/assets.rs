use std::cmp::Reverse;
use std::collections::BinaryHeap;
#[cfg(target_os = "macos")]
use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
#[cfg(test)]
use std::fs;
#[cfg(target_os = "macos")]
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use egui::{ColorImage, Context, TextureHandle, TextureOptions};
// Asset keys are hashed for every visible row on every frame; foldhash is
// far cheaper than SipHash for these long path strings.
use foldhash::{HashMap, HashMapExt, HashSet, HashSetExt};
#[cfg(test)]
use image::codecs::gif::{GifEncoder, Repeat};
#[cfg(test)]
use image::{Delay, Frame, RgbaImage};
use lru::LruCache;

use crate::app::thumbnail_choice;
use crate::app::thumbnail_timeline::{FrameTimeline, Gap};
#[cfg(test)]
use crate::data::files::DirEntry;
use crate::data::files::EntryRef;

use dirfleet_effects::media::cache::{self, thumbnail_cache_path};
#[cfg(test)]
use dirfleet_effects::media::decode;
#[cfg(not(all(test, windows)))]
use dirfleet_effects::media::decode::decoded_from_dynamic;
#[cfg(test)]
use dirfleet_effects::media::failures;
use dirfleet_effects::media::failures::{FailureRecord, Retry};
pub use dirfleet_effects::media::kinds;
#[cfg(feature = "profiling")]
use dirfleet_effects::media::process;
use dirfleet_effects::media::process::{ErrorKind, MediaError};
pub use dirfleet_effects::media::video::tool_paths as ffmpeg_tool_paths;
use dirfleet_effects::media::video::{
    self as media, PreviewSpec, PreviewStep, SourceKey, VideoTask,
};
use dirfleet_effects::media::{
    AnimatedSourceKind, DecodedAnimation, DecodedImage, THUMBNAIL_DEADLINE, ThumbnailKind,
    VIDEO_PREVIEW_FRAME_DELAY, cached_shell_poster, load_or_generate_thumbnail,
    shell_thumbnail_image, thumbnail_kind,
};
#[cfg(test)]
use dirfleet_effects::media::{VIDEO_PREVIEW_FRAMES, encode_animation_gif};

const FOLDER_ICON_KEY: &str = "icon_folder";
const ICON_EXT_PREFIX: &str = "icon_";
const NO_EXT_ICON_KEY: &str = "icon_no_ext";
const TEXTURE_CAPACITY: usize = 512;
const ANIMATION_CAPACITY: usize = 64;
const MAX_STATIC_TEXTURE_BYTES: usize = 128 * 1024 * 1024;
const MAX_ANIMATION_BYTES: usize = 32 * 1024 * 1024;
const MAX_ANIMATION_GPU_BYTES: usize = 16 * 1024 * 1024;
const PREVIEW_DWELL: Duration = Duration::from_millis(250);
#[cfg(test)]
const VIDEO_PREVIEW_MAX_EDGE: u32 = 240;
// Bound texture preparation per UI frame. The renderer performs the GPU upload
// later; the elapsed budget here measures copying and enqueueing texture data.
const MAX_TEXTURES_PER_FRAME: usize = 4;
/// Count cap per frame; the 2 ms upload time budget in `poll_results` is the
/// real limit, so a burst of small thumbnails no longer trickles in 3 per frame.
const MAX_STATIC_TEXTURES_PER_FRAME: usize = 24;
const MAX_RESULTS_PER_FRAME: usize = 64;
const MAX_ASSET_JOBS: usize = 512;
const MAX_RESULT_BYTES: usize = 16 * 1024 * 1024;
static QUEUE_WAIT_MICROS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Thumbnail edge for list rows: their 24-point icons at up to 2× scale.
const LIST_THUMBNAIL_EDGE: u32 = 64;
/// Size of the thumbnail picker's frames. Barely slower to extract than small
/// ones, keeps the large preview sharp and suits any thumbnail size once chosen.
const PICKER_EDGE: u32 = media::MAX_EDGE;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum IconSize {
    Small,
    #[default]
    Medium,
    Large,
    ExtraLarge,
}

impl IconSize {
    pub const ALL: [Self; 4] = [Self::Small, Self::Medium, Self::Large, Self::ExtraLarge];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Small => "Small",
            Self::Medium => "Medium",
            Self::Large => "Large",
            Self::ExtraLarge => "Extra large",
        }
    }

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

    pub const fn cache_suffix(self) -> &'static str {
        match self {
            Self::Small => "_s",
            Self::Medium => "_m",
            Self::Large => "_l",
            Self::ExtraLarge => "_xl",
        }
    }

    /// Narrowest a grid tile gets; tiles widen to fill the row.
    pub const fn tile_width(self) -> f32 {
        match self {
            Self::Small => 120.0,
            Self::Medium => 168.0,
            Self::Large => 220.0,
            Self::ExtraLarge => 330.0,
        }
    }

    /// Height of the preview area of a grid tile, above the name.
    pub const fn preview_height(self) -> f32 {
        match self {
            Self::Small => 60.0,
            Self::Medium => 102.0,
            Self::Large => 150.0,
            Self::ExtraLarge => 248.0,
        }
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

/// Makes animated previews one step at a time, so the worker can yield to
/// more urgent jobs between samples. Tests swap in simpler backends.
trait MediaBackend: Send + Sync {
    fn step_preview(
        &self,
        source: &SourceKey,
        spec: PreviewSpec,
        kind: AnimatedSourceKind,
        cancel: &AtomicBool,
        task: &mut Option<Box<VideoTask>>,
    ) -> Result<PreviewStep, MediaError>;
    /// The length and frame rate the thumbnail picker lays its frames out by.
    fn picker_timing(
        &self,
        source: &SourceKey,
        cancel: &AtomicBool,
    ) -> Result<VideoTiming, MediaError>;
    /// One frame for the thumbnail picker.
    fn picker_frame(
        &self,
        source: &SourceKey,
        at: Duration,
        cancel: &AtomicBool,
    ) -> Result<DecodedImage, MediaError>;
}

/// What the thumbnail picker needs to know about a video.
#[derive(Debug, Clone, Copy)]
struct VideoTiming {
    duration: Option<f64>,
    frame_rate: Option<f64>,
}

struct SystemMediaBackend;

impl MediaBackend for SystemMediaBackend {
    fn step_preview(
        &self,
        source: &SourceKey,
        spec: PreviewSpec,
        kind: AnimatedSourceKind,
        cancel: &AtomicBool,
        task: &mut Option<Box<VideoTask>>,
    ) -> Result<PreviewStep, MediaError> {
        let _access_time = crate::access_time::PreserveAccessTime::new(&source.path);
        task.get_or_insert_with(|| Box::new(VideoTask::new(source.clone(), spec, kind)))
            .step(cancel)
    }

    fn picker_timing(
        &self,
        source: &SourceKey,
        cancel: &AtomicBool,
    ) -> Result<VideoTiming, MediaError> {
        let _access_time = crate::access_time::PreserveAccessTime::new(&source.path);
        let info = media::probe(source, cancel, Instant::now() + THUMBNAIL_DEADLINE)?;
        Ok(VideoTiming {
            duration: info.duration,
            frame_rate: info.frame_rate,
        })
    }

    fn picker_frame(
        &self,
        source: &SourceKey,
        at: Duration,
        cancel: &AtomicBool,
    ) -> Result<DecodedImage, MediaError> {
        let _access_time = crate::access_time::PreserveAccessTime::new(&source.path);
        // Sources without a revision skip the disk cache, as everywhere else.
        let cache_path = (source.revision != 0).then(|| {
            cache::chosen_poster_cache_path(
                &source.path,
                PICKER_EDGE,
                source.revision,
                source.size,
                at.as_millis() as u64,
            )
        });
        media::chosen_poster(source, PICKER_EDGE, at, cache_path, cancel)
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
    /// `at` is the frame to extract, or `None` to look up the video's timing.
    Picker {
        source_path: PathBuf,
        request_key: String,
        source_revision: u128,
        source_size: u64,
        request_id: u64,
        cancel: Arc<AtomicBool>,
        at: Option<u64>,
    },
}

impl AssetJob {
    const fn request_id(&self) -> u64 {
        match self {
            Self::Thumbnail { request_id, .. }
            | Self::SystemIcon { request_id, .. }
            | Self::AnimatedPreview { request_id, .. }
            | Self::Picker { request_id, .. } => *request_id,
        }
    }
    fn request_key(&self) -> &str {
        match self {
            Self::Thumbnail { request_key, .. }
            | Self::SystemIcon { request_key, .. }
            | Self::AnimatedPreview { request_key, .. }
            | Self::Picker { request_key, .. } => request_key,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssetJobClass {
    VisibleThumbnail,
    PrefetchThumbnail,
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
            drop(state);
        }
        if changed {
            self.has_jobs.notify_all();
        }
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
            // Initial feedback stays urgent; refinement follows visible posters.
            entry.priority.0 = RANK_CONTINUING_PREVIEW;
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

/// Where a video overview resumes after each sample: behind visible posters
/// (so a long video can't starve the grid), ahead of prefetching.
const RANK_CONTINUING_PREVIEW: u8 = 4;

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
    /// Failures go to the picker's session, never to the backoff table.
    PickerTiming {
        request_key: String,
        request_id: u64,
        result: Result<VideoTiming, MediaError>,
    },
    PickerFrame {
        request_key: String,
        request_id: u64,
        at: u64,
        result: Result<DecodedImage, MediaError>,
    },
}

/// Why a preview is wanted; later variants win when several apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PreviewIntent {
    /// A selected grid tile. Only animates when the "animate selected
    /// previews" setting is on, and only one tile at a time.
    Selected,
    /// The inspector or Quick Look, where the user is looking at one item.
    /// Always animates.
    Focused,
    /// The pointer rests on the item.
    Hovered,
}

impl PreviewIntent {
    const fn job_class(self) -> AssetJobClass {
        match self {
            Self::Selected => AssetJobClass::SelectedPreview,
            Self::Focused | Self::Hovered => AssetJobClass::HoveredPreview,
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
    repaint_ctx: Arc<Mutex<Option<Context>>>,
    active_directory_source: Option<PathBuf>,
    request_id: u64,
    requests: HashMap<String, RequestControl>,
    desired_assets: HashSet<String>,
    dwell: HashMap<String, Instant>,
    selected_previews_enabled: bool,
    selected_preview_key: Option<String>,
    previews_paused: bool,
    workers: Vec<thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
    frame_uploads: usize,
    upload_time: Duration,
    last_asset_activity: Instant,
    /// The open thumbnail picker's frames and the work left on them.
    picker: Option<PickerSession>,
}

/// A frame offered as a video's thumbnail.
pub struct PickerFrame {
    pub texture: TextureHandle,
    /// Handed to the poster cache when this frame is chosen.
    image: DecodedImage,
}

#[derive(Debug, Clone, Copy)]
enum PickerJob {
    Timing,
    Frame(u64),
}

/// Extracts one frame at a time, so a frame the user just asked for only ever
/// waits for the one in progress.
struct PickerSession {
    path: PathBuf,
    /// Asset key up to the part naming the job.
    key_base: String,
    revision: u128,
    size: u64,
    current: Option<Duration>,
    timing_known: bool,
    error: Option<String>,
    running: Option<(String, PickerJob)>,
    timeline: FrameTimeline<PickerFrame>,
}

/// Memory a texture takes on the GPU (RGBA8).
fn texture_bytes(texture: &TextureHandle) -> usize {
    let [width, height] = texture.size();
    width.saturating_mul(height).saturating_mul(4)
}

/// How a directory is named for job ranking: one syscall-free canonical
/// form, so a job's folder and the active folder compare equal.
fn directory_key(path: &Path) -> String {
    crate::helper::canonical_string(path)
}

pub enum Preview {
    Ready(TextureHandle),
    Pending,
    Unavailable {
        reason: String,
        retry_after: Option<Duration>,
    },
    Fallback,
    /// Animation is paused and no frame is uploaded yet: show the poster.
    Paused,
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
        connect_media_service();
        let (result_tx, result_rx) = mpsc::sync_channel::<AssetJobResult>(32);
        let scheduler = Arc::new(JobScheduler::default());
        let preview_scheduler = Arc::new(JobScheduler::default());
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
            repaint_ctx,
            active_directory_source: None,
            request_id: 0,
            frame_uploads: 0,
            upload_time: Duration::ZERO,
            requests: HashMap::new(),
            desired_assets: HashSet::new(),
            dwell: HashMap::new(),
            selected_previews_enabled: false,
            selected_preview_key: None,
            previews_paused: false,
            workers,
            shutdown,
            last_asset_activity: Instant::now(),
            picker: None,
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
        let picker_job = self
            .picker
            .as_ref()
            .and_then(|session| session.running.as_ref())
            .map(|(key, _)| key);
        let obsolete: HashSet<_> = self
            .requests
            .keys()
            .filter(|key| !self.desired_assets.contains(*key) && picker_job != Some(*key))
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
    /// While paused, animated previews hold still and start no work.
    pub const fn set_previews_paused(&mut self, paused: bool) {
        self.previews_paused = paused;
    }

    pub fn set_active_directory(&mut self, path: Option<&Path>) {
        // Called every frame: skip the `canonicalize` syscall and the scheduler
        // locks unless the active tab's path actually changed.
        if self.active_directory_source.as_deref() == path {
            return;
        }
        self.active_directory_source = path.map(Path::to_path_buf);
        let directory = path.map(directory_key);
        // The video queue is the preview queue, so two schedulers in all.
        self.scheduler.set_active_directory(directory.clone());
        self.preview_scheduler.set_active_directory(directory);
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
                .try_update(AtomicOrdering::AcqRel, AtomicOrdering::Acquire, |bytes| {
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
                let keys: HashSet<String> = std::iter::once(key.to_owned()).collect();
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
                picker @ (AssetJobResult::PickerTiming { .. }
                | AssetJobResult::PickerFrame { .. }) => self.picker_job_result(ctx, picker),
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
        let bytes = texture_bytes(&texture);
        if let Some((_, previous)) = self.textures.push(key, texture) {
            self.texture_bytes = self.texture_bytes.saturating_sub(texture_bytes(&previous));
        }
        self.texture_bytes = self.texture_bytes.saturating_add(bytes);
        while self.texture_bytes > MAX_STATIC_TEXTURE_BYTES {
            let Some((_key, evicted)) = self.textures.pop_lru() else {
                break;
            };
            self.texture_bytes = self.texture_bytes.saturating_sub(texture_bytes(&evicted));
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

    /// A small thumbnail or icon for list rows and placeholders. Rows draw
    /// icons at most 24 points tall, so this never decodes big images.
    pub fn request_entry_texture(&mut self, entry: &EntryRef<'_>) -> Option<TextureHandle> {
        self.request_entry_texture_at_edge(
            entry,
            IconSize::Small,
            LIST_THUMBNAIL_EDGE,
            AssetJobClass::VisibleThumbnail,
        )
    }

    /// The entry's thumbnail at `target` size, or its type icon until the
    /// thumbnail is ready. The flag tells which: `true` for a thumbnail.
    pub fn request_entry_texture_for_size(
        &mut self,
        entry: &EntryRef<'_>,
        target: egui::Vec2,
        pixels_per_point: f32,
    ) -> Option<(TextureHandle, bool)> {
        let size = self.icon_size;
        self.entry_texture_at_edge(
            entry,
            size,
            PreviewSpec::for_display(target.max_elem() * pixels_per_point).max_edge,
            AssetJobClass::VisibleThumbnail,
        )
    }
    pub fn prefetch_entry_texture_for_size(
        &mut self,
        entry: &EntryRef<'_>,
        target: egui::Vec2,
        pixels_per_point: f32,
    ) {
        let size = self.icon_size;
        let _ = self.request_entry_texture_at_edge(
            entry,
            size,
            PreviewSpec::for_display(target.max_elem() * pixels_per_point).max_edge,
            AssetJobClass::PrefetchThumbnail,
        );
    }
    fn request_entry_texture_at_edge(
        &mut self,
        entry: &EntryRef<'_>,
        size: IconSize,
        edge: u32,
        class: AssetJobClass,
    ) -> Option<TextureHandle> {
        self.entry_texture_at_edge(entry, size, edge, class)
            .map(|(texture, _)| texture)
    }
    fn entry_texture_at_edge(
        &mut self,
        entry: &EntryRef<'_>,
        size: IconSize,
        edge: u32,
        class: AssetJobClass,
    ) -> Option<(TextureHandle, bool)> {
        let path = entry.get_path();
        if entry.is_file()
            && let Some(kind) = thumbnail_kind(&path)
        {
            let mut key = format!(
                "{}#{}:{}#thumb_px{}",
                entry.full_path_string(),
                entry.meta.source_revision,
                entry.meta.size,
                edge
            );
            // A chosen frame is a different thumbnail than the default one.
            if kind == ThumbnailKind::Video
                && let Some(at) =
                    thumbnail_choice::get(&path, entry.meta.source_revision, entry.meta.size)
            {
                let _ = write!(key, "_t{}", at.as_millis());
            }
            self.desired_assets.insert(key.clone());
            if let Some(texture) = self.textures.get(&key) {
                return Some((texture.clone(), true));
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
                let enqueue = self.scheduler.enqueue(
                    job,
                    class,
                    Some(directory_key(Path::new(entry.dir.as_ref()))),
                );
                self.clear_evicted_requests(enqueue.evicted);
                if enqueue.accepted {
                    self.pending.insert(key);
                    self.last_asset_activity = Instant::now();
                }
            }
        }
        // Packages are looked up by path, like folders: every app has its own icon.
        let by_extension = entry.is_file() && !entry.is_package();
        self.request_file_icon_texture(&path, by_extension, size, class)
            .map(|texture| (texture, false))
    }

    pub fn request_preview(
        &mut self,
        ctx: &Context,
        entry: &EntryRef<'_>,
        intent: PreviewIntent,
    ) -> Preview {
        self.request_preview_at_size(ctx, entry, intent, egui::Vec2::splat(300.0))
    }
    pub fn request_preview_at_size(
        &mut self,
        ctx: &Context,
        entry: &EntryRef<'_>,
        intent: PreviewIntent,
        target: egui::Vec2,
    ) -> Preview {
        if !entry.is_file() {
            return Preview::Fallback;
        }
        let spec = PreviewSpec::for_display(target.max_elem() * ctx.pixels_per_point());
        match kinds::media_kind(Path::new(entry.file_name)) {
            Some(kinds::MediaKind::Image) => self
                .request_entry_texture_at_edge(
                    entry,
                    IconSize::ExtraLarge,
                    spec.max_edge,
                    AssetJobClass::VisibleThumbnail,
                )
                .map_or(Preview::Pending, Preview::Ready),
            Some(kinds::MediaKind::AnimatedImage) => {
                self.request_animated_preview(ctx, entry, intent, AnimatedSourceKind::Gif, spec)
            }
            Some(kinds::MediaKind::Video) => {
                self.request_animated_preview(ctx, entry, intent, AnimatedSourceKind::Video, spec)
            }
            None => Preview::Fallback,
        }
    }

    /// Starts offering frames of `entry` as its thumbnail; `current` is the
    /// frame chosen so far. Call [`Self::picker_pump`] each frame until
    /// [`Self::picker_close`].
    pub fn picker_open(&mut self, entry: &EntryRef<'_>, current: Option<Duration>) {
        self.picker_close();
        self.picker = Some(PickerSession {
            path: entry.get_path(),
            key_base: format!(
                "{}#{}:{}",
                entry.full_path_string(),
                entry.meta.source_revision,
                entry.meta.size
            ),
            revision: entry.meta.source_revision,
            size: entry.meta.size,
            current,
            timing_known: false,
            error: None,
            running: None,
            timeline: FrameTimeline::default(),
        });
    }

    pub fn picker_close(&mut self) {
        if let Some((key, _)) = self.picker.take().and_then(|session| session.running) {
            self.cancel_requests(&std::iter::once(key).collect());
        }
    }

    /// Starts the next job of the picker unless one is still running.
    pub fn picker_pump(&mut self) {
        let Some(session) = self.picker.as_mut() else {
            return;
        };
        if let Some((key, job)) = &session.running {
            if self.requests.contains_key(key) {
                return;
            }
            // Cancelled, say because the file changed: this must not stall the rest.
            match *job {
                PickerJob::Timing => {
                    session.timing_known = true;
                    session.error = Some("The video could not be read.".to_owned());
                }
                PickerJob::Frame(at) => session.timeline.fail(at),
            }
            session.running = None;
        }
        let job = if session.timing_known {
            match session.timeline.next_job() {
                Some(at) => PickerJob::Frame(at),
                None => return,
            }
        } else {
            PickerJob::Timing
        };
        let (at, name) = match job {
            PickerJob::Timing => (None, "probe".to_owned()),
            PickerJob::Frame(at) => (Some(at), at.to_string()),
        };
        let key = format!("{}#pickerframe_{name}", session.key_base);
        let path = session.path.clone();
        let (revision, size) = (session.revision, session.size);
        session.running = Some((key.clone(), job));
        let (request_id, cancel) = self.start_request(
            &key,
            Some(SourceKey::from_normalized(&path, revision, size)),
        );
        let directory = path.parent().map(directory_key);
        let enqueue = self.preview_scheduler.enqueue(
            AssetJob::Picker {
                source_path: path,
                request_key: key.clone(),
                source_revision: revision,
                source_size: size,
                request_id,
                cancel,
                at,
            },
            AssetJobClass::HoveredPreview,
            directory,
        );
        self.clear_evicted_requests(enqueue.evicted);
        if enqueue.accepted {
            self.pending.insert(key);
            self.last_asset_activity = Instant::now();
        } else {
            self.requests.remove(&key);
        }
    }

    /// The frames of the open picker.
    pub fn picker_timeline(&self) -> Option<&FrameTimeline<PickerFrame>> {
        self.picker.as_ref().map(|session| &session.timeline)
    }

    /// Why the open picker has no frames to offer, if so.
    pub fn picker_error(&self) -> Option<&str> {
        self.picker.as_ref()?.error.as_deref()
    }

    /// Whether the open picker is still working out what it can offer.
    pub fn picker_waiting_for_timing(&self) -> bool {
        self.picker
            .as_ref()
            .is_some_and(|session| !session.timing_known)
    }

    /// Adds more frames across `gap` to the open picker.
    pub fn picker_fill_gap(&mut self, gap: Gap) {
        if let Some(session) = self.picker.as_mut() {
            session.timeline.fill(gap);
        }
    }

    /// Gives the open picker `ready` finished frames of a one-hour video, one
    /// frame in progress that never ends, and the rest queued.
    #[cfg(test)]
    pub fn picker_fill_for_test(&mut self, ctx: &Context, ready: usize) {
        // The harness has drawn a frame by now, which started the real lookup.
        if let Some((key, _)) = self.picker.as_mut().and_then(|s| s.running.take()) {
            self.cancel_requests(&std::iter::once(key).collect());
        }
        let session = self.picker.as_mut().expect("open picker");
        session.timing_known = true;
        session.error = None;
        session.timeline = FrameTimeline::default();
        session.timeline.start(
            Some(3600.0),
            Some(30.0),
            Duration::from_secs(180),
            session.current,
        );
        for index in 0..ready {
            let at = session.timeline.next_job().expect("frame");
            let shade = (32 + index * 16).min(255) as u8;
            let image = DecodedImage {
                name: format!("picker-{at}"),
                width: 16,
                height: 9,
                rgba: [shade, 96, 255 - shade, 255].repeat(16 * 9).into(),
            };
            let texture = ctx.load_texture(
                image.name.clone(),
                ColorImage::from_rgba_unmultiplied([16, 9], &image.rgba),
                TextureOptions::LINEAR,
            );
            let bytes = image.byte_len();
            session
                .timeline
                .finish(at, PickerFrame { texture, image }, bytes);
        }
        let at = session.timeline.next_job().expect("frame in progress");
        let key = format!("{}#pickerframe_{at}", session.key_base);
        session.running = Some((key.clone(), PickerJob::Frame(at)));
        self.start_request(&key, None);
    }

    fn picker_job_result(&mut self, ctx: &Context, result: AssetJobResult) {
        match result {
            AssetJobResult::PickerTiming {
                request_key,
                result,
                ..
            } => self.picker_timing_done(&request_key, result),
            AssetJobResult::PickerFrame {
                request_key,
                at,
                result,
                ..
            } => self.picker_frame_done(ctx, &request_key, at, result),
            _ => {}
        }
    }

    fn picker_job_done(&mut self, key: &str) -> Option<&mut PickerSession> {
        let session = self.picker.as_mut()?;
        let (running, _) = session.running.as_ref()?;
        if running != key {
            return None;
        }
        session.running = None;
        Some(session)
    }

    fn picker_timing_done(&mut self, key: &str, result: Result<VideoTiming, MediaError>) {
        let Some(session) = self.picker_job_done(key) else {
            return;
        };
        session.timing_known = true;
        match result {
            Ok(timing) => {
                let default_at = timing
                    .duration
                    .map_or(Duration::ZERO, |duration| media::sample_times(duration)[0]);
                session.timeline.start(
                    timing.duration,
                    timing.frame_rate,
                    default_at,
                    session.current,
                );
            }
            Err(error) => session.error = Some(error.message),
        }
    }

    fn picker_frame_done(
        &mut self,
        ctx: &Context,
        key: &str,
        at: u64,
        result: Result<DecodedImage, MediaError>,
    ) {
        if self.picker_job_done(key).is_none() {
            return;
        }
        let started = Instant::now();
        let frame = result.ok().map(|image| PickerFrame {
            texture: ctx.load_texture(
                key,
                ColorImage::from_rgba_unmultiplied([image.width, image.height], &image.rgba),
                TextureOptions::LINEAR,
            ),
            image,
        });
        if frame.is_some() {
            self.upload_time += started.elapsed();
            self.frame_uploads += 1;
            self.total_uploads += 1;
        }
        let Some(session) = self.picker.as_mut() else {
            return;
        };
        match frame {
            Some(frame) => {
                let bytes = frame.image.byte_len();
                session.timeline.finish(at, frame, bytes);
            }
            None => session.timeline.fail(at),
        }
    }

    /// The thumbnail frame of `path` changed to `at` (`None`: the default).
    /// Drops the old thumbnails, and hands the chosen frame from the open
    /// picker to the poster cache so it shows without extracting it again.
    pub fn thumbnail_choice_changed(
        &mut self,
        path: &Path,
        revision: u128,
        size: u64,
        at: Option<Duration>,
    ) {
        let chosen = at.and_then(|at| {
            let timeline = self.picker_timeline()?;
            let frame = timeline.frame(at.as_millis() as u64)?;
            Some((at, frame.image.clone()))
        });
        self.invalidate_files([path.to_path_buf()]);
        if let Some((at, frame)) = chosen {
            let source = SourceKey::new(path, revision, size);
            let edge = frame.width.max(frame.height) as u32;
            media::remember_chosen(&source, edge, at, &frame);
        }
    }

    pub fn invalidate_files(&mut self, files: impl IntoIterator<Item = PathBuf>) {
        let files: Vec<PathBuf> = files
            .into_iter()
            .map(|path| crate::helper::canonical_form(&path))
            .collect();
        if files.is_empty() {
            return;
        }
        media::invalidate_sources(&files, false);
        let file_strings: Vec<String> = files
            .iter()
            .map(|file| file.to_string_lossy().into_owned())
            .collect();
        self.forget(
            |key| {
                file_strings
                    .iter()
                    .any(|file| asset_key_matches_file(key, file))
            },
            |source| files.iter().any(|file| file == source),
        );
    }

    pub fn invalidate_directories(&mut self, directories: impl IntoIterator<Item = PathBuf>) {
        let directories: Vec<PathBuf> = directories
            .into_iter()
            .map(|path| crate::helper::canonical_form(&path))
            .collect();
        if directories.is_empty() {
            return;
        }
        media::invalidate_sources(&directories, true);
        // Keys embed already-normalised paths, so a lexical component prefix
        // check suffices (no `canonicalize` per cached texture).
        let inside = |path: &Path| directories.iter().any(|dir| path.starts_with(dir));
        self.forget(
            |key| inside(Path::new(source_path_from_asset_key(key).unwrap_or(key))),
            |source| inside(source),
        );
    }

    /// Drops textures, animations, failures and running requests for the
    /// sources matched by `matches_key` (asset keys) or `matches_source`
    /// (request sources).
    fn forget(
        &mut self,
        matches_key: impl Fn(&str) -> bool,
        matches_source: impl Fn(&Path) -> bool,
    ) {
        let stale: Vec<String> = self
            .textures
            .iter()
            .filter(|(key, _)| matches_key(key))
            .map(|(key, _)| key.clone())
            .collect();
        for key in stale {
            self.textures.pop(&key);
        }
        self.texture_bytes = self
            .textures
            .iter()
            .map(|(_, texture)| texture_bytes(texture))
            .fold(0, usize::saturating_add);
        let stale: Vec<String> = self
            .animations
            .iter()
            .filter(|(key, _)| matches_key(key))
            .map(|(key, _)| key.clone())
            .collect();
        for key in stale {
            self.animations.pop(&key);
        }
        self.animation_bytes = self
            .animations
            .iter()
            .map(|(_, value)| value.byte_len)
            .fold(0, usize::saturating_add);
        let keys: HashSet<_> = self
            .requests
            .iter()
            .filter(|(key, control)| {
                control
                    .source
                    .as_ref()
                    .map_or_else(|| matches_key(key), |source| matches_source(&source.path))
            })
            .map(|(key, _)| key.clone())
            .chain(self.pending.iter().filter(|key| matches_key(key)).cloned())
            .collect();
        self.cancel_requests(&keys);
        self.failed.retain(|key, _| !matches_key(key));
    }

    fn request_animated_preview(
        &mut self,
        ctx: &Context,
        entry: &EntryRef<'_>,
        intent: PreviewIntent,
        source_kind: AnimatedSourceKind,
        spec: PreviewSpec,
    ) -> Preview {
        let path = entry.get_path();
        let key = format!(
            "{}#{}:{}#overview_v7_px{}",
            entry.full_path_string(),
            entry.meta.source_revision,
            entry.meta.size,
            spec.max_edge
        );
        // Not registering the preview as wanted also cancels any job for it,
        // so that work can't hold up whatever the dialog is doing.
        if self.previews_paused {
            return self.held_animated_preview(&key);
        }
        // In the background, keep showing frames already decoded but don't
        // start new work.
        if ctx.input(|input| input.viewport().focused == Some(false)) {
            return if self.animations.contains(&key) {
                self.render_animated_preview(ctx, &key)
            } else {
                Preview::Fallback
            };
        }
        if intent == PreviewIntent::Selected {
            if !self.selected_previews_enabled
                || self
                    .selected_preview_key
                    .as_ref()
                    .is_some_and(|selected| selected != &key)
            {
                return Preview::Fallback;
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
            return Preview::Pending;
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
            return cached_frame.unwrap_or(Preview::Pending);
        }
        if let Some(state) = self.preview_failure_state(&key) {
            if let Preview::Unavailable {
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
            return cached_frame.unwrap_or(Preview::Pending);
        }
        self.enqueue_animated_preview(entry, &path, key, intent, source_kind, spec);
        cached_frame.unwrap_or(Preview::Pending)
    }

    /// Starts decoding an animated preview on the preview worker.
    fn enqueue_animated_preview(
        &mut self,
        entry: &EntryRef<'_>,
        path: &Path,
        key: String,
        intent: PreviewIntent,
        source_kind: AnimatedSourceKind,
        spec: PreviewSpec,
    ) {
        let (request_id, cancel) = self.start_request(
            &key,
            Some(SourceKey::from_normalized(
                path,
                entry.meta.source_revision,
                entry.meta.size,
            )),
        );
        let enqueue = self.preview_scheduler.enqueue(
            AssetJob::AnimatedPreview {
                source_path: path.to_path_buf(),
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
            path.parent().map(directory_key),
        );
        self.clear_evicted_requests(enqueue.evicted);
        if enqueue.accepted {
            self.pending.insert(key.clone());
            self.preview_jobs
                .insert(key, PreviewJobControl { intent, cancel });
            self.last_asset_activity = Instant::now();
        }
    }

    /// The frame on screen, without advancing or scheduling another.
    fn held_animated_preview(&mut self, key: &str) -> Preview {
        let Some(animation) = self.animations.get_mut(key) else {
            return Preview::Paused;
        };
        // Still in use, so `end_frame` must not release the texture.
        animation.last_used = Instant::now();
        animation
            .texture
            .as_ref()
            .map_or(Preview::Paused, |texture| Preview::Ready(texture.clone()))
    }

    fn render_animated_preview(&mut self, ctx: &Context, key: &str) -> Preview {
        let can_upload = self.frame_uploads < MAX_TEXTURES_PER_FRAME
            && (self.frame_uploads == 0 || self.upload_time < Duration::from_millis(2));
        let gpu_bytes: usize = self
            .animations
            .iter()
            .filter_map(|(_, animation)| animation.texture.as_ref())
            .map(TextureHandle::byte_size)
            .sum();
        let Some(animation) = self.animations.get_mut(key) else {
            return Preview::Pending;
        };
        let (index, next_frame) = animation.current_frame();
        if animation.frames.len() > 1 {
            ctx.request_repaint_after(next_frame);
        }
        let Some(frame) = animation.frames.get(index) else {
            return Preview::Fallback;
        };
        if animation.texture_frame == Some(index) {
            return animation
                .texture
                .as_ref()
                .map_or(Preview::Pending, |texture| Preview::Ready(texture.clone()));
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
                .map_or(Preview::Pending, |texture| Preview::Ready(texture.clone()));
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
        Preview::Ready(animation.texture.as_ref().expect("uploaded frame").clone())
    }

    fn preview_failure_state(&self, key: &str) -> Option<Preview> {
        let record = self.failed.get(key)?;
        let retry_after = match record.retry() {
            Retry::Now => return None,
            Retry::Never => None,
            Retry::After(wait) => Some(wait),
        };
        Some(Preview::Unavailable {
            reason: record.reason.clone(),
            retry_after,
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
            path.parent().map(directory_key),
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
        self.failed
            .get(key)
            .is_some_and(|record| record.retry() != Retry::Now)
    }
}

/// Tells the shared media service where this app keeps its files, how its
/// notices become toasts, and where chosen video thumbnails are kept. Only
/// the first call counts.
fn connect_media_service() {
    use dirfleet_effects::media::{MediaDirs, Notice};
    if let Some(dirs) = crate::paths::project_dirs() {
        dirfleet_effects::media::configure(MediaDirs {
            cache: dirs.cache_dir,
            data: dirs.data_local_dir,
        });
    }
    dirfleet_effects::media::set_notifier(|level, text| {
        let kind = match level {
            Notice::Info => egui_mev_widgets::ToastKind::Info,
            Notice::Success => egui_mev_widgets::ToastKind::Ok,
            Notice::Warning => egui_mev_widgets::ToastKind::Warn,
        };
        crate::app::queue_toast(kind, text.to_owned());
    });
    dirfleet_effects::media::set_chosen_frames(thumbnail_choice::get);
}

impl Default for AssetManager {
    fn default() -> Self {
        Self::new()
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
            Self::Cancelled { .. } | Self::PickerTiming { .. } => 0,
            Self::PickerFrame { result, .. } => result.as_ref().map_or(0, DecodedImage::byte_len),
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
            | Self::PickerTiming {
                request_key,
                request_id,
                ..
            }
            | Self::PickerFrame {
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
        .try_update(
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
                match &result {
                    AssetJobResult::Failed { kind, .. } => media::recover_backend(*kind),
                    AssetJobResult::PickerTiming {
                        result: Err(error), ..
                    }
                    | AssetJobResult::PickerFrame {
                        result: Err(error), ..
                    } => media::recover_backend(error.kind),
                    _ => {}
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

/// The step that ends a job whose result is no longer wanted.
const fn cancelled(request_key: String, request_id: u64) -> JobStep {
    JobStep::Complete(AssetJobResult::Cancelled {
        request_key,
        request_id,
    })
}

/// A finished image job as a result for the UI.
fn image_result(
    request_key: String,
    request_id: u64,
    result: Result<DecodedImage, MediaError>,
) -> AssetJobResult {
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
}

/// A video poster that is already cached, by us or by Windows. Anything that
/// needs generating is left to the video worker, so a slow thumbnail handler
/// can't stall image thumbnails on the quick pool.
fn cached_video_poster(
    source: &SourceKey,
    source_path: &Path,
    target_edge: u32,
    cancel: &AtomicBool,
) -> Option<DecodedImage> {
    if let Some(at) = thumbnail_choice::get(source_path, source.revision, source.size) {
        let cache_path = cache::chosen_poster_cache_path(
            source_path,
            target_edge,
            source.revision,
            source.size,
            at.as_millis() as u64,
        );
        return media::cached_chosen_poster(source, target_edge, at, &cache_path, cancel);
    }
    let cache_path = thumbnail_cache_path(source_path, target_edge, source.revision, source.size);
    media::cached_poster_image(source, target_edge, cancel)
        .or_else(|| cached_shell_poster(&cache_path, source_path))
        .or_else(|| shell_thumbnail_image(source_path, target_edge, true))
}

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
            if cancel.load(AtomicOrdering::Acquire) {
                return cancelled(request_key, request_id);
            }
            let source = SourceKey::new(&source_path, source_revision, source_size);
            let changed = |source: &SourceKey| source_revision != 0 && !source.unchanged();
            if kind == ThumbnailKind::Video && !cache_checked {
                return match cached_video_poster(&source, &source_path, target_edge, &cancel) {
                    Some(_) if changed(&source) => cancelled(request_key, request_id),
                    Some(image) => {
                        JobStep::Complete(image_result(request_key, request_id, Ok(image)))
                    }
                    None => JobStep::Transfer(AssetJob::Thumbnail {
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
                    }),
                };
            }
            let result = load_or_generate_thumbnail(
                &source_path,
                kind,
                target_edge,
                source_revision,
                source_size,
                &cancel,
            );
            if cancel.load(AtomicOrdering::Acquire) || changed(&source) {
                return cancelled(request_key, request_id);
            }
            JobStep::Complete(image_result(request_key, request_id, result))
        }
        AssetJob::SystemIcon {
            request_key,
            lookup_arg,
            icon_size,
            request_id,
            cancel,
        } => {
            if cancel.load(AtomicOrdering::Acquire) {
                return cancelled(request_key, request_id);
            }
            let result = load_system_icon_image(&lookup_arg, icon_size).ok_or_else(|| {
                MediaError::new(
                    ErrorKind::InvalidMedia,
                    format!("Could not load system icon for {lookup_arg}"),
                )
            });
            JobStep::Complete(image_result(request_key, request_id, result))
        }
        job @ AssetJob::AnimatedPreview { .. } => step_animated_preview(job, media_backend),
        job @ AssetJob::Picker { .. } => run_picker_job(job, media_backend),
    }
}

/// Looks up a video's timing or extracts one frame for the thumbnail picker.
fn run_picker_job(job: AssetJob, media_backend: &dyn MediaBackend) -> JobStep {
    let AssetJob::Picker {
        source_path,
        request_key,
        source_revision,
        source_size,
        request_id,
        cancel,
        at,
    } = job
    else {
        unreachable!("only picker jobs run here");
    };
    if cancel.load(AtomicOrdering::Acquire) {
        return cancelled(request_key, request_id);
    }
    let source = SourceKey::new(&source_path, source_revision, source_size);
    let result = match at {
        None => AssetJobResult::PickerTiming {
            result: media_backend.picker_timing(&source, &cancel),
            request_key,
            request_id,
        },
        Some(at) => AssetJobResult::PickerFrame {
            result: media_backend.picker_frame(&source, Duration::from_millis(at), &cancel),
            request_key,
            request_id,
            at,
        },
    };
    if (source_revision != 0 && !source.unchanged()) || cancel.load(AtomicOrdering::Acquire) {
        let (request_key, request_id) = result.identity();
        return cancelled(request_key.to_owned(), request_id);
    }
    JobStep::Complete(result)
}

/// Runs one step of an animated preview and decides what happens next.
fn step_animated_preview(job: AssetJob, media_backend: &dyn MediaBackend) -> JobStep {
    let AssetJob::AnimatedPreview {
        source_path,
        request_key,
        source_revision,
        source_size,
        source_kind,
        request_id,
        cancel,
        spec,
        mut task,
    } = job
    else {
        unreachable!("only animated preview jobs are stepped");
    };
    if cancel.load(AtomicOrdering::Acquire) {
        return cancelled(request_key, request_id);
    }
    let source = SourceKey::new(&source_path, source_revision, source_size);
    let step = media_backend.step_preview(&source, spec, source_kind, &cancel, &mut task);
    if (source_revision != 0 && !source.unchanged()) || cancel.load(AtomicOrdering::Acquire) {
        return cancelled(request_key, request_id);
    }
    match step {
        Ok(PreviewStep::Complete(animation)) => JobStep::Complete(AssetJobResult::AnimationReady {
            request_key,
            request_id,
            animation,
        }),
        Ok(PreviewStep::Continue(progress)) => {
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
        Err(error) => JobStep::Complete(AssetJobResult::Failed {
            request_key,
            request_id,
            reason: error.message,
            kind: error.kind,
        }),
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
    } else if spec.starts_with("overview_")
        || spec.starts_with("anim_v")
        || spec.starts_with("pickerframe_")
    {
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

pub fn entry_has_animated_preview(entry: &EntryRef<'_>) -> bool {
    kinds::media_kind(Path::new(entry.file_name)).is_some_and(kinds::MediaKind::is_animated)
}

#[allow(
    clippy::items_after_statements,
    reason = "test fixture bypass precedes platform icon extraction"
)]
#[cfg_attr(
    all(test, windows),
    expect(
        clippy::unnecessary_wraps,
        reason = "the deterministic test loader preserves the fallible native API"
    )
)]
fn load_system_icon_image(lookup_arg: &str, icon_size: IconSize) -> Option<DecodedImage> {
    #[cfg(all(test, windows))]
    {
        Some(deterministic_test_document_icon(lookup_arg, icon_size))
    }

    #[cfg(not(all(test, windows)))]
    {
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
        static SYSTEM_ICON_EXTRACTION: std::sync::LazyLock<Mutex<()>> =
            std::sync::LazyLock::new(|| Mutex::new(()));
        let _guard = SYSTEM_ICON_EXTRACTION
            .lock()
            .expect("system icon extraction mutex poisoned");
        let px = icon_size.system_icon_px();
        // `systemicons` cannot look up a macOS path that is not ASCII, and it
        // goes through a temporary file.
        #[cfg(target_os = "macos")]
        let bytes = {
            let Some(bytes) = crate::macos_tools::icon_png(lookup_arg, f64::from(px)) else {
                log::warn!("icon_png failed for {lookup_arg:?}");
                return None;
            };
            bytes
        };
        #[cfg(not(target_os = "macos"))]
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
#[path = "assets/integration_tests.rs"]
mod media_integration_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use dirfleet_effects::paths::PathHelper;
    use media::load_or_generate_animated_preview;

    /// Test backends that produce a whole preview in one call.
    trait WholePreviewBackend: Send + Sync {
        fn load_animated_preview(
            &self,
            path: &Path,
            source_revision: u128,
            source_size: u64,
            source_kind: AnimatedSourceKind,
            cancel: &AtomicBool,
        ) -> Result<DecodedAnimation, String>;
    }

    impl<T: WholePreviewBackend> MediaBackend for T {
        fn step_preview(
            &self,
            source: &SourceKey,
            _spec: PreviewSpec,
            kind: AnimatedSourceKind,
            cancel: &AtomicBool,
            _task: &mut Option<Box<VideoTask>>,
        ) -> Result<PreviewStep, MediaError> {
            self.load_animated_preview(&source.path, source.revision, source.size, kind, cancel)
                .map(PreviewStep::Complete)
                .map_err(|error| MediaError::new(ErrorKind::InvalidMedia, error))
        }

        fn picker_timing(
            &self,
            _source: &SourceKey,
            _cancel: &AtomicBool,
        ) -> Result<VideoTiming, MediaError> {
            Ok(VideoTiming {
                duration: Some(60.0),
                frame_rate: Some(30.0),
            })
        }

        fn picker_frame(
            &self,
            source: &SourceKey,
            _at: Duration,
            cancel: &AtomicBool,
        ) -> Result<DecodedImage, MediaError> {
            self.load_animated_preview(
                &source.path,
                source.revision,
                source.size,
                AnimatedSourceKind::Video,
                cancel,
            )
            .map(|animation| animation.frames[0].clone())
            .map_err(|error| MediaError::new(ErrorKind::InvalidMedia, error))
        }
    }
    /// Thumbnail decode paths for a 12 MP JPEG. Run with
    /// `cargo test --release perf_thumbnail -- --ignored --nocapture`.
    #[test]
    fn a_resumed_preview_yields_to_visible_thumbnails() {
        let source = SourceKey::new(Path::new("queued.mp4"), 0, 0);
        let mut task =
            VideoTask::refinement_fixture(source, PreviewSpec::new(160), AnimatedSourceKind::Video);
        assert!(matches!(
            task.step(&AtomicBool::new(false)),
            Ok(PreviewStep::Complete(_))
        ));
        let scheduler = JobScheduler::default();
        scheduler.enqueue(
            AssetJob::AnimatedPreview {
                source_path: PathBuf::from("queued.mp4"),
                request_key: "preview".into(),
                source_revision: 0,
                source_size: 0,
                source_kind: AnimatedSourceKind::Video,
                request_id: 1,
                cancel: std::sync::Arc::new(AtomicBool::new(false)),
                spec: PreviewSpec::new(160),
                task: Some(Box::new(task)),
            },
            AssetJobClass::HoveredPreview,
            None,
        );
        let entry = scheduler.recv_entry().expect("asset fixture operation");
        scheduler.resume(entry);
        scheduler.enqueue(
            AssetJob::SystemIcon {
                request_key: "visible".into(),
                lookup_arg: "folder".into(),
                icon_size: IconSize::Small,
                request_id: 2,
                cancel: std::sync::Arc::new(AtomicBool::new(false)),
            },
            AssetJobClass::VisibleThumbnail,
            None,
        );
        assert_eq!(
            scheduler
                .recv_entry()
                .expect("asset fixture operation")
                .job
                .request_key(),
            "visible"
        );
    }

    #[test]
    #[ignore = "performance baseline; run explicitly"]
    fn perf_thumbnail_decode_paths() {
        let path = std::env::temp_dir().join("lwa_fm_perf_photo.jpg");
        if !path.exists() {
            let photo = RgbaImage::from_fn(4000, 3000, |x, y| {
                image::Rgba([(x % 256) as u8, (y % 256) as u8, ((x ^ y) % 256) as u8, 255])
            });
            image::DynamicImage::ImageRgba8(photo)
                .to_rgb8()
                .save(&path)
                .expect("write perf photo");
        }
        let time = |label: &str, mut f: Box<dyn FnMut() -> bool>| {
            let runs: Vec<_> = (0..4)
                .map(|_| {
                    let started = Instant::now();
                    let ok = f();
                    (started.elapsed(), ok)
                })
                .collect();
            eprintln!("perf thumbnail {label:<24} runs={runs:?}");
        };
        let full = path.clone();
        time(
            "full decode + resize",
            Box::new(move || {
                image::ImageReader::open(&full)
                    .and_then(image::ImageReader::with_guessed_format)
                    .ok()
                    .and_then(|reader| reader.decode().ok())
                    .map(|image| image.thumbnail(160, 160))
                    .is_some()
            }),
        );
        #[cfg_attr(not(windows), allow(clippy::redundant_clone))]
        let scaled = path.clone();
        time(
            "DCT-scaled decode",
            Box::new(move || {
                decode::decode_jpeg_scaled(&scaled, 160)
                    .map(|image| image.thumbnail(160, 160))
                    .is_some()
            }),
        );
        #[cfg(windows)]
        {
            let shell = path;
            time(
                "Windows shell cache",
                Box::new(move || {
                    dirfleet_effects::windows_media::shell_thumbnail(&shell, 160, false).is_some()
                }),
            );
            let missing = std::env::temp_dir()
                .join(format!("lwa_fm_perf_uncached_{}.jpg", std::process::id()));
            let _ = fs::copy(std::env::temp_dir().join("lwa_fm_perf_photo.jpg"), &missing);
            time(
                "shell cache miss probe",
                Box::new(move || {
                    dirfleet_effects::windows_media::shell_thumbnail(&missing, 160, true).is_none()
                }),
            );
        }
    }

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
        // A poster at a chosen frame.
        assert!(asset_key_matches_file(
            "/media/clip#scene.mp4#1:20#thumb_px240_t4500",
            "/media/clip#scene.mp4"
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
        impl WholePreviewBackend for WaitingBackend {
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
        assets.cancel_requests(&std::iter::once("same".to_owned()).collect());
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
        impl WholePreviewBackend for MutatingBackend {
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
        let metadata =
            crate::data::files::metadata_of(&fs::metadata(&path).expect("source metadata"));
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
        let one_owned = DirEntry::test_new("C:/media/one.mp4");
        let one = one_owned.entry_ref();
        let two_owned = DirEntry::test_new("C:/media/two.mp4");
        let two = two_owned.entry_ref();
        assets.begin_frame();
        assert!(matches!(
            assets.request_preview(&ctx, &one, PreviewIntent::Hovered),
            Preview::Pending
        ));
        assert!(assets.pending.is_empty());
        assert!(assets.requests.is_empty());
        assets.end_frame();
        assets.begin_frame();
        assets.end_frame();
        assert!(assets.dwell.is_empty());
        assert!(matches!(
            assets.request_preview(&ctx, &one, PreviewIntent::Selected),
            Preview::Fallback
        ));
        // The inspector and Quick Look animate whatever the grid setting says.
        assert!(matches!(
            assets.request_preview(&ctx, &one, PreviewIntent::Focused),
            Preview::Pending
        ));
        assets.set_selected_previews_enabled(true);
        assert!(matches!(
            assets.request_preview(&ctx, &one, PreviewIntent::Selected),
            Preview::Pending
        ));
        assert!(matches!(
            assets.request_preview(&ctx, &two, PreviewIntent::Selected),
            Preview::Fallback
        ));
        assert!(matches!(
            assets.request_preview(&ctx, &two, PreviewIntent::Hovered),
            Preview::Pending
        ));
    }

    #[test]
    fn paused_previews_hold_their_frame_and_start_no_work() {
        let ctx = Context::default();
        let mut assets = AssetManager::new();
        let entry_owned = DirEntry::test_new("C:/media/held.mp4");
        let entry = entry_owned.entry_ref();
        let _ = assets.request_preview(&ctx, &entry, PreviewIntent::Focused);
        let key = assets.dwell.keys().next().expect("dwell key").clone();

        assets.set_previews_paused(true);
        assets.begin_frame();
        assert!(matches!(
            assets.request_preview(&ctx, &entry, PreviewIntent::Focused),
            Preview::Paused
        ));
        assert!(assets.pending.is_empty());
        assert!(assets.requests.is_empty());
        assert!(assets.desired_assets.is_empty());

        let mut decoded = tiny_animation();
        decoded.frames.push(decoded.frames[0].clone());
        decoded.frame_delays.push(VIDEO_PREVIEW_FRAME_DELAY);
        decoded.source_timestamps.push(Duration::from_secs(1));
        assets.put_animation(key.clone(), AnimatedPreview::new(decoded));
        let Preview::Ready(_) = assets.render_animated_preview(&ctx, &key) else {
            panic!("first frame");
        };
        let shown = assets.animations.peek(&key).expect("preview").texture_frame;
        let animation = assets.animations.peek_mut(&key).expect("preview");
        let long_ago = Instant::now()
            .checked_sub(Duration::from_secs(60))
            .expect("past");
        animation.started_at = long_ago + Duration::from_millis(5);
        animation.last_used = long_ago;

        assert!(matches!(
            assets.request_preview(&ctx, &entry, PreviewIntent::Focused),
            Preview::Ready(_)
        ));
        let animation = assets.animations.peek(&key).expect("preview");
        assert_eq!(animation.texture_frame, shown, "the frame must not advance");
        assert!(animation.last_used > long_ago, "must keep its texture");
        assets.end_frame();
        assert!(
            assets
                .animations
                .peek(&key)
                .expect("preview")
                .texture
                .is_some()
        );

        assets.set_previews_paused(false);
        assets.begin_frame();
        let _ = assets.request_preview(&ctx, &entry, PreviewIntent::Focused);
        assert!(
            assets.desired_assets.contains(&key),
            "wanted again once unpaused"
        );
    }

    #[test]
    fn picker_runs_one_job_at_a_time_earliest_first_and_cancels_on_close() {
        let ctx = Context::default();
        let entry_owned = DirEntry::test_new("C:/media/pick.mp4");
        let entry = entry_owned.entry_ref();
        let backend = Arc::new(MockMediaBackend {
            result: Mutex::new(Ok(tiny_animation())),
        });
        let mut assets = AssetManager::with_media_backend(backend);
        assets.picker_open(&entry, None);
        assert!(assets.picker_waiting_for_timing());
        assets.picker_pump();
        assert_eq!(assets.pending.len(), 1, "only the timing lookup");
        wait_for_preview_result(&mut assets, &ctx);
        assert!(!assets.picker_waiting_for_timing());
        let total = assets.picker_timeline().expect("session").progress().1;
        assert!(total > 1);

        // The default frame at 5%, then ascending.
        assets.picker_pump();
        assert_eq!(assets.pending.len(), 1);
        assets.picker_pump();
        assert_eq!(assets.pending.len(), 1, "still the same job");
        wait_for_preview_result(&mut assets, &ctx);
        let timeline = assets.picker_timeline().expect("session");
        assert_eq!(timeline.progress(), (1, total));
        assert!(timeline.frame(3_000).is_some());

        // Survives `end_frame`, which drops whatever isn't asked for.
        assets.picker_pump();
        let key = assets.pending.iter().next().expect("job").clone();
        assets.begin_frame();
        assets.end_frame();
        assert!(assets.requests.contains_key(&key));
        let cancel = Arc::clone(&assets.requests[&key].cancel);
        assets.picker_close();
        assert!(cancel.load(AtomicOrdering::Acquire));
        assert!(assets.pending.is_empty());
        assert!(assets.picker_timeline().is_none());
    }

    #[test]
    fn a_vanished_picker_job_does_not_stall_the_queue() {
        let entry_owned = DirEntry::test_new("C:/media/stall.mp4");
        let entry = entry_owned.entry_ref();
        let ctx = Context::default();
        let backend = Arc::new(MockMediaBackend {
            result: Mutex::new(Ok(tiny_animation())),
        });
        let mut assets = AssetManager::with_media_backend(backend);
        assets.picker_open(&entry, None);
        assets.picker_pump();
        wait_for_preview_result(&mut assets, &ctx);
        assets.picker_pump();
        let total = assets.picker_timeline().expect("session").progress().1;
        // What invalidating the file does to a running job.
        assets.invalidate_files([entry.get_path()]);
        assets.picker_pump();
        let timeline = assets.picker_timeline().expect("session");
        assert_eq!(timeline.failed(), 1);
        assert_eq!(timeline.progress().1, total - 1);
        assert_eq!(assets.pending.len(), 1, "the next frame started");
    }

    #[test]
    fn picker_keys_name_their_source() {
        assert_eq!(
            source_path_from_asset_key("/media/a#b.mp4#1:20#pickerframe_1500"),
            Some("/media/a#b.mp4")
        );
        assert_eq!(
            source_path_from_asset_key("/media/a.mp4#1:20#pickerframe_probe"),
            Some("/media/a.mp4")
        );
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
        let Preview::Ready(first) = assets.render_animated_preview(&ctx, "preview") else {
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
        let Preview::Ready(deferred) = assets.render_animated_preview(&ctx, "preview") else {
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
        let Preview::Ready(next) = assets.render_animated_preview(&ctx, "preview") else {
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
        let entry_owned = DirEntry::test_new("C:/media/partial.mp4");
        let entry = entry_owned.entry_ref();
        let backend = Arc::new(MockMediaBackend {
            result: Mutex::new(Ok(tiny_animation())),
        });
        let mut assets = AssetManager::with_media_backend(backend);
        let _ = assets.request_preview(&ctx, &entry, PreviewIntent::Hovered);
        let key = assets.dwell.keys().next().expect("dwell key").clone();
        let mut preview = AnimatedPreview::new(tiny_animation());
        preview.complete = false;
        assets.put_animation(key.clone(), preview);
        *assets.dwell.get_mut(&key).expect("dwell") =
            Instant::now().checked_sub(PREVIEW_DWELL).expect("past");
        assert!(matches!(
            assets.request_preview(&ctx, &entry, PreviewIntent::Hovered),
            Preview::Ready(_)
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

    impl WholePreviewBackend for MockMediaBackend {
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
        let entry_owned = DirEntry::test_new("C:/media/mock.mp4");
        let entry = entry_owned.entry_ref();
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
            ready_assets.request_preview(&ctx, &entry, PreviewIntent::Hovered),
            Preview::Pending
        ));
        for time in ready_assets.dwell.values_mut() {
            *time = Instant::now()
                .checked_sub(PREVIEW_DWELL)
                .expect("dwell timestamp");
        }
        let _ = ready_assets.request_preview(&ctx, &entry, PreviewIntent::Hovered);
        ready_assets.end_frame();
        wait_for_preview_result(&mut ready_assets, &ctx);
        assert!(matches!(
            ready_assets.request_preview(&ctx, &entry, PreviewIntent::Hovered),
            Preview::Ready(_)
        ));

        let failed_backend = Arc::new(MockMediaBackend {
            result: Mutex::new(Err("FFmpeg executable was not found".to_owned())),
        });
        let mut failed_assets = AssetManager::with_media_backend(failed_backend);
        failed_assets.begin_frame();
        assert!(matches!(
            failed_assets.request_preview(&ctx, &entry, PreviewIntent::Hovered),
            Preview::Pending
        ));
        for time in failed_assets.dwell.values_mut() {
            *time = Instant::now()
                .checked_sub(PREVIEW_DWELL)
                .expect("dwell timestamp");
        }
        let _ = failed_assets.request_preview(&ctx, &entry, PreviewIntent::Hovered);
        failed_assets.end_frame();
        wait_for_preview_result(&mut failed_assets, &ctx);
        assert!(matches!(
            failed_assets.request_preview(&ctx, &entry, PreviewIntent::Hovered),
            Preview::Unavailable {
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
                AssetJobClass::PrefetchThumbnail,
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
                AssetJobClass::PrefetchThumbnail,
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
        scheduler.set_active_directory(Some("new".to_owned()));
        assert_eq!(
            scheduler.state.lock().expect("scheduler state").queue.len(),
            1
        );
    }

    #[test]
    fn source_revision_changes_thumbnail_cache_key() {
        let path = Path::new("example.jpg");
        let first = thumbnail_cache_path(path, 96, 1, 10);
        let second = thumbnail_cache_path(path, 96, 2, 10);
        assert_ne!(first, second);
        assert!(first.to_string_lossy().contains("_v7.jpg"));
    }

    #[test]
    fn animated_cache_key_tracks_revision_and_settings() {
        let path = Path::new("example.mp4");
        let first = animated_preview_cache_path(path, 1, 10);
        let second = animated_preview_cache_path(path, 2, 10);
        assert_ne!(first, second);
        assert!(first.to_string_lossy().contains("_overview_v8.bin"));
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
        let meta = crate::data::files::metadata_of(&metadata);
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
                    tries: failures::RETRY_MAX_TRIES,
                    reason: "Unsupported codec".to_owned(),
                    kind,
                },
            );
            assert!(matches!(
                assets.preview_failure_state("broken"),
                Some(Preview::Unavailable {
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
            Some(Preview::Unavailable {
                retry_after: Some(_),
                ..
            })
        ));
        assets
            .failed
            .get_mut("retryable")
            .expect("failure record")
            .last_attempt = Instant::now()
            .checked_sub(Duration::from_secs(31))
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
        let meta = crate::data::files::metadata_of(&metadata);
        let cache_path = thumbnail_cache_path(&source, 64, meta.source_revision, meta.size);
        let _ = fs::remove_file(&cache_path);

        let first = load_or_generate_thumbnail(
            &source,
            ThumbnailKind::Image,
            64,
            meta.source_revision,
            meta.size,
            &AtomicBool::new(false),
        )
        .expect("thumbnail miss should generate");
        assert_eq!(first.rgba[3], 0, "PNG alpha must be preserved");
        // Transparent thumbnails are cached as PNG next to the `.jpg` name.
        let png_path = cache_path.with_extension("png");
        assert!(png_path.exists() && !cache_path.exists());
        let from_cache = load_or_generate_thumbnail(
            &source,
            ThumbnailKind::Image,
            64,
            meta.source_revision,
            meta.size,
            &AtomicBool::new(false),
        )
        .expect("cached thumbnail");
        assert_eq!(from_cache.rgba[3], 0, "alpha must survive the cache");

        fs::write(&png_path, b"truncated").expect("corrupt cache");
        let recovered = load_or_generate_thumbnail(
            &source,
            ThumbnailKind::Image,
            64,
            meta.source_revision,
            meta.size,
            &AtomicBool::new(false),
        )
        .expect("corrupt cache should regenerate immediately");
        assert_eq!(recovered.rgba[3], 0);

        let _ = fs::remove_file(&png_path);
        let workers = [(), ()].map(|()| {
            let source = source.clone();
            thread::spawn(move || {
                load_or_generate_thumbnail(
                    &source,
                    ThumbnailKind::Image,
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
        assert!(decode::cached_thumbnail(&png_path, "cached".to_owned()).is_some());

        let _ = fs::remove_file(source);
        let _ = fs::remove_file(png_path);
    }
}
