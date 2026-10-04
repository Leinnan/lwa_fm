//! Full-color timeline overviews. Each sparse sample yields back to the asset scheduler.
use std::collections::{VecDeque, hash_map::DefaultHasher};
use std::fmt::Write as _;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant};

use super::cache;
use super::process::{self, ErrorKind, MediaError};
use super::{
    AnimatedSourceKind, DecodedAnimation, DecodedImage, VIDEO_PREVIEW_FRAME_DELAY,
    VIDEO_PREVIEW_FRAMES, decoded_from_dynamic,
};
use image::codecs::gif::GifDecoder;
use image::{AnimationDecoder as _, ImageDecoder as _};
use lru::LruCache;

pub(super) const MAX_EDGE: u32 = 480;
const MAX_ENTRY: usize = 16 * 1024 * 1024;
const MAX_DECODED: usize = 12 * 1024 * 1024;
const MAX_FRAME_DELAY_MS: u64 = 655_350; // GIF's 16-bit centisecond delay.
const MAGIC: &[u8; 8] = b"DFOV0007";
#[derive(Clone)]
struct BackendIdentity {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
    revision: u64,
}
fn resolve_tool(mut path: PathBuf) -> PathBuf {
    if !path.is_absolute()
        && let Some(search) = std::env::var_os("PATH")
    {
        if cfg!(windows) {
            path.set_extension("exe");
        }
        if let Some(found) = std::env::split_paths(&search)
            .map(|directory| directory.join(&path))
            .find(|candidate| candidate.is_file())
        {
            return found;
        }
    }
    path
}
fn resolve_backend() -> BackendIdentity {
    let ffmpeg = resolve_tool(ffmpeg_sidecar::paths::ffmpeg_path());
    let ffprobe = resolve_tool(ffmpeg_sidecar::ffprobe::ffprobe_path());
    let mut hash = DefaultHasher::new();
    for path in [&ffmpeg, &ffprobe] {
        path.hash(&mut hash);
        if let Ok(metadata) = fs::metadata(path) {
            metadata.len().hash(&mut hash);
            metadata.modified().ok().hash(&mut hash);
        }
    }
    BackendIdentity {
        ffmpeg,
        ffprobe,
        revision: hash.finish(),
    }
}
static BACKEND: LazyLock<Mutex<BackendIdentity>> = LazyLock::new(|| Mutex::new(resolve_backend()));
static HDR_CAPABILITIES: Mutex<Option<(u64, bool)>> = Mutex::new(None);
struct SourceGenerations {
    files: LruCache<PathBuf, u64>,
    directories: LruCache<PathBuf, u64>,
    floor: u64,
    next: u64,
}
static SOURCE_GENERATIONS: LazyLock<Mutex<SourceGenerations>> = LazyLock::new(|| {
    Mutex::new(SourceGenerations {
        files: LruCache::new(std::num::NonZero::new(4096).expect("source generation capacity")),
        directories: LruCache::new(
            std::num::NonZero::new(1024).expect("directory generation capacity"),
        ),
        floor: 0,
        next: 0,
    })
});
fn source_generation(path: &Path) -> u64 {
    let mut state = SOURCE_GENERATIONS.lock().expect("source generations");
    let mut generation = state.floor.max(state.files.get(path).copied().unwrap_or(0));
    for ancestor in path.ancestors() {
        generation = generation.max(state.directories.get(ancestor).copied().unwrap_or(0));
    }
    generation
}
fn retire_generations(paths: &[PathBuf], directories: bool) {
    let mut state = SOURCE_GENERATIONS.lock().expect("source generations");
    for path in paths {
        state.next += 1;
        let next = state.next;
        let full = if directories {
            state.directories.len() == state.directories.cap().get()
                && !state.directories.contains(path)
        } else {
            state.files.len() == state.files.cap().get() && !state.files.contains(path)
        };
        if full {
            state.floor = next;
            state.files.clear();
            state.directories.clear();
        }
        if directories {
            state.directories.put(path.clone(), next);
        } else {
            state.files.put(path.clone(), next);
        }
    }
}
// Serialize invalidation with publication, so a cancelled writer cannot recreate stale entries.
pub(super) static PUBLICATION: Mutex<()> = Mutex::new(());
pub(super) fn backend_revision() -> u64 {
    BACKEND.lock().expect("backend identity").revision
}
fn backend_command(probe: bool) -> Command {
    let backend = BACKEND.lock().expect("backend identity");
    Command::new(if probe {
        &backend.ffprobe
    } else {
        &backend.ffmpeg
    })
}
fn refresh_backend() {
    apply_backend(resolve_backend());
}

fn apply_backend(resolved: BackendIdentity) {
    let mut backend = BACKEND.lock().expect("backend identity");
    if backend.revision != resolved.revision {
        *backend = resolved;
        drop(backend);
        PROBES.lock().expect("probe cache").clear();
        POSTERS.lock().expect("poster cache").clear();
        *HDR_CAPABILITIES.lock().expect("HDR capabilities") = None;
        BACKEND_EPOCH.fetch_add(1, Ordering::Release);
    }
}
#[cfg(not(test))]
pub(super) fn check_backend_async() {
    static CHECKED: Mutex<Option<Instant>> = Mutex::new(None);
    let mut checked = CHECKED.lock().expect("backend refresh interval");
    if checked.is_none_or(|at| at.elapsed() >= Duration::from_secs(30)) {
        *checked = Some(Instant::now());
        drop(checked);
        std::thread::spawn(refresh_backend);
    }
}

pub(super) fn invalidate_sources(paths: &[PathBuf], directories: bool) {
    let _publication = PUBLICATION.lock().expect("asset publication");
    retire_generations(paths, directories);
    let matches = |source: &SourceKey| {
        paths.iter().any(|path| {
            if directories {
                source.path.starts_with(path)
            } else {
                &source.path == path
            }
        })
    };
    let mut probes = PROBES.lock().expect("probe cache");
    let keys: Vec<_> = probes
        .iter()
        .filter(|(source, _)| matches(source))
        .map(|(source, _)| source.clone())
        .collect();
    for source in keys {
        probes.pop(&source);
    }
    drop(probes);
    let mut posters = POSTERS.lock().expect("poster cache");
    let keys: Vec<_> = posters
        .iter()
        .filter(|((source, _), _)| matches(source))
        .map(|(key, _)| key.clone())
        .collect();
    for key in keys {
        posters.pop(&key);
    }
    drop(posters);
    cache::purge_sources(paths, directories);
}
pub(super) static CACHE_HITS: AtomicU64 = AtomicU64::new(0);
pub(super) static SAMPLES: AtomicU64 = AtomicU64::new(0);
pub(super) static BACKEND_EPOCH: AtomicU64 = AtomicU64::new(0);
// A poster and the first overview sample have the same timestamp contract.
// Retain only first samples, with a separate small byte budget.
#[derive(Debug, Clone)]
struct Sample {
    timestamp: Duration,
    frame: DecodedImage,
}

static POSTERS: LazyLock<Mutex<LruCache<(SourceKey, u32), Sample>>> = LazyLock::new(|| {
    Mutex::new(LruCache::new(
        std::num::NonZero::new(16).expect("poster capacity"),
    ))
});

fn remember_poster(source: &SourceKey, edge: u32, sample: &Sample) {
    let mut posters = POSTERS.lock().expect("poster cache mutex");
    posters.push((source.clone(), edge), sample.clone());
    while posters
        .iter()
        .map(|(_, sample)| sample.frame.byte_len())
        .sum::<usize>()
        > 4 * 1024 * 1024
    {
        posters.pop_lru();
    }
    drop(posters);
}

