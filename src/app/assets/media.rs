//! Full-color timeline overviews. Each sparse sample yields back to the asset scheduler.
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, mpsc};
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
const MAGIC: &[u8; 8] = b"DFOV0006";
// Backend upgrades can change color conversion; retire their older video caches.
static BACKEND_REVISION: LazyLock<u64> = LazyLock::new(|| {
    let mut path = ffmpeg_sidecar::paths::ffmpeg_path();
    if !path.is_absolute()
        && let Some(search) = std::env::var_os("PATH")
    {
        let name = if cfg!(windows) {
            path.with_extension("exe")
        } else {
            path.clone()
        };
        if let Some(found) = std::env::split_paths(&search)
            .map(|directory| directory.join(&name))
            .find(|candidate| candidate.is_file())
        {
            path = found;
        }
    }
    let mut hash = DefaultHasher::new();
    path.hash(&mut hash);
    if let Ok(metadata) = fs::metadata(path) {
        metadata.len().hash(&mut hash);
        metadata.modified().ok().hash(&mut hash);
    }
    hash.finish()
});
pub(super) fn backend_revision() -> u64 {
    *BACKEND_REVISION
}
pub(super) static CACHE_HITS: AtomicU64 = AtomicU64::new(0);
pub(super) static SAMPLES: AtomicU64 = AtomicU64::new(0);
pub(super) static BACKEND_EPOCH: AtomicU64 = AtomicU64::new(0);
// A poster and the first overview sample have the same timestamp contract.
// Retain only first samples, with a separate small byte budget.
static POSTERS: LazyLock<Mutex<LruCache<(SourceKey, u32), DecodedImage>>> = LazyLock::new(|| {
    Mutex::new(LruCache::new(
        std::num::NonZero::new(16).expect("poster capacity"),
    ))
});

fn remember_poster(source: &SourceKey, edge: u32, frame: &DecodedImage) {
    let mut posters = POSTERS.lock().expect("poster cache mutex");
    posters.push((source.clone(), edge), frame.clone());
    while posters
        .iter()
        .map(|(_, image)| image.byte_len())
        .sum::<usize>()
        > 4 * 1024 * 1024
    {
        posters.pop_lru();
    }
    drop(posters);
}

