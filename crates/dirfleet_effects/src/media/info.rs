//! What a media file says of itself: a picture's or video's size in pixels
//! and how long a video or a sound plays. Pictures are read by their
//! headers alone; videos and sounds are asked of `FFprobe`.
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use dirfleet_core::app::{MediaInfo, ProbeMedia};
use dirfleet_core::listing::kinds::{MediaKind, is_audio, media_kind};

use super::video::{self, SourceKey};

/// How long one file may take.
const PATIENCE: Duration = Duration::from_secs(2);

fn millis(seconds: f64) -> u64 {
    Duration::try_from_secs_f64(seconds).map_or(0, |time| {
        u64::try_from(time.as_millis()).unwrap_or(u64::MAX)
    })
}

/// A picture's width and height, from its header.
fn picture(path: &Path) -> Option<(u32, u32)> {
    image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// What can be read of the file `asked` names; zeros for what it has not or
/// what could not be read.
pub fn read(asked: &ProbeMedia) -> MediaInfo {
    let path = Path::new(&asked.path);
    let cancel = AtomicBool::new(false);
    match media_kind(path) {
        Some(MediaKind::Image | MediaKind::AnimatedImage) => {
            let (width, height) = picture(path).unwrap_or_default();
            MediaInfo {
                width,
                height,
                millis: 0,
            }
        }
        Some(MediaKind::Video) => {
            let source = SourceKey::new(path, asked.revision, asked.size);
            video::probe(&source, &cancel, Instant::now() + PATIENCE).map_or_else(
                |_| MediaInfo::default(),
                |info| MediaInfo {
                    width: info.width,
                    height: info.height,
                    millis: info.duration.map_or(0, millis),
                },
            )
        }
        None if is_audio(path) => {
            let source = SourceKey::new(path, asked.revision, asked.size);
            MediaInfo {
                millis: video::playing_time(&source.path, &cancel, Instant::now() + PATIENCE)
                    .map_or(0, millis),
                ..MediaInfo::default()
            }
        }
        None => MediaInfo::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked(path: &Path) -> ProbeMedia {
        ProbeMedia {
            path: path.to_string_lossy().into_owned(),
            revision: 1,
            size: 1,
        }
    }

    #[test]
    fn a_picture_s_pixels_are_read_and_other_files_have_none() {
        let dir = tempfile::tempdir().expect("temp dir");
        let picture = dir.path().join("p.png");
        image::RgbaImage::new(31, 17).save(&picture).expect("saves");
        assert_eq!(
            read(&asked(&picture)),
            MediaInfo {
                width: 31,
                height: 17,
                millis: 0
            }
        );
        // Not a picture, whatever it is called; and nothing to ask of text.
        let broken = dir.path().join("broken.png");
        std::fs::write(&broken, "not a picture").expect("writes");
        assert_eq!(read(&asked(&broken)), MediaInfo::default());
        let text = dir.path().join("notes.txt");
        std::fs::write(&text, "notes").expect("writes");
        assert_eq!(read(&asked(&text)), MediaInfo::default());
    }

    #[test]
    fn seconds_become_whole_milliseconds() {
        assert_eq!(millis(1.5), 1500);
        assert_eq!(millis(-1.0), 0);
        assert_eq!(millis(f64::NAN), 0);
    }
}