fn disk_poster(source: &SourceKey, edge: u32) -> Option<Sample> {
    let path = cache::thumbnail_cache_path(
        &source.path,
        super::IconSize::Medium,
        edge,
        source.revision,
        source.size,
    );
    let metadata = cache::read_bounded(&path.with_extension("poster"), 52).ok()?;
    let mut reader = Cursor::new(metadata.as_slice());
    if &read_array::<8>(&mut reader).ok()? != b"DFPS0001"
        || u128::from_le_bytes(read_array(&mut reader).ok()?) != source.revision
        || u64::from_le_bytes(read_array(&mut reader).ok()?) != source.size
        || u32::from_le_bytes(read_array(&mut reader).ok()?) != edge
    {
        return None;
    }
    let timestamp = Duration::from_millis(u64::from_le_bytes(read_array(&mut reader).ok()?));
    let hash = u64::from_le_bytes(read_array(&mut reader).ok()?);
    let bytes = cache::read_bounded(&path, MAX_ENTRY).ok()?;
    if checksum(&bytes) != hash {
        return None;
    }
    let frame = image_from_png(&bytes, edge).ok()?;
    CACHE_HITS.fetch_add(1, Ordering::Relaxed);
    Some(Sample { timestamp, frame })
}
fn cached_poster(source: &SourceKey, edge: u32, cancel: &AtomicBool) -> Option<Sample> {
    let mut tiers = vec![edge, 64, 96, 160, 240, 360, MAX_EDGE];
    tiers.extend(
        POSTERS
            .lock()
            .expect("poster cache")
            .iter()
            .filter(|((key, tier), _)| key == source && *tier >= edge)
            .map(|((_, tier), _)| *tier),
    );
    tiers.retain(|tier| *tier >= edge);
    tiers.sort_unstable();
    tiers.dedup();
    for tier in tiers {
        if cancel.load(Ordering::Acquire) {
            return None;
        }
        let memory = POSTERS
            .lock()
            .expect("poster cache")
            .get(&(source.clone(), tier))
            .cloned();
        let sample = memory.or_else(|| disk_poster(source, tier)).or_else(|| {
            let spec = PreviewSpec::new(tier);
            let path = preview_cache_path(source, spec, AnimatedSourceKind::Video);
            let bytes = cache::read_overview_first(&path, MAX_ENTRY).ok()?;
            let animation = match decode_bundle_prefix(&bytes, source, spec, cancel, true) {
                Ok(animation) => animation,
                Err(error) => {
                    if error.kind != ErrorKind::Cancelled {
                        let _ = fs::remove_file(path);
                    }
                    return None;
                }
            };
            CACHE_HITS.fetch_add(1, Ordering::Relaxed);
            Some(Sample {
                timestamp: animation.source_timestamps[0],
                frame: animation.frames.into_iter().next()?,
            })
        });
        if let Some(mut sample) = sample {
            if tier != edge {
                let image = image::RgbaImage::from_raw(
                    sample.frame.width as u32,
                    sample.frame.height as u32,
                    sample.frame.rgba.to_vec(),
                )?;
                sample.frame = decoded_from_dynamic(
                    &image::DynamicImage::ImageRgba8(image).thumbnail(edge, edge),
                    "overview-poster".into(),
                );
            }
            remember_poster(source, edge, &sample);
            return Some(sample);
        }
    }
    None
}
pub(super) fn cached_poster_image(
    source: &SourceKey,
    edge: u32,
    cancel: &AtomicBool,
) -> Option<DecodedImage> {
    cached_poster(source, edge, cancel).map(|sample| sample.frame)
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(super) struct SourceKey {
    pub path: PathBuf,
    pub revision: u128,
    pub size: u64,
    generation: u64,
    backend: u64,
}
impl SourceKey {
    pub fn new(path: &Path, revision: u128, size: u64) -> Self {
        Self::from_normalized(
            Path::new(&crate::helper::normalize_path_string(path)),
            revision,
            size,
        )
    }
    pub fn from_normalized(path: &Path, revision: u128, size: u64) -> Self {
        Self {
            path: path.to_path_buf(),
            revision,
            size,
            generation: source_generation(path),
            backend: backend_revision(),
        }
    }
    pub fn hash_cache_identity(&self, hash: &mut impl Hasher) {
        self.path.hash(hash);
        self.revision.hash(hash);
        self.size.hash(hash);
        self.backend.hash(hash);
    }
    pub fn unchanged(&self) -> bool {
        self.generation == source_generation(&self.path)
            && self.backend == backend_revision()
            && fs::metadata(&self.path).is_ok_and(|metadata| {
                metadata.len() == self.size
                    && metadata
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                        .is_some_and(|age| age.as_nanos() == self.revision)
            })
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub(super) struct PreviewSpec {
    pub max_edge: u32,
}
impl PreviewSpec {
    pub const fn new(max_edge: u32) -> Self {
        Self { max_edge }
    }
    pub fn for_display(edge: f32) -> Self {
        Self::new(
            [160, 240, 360, MAX_EDGE]
                .into_iter()
                .find(|tier| *tier as f32 >= edge)
                .unwrap_or(MAX_EDGE),
        )
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct ProbeInfo {
    pub duration: Option<f64>,
    pub pixels: u64,
    pub frame_rate: Option<f64>,
    pub stream: usize,
    pub hdr: bool,
    pub tone_map: bool,
    pub convert_color: bool,
    pub primaries: Option<String>,
    pub matrix: Option<String>,
    pub range: Option<String>,
}
impl ProbeInfo {
    fn sequential_duration(&self) -> Option<f64> {
        let duration = self.duration?;
        let rate = self.frame_rate?;
        let work = duration * rate * self.pixels as f64;
        (duration <= 12.0 && self.pixels > 0 && work <= 1280.0 * 720.0 * 30.0 * 12.0)
            .then_some(duration)
    }
}
fn parse_frame_rate(text: &str) -> Option<f64> {
    let value = if let Some((numerator, denominator)) = text.split_once('/') {
        numerator.parse::<f64>().ok()? / denominator.parse::<f64>().ok()?
    } else {
        text.parse::<f64>().ok()?
    };
    (value.is_finite() && value > 0.0).then_some(value)
}
type ProbeResult = (Instant, Result<ProbeInfo, MediaError>);
static PROBES: LazyLock<Mutex<LruCache<SourceKey, ProbeResult>>> = LazyLock::new(|| {
    Mutex::new(LruCache::new(
        std::num::NonZero::new(512).expect("probe capacity"),
    ))
});

#[cfg(not(test))]
pub(super) fn prepare_backend_async() {
    static PREPARATION: Mutex<(Option<Instant>, bool)> = Mutex::new((None, false));
    let mut state = PREPARATION.lock().expect("backend preparation mutex");
    if state.1
        || state
            .0
            .is_some_and(|at| at.elapsed() < Duration::from_secs(30))
    {
        return;
    }
    *state = (Some(Instant::now()), true);
    drop(state);
    std::thread::spawn(|| {
        match ffmpeg_sidecar::download::auto_download() {
            Ok(()) => {
                refresh_backend();
                let mut probes = PROBES.lock().expect("probe cache mutex");
                let obsolete: Vec<_> = probes
                    .iter()
                    .filter(|(_, (_, result))| {
                        result
                            .as_ref()
                            .is_err_and(|error| error.kind == ErrorKind::Unavailable)
                    })
                    .map(|(key, _)| key.clone())
                    .collect();
                for key in obsolete {
                    probes.pop(&key);
                }
                drop(probes);
            }
            Err(error) => log::warn!("Media backend preparation failed: {error}"),
        }
        PREPARATION.lock().expect("backend preparation mutex").1 = false;
    });
}

#[cfg(not(test))]
pub(super) fn recover_backend(kind: ErrorKind) {
    if kind == ErrorKind::Unavailable {
        prepare_backend_async();
    }
}

pub(super) fn probe(
    source: &SourceKey,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<ProbeInfo, MediaError> {
    let deadline = deadline.min(Instant::now() + Duration::from_secs(2));
    #[cfg(feature = "profiling")]
    puffin::profile_scope!("lwa_fm::assets::probe");
    process::check(cancel, deadline)?;
    if let Some((at, value)) = PROBES.lock().expect("probe cache mutex").get(source)
        && (value.is_ok() || at.elapsed() < Duration::from_secs(30))
    {
        return value.clone();
    }
    let mut command = backend_command(true);
    command.args(["-v", "error", "-show_entries", "format=duration:stream=index,codec_type,duration,width,height,avg_frame_rate,color_transfer,color_primaries,color_space,color_range:stream_disposition=attached_pic,default:stream_tags=DURATION", "-of", "json"]).arg(&source.path);
    let result = process::run(
        &mut command,
        cancel,
        deadline.min(Instant::now() + Duration::from_secs(2)),
        128 * 1024,
    )
    .and_then(|bytes| parse_probe(&bytes))
    .or_else(|error| {
        if error.kind == ErrorKind::Unavailable {
            probe_with_ffmpeg(source, cancel, deadline)
        } else {
            Err(error)
        }
    })
    .and_then(|mut info| {
        if info.hdr || matches!(info.primaries.as_deref(), Some("bt2020" | "smpte432")) {
            info.convert_color = hdr_filters_available(cancel, deadline)?;
            info.tone_map = info.hdr && info.convert_color;
            if !info.convert_color {
                return Err(MediaError::new(
                    ErrorKind::Unsupported,
                    "HDR preview requires FFmpeg zscale and tonemap filters",
                ));
            }
            if info.hdr && (info.primaries.is_none() || info.matrix.is_none()) {
                return Err(MediaError::new(
                    ErrorKind::Unsupported,
                    "HDR source lacks color primaries or matrix metadata",
                ));
            }
        }
        Ok(info)
    });
    if !matches!(
        result.as_ref().err().map(|error| error.kind),
        Some(ErrorKind::Cancelled | ErrorKind::Timeout)
    ) {
        PROBES
            .lock()
            .expect("probe cache mutex")
            .put(source.clone(), (Instant::now(), result.clone()));
    }
    result
}

fn parse_probe(bytes: &[u8]) -> Result<ProbeInfo, MediaError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|err| MediaError::new(ErrorKind::InvalidMedia, err.to_string()))?;
    let streams = value["streams"]
        .as_array()
        .ok_or_else(|| MediaError::new(ErrorKind::InvalidMedia, "No video streams"))?;
    let stream = streams
        .iter()
        .filter(|stream| {
            stream["codec_type"] == "video"
                && stream["disposition"]["attached_pic"].as_u64().unwrap_or(0) == 0
        })
        .max_by_key(|stream| {
            (
                stream["disposition"]["default"].as_u64().unwrap_or(0),
                stream["width"]
                    .as_u64()
                    .unwrap_or(0)
                    .saturating_mul(stream["height"].as_u64().unwrap_or(0)),
                std::cmp::Reverse(stream["index"].as_u64().unwrap_or(0)),
            )
        })
        .ok_or_else(|| MediaError::new(ErrorKind::InvalidMedia, "No usable video stream"))?;
    let valid_duration = |value: &serde_json::Value| {
        value
            .as_str()
            .and_then(|text| text.parse::<f64>().ok())
            .filter(|duration| *duration > 0.0 && Duration::try_from_secs_f64(*duration).is_ok())
    };
    let pixels = stream["width"]
        .as_u64()
        .unwrap_or(0)
        .saturating_mul(stream["height"].as_u64().unwrap_or(0));
    if pixels > 8192 * 8192 {
        return Err(MediaError::new(
            ErrorKind::Limit,
            "Video dimensions exceed thumbnail decoding limit",
        ));
    }
    Ok(ProbeInfo {
        pixels,
        frame_rate: stream["avg_frame_rate"].as_str().and_then(parse_frame_rate),
        duration: valid_duration(&stream["duration"])
            .or_else(|| {
                stream["tags"]["DURATION"]
                    .as_str()
                    .and_then(parse_clock_duration)
            })
            .or_else(|| valid_duration(&value["format"]["duration"])),
        stream: stream["index"].as_u64().unwrap_or(0) as usize,
        hdr: matches!(
            stream["color_transfer"].as_str(),
            Some("smpte2084" | "arib-std-b67")
        ),
        tone_map: false,
        convert_color: false,
        primaries: stream["color_primaries"]
            .as_str()
            .filter(|value| *value != "unknown")
            .map(str::to_owned),
        matrix: stream["color_space"]
            .as_str()
            .filter(|value| *value != "unknown")
            .map(str::to_owned),
        range: stream["color_range"]
            .as_str()
            .filter(|value| *value != "unknown")
            .map(str::to_owned),
    })
}

fn parse_clock_duration(text: &str) -> Option<f64> {
    let mut parts = text.split(':');
    let hours: f64 = parts.next()?.parse().ok()?;
    let minutes: f64 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next()?.parse().ok()?;
    let total = hours.mul_add(3600.0, minutes.mul_add(60.0, seconds));
    (parts.next().is_none()
        && hours >= 0.0
        && (0.0..60.0).contains(&minutes)
        && (0.0..60.0).contains(&seconds)
        && total > 0.0
        && Duration::try_from_secs_f64(total).is_ok())
    .then_some(total)
}

// Some sidecar distributions (notably macOS) contain FFmpeg without FFprobe.
// Reading its bounded input header still supplies duration and the selected video stream.
fn probe_with_ffmpeg(
    source: &SourceKey,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<ProbeInfo, MediaError> {
    let mut command = backend_command(false);
    command
        .args([
            "-hide_banner",
            "-nostdin",
            "-max_alloc",
            "134217728",
            "-threads",
            "1",
            "-i",
        ])
        .arg(&source.path);
    match process::run(
        &mut command,
        cancel,
        deadline.min(Instant::now() + Duration::from_secs(2)),
        1024,
    ) {
        Err(error) if error.kind == ErrorKind::InvalidMedia => parse_probe_stderr(&error.message),
        Err(error) => Err(error),
        Ok(_) => Err(MediaError::new(
            ErrorKind::InvalidMedia,
            "No media input header",
        )),
    }
}

fn parse_probe_stderr(header: &str) -> Result<ProbeInfo, MediaError> {
    static GEOMETRY: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r", (\d+)x(\d+)(?:[ ,\[])").expect("header geometry pattern")
    });
    let stream = header
        .lines()
        .find(|line| line.contains("Video:") && !line.contains("attached pic"))
        .ok_or_else(|| {
            MediaError::new(
                ErrorKind::InvalidMedia,
                "No usable video stream in media input header",
            )
        })?;
    let stream_index = stream
        .split("Stream #0:")
        .nth(1)
        .and_then(|index| {
            index
                .split(|character: char| !character.is_ascii_digit())
                .next()
        })
        .and_then(|index| index.parse().ok())
        .ok_or_else(|| MediaError::new(ErrorKind::InvalidMedia, "Invalid video stream index"))?;
    let dimensions = GEOMETRY
        .captures(stream)
        .ok_or_else(|| MediaError::new(ErrorKind::InvalidMedia, "Missing video dimensions"))?;
    let width: u64 = dimensions[1]
        .parse()
        .map_err(|_| MediaError::new(ErrorKind::Limit, "Invalid video width"))?;
    let height: u64 = dimensions[2]
        .parse()
        .map_err(|_| MediaError::new(ErrorKind::Limit, "Invalid video height"))?;
    if width.saturating_mul(height) > 8192 * 8192 {
        return Err(MediaError::new(
            ErrorKind::Limit,
            "Video dimensions exceed thumbnail decoding limit",
        ));
    }
    let duration = header
        .split("Duration:")
        .nth(1)
        .and_then(|value| value.trim().split(',').next())
        .and_then(|value| {
            let mut values = value.split(':').map(str::parse::<f64>);
            let hours = values.next()?.ok()?;
            let minutes = values.next()?.ok()?;
            let seconds = hours.mul_add(3600.0, minutes.mul_add(60.0, values.next()?.ok()?));
            (seconds > 0.0 && Duration::try_from_secs_f64(seconds).is_ok()).then_some(seconds)
        });
    Ok(ProbeInfo {
        pixels: width.saturating_mul(height),
        frame_rate: stream
            .split(',')
            .find_map(|part| part.trim().strip_suffix(" fps").and_then(parse_frame_rate)),
        duration,
        stream: stream_index,
        hdr: stream.contains("smpte2084") || stream.contains("arib-std-b67"),
        tone_map: false,
        convert_color: false,
        primaries: stream.contains("bt2020").then(|| "bt2020".into()),
        matrix: stream.contains("bt2020nc").then(|| "bt2020nc".into()),
        range: None,
    })
}

fn hdr_filters_available(cancel: &AtomicBool, deadline: Instant) -> Result<bool, MediaError> {
    let revision = backend_revision();
    let cached = *HDR_CAPABILITIES
        .lock()
        .expect("color filter capability mutex");
    if let Some((cached_revision, available)) = cached
        && cached_revision == revision
    {
        return Ok(available);
    }
    let mut command = backend_command(false);
    command.args(["-hide_banner", "-filters"]);
    let bytes = process::run(
        &mut command,
        cancel,
        deadline.min(Instant::now() + Duration::from_secs(1)),
        128 * 1024,
    )?;
    let listing = String::from_utf8_lossy(&bytes);
    let available = listing
        .lines()
        .any(|line| line.split_whitespace().nth(1) == Some("zscale"))
        && listing
            .lines()
            .any(|line| line.split_whitespace().nth(1) == Some("tonemap"));
    *HDR_CAPABILITIES
        .lock()
        .expect("color filter capability mutex") = Some((revision, available));
    Ok(available)
}

pub(super) fn sample_times(duration: f64) -> Vec<Duration> {
    if duration <= 0.0 || Duration::try_from_secs_f64(duration).is_err() {
        return vec![Duration::ZERO];
    }
    let end = (duration * 0.90).min((duration - 0.001).max(0.0));
    let start = (duration * 0.05).min(end);
    (0..VIDEO_PREVIEW_FRAMES)
        .map(|index| {
            Duration::from_secs_f64(
                (end - start)
                    .mul_add(
                        f64::from(index) / f64::from(VIDEO_PREVIEW_FRAMES - 1),
                        start,
                    )
                    .max(0.0),
            )
        })
        .fold(Vec::new(), |mut values, time| {
            if values.last().is_none_or(|last| *last != time) {
                values.push(time);
            }
            values
        })
}

fn decoder_threads() -> u32 {
    static THREADS: LazyLock<u32> = LazyLock::new(|| {
        if std::thread::available_parallelism().is_ok_and(|cpus| cpus.get() >= 4) {
            2
        } else {
            1
        }
    });
    *THREADS
}

fn ffmpeg(
    source: &SourceKey,
    info: &ProbeInfo,
    timestamp: Duration,
    edge: u32,
    selection: Option<&str>,
) -> Command {
    ffmpeg_with_threads(source, info, timestamp, edge, selection, decoder_threads())
}

fn ffmpeg_with_threads(
    source: &SourceKey,
    info: &ProbeInfo,
    timestamp: Duration,
    edge: u32,
    selection: Option<&str>,
    threads: u32,
) -> Command {
    let mut command = backend_command(false);
    let mut scale = format!(
        "scale=w='max(1,trunc(iw*sar*min(1,min({edge}/(iw*sar),{edge}/ih))))':h='max(1,trunc(ih*min(1,min({edge}/(iw*sar),{edge}/ih))))':flags=lanczos,setsar=1"
    );
    if info.convert_color {
        let range = match info.range.as_deref() {
            Some("tv") => ":rangein=limited",
            Some("pc") => ":rangein=full",
            _ => "",
        };
        let _ = write!(
            scale,
            ",format=yuv444p16le,zscale=transfer=linear:npl=100{range},format=gbrpf32le,zscale=primaries=bt709"
        );
        if info.tone_map {
            scale.push_str(",tonemap=tonemap=hable:desat=2");
        }
        scale.push_str(",zscale=transfer=iec61966-2-1:matrix=bt709:range=full,format=rgb24");
    }
    command
        .args([
            "-hide_banner",
            "-loglevel",
            "info",
            "-nostdin",
            "-max_alloc",
            "134217728",
            "-filter_threads",
            "1",
            "-threads",
            &threads.to_string(),
            "-ss",
        ])
        .arg(format!("{:.6}", timestamp.as_secs_f64()))
        .arg("-i")
        .arg(&source.path)
        .args([
            "-map",
            &format!("0:{}", info.stream),
            "-an",
            "-sn",
            "-dn",
            "-threads:v",
            "1",
            "-vf",
        ])
        .arg(selection.map_or_else(
            || format!("{scale},showinfo"),
            |selection| format!("{selection},{scale},showinfo"),
        ));
    command
}

fn image_from_png(bytes: &[u8], edge: u32) -> Result<DecodedImage, MediaError> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|err| MediaError::new(ErrorKind::InvalidMedia, err.to_string()))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(edge);
    limits.max_image_height = Some(edge);
    limits.max_alloc = Some(MAX_DECODED as u64);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|err| MediaError::new(ErrorKind::InvalidMedia, err.to_string()))?;
    Ok(decoded_from_dynamic(&image, "video-overview-frame".into()))
}