fn cached_poster(source: &SourceKey, edge: u32, cancel: &AtomicBool) -> Option<DecodedImage> {
    let tiers = [edge, 160, 240, 360, MAX_EDGE];
    for tier in tiers.into_iter().filter(|tier| *tier >= edge) {
        let memory = POSTERS
            .lock()
            .expect("poster cache mutex")
            .get(&(source.clone(), tier))
            .cloned();
        let frame = memory.or_else(|| {
            let spec = PreviewSpec::new(tier);
            let bytes = cache::read_bounded(
                &preview_cache_path(source, spec, AnimatedSourceKind::Video),
                MAX_ENTRY,
            )
            .ok()?;
            let animation = decode_bundle_prefix(&bytes, source, spec, cancel, true).ok()?;
            CACHE_HITS.fetch_add(1, Ordering::Relaxed);
            animation.frames.into_iter().next()
        });
        if let Some(frame) = frame {
            if tier == edge {
                return Some(frame);
            }
            let image = image::RgbaImage::from_raw(
                frame.width as u32,
                frame.height as u32,
                frame.rgba.to_vec(),
            )?;
            return Some(decoded_from_dynamic(
                &image::DynamicImage::ImageRgba8(image).thumbnail(edge, edge),
                "overview-poster".into(),
            ));
        }
    }
    None
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(super) struct SourceKey {
    pub path: PathBuf,
    pub revision: u128,
    pub size: u64,
}
impl SourceKey {
    pub fn new(path: &Path, revision: u128, size: u64) -> Self {
        Self {
            path: crate::helper::normalize_path(path),
            revision,
            size,
        }
    }
    pub fn unchanged(&self) -> bool {
        fs::metadata(&self.path).is_ok_and(|metadata| {
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
    pub stream: usize,
    pub hdr: bool,
    pub tone_map: bool,
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
                BACKEND_EPOCH.fetch_add(1, Ordering::Release);
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
    let mut command = Command::new(ffmpeg_sidecar::ffprobe::ffprobe_path());
    command.args(["-v", "error", "-show_entries", "format=duration:stream=index,codec_type,duration,width,height,color_transfer:stream_disposition=attached_pic", "-of", "json"]).arg(&source.path);
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
        if info.hdr {
            info.tone_map = hdr_filters_available(cancel, deadline)?;
            if !info.tone_map {
                log::warn!(
                    "HDR thumbnail tone mapping requires FFmpeg with zscale and tonemap: {}",
                    source.path.display()
                );
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
        .find(|stream| {
            stream["codec_type"] == "video"
                && stream["disposition"]["attached_pic"].as_u64().unwrap_or(0) == 0
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
        duration: valid_duration(&stream["duration"])
            .or_else(|| valid_duration(&value["format"]["duration"])),
        stream: stream["index"].as_u64().unwrap_or(0) as usize,
        hdr: matches!(
            stream["color_transfer"].as_str(),
            Some("smpte2084" | "arib-std-b67")
        ),
        tone_map: false,
    })
}

// Some sidecar distributions (notably macOS) contain FFmpeg without FFprobe.
// Reading its bounded input header still supplies duration and the selected video stream.
fn probe_with_ffmpeg(
    source: &SourceKey,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<ProbeInfo, MediaError> {
    let mut command = Command::new(ffmpeg_sidecar::paths::ffmpeg_path());
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
        duration,
        stream: stream_index,
        hdr: stream.contains("smpte2084") || stream.contains("arib-std-b67"),
        tone_map: false,
    })
}

fn hdr_filters_available(cancel: &AtomicBool, deadline: Instant) -> Result<bool, MediaError> {
    static AVAILABLE: Mutex<Option<bool>> = Mutex::new(None);
    let cached = *AVAILABLE.lock().expect("color filter capability mutex");
    if let Some(available) = cached {
        return Ok(available);
    }
    let mut command = Command::new(ffmpeg_sidecar::paths::ffmpeg_path());
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
    *AVAILABLE.lock().expect("color filter capability mutex") = Some(available);
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

fn ffmpeg(
    source: &SourceKey,
    info: &ProbeInfo,
    timestamp: Duration,
    edge: u32,
    selection: Option<&str>,
) -> Command {
    let mut command = Command::new(ffmpeg_sidecar::paths::ffmpeg_path());
    let mut scale = format!(
        "scale=w='max(1,trunc(iw*sar*min(1,min({edge}/(iw*sar),{edge}/ih))))':h='max(1,trunc(ih*min(1,min({edge}/(iw*sar),{edge}/ih))))':flags=lanczos,setsar=1"
    );
    if info.tone_map {
        // Tone map only after reducing geometry, retaining linear floating-point RGB.
        scale.push_str(",format=yuv444p16le,zscale=transfer=linear:npl=100,format=gbrpf32le,zscale=primaries=bt709,tonemap=tonemap=hable:desat=2,zscale=transfer=iec61966-2-1:matrix=bt709:range=full,format=rgb24");
    }
    command
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-max_alloc",
            "134217728",
            "-filter_threads",
            "1",
            "-threads",
            "1",
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
        .arg(selection.map_or_else(|| scale.clone(), |selection| format!("{selection},{scale}")));
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

pub(super) fn poster(
    source: &SourceKey,
    edge: u32,
    cancel: &AtomicBool,
) -> Result<DecodedImage, MediaError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    process::check(cancel, deadline)?;
    if let Some(frame) = cached_poster(source, edge, cancel) {
        return Ok(frame);
    }
    let info = probe(source, cancel, deadline)?;
    let timestamp = info
        .duration
        .map_or(Duration::ZERO, |duration| sample_times(duration)[0]);
    let mut command = ffmpeg(source, &info, timestamp, edge, None);
    command.args([
        "-frames:v",
        "1",
        "-c:v",
        "png",
        "-f",
        "image2pipe",
        "pipe:1",
    ]);
    let mut bytes = process::run(
        &mut command,
        cancel,
        deadline.min(Instant::now() + Duration::from_secs(2)),
        MAX_ENTRY,
    )?;
    let original_sample = if bytes.is_empty() && !timestamp.is_zero() {
        let mut retry = ffmpeg(source, &info, Duration::ZERO, edge, None);
        retry.args([
            "-frames:v",
            "1",
            "-c:v",
            "png",
            "-f",
            "image2pipe",
            "pipe:1",
        ]);
        bytes = process::run(
            &mut retry,
            cancel,
            deadline.min(Instant::now() + Duration::from_secs(2)),
            MAX_ENTRY,
        )?;
        false
    } else {
        info.duration.is_some()
    };
    let frame = image_from_png(&bytes, edge)?;
    if original_sample {
        remember_poster(source, edge, &frame);
    }
    Ok(frame)
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
    info: Option<ProbeInfo>,
    times: Vec<Duration>,
    order: Vec<usize>,
    samples: Vec<(usize, DecodedImage)>,
}
impl VideoTask {
    pub fn new(source: SourceKey, spec: PreviewSpec, kind: AnimatedSourceKind) -> Self {
        Self {
            source,
            spec,
            kind,
            deadline: Instant::now() + Duration::from_secs(8),
            info: None,
            times: Vec::new(),
            order: Vec::new(),
            samples: Vec::new(),
        }
    }
    fn animation(&self) -> DecodedAnimation {
        let mut samples: Vec<_> = self.samples.iter().collect();
        samples.sort_by_key(|(index, _)| *index);
        DecodedAnimation {
            frames: samples.iter().map(|(_, frame)| frame.clone()).collect(),
            frame_delays: vec![VIDEO_PREVIEW_FRAME_DELAY; samples.len()],
            source_timestamps: samples
                .iter()
                .map(|(index, _)| self.times[*index])
                .collect(),
        }
    }
    #[allow(
        clippy::too_many_lines,
        reason = "bounded cache, probe, short-clip, and sparse-sample stages"
    )]
    pub fn step(&mut self, cancel: &AtomicBool) -> Result<PreviewStep, MediaError> {
        #[cfg(feature = "profiling")]
        puffin::profile_scope!("lwa_fm::assets::overview_step");
        process::check(cancel, self.deadline)?;
        if self.info.is_none() {
            let path = preview_cache_path(&self.source, self.spec, self.kind);
            if let Ok(bytes) = cache::read_bounded(&path, MAX_ENTRY) {
                match decode_bundle(&bytes, &self.source, self.spec, cancel) {
                    Ok(animation) => {
                        CACHE_HITS.fetch_add(1, Ordering::Relaxed);
                        if self.kind == AnimatedSourceKind::Video {
                            remember_poster(&self.source, self.spec.max_edge, &animation.frames[0]);
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
            if info.duration.is_some_and(|duration| duration > 8.0)
                && let Some(frame) = cached_poster(&self.source, self.spec.max_edge, cancel)
            {
                self.samples.push((0, frame));
            }
            self.info = Some(info);
            return Ok(PreviewStep::Continue(None));
        }
        if self.samples.is_empty()
            && let Some(duration) = self.info.as_ref().and_then(|info| info.duration)
            && duration <= 8.0
        {
            let span = duration * 0.85;
            let selection = format!(
                "select='gte(t,selected_n*{:.9})'",
                span / (self.times.len() - 1).max(1) as f64
            );
            let mut command = ffmpeg(
                &self.source,
                self.info.as_ref().expect("probe"),
                self.times[0],
                self.spec.max_edge,
                Some(&selection),
            );
            // The sequential path is bounded to short clips; long inputs always use seeks.
            command
                .args(["-fps_mode", "vfr"])
                .arg("-t")
                .arg(format!("{duration:.6}"))
                .args(["-frames:v"])
                .arg(self.times.len().to_string())
                .args(["-c:v", "png", "-f", "image2pipe", "pipe:1"]);
            let bytes = process::run(&mut command, cancel, self.deadline, MAX_ENTRY)?;
            for (index, png) in split_pngs(&bytes)?
                .into_iter()
                .enumerate()
                .take(self.times.len())
            {
                process::check(cancel, self.deadline)?;
                self.samples
                    .push((index, image_from_png(png, self.spec.max_edge)?));
            }
            if self.samples.is_empty() {
                return Err(MediaError::new(
                    ErrorKind::InvalidMedia,
                    "No video preview frames",
                ));
            }
            SAMPLES.fetch_add(self.samples.len() as u64, Ordering::Relaxed);
            let animation = self.animation();
            remember_poster(&self.source, self.spec.max_edge, &animation.frames[0]);
            schedule_cache(self.source.clone(), self.spec, self.kind, animation.clone());
            return Ok(PreviewStep::Complete(animation));
        }
        let index = self.order[self.samples.len()];
        let mut command = ffmpeg(
            &self.source,
            self.info.as_ref().expect("probed task"),
            self.times[index],
            self.spec.max_edge,
            None,
        );
        command.args([
            "-frames:v",
            "1",
            "-c:v",
            "png",
            "-f",
            "image2pipe",
            "pipe:1",
        ]);
        let bytes = process::run(
            &mut command,
            cancel,
            self.deadline.min(Instant::now() + Duration::from_secs(2)),
            MAX_ENTRY,
        )?;
        let image = image_from_png(&bytes, self.spec.max_edge)?;
        if index == 0
            && self
                .info
                .as_ref()
                .is_some_and(|info| info.duration.is_some())
        {
            remember_poster(&self.source, self.spec.max_edge, &image);
        }
        SAMPLES.fetch_add(1, Ordering::Relaxed);
        self.samples.push((index, image));
        if self.samples.len() == self.times.len() {
            let animation = self.animation();
            if self
                .info
                .as_ref()
                .is_some_and(|info| info.duration.is_some())
            {
                schedule_cache(self.source.clone(), self.spec, self.kind, animation.clone());
            }
            return Ok(PreviewStep::Complete(animation));
        }
        Ok(PreviewStep::Continue(
            (self.samples.len() == 4).then(|| self.animation()),
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
    source.hash(&mut hash);
    if kind == AnimatedSourceKind::Video {
        BACKEND_REVISION.hash(&mut hash);
    }
    spec.hash(&mut hash);
    kind.hash(&mut hash);
    (6_u8, VIDEO_PREVIEW_FRAMES, VIDEO_PREVIEW_FRAME_DELAY, 90_u8).hash(&mut hash);
    let hash = format!("{:016x}", hash.finish());
    let shard = cache::thumbnail_cache_dir().join(&hash[..2]);
    let _ = fs::create_dir_all(&shard);
    shard.join(format!("{hash}_overview_v6.bin"))
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

enum CacheWrite {
    Preview(SourceKey, PreviewSpec, AnimatedSourceKind, DecodedAnimation),
    Poster(SourceKey, PathBuf, DecodedImage),
    Flush(mpsc::Sender<()>),
}
static WRITER: LazyLock<mpsc::SyncSender<CacheWrite>> = LazyLock::new(|| {
    let (tx, rx) = mpsc::sync_channel::<CacheWrite>(2);
    std::thread::spawn(move || {
        for write in rx {
            match write {
                CacheWrite::Preview(source, spec, kind, animation) => {
                    if source.unchanged()
                        && let Ok(bytes) =
                            encode_bundle(&animation, &source, spec, kind, &AtomicBool::new(false))
                        && source.unchanged()
                    {
                        cache::atomic_save_bytes(&bytes, &preview_cache_path(&source, spec, kind));
                    }
                }
                CacheWrite::Poster(source, path, frame) => {
                    if source.unchanged()
                        && let Some(image) = image::RgbaImage::from_raw(
                            frame.width as u32,
                            frame.height as u32,
                            frame.rgba.to_vec(),
                        )
                    {
                        let mut bytes = Vec::new();
                        if image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 90)
                            .encode_image(&image::DynamicImage::ImageRgba8(image).to_rgb8())
                            .is_ok()
                            && source.unchanged()
                        {
                            cache::atomic_save_bytes(&bytes, &path);
                        }
                    }
                }
                CacheWrite::Flush(ack) => {
                    let _ = ack.send(());
                }
            }
        }
    });
    tx
});
fn schedule_cache(
    source: SourceKey,
    spec: PreviewSpec,
    kind: AnimatedSourceKind,
    animation: DecodedAnimation,
) {
    let _ = WRITER.try_send(CacheWrite::Preview(source, spec, kind, animation));
}
pub(super) fn schedule_poster_cache(source: SourceKey, path: PathBuf, frame: DecodedImage) {
    let _ = WRITER.try_send(CacheWrite::Poster(source, path, frame));
}
pub(super) fn flush_cache() {
    let (tx, rx) = mpsc::channel();
    if WRITER.send(CacheWrite::Flush(tx)).is_ok() {
        let _ = rx.recv_timeout(Duration::from_secs(5));
    }
}

#[cfg(test)]
#[path = "integration_tests.rs"]
mod integration_tests;

#[cfg(test)]
mod tests {
    use super::*;
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
