//! Thumbnails from the system's own frameworks on macOS. `ImageIO` reads every
//! image format the system knows, HEIC included, at the size asked for (a
//! JPEG's embedded thumbnail or a subsampled decode where it can); and
//! `AVFoundation` decodes a video frame with the media engine, with no process
//! to start. `None` when they can't, so the portable path takes over.
// Calling the frameworks is unsafe by binding; nothing here holds raw
// pointers past the call.
#![allow(unsafe_code)]
use std::path::Path;
use std::time::Duration;

use objc2_av_foundation::{AVAssetImageGenerator, AVURLAsset};
use objc2_core_foundation::{
    CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType, CFURL, CGSize,
};
use objc2_core_graphics::{CGImage, CGImageAlphaInfo};
use objc2_core_media::CMTime;
use objc2_foundation::{NSString, NSURL};
use objc2_image_io::{
    CGImageDestination, CGImageSource, kCGImageDestinationLossyCompressionQuality,
    kCGImageSourceCreateThumbnailFromImageAlways, kCGImageSourceCreateThumbnailWithTransform,
    kCGImageSourceThumbnailMaxPixelSize,
};

/// Whether `ImageIO` makes this image's thumbnail best: it is the only
/// decoder for HEIC, HEIF and AVIF, and faster than the portable one for
/// JPEG (12 ms against 19 ms for a 12 MP photo) and TIFF. PNG decodes
/// faster in the portable decoder, which scales as it goes.
pub fn decodes_best(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            ["jpg", "jpeg", "heic", "heif", "avif", "tif", "tiff"]
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
}

/// An image's thumbnail, at most `edge` pixels on its longer side, written
/// to `file` (as PNG when it has transparency). The file actually written.
pub fn image_thumbnail(path: &Path, edge: u32, file: &Path) -> Option<std::path::PathBuf> {
    let url = CFURL::from_file_path(path)?;
    // SAFETY: the dictionaries hold CFString keys and CF values, as ImageIO
    // expects.
    unsafe {
        let source = CGImageSource::with_url(&url, None)?;
        let max = CFNumber::new_i64(i64::from(edge));
        let options = CFDictionary::<CFString, CFType>::from_slices(
            &[
                kCGImageSourceCreateThumbnailFromImageAlways,
                kCGImageSourceCreateThumbnailWithTransform,
                kCGImageSourceThumbnailMaxPixelSize,
            ],
            &[
                CFBoolean::new(true).as_ref(),
                CFBoolean::new(true).as_ref(),
                max.as_ref(),
            ],
        );
        let image = source.thumbnail_at_index(0, Some(options.as_opaque()))?;
        write(&image, file)
    }
}

/// A video's frame at `at` (clamped into the video), at most `edge` pixels
/// on its longer side, written to `file`. `at` of `None` takes the poster
/// frame the portable path takes: 5% into the video.
pub fn video_poster(
    path: &Path,
    edge: u32,
    at: Option<Duration>,
    file: &Path,
) -> Option<std::path::PathBuf> {
    // SAFETY: plain AVFoundation calls on objects made here, used on this
    // thread only.
    unsafe {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let asset = AVURLAsset::URLAssetWithURL_options(&url, None);
        let generator = AVAssetImageGenerator::assetImageGeneratorWithAsset(&asset);
        generator.setAppliesPreferredTrackTransform(true);
        let edge = f64::from(edge);
        generator.setMaximumSize(CGSize {
            width: edge,
            height: edge,
        });
        #[allow(deprecated)]
        let duration = asset.duration().seconds();
        let seconds = match at {
            Some(at) => at.as_secs_f64(),
            None if duration.is_finite() && duration > 0.0 => duration * 0.05,
            None => 0.0,
        };
        let seconds = if duration.is_finite() && duration > 0.0 {
            seconds.min((duration - 0.001).max(0.0))
        } else {
            seconds
        };
        let time = CMTime::with_seconds(seconds, 600);
        #[allow(deprecated)]
        let image = generator
            .copyCGImageAtTime_actualTime_error(time, std::ptr::null_mut())
            .ok()?;
        write(&image, file)
    }
}

