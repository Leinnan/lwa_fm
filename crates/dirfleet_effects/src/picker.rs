//! The thumbnail picker's work: a video's length and where its default
//! thumbnail is, then its frames one at a time, each as an image file beside
//! the thumbnails. Reading the video doesn't count as opening it.
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use dirfleet_core::app::{PickerFrame, PickerTiming, VideoLength, VideoTiming};
use image::RgbaImage;

use crate::access_time::PreserveAccessTime;
use crate::media::video::{self, MAX_EDGE, SourceKey};
use crate::media::{THUMBNAIL_DEADLINE, cache};

/// Picker frames are as sharp as the largest thumbnails, which they become.
const EDGE: u32 = MAX_EDGE;

fn millis(at: Duration) -> u64 {
    u64::try_from(at.as_millis()).unwrap_or(u64::MAX)
}

pub fn timing(op: &PickerTiming) -> VideoTiming {
    let path = Path::new(&op.path);
    let _access_time = PreserveAccessTime::new(path);
    let source = SourceKey::new(path, op.revision, op.size);
    let cancel = AtomicBool::new(false);
    VideoTiming(
        video::probe(&source, &cancel, Instant::now() + THUMBNAIL_DEADLINE)
            .map(|info| VideoLength {
                duration_ms: info
                    .duration
                    .filter(|duration| duration.is_finite() && *duration > 0.0)
                    .map(|duration| millis(Duration::from_secs_f64(duration))),
                // Frames per second, in thousandths.
                frame_rate_milli: info
                    .frame_rate
                    .filter(|rate| rate.is_finite() && *rate > 0.0)
                    .map(|rate| millis(Duration::from_secs_f64(rate))),
                default_ms: info
                    .duration
                    .and_then(|duration| video::sample_times(duration).first().copied())
                    .map_or(0, millis),
            })
            .map_err(|error| error.message),
    )
}

/// The frame at `op.at_ms` as a plain image file, cached like thumbnails.
pub fn frame(op: &PickerFrame) -> Option<String> {
    let path = Path::new(&op.path);
    let poster = cache::chosen_poster_cache_path(path, EDGE, op.revision, op.size, op.at_ms);
    let stem = poster.file_stem()?.to_string_lossy().into_owned();
    let file = poster.with_file_name(format!("{stem}_frame.jpg"));
    if let Some(found) = cache::cached_variant(&file) {
        cache::touch(&found);
        return Some(found.to_string_lossy().into_owned());
    }
    let _access_time = PreserveAccessTime::new(path);
    let source = SourceKey::new(path, op.revision, op.size);
    let cancel = AtomicBool::new(false);
    // Sources without a revision skip the disk cache, as everywhere else.
    let cache_path = (op.revision != 0).then_some(poster);
    let image = video::chosen_poster(
        &source,
        EDGE,
        Duration::from_millis(op.at_ms),
        cache_path,
        &cancel,
    )
    .ok()?;
    let width = u32::try_from(image.width).ok()?;
    let height = u32::try_from(image.height).ok()?;
    let rgba = RgbaImage::from_raw(width, height, image.rgba.to_vec())?;
    let (bytes, ext) = cache::encode_thumbnail(&image::DynamicImage::ImageRgba8(rgba))?;
    cache::atomic_write_thumbnail(&file, &bytes, ext);
    cache::cached_variant(&file).map(|found| found.to_string_lossy().into_owned())
}