#[derive(Debug, Clone, Copy)]
enum FrameTransport {
    Png,
    #[cfg(test)]
    Ppm,
}
#[cfg(test)]
fn ppm_frames(bytes: &[u8], edge: u32) -> Result<Vec<DecodedImage>, MediaError> {
    fn token<'a>(bytes: &'a [u8], cursor: &mut usize) -> Option<&'a [u8]> {
        while bytes.get(*cursor).is_some_and(u8::is_ascii_whitespace) {
            *cursor += 1;
        }
        let start = *cursor;
        while bytes
            .get(*cursor)
            .is_some_and(|byte| !byte.is_ascii_whitespace())
        {
            *cursor += 1;
        }
        (*cursor > start).then(|| &bytes[start..*cursor])
    }
    if edge > MAX_EDGE {
        return Err(MediaError::new(ErrorKind::Limit, "RGB edge limit exceeded"));
    }
    let invalid = || MediaError::new(ErrorKind::InvalidMedia, "Invalid RGB frame stream");
    let mut cursor = 0;
    let mut frames = Vec::new();
    while cursor < bytes.len() {
        if token(bytes, &mut cursor) != Some(b"P6") {
            return Err(invalid());
        }
        let width: usize = std::str::from_utf8(token(bytes, &mut cursor).ok_or_else(invalid)?)
            .map_err(|_| invalid())?
            .parse()
            .map_err(|_| invalid())?;
        let height: usize = std::str::from_utf8(token(bytes, &mut cursor).ok_or_else(invalid)?)
            .map_err(|_| invalid())?
            .parse()
            .map_err(|_| invalid())?;
        if token(bytes, &mut cursor) != Some(b"255")
            || !bytes.get(cursor).is_some_and(u8::is_ascii_whitespace)
        {
            return Err(invalid());
        }
        if bytes.get(cursor..cursor + 2) == Some(b"\r\n") {
            cursor += 2;
        } else {
            cursor += 1;
        }
        if width == 0
            || height == 0
            || width.max(height) > edge as usize
            || frames.len() >= VIDEO_PREVIEW_FRAMES as usize
        {
            return Err(MediaError::new(
                ErrorKind::Limit,
                "RGB geometry or frame limit exceeded",
            ));
        }
        let length = width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(3))
            .ok_or_else(invalid)?;
        let end = cursor
            .checked_add(length)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(invalid)?;
        let mut rgba = Vec::with_capacity(width * height * 4);
        for pixel in bytes[cursor..end].as_chunks::<3>().0 {
            rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
        }
        frames.push(DecodedImage {
            name: "video-overview-frame".into(),
            width,
            height,
            rgba: rgba.into(),
        });
        cursor = end;
    }
    Ok(frames)
}
fn decode_samples(
    command: &mut Command,
    seek: Duration,
    edge: u32,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<Vec<Sample>, MediaError> {
    decode_samples_with_transport(command, seek, edge, cancel, deadline, FrameTransport::Png)
}

fn decode_samples_with_transport(
    command: &mut Command,
    seek: Duration,
    edge: u32,
    cancel: &AtomicBool,
    deadline: Instant,
    transport: FrameTransport,
) -> Result<Vec<Sample>, MediaError> {
    command.args([
        "-fps_mode",
        "vfr",
        "-c:v",
        match transport {
            FrameTransport::Png => "png",
            #[cfg(test)]
            FrameTransport::Ppm => "ppm",
        },
        "-f",
        "image2pipe",
        "pipe:1",
    ]);
    let output = process::run_capture(command, cancel, deadline, MAX_ENTRY)?;
    let frames = match transport {
        FrameTransport::Png => split_pngs(&output.bytes)?
            .into_iter()
            .map(|png| image_from_png(png, edge))
            .collect::<Result<Vec<_>, _>>()?,
        #[cfg(test)]
        FrameTransport::Ppm => ppm_frames(&output.bytes, edge)?,
    };
    let timestamps: Vec<_> = output
        .stderr
        .lines()
        .filter(|line| line.contains("Parsed_showinfo_") && line.contains(" pts_time:"))
        .filter_map(|line| {
            line.split("pts_time:")
                .nth(1)?
                .split_whitespace()
                .next()?
                .parse::<f64>()
                .ok()
        })
        .filter(|pts| pts.is_finite())
        .filter_map(|pts| Duration::try_from_secs_f64(pts.max(0.0)).ok())
        .map(|pts| seek.saturating_add(pts))
        .collect();
    if timestamps.len() < frames.len() {
        return Err(MediaError::new(
            ErrorKind::InvalidMedia,
            "Missing decoded frame timestamps",
        ));
    }
    frames
        .into_iter()
        .zip(timestamps)
        .map(|(frame, timestamp)| {
            process::check(cancel, deadline)?;
            Ok(Sample { timestamp, frame })
        })
        .collect()
}

fn sample_at(
    source: &SourceKey,
    info: &ProbeInfo,
    timestamp: Duration,
    edge: u32,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<Option<Sample>, MediaError> {
    let mut command = ffmpeg(source, info, timestamp, edge, None);
    command.args(["-frames:v", "1"]);
    Ok(decode_samples(
        &mut command,
        timestamp,
        edge,
        cancel,
        deadline.min(Instant::now() + Duration::from_secs(2)),
    )?
    .into_iter()
    .next())
}

pub(super) fn poster(
    source: &SourceKey,
    edge: u32,
    cancel: &AtomicBool,
) -> Result<DecodedImage, MediaError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    process::check(cancel, deadline)?;
    if let Some(sample) = cached_poster(source, edge, cancel) {
        remember_poster(source, edge, &sample);
        return Ok(sample.frame);
    }
    let info = probe(source, cancel, deadline)?;
    let timestamp = info
        .duration
        .map_or(Duration::ZERO, |duration| sample_times(duration)[0]);
    let sample = match sample_at(source, &info, timestamp, edge, cancel, deadline)? {
        Some(sample) => sample,
        None => sample_at(source, &info, Duration::ZERO, edge, cancel, deadline)?
            .ok_or_else(|| MediaError::new(ErrorKind::InvalidMedia, "No video frames"))?,
    };
    remember_poster(source, edge, &sample);
    Ok(sample.frame)
}

#[derive(Debug)]
pub(super) enum PreviewStep {
    Continue(Option<DecodedAnimation>),
    Complete(DecodedAnimation),
}

#[derive(Debug)]
pub(super) struct VideoTask {
    source: SourceKey,
    spec: PreviewSpec,
    kind: AnimatedSourceKind,
    deadline: Instant,
    expires: Instant,
    yielded_at: Instant,
    info: Option<ProbeInfo>,
    times: Vec<Duration>,
    order: Vec<usize>,
    next_sample: usize,
    samples: Vec<Sample>,
}
impl VideoTask {
    pub fn new(source: SourceKey, spec: PreviewSpec, kind: AnimatedSourceKind) -> Self {
        Self {
            source,
            spec,
            kind,
            deadline: Instant::now() + Duration::from_secs(8),
            expires: Instant::now() + Duration::from_secs(30),
            yielded_at: Instant::now(),
            info: None,
            times: Vec::new(),
            order: Vec::new(),
            next_sample: 0,
            samples: Vec::new(),
        }
    }
    fn animation(&self) -> DecodedAnimation {
        let mut samples: Vec<_> = self.samples.iter().collect();
        samples.sort_by_key(|sample| sample.timestamp);
        samples.dedup_by_key(|sample| sample.timestamp);
        DecodedAnimation {
            frames: samples.iter().map(|sample| sample.frame.clone()).collect(),
            frame_delays: vec![VIDEO_PREVIEW_FRAME_DELAY; samples.len()],
            source_timestamps: samples.iter().map(|sample| sample.timestamp).collect(),
        }
    }
    fn complete(&self) -> Result<PreviewStep, MediaError> {
        let animation = self.animation();
        validate_animation(&animation, self.spec)?;
        remember_poster(
            &self.source,
            self.spec.max_edge,
            &Sample {
                timestamp: animation.source_timestamps[0],
                frame: animation.frames[0].clone(),
            },
        );
        schedule_cache(self.source.clone(), self.spec, self.kind, animation.clone());
        Ok(PreviewStep::Complete(animation))
    }
    pub const fn is_refinement(&self) -> bool {
        self.next_sample >= 4 && !self.samples.is_empty()
    }
    pub fn step(&mut self, cancel: &AtomicBool) -> Result<PreviewStep, MediaError> {
        let idle = self.yielded_at.elapsed();
        self.deadline = self
            .deadline
            .checked_add(idle)
            .unwrap_or(self.expires)
            .min(self.expires);
        let result = self.step_inner(cancel);
        self.yielded_at = Instant::now();
        if result
            .as_ref()
            .is_err_and(|error| error.kind == ErrorKind::Timeout)
            && !cancel.load(Ordering::Acquire)
            && !self.samples.is_empty()
        {
            return Ok(PreviewStep::Complete(self.animation()));
        }
        result
    }
    fn sequential_samples(
        &mut self,
        duration: f64,
        cancel: &AtomicBool,
    ) -> Result<PreviewStep, MediaError> {
        let info = self.info.as_ref().expect("probed task");
        let selection = format!(
            "select='gte(t,(selected_n+{})*{:.9})'",
            self.samples.len(),
            duration * 0.85 / (self.times.len() - 1).max(1) as f64
        );
        let mut command = ffmpeg(
            &self.source,
            info,
            self.times[0],
            self.spec.max_edge,
            Some(&selection),
        );
        command
            .arg("-t")
            .arg(format!("{duration:.6}"))
            .arg("-frames:v")
            .arg((self.times.len() - self.samples.len()).max(1).to_string());
        let decoded = decode_samples(
            &mut command,
            self.times[0],
            self.spec.max_edge,
            cancel,
            self.deadline.min(Instant::now() + Duration::from_secs(2)),
        )?;
        SAMPLES.fetch_add(decoded.len() as u64, Ordering::Relaxed);
        self.samples.extend(decoded);
        if self.samples.is_empty()
            && let Some(sample) = sample_at(
                &self.source,
                info,
                Duration::ZERO,
                self.spec.max_edge,
                cancel,
                self.deadline,
            )?
        {
            self.samples.push(sample);
        }
        self.complete()
    }

    fn initialize(&mut self, cancel: &AtomicBool) -> Result<PreviewStep, MediaError> {
        let path = preview_cache_path(&self.source, self.spec, self.kind);
        if let Ok(bytes) = cache::read_bounded(&path, MAX_ENTRY) {
            match decode_bundle(&bytes, &self.source, self.spec, cancel) {
                Ok(animation) => {
                    CACHE_HITS.fetch_add(1, Ordering::Relaxed);
                    if self.kind == AnimatedSourceKind::Video {
                        remember_poster(
                            &self.source,
                            self.spec.max_edge,
                            &Sample {
                                timestamp: animation.source_timestamps[0],
                                frame: animation.frames[0].clone(),
                            },
                        );
                    }
                    return Ok(PreviewStep::Complete(animation));
                }
                Err(error) if error.kind == ErrorKind::Cancelled => return Err(error),
                Err(_) => {
                    let _ = fs::remove_file(path);
                }
            }
        }
        if self.kind == AnimatedSourceKind::Gif {
            let bytes = cache::read_bounded(&self.source.path, MAX_ENTRY)
                .map_err(|err| MediaError::new(ErrorKind::Limit, err.to_string()))?;
            let animation = decode_gif(&bytes, self.spec, cancel)?;
            schedule_cache(self.source.clone(), self.spec, self.kind, animation.clone());
            return Ok(PreviewStep::Complete(animation));
        }
        let info = probe(&self.source, cancel, self.deadline)?;
        self.times = info
            .duration
            .map_or_else(|| vec![Duration::ZERO], sample_times);
        self.order = [
            0,
            self.times.len() - 1,
            self.times.len() / 3,
            self.times.len() * 2 / 3,
        ]
        .into_iter()
        .chain(0..self.times.len())
        .fold(Vec::new(), |mut order, index| {
            if !order.contains(&index) {
                order.push(index);
            }
            order
        });
        if let Some(sample) = cached_poster(&self.source, self.spec.max_edge, cancel) {
            self.samples.push(sample);
            self.next_sample = 1;
        }
        self.info = Some(info);
        Ok(PreviewStep::Continue(
            (!self.samples.is_empty()).then(|| self.animation()),
        ))
    }

    fn step_inner(&mut self, cancel: &AtomicBool) -> Result<PreviewStep, MediaError> {
        process::check(cancel, self.deadline)?;
        if self.info.is_none() {
            return self.initialize(cancel);
        }
        let info = self.info.as_ref().expect("probed task");
        if self.next_sample <= 1
            && let Some(duration) = info.sequential_duration()
        {
            return self.sequential_samples(duration, cancel);
        }
        if self.next_sample >= self.order.len() {
            return self.complete();
        }
        let index = self.order[self.next_sample];
        self.next_sample += 1;
        let mut sample = sample_at(
            &self.source,
            info,
            self.times[index],
            self.spec.max_edge,
            cancel,
            self.deadline,
        )?;
        if sample.is_none() && self.samples.is_empty() {
            sample = sample_at(
                &self.source,
                info,
                Duration::ZERO,
                self.spec.max_edge,
                cancel,
                self.deadline,
            )?;
        }
        if let Some(sample) = sample {
            if index == 0 {
                remember_poster(&self.source, self.spec.max_edge, &sample);
            }
            SAMPLES.fetch_add(1, Ordering::Relaxed);
            self.samples.push(sample);
        }
        if self.next_sample == self.order.len() {
            return self.complete();
        }
        Ok(PreviewStep::Continue(
            (self.next_sample == 4 && !self.samples.is_empty()).then(|| self.animation()),
        ))
    }
}

fn split_pngs(bytes: &[u8]) -> Result<Vec<&[u8]>, MediaError> {
    let error = || MediaError::new(ErrorKind::InvalidMedia, "Invalid PNG frame stream");
    let mut frames = Vec::new();
    let mut offset: usize = 0;
    while offset < bytes.len() {
        let start = offset;
        if bytes.get(offset..offset + 8) != Some(b"\x89PNG\r\n\x1a\n") {
            return Err(error());
        }
        offset += 8;
        loop {
            let length = bytes.get(offset..offset + 4).ok_or_else(error)?;
            let length = u32::from_be_bytes(length.try_into().map_err(|_| error())?) as usize;
            let end = offset
                .checked_add(12)
                .and_then(|end| end.checked_add(length))
                .filter(|end| *end <= bytes.len())
                .ok_or_else(error)?;
            let last = &bytes[offset + 4..offset + 8] == b"IEND";
            offset = end;
            if last {
                break;
            }
        }
        frames.push(&bytes[start..offset]);
        if frames.len() > VIDEO_PREVIEW_FRAMES as usize {
            return Err(error());
        }
    }
    Ok(frames)
}

pub(super) fn load_or_generate_animated_preview(
    path: &Path,
    revision: u128,
    size: u64,
    kind: AnimatedSourceKind,
    cancel: &AtomicBool,
) -> Result<DecodedAnimation, String> {
    let mut task = VideoTask::new(
        SourceKey::new(path, revision, size),
        PreviewSpec::new(240),
        kind,
    );
    loop {
        match task.step(cancel).map_err(|error| error.to_string())? {
            PreviewStep::Complete(animation) => {
                flush_cache();
                return Ok(animation);
            }
            PreviewStep::Continue(_) => {}
        }
    }
}

pub(super) fn preview_cache_path(
    source: &SourceKey,
    spec: PreviewSpec,
    kind: AnimatedSourceKind,
) -> PathBuf {
    let mut hash = DefaultHasher::new();
    source.hash_cache_identity(&mut hash);
    spec.hash(&mut hash);
    kind.hash(&mut hash);
    (7_u8, VIDEO_PREVIEW_FRAMES, VIDEO_PREVIEW_FRAME_DELAY, 90_u8).hash(&mut hash);
    let hash = format!("{:016x}", hash.finish());
    let prefix = cache::source_prefix(&source.path);
    cache::register_source(&source.path, &prefix);
    let shard = cache::thumbnail_cache_dir().join(&prefix[..2]);
    let _ = fs::create_dir_all(&shard);
    shard.join(format!("{prefix}_{hash}_overview_v7.bin"))
}

fn checksum(bytes: &[u8]) -> u64 {
    let mut hash = DefaultHasher::new();
    bytes.hash(&mut hash);
    hash.finish()
}

pub(super) fn encode_bundle(
    animation: &DecodedAnimation,
    source: &SourceKey,
    spec: PreviewSpec,
    kind: AnimatedSourceKind,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, MediaError> {
    #[cfg(feature = "profiling")]
    puffin::profile_scope!("lwa_fm::assets::overview_cache_encode");
    validate_animation(animation, spec)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&source.revision.to_le_bytes());
    bytes.extend_from_slice(&source.size.to_le_bytes());
    bytes.extend_from_slice(&spec.max_edge.to_le_bytes());
    bytes.extend_from_slice(&(animation.frames.len() as u32).to_le_bytes());
    for (index, frame) in animation.frames.iter().enumerate() {
        process::check(cancel, deadline)?;
        let image = image::RgbaImage::from_raw(
            frame.width as u32,
            frame.height as u32,
            frame.rgba.to_vec(),
        )
        .ok_or_else(|| MediaError::new(ErrorKind::InvalidMedia, "Invalid frame pixels"))?;
        let dynamic = image::DynamicImage::ImageRgba8(image);
        let mut encoded = Vec::new();
        if kind == AnimatedSourceKind::Video {
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 90)
                .encode_image(&dynamic.to_rgb8())
                .map_err(|err| MediaError::new(ErrorKind::InvalidMedia, err.to_string()))?;
        } else {
            dynamic
                .write_to(&mut Cursor::new(&mut encoded), image::ImageFormat::Png)
                .map_err(|err| MediaError::new(ErrorKind::InvalidMedia, err.to_string()))?;
        }
        bytes.extend_from_slice(
            &(animation.source_timestamps[index].as_millis() as u64).to_le_bytes(),
        );
        bytes.extend_from_slice(&(animation.frame_delays[index].as_millis() as u64).to_le_bytes());
        bytes.extend_from_slice(&(frame.width as u32).to_le_bytes());
        bytes.extend_from_slice(&(frame.height as u32).to_le_bytes());
        bytes.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&checksum(&encoded).to_le_bytes());
        bytes.extend_from_slice(&encoded);
        if bytes.len() > MAX_ENTRY {
            return Err(MediaError::new(
                ErrorKind::Limit,
                "Overview cache exceeds byte limit",
            ));
        }
    }
    Ok(bytes)
}