/// Saves `image` as JPEG beside `file`'s name, or as PNG when it has
/// transparency, the way the cache keeps thumbnails.
fn write(image: &CGImage, file: &Path) -> Option<std::path::PathBuf> {
    let alpha = CGImage::alpha_info(Some(image));
    let opaque = matches!(
        alpha,
        CGImageAlphaInfo::None | CGImageAlphaInfo::NoneSkipFirst | CGImageAlphaInfo::NoneSkipLast
    );
    let (target, kind) = if opaque {
        (file.with_extension("jpg"), "public.jpeg")
    } else {
        (file.with_extension("png"), "public.png")
    };
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Written beside it and renamed, so a reader never sees half a file.
    let partial = target.with_extension(format!("partial-{}", std::process::id()));
    let url = CFURL::from_file_path(&partial)?;
    let written = unsafe {
        let destination =
            CGImageDestination::with_url(&url, &CFString::from_static_str(kind), 1, None)?;
        let quality = CFNumber::new_f64(0.85);
        let properties: CFRetained<CFDictionary<CFString, CFType>> = CFDictionary::from_slices(
            &[kCGImageDestinationLossyCompressionQuality],
            &[quality.as_ref()],
        );
        destination.add_image(image, Some(properties.as_opaque()));
        destination.finalize()
    };
    if !written || std::fs::rename(&partial, &target).is_err() {
        let _ = std::fs::remove_file(&partial);
        return None;
    }
    Some(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_image_thumbnail_is_scaled_and_keeps_transparency() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().join("big.png");
        let mut image = image::RgbaImage::from_pixel(1200, 600, image::Rgba([10, 20, 30, 255]));
        image.put_pixel(0, 0, image::Rgba([0, 0, 0, 0]));
        image.save(&source).expect("save");
        let written =
            image_thumbnail(&source, 256, &dir.path().join("thumb.jpg")).expect("thumbnail");
        assert_eq!(
            written.extension().and_then(|e| e.to_str()),
            Some("png"),
            "transparent, so PNG"
        );
        let thumb = image::open(&written).expect("readable");
        assert_eq!((thumb.width(), thumb.height()), (256, 128));

        let opaque = dir.path().join("opaque.jpg");
        image::RgbImage::from_pixel(800, 800, image::Rgb([200, 10, 10]))
            .save(&opaque)
            .expect("save");
        let written = image_thumbnail(&opaque, 100, &dir.path().join("o.jpg")).expect("thumbnail");
        assert_eq!(written.extension().and_then(|e| e.to_str()), Some("jpg"));
        assert!(
            image_thumbnail(
                &dir.path().join("missing.png"),
                100,
                &dir.path().join("m.jpg")
            )
            .is_none()
        );
    }

    /// Times both paths on the Desktop-shaped fixture
    /// (`scripts/desktop_like_fixture.sh`): `cargo test -p dirfleet_effects
    /// --release native_vs_portable -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs the performance fixture"]
    fn native_vs_portable() {
        let fixture = std::env::temp_dir().join("dirfleet_perf_desktop/.originals");
        let out = tempfile::tempdir().expect("temp dir");
        for name in [
            "shot0.png",
            "shot1.png",
            "shot3.png",
            "photo.jpg",
            "photo.heic",
        ]
        .repeat(2)
        {
            let path = fixture.join(name);
            let start = std::time::Instant::now();
            let native =
                image_thumbnail(&path, 256, &out.path().join(format!("{name}.native.jpg")));
            let native_time = start.elapsed();
            let start = std::time::Instant::now();
            let portable = crate::media::decode::thumbnail_from_source(&path, 256).ok();
            println!(
                "{name}: ImageIO {native_time:?} ({}), image crate {:?} ({})",
                native.is_some(),
                start.elapsed(),
                portable.is_some()
            );
        }
        for name in ["rec_h264.mov", "rec_hevc.mov", "rec.mp4"].repeat(2) {
            let start = std::time::Instant::now();
            let poster = video_poster(
                &fixture.join(name),
                256,
                None,
                &out.path().join(format!("{name}.jpg")),
            );
            println!(
                "{name}: AVFoundation {:?} ({})",
                start.elapsed(),
                poster.is_some()
            );
        }
    }
}
