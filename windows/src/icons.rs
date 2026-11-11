//! File icons from the Windows shell, as images the list can draw.
//!
//! An icon is looked up by the kind of file (its extension, or "a folder"),
//! without touching the file, so a lookup is quick enough for the UI thread
//! and is done once per kind and size.
#![expect(unsafe_code, reason = "shell icon lookup and GDI bitmap reads")]

use std::cell::RefCell;
use std::collections::HashMap;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits,
    GetObjectW, HBITMAP, ReleaseDC,
};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES,
};
use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
use windows::Win32::UI::Shell::{
    SHFILEINFOW, SHGFI_SYSICONINDEX, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW, SHGetImageList,
    SHIL_EXTRALARGE, SHIL_JUMBO, SHIL_LARGE,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};
use windows::core::HSTRING;
use windows_reactor::EncodedImage;

/// How large an icon is drawn, in device-independent pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IconSize {
    /// List rows and small buttons: 16 to 32 pixels.
    Small,
    /// Tiles up to 64 pixels.
    Medium,
    /// Larger tiles.
    Large,
}

impl IconSize {
    /// The size to ask the shell for when the icon is drawn `points` large.
    pub fn for_points(points: f64) -> Self {
        if points <= 24.0 {
            Self::Small
        } else if points <= 64.0 {
            Self::Medium
        } else {
            Self::Large
        }
    }

    /// The shell's image list holding icons of this size.
    fn list(self) -> i32 {
        let list = match self {
            // 32 pixels: sharp at 16 points on screens scaled up to 200%.
            Self::Small => SHIL_LARGE,
            Self::Medium => SHIL_EXTRALARGE,
            Self::Large => SHIL_JUMBO,
        };
        i32::try_from(list).unwrap_or_default()
    }
}

thread_local! {
    static CACHE: RefCell<HashMap<(String, IconSize), Option<EncodedImage>>> =
        RefCell::new(HashMap::new());
}

/// The icon of a file called `name` (or of a folder), `None` when the shell has none.
pub fn for_entry(name: &str, is_dir: bool, size: IconSize) -> Option<EncodedImage> {
    let kind = if is_dir {
        String::new()
    } else {
        // Only the extension decides, so "a.TXT" and "b.txt" share one icon.
        // A name without one stands for every such file.
        std::path::Path::new(name).extension().map_or_else(
            || "file".to_owned(),
            |extension| format!(".{}", extension.to_string_lossy().to_lowercase()),
        )
    };
    CACHE.with(|cache| {
        cache
            .borrow_mut()
            .entry((kind, size))
            .or_insert_with_key(|(kind, size)| {
                let png = load(kind, is_dir, *size)?;
                Some(EncodedImage::new(png))
            })
            .clone()
    })
}

/// Looks the icon up and encodes it as PNG.
fn load(kind: &str, is_dir: bool, size: IconSize) -> Option<Vec<u8>> {
    let (attributes, name): (FILE_FLAGS_AND_ATTRIBUTES, &str) = if is_dir {
        (FILE_ATTRIBUTE_DIRECTORY, "folder")
    } else {
        (FILE_ATTRIBUTE_NORMAL, kind)
    };
    let mut info = SHFILEINFOW::default();
    // SAFETY: plain shell calls with valid, live arguments; the icon handle
    // made here is destroyed here.
    unsafe {
        let found = SHGetFileInfoW(
            &HSTRING::from(name),
            attributes,
            Some(&raw mut info),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_SYSICONINDEX | SHGFI_USEFILEATTRIBUTES,
        );
        if found == 0 {
            return None;
        }
        let list: IImageList = SHGetImageList(size.list()).ok()?;
        let icon = list.GetIcon(info.iIcon, ILD_TRANSPARENT.0).ok()?;
        let pixels = pixels_of(icon);
        let _ = DestroyIcon(icon);
        let (width, height, rgba) = pixels?;
        encode_png(width, height, &rgba)
    }
}

/// The icon's pixels as RGBA, top row first.
///
/// # Safety
/// `icon` must be a live icon handle.
unsafe fn pixels_of(icon: HICON) -> Option<(u32, u32, Vec<u8>)> {
    // SAFETY: the handles `GetIconInfo` returns are deleted before returning,
    // and every buffer is as large as the call it is passed to is told.
    unsafe {
        let mut info = ICONINFO::default();
        GetIconInfo(icon, &raw mut info).ok()?;
        let result = bitmap_rgba(info.hbmColor);
        let _ = DeleteObject(info.hbmColor.into());
        let _ = DeleteObject(info.hbmMask.into());
        result
    }
}

/// # Safety
/// `bitmap` must be a live bitmap handle.
unsafe fn bitmap_rgba(bitmap: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
    // SAFETY: see `pixels_of`.
    unsafe {
        let mut shape = BITMAP::default();
        let read = GetObjectW(
            bitmap.into(),
            i32::try_from(size_of::<BITMAP>()).unwrap_or(i32::MAX),
            Some((&raw mut shape).cast()),
        );
        if read == 0 || shape.bmWidth <= 0 || shape.bmHeight <= 0 {
            return None;
        }
        let (width, height) = (shape.bmWidth, shape.bmHeight);
        let mut header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // Negative: rows come top first.
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let screen = GetDC(Some(HWND::default()));
        let rows = GetDIBits(
            screen,
            bitmap,
            0,
            height as u32,
            Some(pixels.as_mut_ptr().cast()),
            &raw mut header,
            DIB_RGB_COLORS,
        );
        ReleaseDC(Some(HWND::default()), screen);
        if rows == 0 {
            return None;
        }
        // BGRA to RGBA. Old icons have no alpha at all: they come out opaque.
        let transparent = pixels.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0);
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
            if transparent {
                pixel[3] = 255;
            }
        }
        Some((width as u32, height as u32, pixels))
    }
}

fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    // The image is decoded once and kept; size matters less than speed here.
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(rgba).ok()?;
    writer.finish().ok()?;
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_follow_how_large_the_icon_is_drawn() {
        assert_eq!(IconSize::for_points(16.0), IconSize::Small);
        assert_eq!(IconSize::for_points(40.0), IconSize::Medium);
        assert_eq!(IconSize::for_points(56.0), IconSize::Medium);
        assert_eq!(IconSize::for_points(80.0), IconSize::Large);
    }

    #[test]
    fn the_shell_has_an_icon_for_folders_and_text_files() {
        let folder = for_entry("anything", true, IconSize::Small);
        let text = for_entry("notes.txt", false, IconSize::Small);
        assert!(folder.is_some_and(|icon| icon.as_bytes().starts_with(b"\x89PNG")));
        assert!(text.is_some_and(|icon| icon.as_bytes().starts_with(b"\x89PNG")));
    }

    #[test]
    fn pixels_encode_as_png() {
        let png = encode_png(2, 1, &[255, 0, 0, 255, 0, 0, 255, 128]);
        assert!(png.is_some_and(|bytes| bytes.starts_with(b"\x89PNG")));
    }
}