fn read_array<const N: usize>(reader: &mut Cursor<&[u8]>) -> Result<[u8; N], MediaError> {
    let mut bytes = [0; N];
    reader
        .read_exact(&mut bytes)
        .map_err(|_| MediaError::new(ErrorKind::InvalidMedia, "Truncated overview cache"))?;
    Ok(bytes)
}
pub(super) fn decode_bundle(
    bytes: &[u8],
    source: &SourceKey,
    spec: PreviewSpec,
    cancel: &AtomicBool,
) -> Result<DecodedAnimation, MediaError> {
    decode_bundle_prefix(bytes, source, spec, cancel, false)
}

fn decode_bundle_prefix(
    bytes: &[u8],
    source: &SourceKey,
    spec: PreviewSpec,
    cancel: &AtomicBool,
    first_only: bool,
) -> Result<DecodedAnimation, MediaError> {
    #[cfg(feature = "profiling")]
    puffin::profile_scope!("lwa_fm::assets::overview_cache_decode");
    let error = || MediaError::new(ErrorKind::InvalidMedia, "Invalid overview cache");
    if bytes.len() > MAX_ENTRY {
        return Err(error());
    }
    let mut reader = Cursor::new(bytes);
    if &read_array::<8>(&mut reader)? != MAGIC
        || u128::from_le_bytes(read_array(&mut reader)?) != source.revision
        || u64::from_le_bytes(read_array(&mut reader)?) != source.size
        || u32::from_le_bytes(read_array(&mut reader)?) != spec.max_edge
    {
        return Err(error());
    }
    let count = u32::from_le_bytes(read_array(&mut reader)?) as usize;
    if count == 0 || count > VIDEO_PREVIEW_FRAMES as usize || spec.max_edge > MAX_EDGE {
        return Err(error());
    }
    let mut animation = DecodedAnimation {
        frames: Vec::new(),
        frame_delays: Vec::new(),
        source_timestamps: Vec::new(),
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    for _ in 0..count {
        process::check(cancel, deadline)?;
        let timestamp = u64::from_le_bytes(read_array(&mut reader)?);
        let delay = u64::from_le_bytes(read_array(&mut reader)?);
        let width = u32::from_le_bytes(read_array(&mut reader)?);
        let height = u32::from_le_bytes(read_array(&mut reader)?);
        let length = u32::from_le_bytes(read_array(&mut reader)?) as usize;
        let hash = u64::from_le_bytes(read_array(&mut reader)?);
        if width == 0
            || height == 0
            || width.max(height) > spec.max_edge
            || !(1..=MAX_FRAME_DELAY_MS * 120).contains(&delay)
        {
            return Err(error());
        }
        let start = reader.position() as usize;
        let end = start
            .checked_add(length)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(error)?;
        let encoded = &bytes[start..end];
        if checksum(encoded) != hash {
            return Err(error());
        }
        let frame = image_from_png(encoded, spec.max_edge)?;
        if frame.width != width as usize || frame.height != height as usize {
            return Err(error());
        }
        reader.set_position(end as u64);
        animation.frames.push(frame);
        animation.frame_delays.push(Duration::from_millis(delay));
        animation
            .source_timestamps
            .push(Duration::from_millis(timestamp));
        if first_only {
            break;
        }
    }
    if !first_only && reader.position() as usize != bytes.len() {
        return Err(error());
    }
    validate_animation(&animation, spec)?;
    Ok(animation)
}

fn validate_animation(animation: &DecodedAnimation, spec: PreviewSpec) -> Result<(), MediaError> {
    if animation.frames.is_empty()
        || animation.frames.len() > VIDEO_PREVIEW_FRAMES as usize
        || animation.frame_delays.len() != animation.frames.len()
        || animation.source_timestamps.len() != animation.frames.len()
        || animation
            .source_timestamps
            .windows(2)
            .any(|times| times[0] > times[1])
        || animation.frame_delays.iter().any(|delay| {
            delay.is_zero() || delay.as_millis() > u128::from(MAX_FRAME_DELAY_MS) * 120
        })
        || animation.byte_len() > MAX_DECODED
        || animation.frames.iter().any(|frame| {
            frame.width == 0
                || frame.height == 0
                || frame.width.max(frame.height) > spec.max_edge as usize
                || frame.rgba.len() != frame.width * frame.height * 4
        })
    {
        return Err(MediaError::new(
            ErrorKind::Limit,
            "Overview violates frame or memory limits",
        ));
    }
    Ok(())
}

pub(super) fn decode_gif(
    bytes: &[u8],
    spec: PreviewSpec,
    cancel: &AtomicBool,
) -> Result<DecodedAnimation, MediaError> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut decoder = GifDecoder::new(Cursor::new(bytes))
        .map_err(|err| MediaError::new(ErrorKind::InvalidMedia, err.to_string()))?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(32 * 1024 * 1024);
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    decoder
        .set_limits(limits)
        .map_err(|err| MediaError::new(ErrorKind::Limit, err.to_string()))?;
    let mut animation = DecodedAnimation {
        frames: Vec::new(),
        frame_delays: Vec::new(),
        source_timestamps: Vec::new(),
    };
    let mut elapsed = Duration::ZERO;
    for frame in decoder.into_frames().take(120) {
        process::check(cancel, deadline)?;
        let frame =
            frame.map_err(|err| MediaError::new(ErrorKind::InvalidMedia, err.to_string()))?;
        let delay: Duration = frame.delay().into();
        let delay = if delay.is_zero() {
            Duration::from_millis(100)
        } else {
            delay
        };
        let image = image::DynamicImage::ImageRgba8(frame.into_buffer())
            .thumbnail(spec.max_edge, spec.max_edge);
        animation
            .frames
            .push(decoded_from_dynamic(&image, "gif-frame".into()));
        animation.frame_delays.push(delay);
        animation.source_timestamps.push(elapsed);
        elapsed += delay;
        if animation.frames.len() > VIDEO_PREVIEW_FRAMES as usize {
            let index = (0..animation.frames.len() - 1)
                .min_by_key(|index| {
                    animation.frame_delays[*index] + animation.frame_delays[*index + 1]
                })
                .expect("GIF delay pair");
            let removed_delay = animation.frame_delays.remove(index + 1);
            animation.frame_delays[index] += removed_delay;
            animation.frames.remove(index + 1);
            animation.source_timestamps.remove(index + 1);
        }
    }
    validate_animation(&animation, spec)?;
    Ok(animation)
}

