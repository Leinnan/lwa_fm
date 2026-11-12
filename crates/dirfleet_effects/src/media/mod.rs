//! Thumbnails and previews of images and videos, for every shell: decoding,
//! the disk cache, `FFmpeg` posters and overviews, and (on Windows) the
//! shell's own thumbnails.
//!
//! What the app around it decides is set once at start: where the cache and
//! downloaded tools live ([`configure`]), how notices reach the user
//! ([`set_notifier`]) and which video frame the user chose as a thumbnail
//! ([`set_chosen_frames`]).
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    reason = "the media service's parts serve each other and the shells' asset schedulers; they were crate-private in the egui app"
)]
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock, PoisonError, RwLock};
use std::time::{Duration, Instant};

#[cfg(all(windows, not(any(test, feature = "test-support"))))]
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
#[cfg(all(windows, not(any(test, feature = "test-support"))))]
use std::sync::mpsc;
#[cfg(all(windows, not(any(test, feature = "test-support"))))]
use std::thread;

use image::codecs::gif::{GifEncoder, Repeat};
use image::{Delay, Frame, RgbaImage};

pub mod cache;
pub mod decode;
pub mod failures;
pub mod ffmpeg_install;
pub mod info;
pub mod kinds;
#[cfg(target_os = "macos")]
mod macos;
pub mod process;
pub mod video;

use process::MediaError;
use video::SourceKey;

/// Where the media service keeps its files.
#[derive(Debug, Clone)]
pub struct MediaDirs {
    /// Thumbnails go in `thumbnails` below it.
    pub cache: PathBuf,
    /// Downloaded `FFmpeg` goes in `ffmpeg` below it.
    pub data: PathBuf,
}

static DIRS: OnceLock<MediaDirs> = OnceLock::new();

/// Sets where the cache and downloaded tools live. The first call wins; it
/// must come before the first thumbnail.
pub fn configure(dirs: MediaDirs) {
    let _ = DIRS.set(dirs);
}

/// The configured folders, or ones in the temporary folder.
pub fn dirs() -> &'static MediaDirs {
    DIRS.get_or_init(|| {
        let root = std::env::temp_dir().join("dirfleet-media");
        MediaDirs {
            cache: root.clone(),
            data: root,
        }
    })
}

/// How serious a notice is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    Info,
    Success,
    Warning,
}

type Notifier = dyn Fn(Notice, &str) + Send + Sync;
static NOTIFIER: RwLock<Option<Box<Notifier>>> = RwLock::new(None);

/// Sets how notices (`FFmpeg` being downloaded, for one) reach the user,
/// replacing the earlier way.
pub fn set_notifier(notify: impl Fn(Notice, &str) + Send + Sync + 'static) {
    *NOTIFIER.write().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(notify));
}

#[cfg_attr(any(test, feature = "test-support"), allow(dead_code))]
pub(crate) fn notify(level: Notice, text: &str) {
    match NOTIFIER
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
    {
        Some(notify) => notify(level, text),
        None => log::info!("{text}"),
    }
}

type ChosenFrames = dyn Fn(&Path, u128, u64) -> Option<Duration> + Send + Sync;
static CHOSEN_FRAMES: OnceLock<Box<ChosenFrames>> = OnceLock::new();

/// Sets where the frame a user chose as a video's thumbnail is looked up:
/// `(path, revision, size)` to the moment in the video.
pub fn set_chosen_frames(
    lookup: impl Fn(&Path, u128, u64) -> Option<Duration> + Send + Sync + 'static,
) {
    let _ = CHOSEN_FRAMES.set(Box::new(lookup));
}

/// The frame chosen as the video's thumbnail, if any.
pub fn chosen_frame(path: &Path, revision: u128, size: u64) -> Option<Duration> {
    CHOSEN_FRAMES.get()?(path, revision, size)
}

pub const VIDEO_PREVIEW_FRAMES: u32 = 12;

pub const VIDEO_PREVIEW_FRAME_DELAY: Duration = Duration::from_millis(250);

/// The whole budget for one image thumbnail or video poster.
pub const THUMBNAIL_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct DecodedImage {
    pub name: String,
    pub width: usize,
    pub height: usize,
    pub rgba: Arc<[u8]>,
}

