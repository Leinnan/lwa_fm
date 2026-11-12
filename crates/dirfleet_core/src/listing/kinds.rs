//! Which files get thumbnails and previews, decided by extension in one
//! place. Matching never allocates: it runs for every visible row each frame.

use std::path::Path;

/// Still images we can decode. AVIF is left out: the `image` crate here is
/// built without an AVIF decoder, so those files keep their system icon.
pub const IMAGE_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "bmp", "webp", "tiff", "tif", "ico", "tga",
];

/// Still images only the system decodes: on macOS, `ImageIO` makes their
/// thumbnails; elsewhere they keep their icon.
pub const SYSTEM_IMAGE_EXTS: &[&str] = &["heic", "heif", "avif"];

/// Animated images, previewed as a short frame overview.
pub const ANIMATED_IMAGE_EXTS: &[&str] = &["gif"];

pub const VIDEO_EXTS: &[&str] = &[
    "mp4", "mov", "mkv", "avi", "webm", "wmv", "flv", "m4v", "3gp", "ogv",
];

/// Sounds: no picture, but they play for some time.
pub const AUDIO_EXTS: &[&str] = &[
    "mp3", "wav", "flac", "ogg", "m4a", "aac", "wma", "opus", "aiff",
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
    if is_one_of(ext, IMAGE_EXTS)
        || (cfg!(target_os = "macos") && is_one_of(ext, SYSTEM_IMAGE_EXTS))
    {
        Some(MediaKind::Image)
    } else if is_one_of(ext, ANIMATED_IMAGE_EXTS) {
        Some(MediaKind::AnimatedImage)
    } else if is_one_of(ext, VIDEO_EXTS) {
        Some(MediaKind::Video)
    } else {
        None
    }
}

/// Whether a file name or path is a sound's, by its extension.
pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|ext| is_one_of(ext, AUDIO_EXTS))
}

/// What a file is, for the Kind column: "Folder", "Application", "PNG
/// image", "TXT file".
pub fn kind_label(name: &str, entry_type: super::EntryType) -> String {
    match entry_type {
        super::EntryType::Directory => return "Folder".to_owned(),
        super::EntryType::Package => {
            let app = std::path::Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("app"));
            return if app { "Application" } else { "Package" }.to_owned();
        }
        super::EntryType::File => {}
    }
    let Some(extension) = std::path::Path::new(name)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|ext| !ext.is_empty())
    else {
        return "File".to_owned();
    };
    let what = match media_kind(std::path::Path::new(name)) {
        Some(MediaKind::Video) => "video",
        Some(_) => "image",
        None => "file",
    };
    format!("{} {what}", extension.to_uppercase())
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
        // Only the system decodes these: thumbnails on macOS, icons elsewhere.
        let system = cfg!(target_os = "macos").then_some(MediaKind::Image);
        assert_eq!(media_kind(Path::new("photo.avif")), system);
        assert_eq!(media_kind(Path::new("IMG_0162.HEIC")), system);
        assert_eq!(media_kind(Path::new("notes")), None);
        assert!(super::is_audio(Path::new("a/Song.FLAC")));
        assert!(!super::is_audio(Path::new("clip.mkv")));
    }
}
