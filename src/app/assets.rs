use std::borrow::Cow;
use std::collections::{HashMap, HashSet, hash_map::DefaultHasher};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, OnceLock};
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
const ICON_RETRY_SECS: u64 = 30;
const TEXTURE_CAPACITY: usize = 512;
const GIF_BYTES_CAPACITY: usize = 128;
const VIDEO_GIF_FRAMES: u32 = 15;
const VIDEO_GIF_FRAME_DELAY_MS: u16 = 600;

static FFMPEG_READY: OnceLock<Result<(), String>> = OnceLock::new();

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum IconSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl IconSize {
    pub const fn system_icon_px(self) -> i32 {
        match self {
            Self::Small => 32,
            Self::Medium => 48,
            Self::Large => 64,
        }
    }

    pub const fn render_px(self) -> f32 {
        match self {
            Self::Small => 18.0,
            Self::Medium => 24.0,
            Self::Large => 40.0,
        }
    }

    pub const fn decode_px(self) -> u32 {
        match self {
            Self::Small => 64,
            Self::Medium => 96,
            Self::Large => 160,
        }
    }

    pub const fn cache_suffix(self) -> &'static str {
        match self {
            Self::Small => "_s",
            Self::Medium => "_m",
            Self::Large => "_l",
        }
    }

    pub const fn row_height_multiplier(self) -> f32 {
        match self {
            Self::Small => 1.1,
            Self::Medium => 1.25,
            Self::Large => 1.4,
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

pub struct AssetManager {
    textures: LruCache<String, TextureHandle>,
    gif_bytes: LruCache<String, Arc<[u8]>>,
    pending: HashSet<String>,
    failed: HashMap<String, Instant>,
    sender: Sender<AssetJob>,
    receiver: Receiver<AssetJobResult>,
    icon_size: IconSize,
    per_dir_icon_size: HashMap<String, IconSize>,
    gif_store: PersistentGifStore,
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
        let (job_tx, job_rx) = mpsc::channel::<AssetJob>();
        let (result_tx, result_rx) = mpsc::channel::<AssetJobResult>();
        let job_rx = Arc::new(std::sync::Mutex::new(job_rx));

        for _ in 0..2 {
            let worker_rx = Arc::clone(&job_rx);
            let worker_tx = result_tx.clone();
            thread::spawn(move || {
                loop {
                    let job = {
                        let Ok(lock) = worker_rx.lock() else {
                            return;
                        };
                        let Ok(job) = lock.recv() else {
                            return;
                        };
                        job
                    };

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
            sender: job_tx,
            receiver: result_rx,
            icon_size: IconSize::default(),
            per_dir_icon_size: HashMap::new(),
            gif_store: PersistentGifStore::open(),
        }
    }

    pub fn poll_results(&mut self, ctx: &Context) {
        let mut received_any = false;
        loop {
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
                }
                Ok(AssetJobResult::Failed { request_key }) => {
                    self.pending.remove(&request_key);
                    self.failed.insert(request_key, Instant::now());
                    received_any = true;
                }
                Ok(AssetJobResult::GifReady {
                    source_path,
                    request_key,
                    gif_bytes,
                }) => {
                    self.pending.remove(&request_key);
                    self.failed.remove(&request_key);
                    self.gif_store.persist(&source_path, &gif_bytes);
                    self.gif_bytes.put(request_key, gif_bytes);
                    received_any = true;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        if received_any {
            ctx.request_repaint();
        }
    }

    #[expect(dead_code, reason = "public API for future icon size UI controls")]
    pub fn set_icon_size(&mut self, size: IconSize) {
        if self.icon_size != size {
            self.icon_size = size;
            self.textures.clear();
            self.gif_bytes.clear();
            self.pending.clear();
            self.failed.clear();
        }
    }

    #[expect(dead_code, reason = "public API for future icon size UI controls")]
    pub const fn icon_size(&self) -> IconSize {
        self.icon_size
    }

    #[expect(dead_code, reason = "public API for future icon size UI controls")]
    pub fn set_icon_size_for_dir(&mut self, dir_path: &str, size: IconSize) {
        self.per_dir_icon_size.insert(dir_path.to_string(), size);
    }

    #[expect(dead_code, reason = "public API for future icon size UI controls")]
    pub fn icon_size_for_dir(&self, dir_path: &str) -> IconSize {
        self.per_dir_icon_size
            .get(dir_path)
            .copied()
            .unwrap_or(self.icon_size)
    }

    pub fn request_entry_texture(&mut self, entry: &DirEntry) -> Option<TextureHandle> {
        let path = entry.get_path();
        let path_string = path.to_full_path_string();
        let size = self.effective_icon_size(entry);

        if let Some(kind) = thumbnail_kind(path) {
            let cache_key = format!("{}{}", path_string, size.cache_suffix());
            if let Some(texture) = self.textures.get(&cache_key) {
                return Some(texture.clone());
            }
            if !self.is_pending_or_failed(&cache_key) {
                let cache_path = thumbnail_cache_path(path, size);
                let _ = self.sender.send(AssetJob::Thumbnail {
                    source_path: path.to_path_buf(),
                    cache_path,
                    request_key: cache_key.clone(),
                    kind,
                    icon_size: size,
                });
                self.pending.insert(cache_key);
            }
            return self.request_file_icon_texture(path, entry.is_file(), size);
        }

        self.request_file_icon_texture(path, entry.is_file(), size)
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
        let _ = self.sender.send(AssetJob::SystemIcon {
            request_key: cache_key.clone(),
            lookup_arg,
            icon_size: size,
        });
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
            "mp4" | "mov" | "mkv" | "avi" | "webm" | "wmv" | "flv" | "m4v" | "3gp" | "ogv" => {
                self.request_video_hover_preview(entry.get_path())
            }
            _ => HoverPreview::Fallback,
        }
    }

    pub fn invalidate_directories(&mut self, directories: impl IntoIterator<Item = PathBuf>) {
        let directories: Vec<String> = directories
            .into_iter()
            .map(|path| path.to_full_path_string())
            .collect();
        if directories.is_empty() {
            return;
        }

        let matches_dir = |key: &str| -> bool {
            let entry_path = key.strip_prefix("sidebar:").unwrap_or(key);
            directories.iter().any(|dir| entry_path.starts_with(dir))
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
        if let Some(bytes) = self.gif_store.get(path) {
            self.gif_bytes.put(request_key.clone(), bytes.clone());
            return HoverPreview::GifBytes {
                uri: Cow::Owned(format!("bytes://{request_key}.gif")),
                bytes,
            };
        }
        if !self.is_pending_or_failed(&request_key) {
            let _ = self.sender.send(AssetJob::VideoGif {
                source_path: path.to_path_buf(),
                request_key: request_key.clone(),
            });
            self.pending.insert(request_key);
        }
        HoverPreview::Loading
    }

    fn request_file_icon_texture(
        &mut self,
        path: &Path,
        is_file: bool,
        size: IconSize,
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

        let _ = self.sender.send(AssetJob::SystemIcon {
            request_key: key.clone(),
            lookup_arg,
            icon_size: size,
        });
        self.pending.insert(key);
        None
    }

    fn is_pending_or_failed(&self, key: &str) -> bool {
        if self.pending.contains(key) {
            return true;
        }
        if let Some(fail_time) = self.failed.get(key) {
            return fail_time.elapsed().as_secs() < ICON_RETRY_SECS;
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

fn thumbnail_cache_path(path: &Path, size: IconSize) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    if let Ok(metadata) = fs::metadata(path)
        && let Ok(modified) = metadata.modified()
    {
        modified.hash(&mut hasher);
    }
    thumbnail_cache_dir().join(format!("{:x}{}.png", hasher.finish(), size.cache_suffix()))
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
    ensure_ffmpeg().ok()?;
    let decode_px = icon_size.decode_px();
    let mut ffmpeg = FfmpegCommand::new()
        .seek(video_seek_timestamp(source_path))
        .input(source_path.as_os_str().to_string_lossy())
        .frames(1)
        .args([
            "-vf",
            &format!("thumbnail=n=24,scale='min({decode_px}\\,iw)':-1"),
        ])
        .output(cache_path.as_os_str().to_string_lossy())
        .spawn()
        .ok()?;
    ffmpeg.wait().ok()?;
    image::open(cache_path).ok()
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
    ensure_ffmpeg().ok()?;

    let duration_secs = probe_video_duration(path).unwrap_or(30.0).max(1.0);
    let start_pct = 0.05_f64;
    let end_pct = 0.90_f64;
    let span = (end_pct - start_pct) * duration_secs;
    let frame_step = span / f64::from(VIDEO_GIF_FRAMES);
    let frame_dir = video_frame_temp_dir(path)?;
    let file_stem = path.file_stem()?.to_string_lossy();
    let mut frame_paths = Vec::with_capacity(VIDEO_GIF_FRAMES as usize);

    for index in 0..VIDEO_GIF_FRAMES {
        let timestamp = start_pct.mul_add(duration_secs, f64::from(index) * frame_step);
        let frame_path = frame_dir.join(format!("{file_stem}_frame_{index:03}.png"));
        let mut ffmpeg = FfmpegCommand::new()
            .seek(format!("{timestamp:.3}"))
            .input(path.to_string_lossy())
            .frames(1)
            .args(["-vf", "scale='min(320,iw)':-2"])
            .args(["-q:v", "3"])
            .output(frame_path.to_string_lossy())
            .spawn()
            .ok()?;
        ffmpeg.wait().ok()?;
        if frame_path.exists() {
            frame_paths.push(frame_path);
        }
    }

    if frame_paths.is_empty() {
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

fn video_seek_timestamp(path: &Path) -> String {
    let duration = probe_video_duration(path).unwrap_or(10.0);
    let seek = (duration * 0.15).max(1.0);
    format!("{seek:.3}")
}

fn probe_video_duration(path: &Path) -> Option<f64> {
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
        command.creation_flags(0x08000000);
    }
    let output = command.output().ok()?;
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}