impl DecodedImage {
    pub fn byte_len(&self) -> usize {
        self.rgba.len()
    }
}

#[derive(Debug, Clone)]
pub struct DecodedAnimation {
    pub frames: Vec<DecodedImage>,
    pub frame_delays: Vec<Duration>,
    pub source_timestamps: Vec<Duration>,
}

impl DecodedAnimation {
    pub fn byte_len(&self) -> usize {
        self.frames.iter().map(DecodedImage::byte_len).sum()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnimatedSourceKind {
    Gif,
    Video,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailKind {
    Image,
    Video,
}

pub fn thumbnail_kind(path: &Path) -> Option<ThumbnailKind> {
    // A GIF's thumbnail is its first frame, like any other image.
    kinds::media_kind(path).map(|kind| match kind {
        kinds::MediaKind::Image | kinds::MediaKind::AnimatedImage => ThumbnailKind::Image,
        kinds::MediaKind::Video => ThumbnailKind::Video,
    })
}

/// The thumbnail of an image or video: from our disk cache when it is still
/// current, otherwise decoded (or, for video, sampled) and cached.
pub fn load_or_generate_thumbnail(
    source_path: &Path,
    kind: ThumbnailKind,
    target_edge: u32,
    source_revision: u128,
    source_size: u64,
    cancel: &AtomicBool,
) -> Result<DecodedImage, MediaError> {
    let deadline = Instant::now() + THUMBNAIL_DEADLINE;
    process::check(cancel, deadline)?;
    let name = source_path.to_string_lossy().into_owned();
    // Without a revision (metadata unavailable) we can't tell a stale cache
    // entry from a current one, so the disk cache is skipped.
    let cacheable = source_revision != 0;
    let path = cache::thumbnail_cache_path(source_path, target_edge, source_revision, source_size);
    let source = SourceKey::new(source_path, source_revision, source_size);
    if kind == ThumbnailKind::Video
        && let Some(at) = chosen_frame(source_path, source_revision, source_size)
    {
        // The shell's poster can't show the chosen frame, so it is skipped.
        let _access_time = crate::access_time::PreserveAccessTime::new(source_path);
        let path = cacheable.then(|| {
            cache::chosen_poster_cache_path(
                source_path,
                target_edge,
                source_revision,
                source_size,
                at.as_millis() as u64,
            )
        });
        return video::chosen_poster(&source, target_edge, at, path, cancel);
    }
    if kind == ThumbnailKind::Video {
        if let Some(image) = video::cached_poster_image(&source, target_edge, cancel) {
            // Posters made while building an overview live only in memory;
            // save them once so they survive a restart, without rewriting
            // the file on every hit.
            if cacheable && !path.is_file() {
                video::schedule_poster_cache(source, path, target_edge, image.clone());
            }
            return Ok(image);
        }
    } else if cacheable && let Some(cached) = cache::cached_variant(&path) {
        if let Some(image) = decode::cached_thumbnail(&cached, name.clone()) {
            cache::touch(&cached);
            return Ok(image);
        }
        let _ = fs::remove_file(&cached);
    }
    // Making a thumbnail must not count as the user opening the file.
    let _access_time = crate::access_time::PreserveAccessTime::new(source_path);
    let image = match kind {
        ThumbnailKind::Video => {
            if let Some(image) = generate_shell_poster(source_path, target_edge, &path, cacheable) {
                return Ok(image);
            }
            video::poster_cached_to(
                &source,
                target_edge,
                cancel,
                cacheable.then(|| path.clone()),
            )?
        }
        ThumbnailKind::Image => {
            // ImageIO writes the cached thumbnail itself, scaled as it
            // decodes; reading that small file back is cheap. Only for the
            // formats where it is faster (JPEG) or the only decoder (HEIC).
            #[cfg(target_os = "macos")]
            if cacheable
                && macos::decodes_best(source_path)
                && let Some(file) = macos::image_thumbnail(source_path, target_edge, &path)
                && let Some(image) = decode::cached_thumbnail(&file, name.clone())
            {
                return Ok(image);
            }
            let image = decode::thumbnail_from_source(source_path, target_edge)?;
            process::check(cancel, deadline)?;
            if cacheable && source.unchanged() {
                // Encode before taking the publication lock; hold it only to
                // check the source is still current and to rename the file.
                if let Some((bytes, ext)) = cache::encode_thumbnail(&image) {
                    let _publication = video::PUBLICATION.lock().expect("asset publication");
                    if source.unchanged() {
                        cache::atomic_write_thumbnail(&path, &bytes, ext);
                    }
                }
            }
            decode::decoded_from_dynamic(&image, name)
        }
    };
    process::check(cancel, deadline)?;
    Ok(image)
}

/// Thumbnail from the Windows shell thumbnail cache, fitted to `edge`.
///
/// Used for video posters, where it replaces an ffmpeg process. For images it is
/// not worth it: measured on a 12 MP JPEG, a cache-miss probe costs ~7 ms and
/// a cold generation ~90 ms, versus ~23 ms for our DCT-scaled decode.
/// Disabled in tests so snapshots do not depend on host thumbnail handlers.
#[cfg(all(windows, not(any(test, feature = "test-support"))))]
pub fn shell_thumbnail_image(path: &Path, edge: u32, cached_only: bool) -> Option<DecodedImage> {
    let (width, height, pixels) = crate::windows_media::shell_thumbnail(path, edge, cached_only)?;
    let mut image = image::RgbaImage::from_raw(width, height, pixels)?;
    let longest = width.max(height);
    if longest > edge {
        let scale =
            |side: u32| (u64::from(side) * u64::from(edge) / u64::from(longest)).max(1) as u32;
        image = image::imageops::thumbnail(&image, scale(width), scale(height));
    }
    Some(DecodedImage {
        name: path.to_string_lossy().into_owned(),
        width: image.width() as usize,
        height: image.height() as usize,
        rgba: image.into_raw().into(),
    })
}

/// Where a poster made by the Windows Shell is cached, next to (not in) our
/// `FFmpeg` posters: Shell posters aren't frames at a known time, so they must
/// not seed video overviews.
fn shell_poster_path(cache_path: &Path) -> PathBuf {
    cache_path.with_extension("shell.jpg")
}

pub fn cached_shell_poster(cache_path: &Path, source_path: &Path) -> Option<DecodedImage> {
    let path = shell_poster_path(cache_path);
    let image = decode::cached_thumbnail(&path, source_path.to_string_lossy().into_owned())?;
    cache::touch(&path);
    Some(image)
}

/// How long the Windows Shell may take to make a video thumbnail.
#[cfg(all(windows, not(any(test, feature = "test-support"))))]
pub const SHELL_GENERATION_TIMEOUT: Duration = Duration::from_secs(3);

/// Shell calls that timed out and are still running; past this many we stop
/// asking the Shell until they finish.
#[cfg(all(windows, not(any(test, feature = "test-support"))))]
pub const MAX_STUCK_SHELL_CALLS: usize = 2;

/// Asks the Windows Shell to make a video thumbnail, giving up after
/// [`SHELL_GENERATION_TIMEOUT`] (third-party handlers, offline shares). The
/// result is cached so it is made only once.
#[cfg(all(windows, not(any(test, feature = "test-support"))))]
fn generate_shell_poster(
    source_path: &Path,
    edge: u32,
    cache_path: &Path,
    cacheable: bool,
) -> Option<DecodedImage> {
    static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
    if IN_FLIGHT.load(AtomicOrdering::Acquire) > MAX_STUCK_SHELL_CALLS {
        return None;
    }
    IN_FLIGHT.fetch_add(1, AtomicOrdering::AcqRel);
    let (sender, receiver) = mpsc::channel();
    let source = source_path.to_path_buf();
    thread::spawn(move || {
        let _ = sender.send(shell_thumbnail_image(&source, edge, false));
        IN_FLIGHT.fetch_sub(1, AtomicOrdering::AcqRel);
    });
    let image = receiver
        .recv_timeout(SHELL_GENERATION_TIMEOUT)
        .ok()
        .flatten()?;
    if cacheable
        && let Some(rgba) =
            image::RgbaImage::from_raw(image.width as u32, image.height as u32, image.rgba.to_vec())
        && let Some((bytes, _)) = cache::encode_thumbnail(&image::DynamicImage::ImageRgba8(rgba))
    {
        cache::atomic_save_bytes(&bytes, &shell_poster_path(cache_path));
    }
    Some(image)
}

#[cfg(not(all(windows, not(any(test, feature = "test-support")))))]
const fn generate_shell_poster(
    _source_path: &Path,
    _edge: u32,
    _cache_path: &Path,
    _cacheable: bool,
) -> Option<DecodedImage> {
    None
}

#[cfg(not(all(windows, not(any(test, feature = "test-support")))))]
pub const fn shell_thumbnail_image(
    _path: &Path,
    _edge: u32,
    _cached_only: bool,
) -> Option<DecodedImage> {
    None
}

/// An animation as a looping GIF.
///
/// # Errors
/// When a frame is too large or its pixels don't match its size.
pub fn encode_animation_gif(animation: &DecodedAnimation) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    {
        // Speed 10 quantizes in a fraction of the time of the default 1
        // ("quality over performance at any cost"); previews are small.
        let mut encoder = GifEncoder::new_with_speed(&mut bytes, 10);
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

/// The thumbnail of `path` as an image file a shell can draw.
///
/// It comes from the cache when it is current, otherwise it is made and
/// saved there. `revision` and
/// `size` say which version of the file it is for (modified time in
/// nanoseconds, length); without a revision nothing is cached, so there is
/// no file. `None` when the file has no thumbnail.
pub fn thumbnail_file(
    path: &Path,
    edge: u32,
    revision: u128,
    size: u64,
    cancel: &AtomicBool,
) -> Option<PathBuf> {
    let kind = thumbnail_kind(path)?;
    if revision == 0 {
        return None;
    }
    let cache_path = cache::thumbnail_cache_path(path, edge, revision, size);
    // Video posters keep their own format in the cache, so what shells draw
    // is saved beside them as a plain image.
    let file = match kind {
        ThumbnailKind::Image => cache_path,
        ThumbnailKind::Video => {
            let stem = cache_path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            let chosen = chosen_frame(path, revision, size)
                .map(|at| format!("_t{}", at.as_millis()))
                .unwrap_or_default();
            cache_path.with_file_name(format!("{stem}_still{chosen}.jpg"))
        }
    };
    if let Some(found) = cache::cached_variant(&file) {
        cache::touch(&found);
        return Some(found);
    }
    // AVFoundation writes a video's poster directly, with no process; images
    // go through the cache below, which takes ImageIO where it is best.
    #[cfg(target_os = "macos")]
    {
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            return None;
        }
        let _access_time = crate::access_time::PreserveAccessTime::new(path);
        if kind == ThumbnailKind::Video {
            let poster = macos::video_poster(path, edge, chosen_frame(path, revision, size), &file);
            if poster.is_some() {
                return poster;
            }
        } else if macos::decodes_best(path) {
            // ImageIO writes the file a shell draws; reading it back to
            // hand over pixels nobody asked for would only cost time.
            let written = macos::image_thumbnail(path, edge, &file);
            if written.is_some() {
                return written;
            }
        }
    }
    let image = load_or_generate_thumbnail(path, kind, edge, revision, size, cancel).ok()?;
    if let Some(found) = cache::cached_variant(&file) {
        return Some(found);
    }
    let width = u32::try_from(image.width).ok()?;
    let height = u32::try_from(image.height).ok()?;
    let rgba = RgbaImage::from_raw(width, height, image.rgba.to_vec())?;
    let (bytes, ext) = cache::encode_thumbnail(&image::DynamicImage::ImageRgba8(rgba))?;
    cache::atomic_write_thumbnail(&file, &bytes, ext);
    cache::cached_variant(&file)
}

/// A video's or GIF's frames as a looping GIF a shell can play.
///
/// It is fitted to `edge` pixels, made once and kept in the cache like
/// thumbnails. `None` for other files, without a revision, or when no frames
/// could be read.
pub fn animated_preview_file(
    path: &Path,
    edge: u32,
    revision: u128,
    size: u64,
    cancel: &AtomicBool,
) -> Option<PathBuf> {
    let kind = match kinds::media_kind(path)? {
        kinds::MediaKind::AnimatedImage => AnimatedSourceKind::Gif,
        kinds::MediaKind::Video => AnimatedSourceKind::Video,
        kinds::MediaKind::Image => return None,
    };
    if revision == 0 {
        return None;
    }
    let spec = video::PreviewSpec::for_display(edge as f32);
    let cache_path = cache::thumbnail_cache_path(path, spec.max_edge, revision, size);
    let stem = cache_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let file = cache_path.with_file_name(format!("{stem}_playing.gif"));
    if file.is_file() {
        cache::touch(&file);
        return Some(file);
    }
    // Reading frames must not count as the user opening the file.
    let _access_time = crate::access_time::PreserveAccessTime::new(path);
    let mut task = video::VideoTask::new(SourceKey::new(path, revision, size), spec, kind);
    let animation = loop {
        match task.step(cancel).ok()? {
            video::PreviewStep::Complete(animation) => break animation,
            video::PreviewStep::Continue(_) => {}
        }
    };
    let bytes = encode_animation_gif(&animation).ok()?;
    cache::atomic_save_bytes(&bytes, &file).then_some(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn revision_of(path: &Path) -> (u128, u64) {
        let metadata = fs::metadata(path).expect("fixture metadata");
        let modified = metadata
            .modified()
            .expect("modified time")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after 1970")
            .as_nanos();
        (modified, metadata.len())
    }

    #[test]
    fn an_image_thumbnail_is_saved_once_and_found_again() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().join("wide.png");
        RgbaImage::from_pixel(400, 200, image::Rgba([200, 40, 10, 255]))
            .save(&source)
            .expect("write fixture");
        let (revision, size) = revision_of(&source);
        let cancel = AtomicBool::new(false);

        let file = thumbnail_file(&source, 64, revision, size, &cancel).expect("a thumbnail");
        let made = image::open(&file).expect("the thumbnail decodes");
        assert_eq!((made.width(), made.height()), (64, 32));
        assert_eq!(
            thumbnail_file(&source, 64, revision, size, &cancel),
            Some(file.clone()),
            "the cached file is used again"
        );
        assert_ne!(
            thumbnail_file(&source, 128, revision, size, &cancel),
            Some(file),
            "each size has its own file"
        );
        assert_eq!(
            thumbnail_file(&dir.path().join("notes.txt"), 64, revision, size, &cancel),
            None
        );
        assert_eq!(
            thumbnail_file(&source, 64, 0, size, &cancel),
            None,
            "no revision, no cache"
        );
    }

    #[test]
    fn a_gif_plays_from_a_cached_animation() {
        use image::AnimationDecoder as _;
        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().join("loop.gif");
        let frame = |shade: u8| DecodedImage {
            name: "frame".into(),
            width: 300,
            height: 150,
            rgba: vec![shade; 300 * 150 * 4].into(),
        };
        let animation = DecodedAnimation {
            frames: vec![frame(40), frame(200)],
            frame_delays: vec![Duration::from_millis(100); 2],
            source_timestamps: vec![Duration::ZERO, Duration::from_millis(100)],
        };
        fs::write(&source, encode_animation_gif(&animation).expect("encode")).expect("write");
        let (revision, size) = revision_of(&source);
        let cancel = AtomicBool::new(false);

        let file =
            animated_preview_file(&source, 112, revision, size, &cancel).expect("an animation");
        let bytes = fs::read(&file).expect("the animation is saved");
        let frames = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes))
            .expect("a GIF")
            .into_frames()
            .collect_frames()
            .expect("its frames");
        assert_eq!(frames.len(), 2);
        assert!(
            frames[0].buffer().width() <= 160,
            "fitted to the tile's tier"
        );
        assert_eq!(
            animated_preview_file(&source, 112, revision, size, &cancel),
            Some(file),
            "made once"
        );
        let still = dir.path().join("still.png");
        RgbaImage::new(4, 4).save(&still).expect("write");
        let (revision, size) = revision_of(&still);
        assert_eq!(
            animated_preview_file(&still, 112, revision, size, &cancel),
            None
        );
    }
}