const MAX_WRITE_BYTES: usize = 16 * 1024 * 1024;
const MAX_WRITES: usize = 64;
pub(super) static WRITES_DROPPED: AtomicU64 = AtomicU64::new(0);
pub(super) static WRITES_COALESCED: AtomicU64 = AtomicU64::new(0);
pub(super) static WRITES_FAILED: AtomicU64 = AtomicU64::new(0);
enum CachePayload {
    Preview(PreviewSpec, AnimatedSourceKind, DecodedAnimation),
    Poster(u32, Sample),
}
struct CacheWrite {
    source: SourceKey,
    path: PathBuf,
    payload: CachePayload,
}
impl CacheWrite {
    fn byte_len(&self) -> usize {
        match &self.payload {
            CachePayload::Preview(_, _, animation) => animation.byte_len(),
            CachePayload::Poster(_, sample) => sample.frame.byte_len(),
        }
    }
    const fn poster(&self) -> bool {
        matches!(self.payload, CachePayload::Poster(_, _))
    }
}
#[derive(Default)]
struct WriterState {
    queue: VecDeque<CacheWrite>,
    bytes: usize,
    active: bool,
}
#[derive(Default)]
struct CacheWriter {
    state: Mutex<WriterState>,
    ready: Condvar,
}
impl CacheWriter {
    fn enqueue(&self, write: CacheWrite) {
        let size = write.byte_len();
        let mut state = self.state.lock().expect("cache writer queue");
        if let Some(index) = state.queue.iter().position(|old| old.path == write.path) {
            let old = state.queue.remove(index).expect("coalesced write");
            state.bytes -= old.byte_len();
            WRITES_COALESCED.fetch_add(1, Ordering::Relaxed);
        }
        while state.bytes + size > MAX_WRITE_BYTES || state.queue.len() >= MAX_WRITES {
            let victim = state
                .queue
                .iter()
                .position(|old| !old.poster())
                .or_else(|| {
                    write
                        .poster()
                        .then_some(0)
                        .filter(|_| !state.queue.is_empty())
                });
            let Some(index) = victim else {
                WRITES_DROPPED.fetch_add(1, Ordering::Relaxed);
                return;
            };
            let old = state.queue.remove(index).expect("evicted write");
            state.bytes -= old.byte_len();
            WRITES_DROPPED.fetch_add(1, Ordering::Relaxed);
        }
        state.bytes += size;
        state.queue.push_back(write);
        drop(state);
        self.ready.notify_all();
    }
}
static WRITER: LazyLock<Arc<CacheWriter>> = LazyLock::new(|| {
    let writer = Arc::new(CacheWriter::default());
    let worker = Arc::clone(&writer);
    std::thread::spawn(move || {
        loop {
            let write = {
                let mut state = worker.state.lock().expect("cache writer queue");
                while state.queue.is_empty() {
                    state = worker.ready.wait(state).expect("cache writer wait");
                }
                let write = state.queue.pop_front().expect("pending write");
                state.bytes -= write.byte_len();
                state.active = true;
                write
            };
            if let Err(error) = write_cache(write) {
                WRITES_FAILED.fetch_add(1, Ordering::Relaxed);
                log::warn!("Media cache write failed: {error}");
            }
            worker.state.lock().expect("cache writer queue").active = false;
            worker.ready.notify_all();
        }
    });
    writer
});
fn write_cache(write: CacheWrite) -> Result<(), MediaError> {
    if !write.source.unchanged() {
        return Ok(());
    }
    let (bytes, metadata) = match write.payload {
        CachePayload::Preview(spec, kind, animation) => (
            encode_bundle(
                &animation,
                &write.source,
                spec,
                kind,
                &AtomicBool::new(false),
            )?,
            None,
        ),
        CachePayload::Poster(edge, sample) => {
            let image = image::RgbaImage::from_raw(
                sample.frame.width as u32,
                sample.frame.height as u32,
                sample.frame.rgba.to_vec(),
            )
            .ok_or_else(|| MediaError::new(ErrorKind::InvalidMedia, "Invalid poster pixels"))?;
            let mut bytes = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 90)
                .encode_image(&image::DynamicImage::ImageRgba8(image).to_rgb8())
                .map_err(|error| MediaError::new(ErrorKind::InvalidMedia, error.to_string()))?;
            let mut metadata = b"DFPS0001".to_vec();
            metadata.extend_from_slice(&write.source.revision.to_le_bytes());
            metadata.extend_from_slice(&write.source.size.to_le_bytes());
            metadata.extend_from_slice(&edge.to_le_bytes());
            metadata.extend_from_slice(&(sample.timestamp.as_millis() as u64).to_le_bytes());
            metadata.extend_from_slice(&checksum(&bytes).to_le_bytes());
            (bytes, Some(metadata))
        }
    };
    let _publication = PUBLICATION.lock().expect("asset publication");
    if !write.source.unchanged() {
        return Ok(());
    }
    if !cache::atomic_save_bytes(&bytes, &write.path)
        || metadata.as_ref().is_some_and(|metadata| {
            !cache::atomic_save_bytes(metadata, &write.path.with_extension("poster"))
        })
    {
        return Err(MediaError::new(
            ErrorKind::Unavailable,
            "Could not publish cache entry",
        ));
    }
    Ok(())
}
fn schedule_cache(
    source: SourceKey,
    spec: PreviewSpec,
    kind: AnimatedSourceKind,
    animation: DecodedAnimation,
) {
    let path = preview_cache_path(&source, spec, kind);
    WRITER.enqueue(CacheWrite {
        source,
        path,
        payload: CachePayload::Preview(spec, kind, animation),
    });
}
pub(super) fn schedule_poster_cache(
    source: SourceKey,
    path: PathBuf,
    edge: u32,
    frame: DecodedImage,
) {
    let Some(timestamp) = POSTERS
        .lock()
        .expect("poster cache")
        .get(&(source.clone(), edge))
        .map(|sample| sample.timestamp)
    else {
        return;
    };
    WRITER.enqueue(CacheWrite {
        source,
        path,
        payload: CachePayload::Poster(edge, Sample { timestamp, frame }),
    });
}
pub(super) fn flush_cache() {
    let state = WRITER.state.lock().expect("cache writer queue");
    let _ = WRITER
        .ready
        .wait_timeout_while(state, Duration::from_secs(5), |state| {
            state.active || !state.queue.is_empty()
        })
        .expect("cache flush wait");
}

