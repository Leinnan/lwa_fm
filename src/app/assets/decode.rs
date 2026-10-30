//! Decoding still images into thumbnails: a fast DCT-scaled path for JPEG,
//! the generic `image` decoder for everything else, and cached thumbnails.
//! EXIF orientation is applied on both paths, so phone photos stand upright.

use super::DecodedImage;
use super::cache;
use super::media;
use super::process::{ErrorKind, MediaError};
use image::ImageDecoder;
use image::metadata::Orientation;
use std::path::Path;

/// Largest decoded source image (bytes of RGBA) we are willing to allocate.
const MAX_SOURCE_ALLOC: u64 = 128 * 1024 * 1024;
/// Largest cached thumbnail file we read back, and its decoded size limit.
const MAX_CACHED_BYTES: usize = 16 * 1024 * 1024;

fn invalid(error: &impl std::fmt::Display) -> MediaError {
    MediaError::new(ErrorKind::InvalidMedia, error.to_string())
}

/// Decodes `path` and scales it to fit `edge`, upright.
pub fn thumbnail_from_source(path: &Path, edge: u32) -> Result<image::DynamicImage, MediaError> {
    if let Some(image) = decode_jpeg_scaled(path, edge) {
        return Ok(image.thumbnail(edge, edge));
    }
    let mut reader = image::ImageReader::open(path).map_err(|err| invalid(&err))?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_SOURCE_ALLOC);
    reader.limits(limits);
    let mut decoder = reader
        .with_guessed_format()
        .map_err(|err| invalid(&err))?
        .into_decoder()
        .map_err(|err| invalid(&err))?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut image = image::DynamicImage::from_decoder(decoder)
        .map_err(|err| invalid(&err))?
        .thumbnail(edge, edge);
    image.apply_orientation(orientation);
    Ok(image)
}

/// Decode a JPEG with DCT-domain downscaling (1/2, 1/4 or 1/8) to the
/// smallest size still covering `edge`, which skips most of the IDCT work for
/// camera photos. `None` for non-JPEG input or unusual pixel formats, which
/// fall back to the generic decoder.
pub(super) fn decode_jpeg_scaled(path: &Path, edge: u32) -> Option<image::DynamicImage> {
    let is_jpeg = path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg"));
    if !is_jpeg {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let mut decoder = jpeg_decoder::Decoder::new(std::io::BufReader::new(file));
    decoder.read_info().ok()?;
    let info = decoder.info()?;
    // Same allocation bound as the generic decoder path.
    if u64::from(info.width) * u64::from(info.height) * 4 > MAX_SOURCE_ALLOC {
        return None;
    }
    let edge = u16::try_from(edge).unwrap_or(u16::MAX);
    let (width, height) = decoder.scale(edge, edge).ok()?;
    let pixels = decoder.decode().ok()?;
    let orientation = decoder
        .exif_data()
        .and_then(Orientation::from_exif_chunk)
        .unwrap_or(Orientation::NoTransforms);
    let (width, height) = (u32::from(width), u32::from(height));
    let mut image = match decoder.info()?.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => {
            image::RgbImage::from_raw(width, height, pixels).map(image::DynamicImage::ImageRgb8)
        }
        jpeg_decoder::PixelFormat::L8 => {
            image::GrayImage::from_raw(width, height, pixels).map(image::DynamicImage::ImageLuma8)
        }
        _ => None,
    }?;
    image.apply_orientation(orientation);
    Some(image)
}

/// Reads a thumbnail we cached earlier.
pub fn cached_thumbnail(path: &Path, name: String) -> Option<DecodedImage> {
    let bytes = cache::read_bounded(path, MAX_CACHED_BYTES).ok()?;
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_CACHED_BYTES as u64);
    limits.max_image_width = Some(media::MAX_EDGE);
    limits.max_image_height = Some(media::MAX_EDGE);
    reader.limits(limits);
    Some(decoded_from_dynamic(&reader.decode().ok()?, name))
}

/// Converts to 8-bit RGBA; 16-bit and float sources are rounded to 8 bits.
pub fn decoded_from_dynamic(image: &image::DynamicImage, name: String) -> DecodedImage {
    let rgba = image.to_rgba8();
    let width = rgba.width() as usize;
    let height = rgba.height() as usize;
    DecodedImage {
        name,
        width,
        height,
        rgba: rgba.into_raw().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::thumbnail_from_source;

    /// A 2×1 JPEG whose EXIF says "rotate 90°", written by the test.
    fn rotated_jpeg(path: &std::path::Path) {
        let mut bytes = Vec::new();
        let image = image::RgbImage::from_pixel(40, 20, image::Rgb([200, 30, 30]));
        image::codecs::jpeg::JpegEncoder::new(&mut bytes)
            .encode_image(&image)
            .expect("encode");
        // APP1 Exif segment, little endian, one IFD entry: Orientation = 6.
        let tiff: Vec<u8> = [
            &b"II*\0"[..],
            &8u32.to_le_bytes(),
            &1u16.to_le_bytes(),
            &0x0112u16.to_le_bytes(),
            &3u16.to_le_bytes(),
            &1u32.to_le_bytes(),
            &6u16.to_le_bytes(),
            &[0, 0],
            &0u32.to_le_bytes(),
        ]
        .concat();
        let payload = [&b"Exif\0\0"[..], &tiff].concat();
        let length = u16::try_from(payload.len() + 2).expect("segment length");
        let mut out = bytes[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(&payload);
        out.extend_from_slice(&bytes[2..]);
        std::fs::write(path, out).expect("write");
    }

    #[test]
    fn exif_orientation_is_applied_on_the_jpeg_path() {
        let dir = std::env::temp_dir().join(format!("lwa_fm_exif_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("rotated.jpg");
        rotated_jpeg(&path);
        let image = thumbnail_from_source(&path, 64).expect("decode");
        // 40×20 landscape pixels displayed upright are 20×40 portrait.
        assert!(
            image.height() > image.width(),
            "{}x{}",
            image.width(),
            image.height()
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
