//! Which files get thumbnails and previews, decided by extension in one
//! place. Matching never allocates: it runs for every visible row each frame.

use std::path::Path;

/// Still images we can decode. AVIF is left out: the `image` crate here is
/// built without an AVIF decoder, so those files keep their system icon.
pub const IMAGE_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "bmp", "webp", "tiff", "tif", "ico", "tga",
];

/// Animated images, previewed as a short frame overview.
pub const ANIMATED_IMAGE_EXTS: &[&str] = &["gif"];

pub const VIDEO_EXTS: &[&str] = &[
    "mp4", "mov", "mkv", "avi", "webm", "wmv", "flv", "m4v", "3gp", "ogv",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    AnimatedImage,
    Video,
}

impl MediaKind {
    /// Has a moving preview (GIF overview or video frames).
    pub const fn is_animated(self) -> bool {
        matches!(self, Self::AnimatedImage | Self::Video)
    }
}

fn is_one_of(ext: &str, list: &[&str]) -> bool {
    list.iter()
        .any(|candidate| ext.eq_ignore_ascii_case(candidate))
}

/// The media kind of a file name or path, from its extension.
pub fn media_kind(path: &Path) -> Option<MediaKind> {
    let ext = path.extension()?.to_str()?;
    if is_one_of(ext, IMAGE_EXTS) {
        Some(MediaKind::Image)
    } else if is_one_of(ext, ANIMATED_IMAGE_EXTS) {
        Some(MediaKind::AnimatedImage)
    } else if is_one_of(ext, VIDEO_EXTS) {
        Some(MediaKind::Video)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{MediaKind, media_kind};
    use std::path::Path;

    #[test]
    fn kinds_ignore_case_and_skip_undecodable_formats() {
        assert_eq!(media_kind(Path::new("a/B.JPG")), Some(MediaKind::Image));
        assert_eq!(
            media_kind(Path::new("a.Gif")),
            Some(MediaKind::AnimatedImage)
        );
        assert_eq!(media_kind(Path::new("clip.MKV")), Some(MediaKind::Video));
        assert_eq!(media_kind(Path::new("photo.avif")), None);
        assert_eq!(media_kind(Path::new("notes")), None);
    }
}