#[cfg(test)]
#[path = "integration_tests.rs"]
mod integration_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn batching_requires_bounded_known_decode_work() {
        let mut info = ProbeInfo {
            duration: Some(10.0),
            pixels: 1280 * 720,
            frame_rate: Some(30.0),
            ..ProbeInfo::default()
        };
        assert_eq!(info.sequential_duration(), Some(10.0));
        info.pixels = 3840 * 2160;
        assert_eq!(info.sequential_duration(), None);
        info.pixels = 1280 * 720;
        info.frame_rate = Some(120.0);
        assert_eq!(info.sequential_duration(), None);
        info.frame_rate = None;
        assert_eq!(info.sequential_duration(), None);
        assert_eq!(parse_frame_rate("30000/1001"), Some(30000.0 / 1001.0));
        for invalid in ["0/0", "30/0", "NaN", "-1", "0"] {
            assert_eq!(parse_frame_rate(invalid), None);
        }
    }
    #[test]
    fn rgb_transport_preserves_whitespace_pixels_and_rejects_bad_frames() {
        let bytes = b"P6\n1 1\n255\n\x20\x0a\x09P6\n1 1\n255\n\x01\x02\x03";
        let frames = ppm_frames(bytes, 1).expect("RGB fixture");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].rgba.as_ref(), &[32, 10, 9, 255]);
        assert_eq!(frames[1].rgba.as_ref(), &[1, 2, 3, 255]);
        for invalid in [
            b"P6\n1 1\n255\n\x01".as_slice(),
            b"P6\n0 1\n255\n",
            b"P6\n2 1\n255\n",
        ] {
            assert!(ppm_frames(invalid, 1).is_err());
        }
    }
    #[test]
    fn poster_reads_only_the_first_bundle_frame() {
        let source = SourceKey::new(Path::new("prefix.mp4"), 1, 10);
        let spec = PreviewSpec::new(160);
        let frame = decoded_from_dynamic(
            &image::DynamicImage::ImageRgba8(image::RgbaImage::new(2, 2)),
            "fixture".into(),
        );
        let animation = DecodedAnimation {
            frames: vec![frame.clone(), frame],
            frame_delays: vec![VIDEO_PREVIEW_FRAME_DELAY; 2],
            source_timestamps: vec![Duration::ZERO, Duration::from_secs(1)],
        };
        let bytes = encode_bundle(
            &animation,
            &source,
            spec,
            AnimatedSourceKind::Video,
            &AtomicBool::new(false),
        )
        .expect("asset fixture operation");
        let path = preview_cache_path(&source, spec, AnimatedSourceKind::Video);
        assert!(cache::atomic_save_bytes(&bytes, &path));
        let prefix = cache::read_overview_first(&path, MAX_ENTRY).expect("asset fixture operation");
        assert!(prefix.len() < bytes.len());
        assert_eq!(
            decode_bundle_prefix(&prefix, &source, spec, &AtomicBool::new(false), true)
                .expect("asset fixture operation")
                .frames
                .len(),
            1
        );
        cache::purge_sources(&[source.path], false);
    }
    #[test]
    fn write_queue_coalesces_and_preserves_posters_under_pressure() {
        let writer = CacheWriter::default();
        let source = SourceKey::new(Path::new("queue.mp4"), 0, 0);
        let frame = decoded_from_dynamic(
            &image::DynamicImage::ImageRgba8(image::RgbaImage::new(2, 2)),
            "fixture".into(),
        );
        for index in 0..MAX_WRITES {
            writer.enqueue(CacheWrite {
                source: source.clone(),
                path: PathBuf::from(format!("{index}.jpg")),
                payload: CachePayload::Poster(
                    160,
                    Sample {
                        timestamp: Duration::ZERO,
                        frame: frame.clone(),
                    },
                ),
            });
        }
        writer.enqueue(CacheWrite {
            source: source.clone(),
            path: PathBuf::from("0.jpg"),
            payload: CachePayload::Poster(
                160,
                Sample {
                    timestamp: Duration::from_secs(1),
                    frame: frame.clone(),
                },
            ),
        });
        writer.enqueue(CacheWrite {
            source,
            path: PathBuf::from("preview.bin"),
            payload: CachePayload::Preview(
                PreviewSpec::new(160),
                AnimatedSourceKind::Video,
                DecodedAnimation {
                    frames: vec![frame],
                    source_timestamps: vec![Duration::ZERO],
                    frame_delays: vec![VIDEO_PREVIEW_FRAME_DELAY],
                },
            ),
        });
        let state = writer.state.lock().expect("asset fixture operation");
        assert_eq!(state.queue.len(), MAX_WRITES);
        assert!(state.bytes <= MAX_WRITE_BYTES);
        assert!(state.queue.iter().all(CacheWrite::poster));
        assert_eq!(
            state
                .queue
                .iter()
                .filter(|write| write.path == Path::new("0.jpg"))
                .count(),
            1
        );
    }
    #[test]
    fn refinement_yields_to_visible_posters_and_queue_wait_preserves_budget() {
        use super::super::{AssetJob, AssetJobClass, JobScheduler};
        let source = SourceKey::new(Path::new("queued.mp4"), 0, 0);
        let mut task = VideoTask::new(source, PreviewSpec::new(160), AnimatedSourceKind::Video);
        task.next_sample = 4;
        task.samples.push(Sample {
            timestamp: Duration::ZERO,
            frame: decoded_from_dynamic(
                &image::DynamicImage::ImageRgba8(image::RgbaImage::new(2, 2)),
                "fixture".into(),
            ),
        });
        task.info = Some(ProbeInfo::default());
        task.deadline = Instant::now()
            .checked_sub(Duration::from_secs(2))
            .expect("fixture instant");
        task.yielded_at = Instant::now()
            .checked_sub(Duration::from_secs(10))
            .expect("fixture instant");
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
                icon_size: super::super::IconSize::Small,
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
    fn backend_change_retires_capabilities_and_memory() {
        if std::env::var_os("LWA_BACKEND_REFRESH_TEST_CHILD").is_none() {
            let mut command =
                Command::new(std::env::current_exe().expect("asset fixture operation"));
            command
                .args([
                    "--exact",
                    "app::assets::media::tests::backend_change_retires_capabilities_and_memory",
                    "--nocapture",
                ])
                .env("LWA_BACKEND_REFRESH_TEST_CHILD", "1");
            process::run(
                &mut command,
                &AtomicBool::new(false),
                Instant::now() + Duration::from_secs(5),
                64 * 1024,
            )
            .expect("asset fixture operation");
            return;
        }
        let original = BACKEND.lock().expect("asset fixture operation").clone();
        let epoch = BACKEND_EPOCH.load(Ordering::Acquire);
        *HDR_CAPABILITIES.lock().expect("asset fixture operation") =
            Some((original.revision, true));
        let mut changed = original.clone();
        changed.revision = original.revision.wrapping_add(1);
        apply_backend(changed);
        assert!(
            HDR_CAPABILITIES
                .lock()
                .expect("asset fixture operation")
                .is_none()
        );
        assert!(BACKEND_EPOCH.load(Ordering::Acquire) > epoch);
        apply_backend(original);
        refresh_backend();
    }
    #[test]
    fn invalidation_retires_memory_disk_and_pending_publication() {
        let path =
            std::env::temp_dir().join(format!("lwa_invalidation_{}.mp4", std::process::id()));
        fs::write(&path, b"source").expect("asset fixture operation");
        let metadata = fs::metadata(&path).expect("asset fixture operation");
        let revision = metadata
            .modified()
            .expect("asset fixture operation")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("asset fixture operation")
            .as_nanos();
        let source = SourceKey::new(&path, revision, metadata.len());
        let spec = PreviewSpec::new(160);
        let cache_path = preview_cache_path(&source, spec, AnimatedSourceKind::Video);
        cache::atomic_save_bytes(b"old", &cache_path);
        let sample = Sample {
            timestamp: Duration::ZERO,
            frame: decoded_from_dynamic(
                &image::DynamicImage::ImageRgba8(image::RgbaImage::new(2, 2)),
                "fixture".into(),
            ),
        };
        remember_poster(&source, 160, &sample);
        assert!(source.unchanged());
        invalidate_sources(std::slice::from_ref(&source.path), false);
        assert!(!source.unchanged());
        assert!(!cache_path.exists());
        let fresh = SourceKey::new(&path, revision, metadata.len());
        assert!(fresh.unchanged());
        assert!(cached_poster(&fresh, 160, &AtomicBool::new(false)).is_none());
        schedule_poster_cache(source, cache_path.clone(), 160, sample.frame);
        flush_cache();
        assert!(!cache_path.exists());
        fs::remove_file(path).expect("asset fixture operation");
    }
    #[test]
    fn timestamps_cover_timeline_and_handle_short_or_unknown_sources() {
        let times = sample_times(600.0);
        assert_eq!(times.len(), 12);
        assert_eq!(times[0], Duration::from_secs(30));
        assert_eq!(times[11], Duration::from_secs(540));
        assert!(
            sample_times(0.5)
                .iter()
                .all(|time| time.as_secs_f64() < 0.5)
        );
        assert_eq!(sample_times(f64::NAN), vec![Duration::ZERO]);
        assert_eq!(sample_times(f64::MAX), vec![Duration::ZERO]);
        assert_eq!(sample_times(0.0005), vec![Duration::ZERO]);
    }
    #[test]
    fn probe_selects_video_instead_of_cover_art_and_rejects_invalid_duration() {
        let info = parse_probe(br#"{"format":{"duration":"NaN"},"streams":[{"codec_type":"video","index":0,"disposition":{"attached_pic":1}},{"codec_type":"video","index":2,"duration":"0.5","color_transfer":"smpte2084"}]}"#).expect("probe");
        assert_eq!(info.stream, 2);
        assert_eq!(info.duration, Some(0.5));
        assert!(info.hdr);
        assert!(
            parse_probe(br#"{"streams":[{"codec_type":"video","width":100000,"height":100000}]}"#)
                .is_err()
        );
    }

    #[test]
    fn probe_prefers_default_stream_and_its_duration_tag() {
        let info = parse_probe(br#"{"format":{"duration":"20"},"streams":[{"index":0,"codec_type":"video","width":1920,"height":1080},{"index":1,"codec_type":"video","width":640,"height":360,"disposition":{"default":1},"tags":{"DURATION":"00:00:00.500000000"}}]}"#).expect("asset fixture operation");
        assert_eq!(info.stream, 1);
        assert_eq!(info.duration, Some(0.5));
        assert_eq!(parse_clock_duration("00:60:01"), None);
        assert_eq!(parse_clock_duration("NaN:00:01"), None);
    }

    #[test]
    fn ffmpeg_header_probe_skips_cover_art_and_handles_unknown_duration() {
        let header = "Duration: 01:02:03.50, start: 0.0\nStream #0:0: Video: png, rgb24, 640x360 (attached pic)\nStream #0:1: Audio: aac\nStream #0:2(und): Video: hevc, yuv420p10le(bt2020nc/bt2020/smpte2084), 1920x1080 [SAR 1:1]";
        let info = parse_probe_stderr(header).expect("header probe");
        assert_eq!(info.duration, Some(3723.5));
        assert_eq!(info.stream, 2);
        assert!(info.hdr);
        let unknown = parse_probe_stderr(
            "Duration: N/A, start: 0.0\nStream #0:1: Video: h264, yuv420p, 640x360 [SAR 1:1]",
        )
        .expect("unknown duration");
        assert_eq!(unknown.duration, None);
        assert_eq!(unknown.stream, 1);
    }

    #[test]
    fn gif_preserves_fast_frames_and_long_holds() {
        let animation = DecodedAnimation {
            frames: vec![
                DecodedImage {
                    name: "first".into(),
                    width: 2,
                    height: 2,
                    rgba: vec![255; 16].into()
                };
                2
            ],
            frame_delays: vec![Duration::from_millis(10), Duration::from_secs(10)],
            source_timestamps: vec![Duration::ZERO, Duration::from_millis(10)],
        };
        let bytes = super::super::encode_animation_gif(&animation).expect("GIF fixture");
        let decoded =
            decode_gif(&bytes, PreviewSpec::new(160), &AtomicBool::new(false)).expect("GIF decode");
        assert_eq!(
            decoded.frame_delays.iter().copied().sum::<Duration>(),
            Duration::from_millis(10010)
        );
    }
    #[test]
    fn bundle_validates_identity_checksum_dimensions_and_lengths() {
        let source = SourceKey::new(Path::new("clip.mp4"), 1, 10);
        let spec = PreviewSpec::new(160);
        let animation = DecodedAnimation {
            frames: vec![DecodedImage {
                name: "frame".into(),
                width: 2,
                height: 2,
                rgba: vec![200; 16].into(),
            }],
            frame_delays: vec![VIDEO_PREVIEW_FRAME_DELAY],
            source_timestamps: vec![Duration::from_secs(3)],
        };
        let cancel = AtomicBool::new(false);
        let bytes = encode_bundle(
            &animation,
            &source,
            spec,
            AnimatedSourceKind::Video,
            &cancel,
        )
        .expect("encode");
        let decoded = decode_bundle(&bytes, &source, spec, &cancel).expect("decode");
        assert_eq!(decoded.source_timestamps, animation.source_timestamps);
        assert!(
            decode_bundle(
                &bytes,
                &SourceKey::new(Path::new("clip.mp4"), 2, 10),
                spec,
                &cancel
            )
            .is_err()
        );
        assert!(decode_bundle(&bytes[..bytes.len() - 1], &source, spec, &cancel).is_err());
        let mut corrupt = bytes;
        *corrupt.last_mut().expect("bytes") ^= 1;
        assert!(decode_bundle(&corrupt, &source, spec, &cancel).is_err());
    }
}
