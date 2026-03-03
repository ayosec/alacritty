//! Kitty graphics payload decode pipeline.
//!
//! Pipeline: base64 → zlib (optional) → format decode → GraphicData
//!
//! Supported transmission mediums:
//! - `Direct` (`t=d`): inline base64 payload
//! - `File` (`t=f`): base64-encoded file path
//! - `TempFile` (`t=t`): base64-encoded path relative to temp dir
//! - `SharedMemory` (`t=s`): base64-encoded POSIX shm object name (unix only)

use std::io::Read;

use base64::Engine;
use base64::alphabet;
use base64::engine::{GeneralPurpose, GeneralPurposeConfig, DecodePaddingMode};

/// Lenient base64 decoder: accepts both padded and unpadded input.
///
/// Chunked kitty transfers concatenate base64 across multiple APC sequences.
/// Some clients (e.g. chafa) pad each chunk with `=`, so the merged payload
/// can have `=` in the middle. Stripping padding before decode handles this.
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);
use flate2::read::ZlibDecoder;

use crate::graphics::kitty_parser::{Compression, Format, KittyCommand, Medium};
use crate::graphics::{ColorType, GraphicData, MAX_GRAPHIC_DIMENSIONS};

/// Maximum dimensions for a single image.
const MAX_IMAGE_WIDTH: u32 = 10000;
const MAX_IMAGE_HEIGHT: u32 = 10000;

/// Errors that can occur during kitty image decoding.
#[derive(Debug)]
pub enum DecodeError {
    Base64(String),
    Zlib(String),
    Png(String),
    InvalidDimensions(String),
    UnsupportedMedium(Medium),
    MissingDimensions,
    IoError(String),
    PathTraversal(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Base64(e) => write!(f, "base64 decode error: {e}"),
            DecodeError::Zlib(e) => write!(f, "zlib decompress error: {e}"),
            DecodeError::Png(e) => write!(f, "PNG decode error: {e}"),
            DecodeError::InvalidDimensions(e) => write!(f, "invalid dimensions: {e}"),
            DecodeError::UnsupportedMedium(m) => write!(f, "unsupported medium: {m:?}"),
            DecodeError::MissingDimensions => write!(f, "missing width/height for raw format"),
            DecodeError::IoError(e) => write!(f, "I/O error: {e}"),
            DecodeError::PathTraversal(e) => write!(f, "path traversal rejected: {e}"),
        }
    }
}

/// Decode a kitty graphics payload into a `GraphicData`.
///
/// Pipeline: medium-specific read → zlib (optional) → format decode → GraphicData
///
/// When `cmd.pre_decoded` is true (set by chunked transfer finalization),
/// the payload already contains raw decoded bytes — base64 is skipped.
/// This handles clients like chafa that independently base64-encode each
/// chunk, which would corrupt group alignment if concatenated as strings.
pub fn decode_payload(cmd: &KittyCommand, raw_payload: &[u8]) -> Result<GraphicData, DecodeError> {
    let raw_bytes = if cmd.pre_decoded {
        // Chunked transfer already decoded each chunk's base64 on arrival.
        raw_payload.to_vec()
    } else {
        match cmd.medium {
            Medium::Direct => decode_direct_payload(raw_payload)?,
            Medium::File => read_file_payload(raw_payload, cmd.data_offset, cmd.data_size, false)?,
            Medium::TempFile => read_file_payload(raw_payload, cmd.data_offset, cmd.data_size, true)?,
            #[cfg(unix)]
            Medium::SharedMemory => read_shm_payload(raw_payload, cmd.data_offset, cmd.data_size)?,
            #[cfg(not(unix))]
            Medium::SharedMemory => return Err(DecodeError::UnsupportedMedium(cmd.medium)),
        }
    };

    decode_raw_bytes(cmd, raw_bytes)
}

/// Decode an inline base64 payload (Medium::Direct).
///
/// Strips `=` padding before decoding to handle chunked transfers where
/// intermediate chunks include trailing padding characters.
fn decode_direct_payload(raw_payload: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let cleaned: Vec<u8> = raw_payload.iter().copied().filter(|&b| b != b'=').collect();
    BASE64
        .decode(&cleaned)
        .map_err(|e| DecodeError::Base64(e.to_string()))
}

/// Read payload from a file path (Medium::File or Medium::TempFile).
///
/// The `raw_payload` is a base64-encoded filesystem path. For TempFile,
/// the decoded path is joined relative to `std::env::temp_dir()` and must
/// not contain `..` components or escape the temp directory.
fn read_file_payload(
    raw_payload: &[u8],
    data_offset: u32,
    data_size: u32,
    is_temp: bool,
) -> Result<Vec<u8>, DecodeError> {
    use std::io::{Read, Seek, SeekFrom};

    let path_bytes = BASE64
        .decode(raw_payload)
        .map_err(|e| DecodeError::Base64(e.to_string()))?;
    let path_str = String::from_utf8(path_bytes)
        .map_err(|e| DecodeError::IoError(format!("invalid UTF-8 in path: {e}")))?;

    let resolved_path = if is_temp {
        validate_temp_path(&path_str)?;
        let temp_dir = std::env::temp_dir();
        temp_dir.join(&path_str)
    } else {
        std::path::PathBuf::from(&path_str)
    };

    let mut file = std::fs::File::open(&resolved_path)
        .map_err(|e| DecodeError::IoError(format!("open {:?}: {e}", resolved_path)))?;

    if data_offset > 0 {
        file.seek(SeekFrom::Start(u64::from(data_offset)))
            .map_err(|e| DecodeError::IoError(format!("seek: {e}")))?;
    }

    let data = if data_size > 0 {
        let mut buf = vec![0u8; data_size as usize];
        file.read_exact(&mut buf)
            .map_err(|e| DecodeError::IoError(format!("read_exact: {e}")))?;
        buf
    } else {
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)
            .map_err(|e| DecodeError::IoError(format!("read_to_end: {e}")))?;
        buf
    };

    // For temp files, delete after reading (kitty/wezterm behavior).
    if is_temp {
        if let Err(e) = std::fs::remove_file(&resolved_path) {
            log::warn!(
                "[kitty] failed to remove temp file {:?}: {e}",
                resolved_path
            );
        }
    }

    Ok(data)
}

/// Validate that a temp-file relative path does not escape the temp directory.
///
/// Rejects paths containing `..` components, absolute paths, and any other
/// attempt to traverse outside the temp dir.
fn validate_temp_path(path: &str) -> Result<(), DecodeError> {
    use std::path::Component;

    let path_obj = std::path::Path::new(path);

    // Reject absolute paths — temp file paths must be relative.
    if path_obj.is_absolute() {
        return Err(DecodeError::PathTraversal(format!(
            "absolute path not allowed for temp file: {path}"
        )));
    }

    for component in path_obj.components() {
        match component {
            Component::ParentDir => {
                return Err(DecodeError::PathTraversal(format!(
                    "path contains '..': {path}"
                )));
            },
            Component::RootDir | Component::Prefix(_) => {
                return Err(DecodeError::PathTraversal(format!(
                    "path escapes temp directory: {path}"
                )));
            },
            Component::CurDir | Component::Normal(_) => {},
        }
    }

    Ok(())
}

/// Read payload from a POSIX shared memory object (Medium::SharedMemory).
///
/// Opens the named shm object via `shm_open`, determines its size with
/// `fstat`, maps it with `mmap`, copies the requested range, then cleans
/// up with `munmap`, `close`, and `shm_unlink`.
///
/// We use `mmap` rather than `read(2)` because macOS shm file descriptors
/// do not support regular file I/O (seek / read return ENODEV).
#[cfg(unix)]
fn read_shm_payload(
    raw_payload: &[u8],
    data_offset: u32,
    data_size: u32,
) -> Result<Vec<u8>, DecodeError> {
    use std::ffi::CString;

    let name_bytes = BASE64
        .decode(raw_payload)
        .map_err(|e| DecodeError::Base64(e.to_string()))?;
    let name_str = String::from_utf8(name_bytes)
        .map_err(|e| DecodeError::IoError(format!("invalid UTF-8 in shm name: {e}")))?;

    let c_name = CString::new(name_str.as_bytes())
        .map_err(|e| DecodeError::IoError(format!("invalid shm name: {e}")))?;

    // Open the shared memory object (read-only).
    let fd = unsafe { libc::shm_open(c_name.as_ptr(), libc::O_RDONLY, 0) };
    if fd < 0 {
        let err = std::io::Error::last_os_error();
        return Err(DecodeError::IoError(format!("shm_open '{}': {err}", name_str)));
    }

    // Determine total size of the shm object via fstat.
    let total_size = unsafe {
        let mut stat: libc::stat = std::mem::zeroed();
        if libc::fstat(fd, &mut stat) != 0 {
            let err = std::io::Error::last_os_error();
            libc::close(fd);
            return Err(DecodeError::IoError(format!("fstat shm '{}': {err}", name_str)));
        }
        stat.st_size as usize
    };

    if total_size == 0 {
        unsafe {
            libc::close(fd);
            libc::shm_unlink(c_name.as_ptr());
        }
        return Err(DecodeError::IoError(format!("shm '{}' has zero size", name_str)));
    }

    // mmap the entire object read-only.
    let ptr = unsafe {
        libc::mmap(std::ptr::null_mut(), total_size, libc::PROT_READ, libc::MAP_SHARED, fd, 0)
    };

    // Close the fd immediately — the mapping keeps the memory accessible.
    unsafe { libc::close(fd) };

    if ptr == libc::MAP_FAILED {
        let err = std::io::Error::last_os_error();
        unsafe { libc::shm_unlink(c_name.as_ptr()) };
        return Err(DecodeError::IoError(format!("mmap shm '{}': {err}", name_str)));
    }

    // Compute the slice we need to copy.
    let offset = data_offset as usize;
    let len = if data_size > 0 { data_size as usize } else { total_size.saturating_sub(offset) };

    let result = if offset + len > total_size {
        Err(DecodeError::IoError(format!(
            "shm '{}': offset({offset}) + size({len}) exceeds total({total_size})",
            name_str
        )))
    } else {
        let src = unsafe { std::slice::from_raw_parts((ptr as *const u8).add(offset), len) };
        Ok(src.to_vec())
    };

    // Unmap + unlink regardless of whether the copy succeeded.
    unsafe {
        libc::munmap(ptr, total_size);
    }
    let ret = unsafe { libc::shm_unlink(c_name.as_ptr()) };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        log::warn!("[kitty] shm_unlink '{}': {err}", name_str);
    }

    result
}

/// Shared decode tail: decompress (optional zlib) then format-decode.
fn decode_raw_bytes(cmd: &KittyCommand, raw: Vec<u8>) -> Result<GraphicData, DecodeError> {
    let decompressed = match cmd.compression {
        Compression::Zlib => {
            let mut decoder = ZlibDecoder::new(&raw[..]);
            let mut buf = Vec::new();
            decoder
                .read_to_end(&mut buf)
                .map_err(|e| DecodeError::Zlib(e.to_string()))?;
            buf
        },
        Compression::None => raw,
    };

    match cmd.format {
        Format::Png => decode_png(&decompressed),
        Format::Rgba => decode_raw(&decompressed, cmd.width, cmd.height, 4),
        Format::Rgb => decode_rgb_to_rgba(&decompressed, cmd.width, cmd.height),
    }
}

fn decode_png(data: &[u8]) -> Result<GraphicData, DecodeError> {
    let decoder = png::Decoder::new(std::io::Cursor::new(data));
    let mut reader = decoder.read_info().map_err(|e| DecodeError::Png(e.to_string()))?;

    let info = reader.info();
    let width = info.width as usize;
    let height = info.height as usize;

    if width > MAX_IMAGE_WIDTH as usize || height > MAX_IMAGE_HEIGHT as usize {
        return Err(DecodeError::InvalidDimensions(format!(
            "PNG {width}x{height} exceeds max {MAX_IMAGE_WIDTH}x{MAX_IMAGE_HEIGHT}"
        )));
    }

    if width > MAX_GRAPHIC_DIMENSIONS[0] || height > MAX_GRAPHIC_DIMENSIONS[1] {
        return Err(DecodeError::InvalidDimensions(format!(
            "PNG {width}x{height} exceeds max graphic dimensions"
        )));
    }

    let buf_size = reader.output_buffer_size().unwrap_or(width * height * 4);
    let mut buf = vec![0u8; buf_size];
    let output_info = reader.next_frame(&mut buf).map_err(|e| DecodeError::Png(e.to_string()))?;
    buf.truncate(output_info.buffer_size());

    let (pixels, color_type) = match output_info.color_type {
        png::ColorType::Rgba => (buf, ColorType::Rgba),
        png::ColorType::Rgb => {
            let rgba = rgb_bytes_to_rgba(&buf);
            (rgba, ColorType::Rgba)
        },
        png::ColorType::GrayscaleAlpha => {
            let rgba: Vec<u8> =
                buf.chunks_exact(2).flat_map(|ga| [ga[0], ga[0], ga[0], ga[1]]).collect();
            (rgba, ColorType::Rgba)
        },
        png::ColorType::Grayscale => {
            let rgba: Vec<u8> = buf.iter().flat_map(|&g| [g, g, g, 255]).collect();
            (rgba, ColorType::Rgba)
        },
        png::ColorType::Indexed => {
            return Err(DecodeError::Png("indexed PNG not directly supported".into()));
        },
    };

    Ok(GraphicData {
        id: crate::graphics::GraphicId(0),
        width,
        height,
        color_type,
        pixels,
        is_opaque: color_type == ColorType::Rgb,
    })
}

fn decode_raw(
    data: &[u8],
    width: u32,
    height: u32,
    bpp: usize,
) -> Result<GraphicData, DecodeError> {
    let w = width as usize;
    let h = height as usize;

    if w == 0 || h == 0 {
        return Err(DecodeError::MissingDimensions);
    }

    let expected = w * h * bpp;
    // Tolerate slightly more data than expected — some clients (e.g. chafa)
    // produce a few extra trailing bytes due to base64 chunk alignment.
    // Reject if data is too short or wildly too large (>1 row extra).
    let max_tolerated = expected + w * bpp;
    if data.len() < expected || data.len() > max_tolerated {
        return Err(DecodeError::InvalidDimensions(format!(
            "expected {expected} bytes for {w}x{h}x{bpp}, got {}",
            data.len()
        )));
    }
    let data = &data[..expected];

    if w > MAX_IMAGE_WIDTH as usize || h > MAX_IMAGE_HEIGHT as usize {
        return Err(DecodeError::InvalidDimensions(format!(
            "{w}x{h} exceeds max {MAX_IMAGE_WIDTH}x{MAX_IMAGE_HEIGHT}"
        )));
    }

    let color_type = if bpp == 4 { ColorType::Rgba } else { ColorType::Rgb };

    Ok(GraphicData {
        id: crate::graphics::GraphicId(0),
        width: w,
        height: h,
        color_type,
        pixels: data.to_vec(),
        is_opaque: bpp == 3,
    })
}

fn decode_rgb_to_rgba(data: &[u8], width: u32, height: u32) -> Result<GraphicData, DecodeError> {
    let w = width as usize;
    let h = height as usize;

    if w == 0 || h == 0 {
        return Err(DecodeError::MissingDimensions);
    }

    let expected = w * h * 3;
    let max_tolerated = expected + w * 3;
    if data.len() < expected || data.len() > max_tolerated {
        return Err(DecodeError::InvalidDimensions(format!(
            "expected {expected} bytes for {w}x{h}x3 (RGB), got {}",
            data.len()
        )));
    }
    let data = &data[..expected];

    if w > MAX_IMAGE_WIDTH as usize || h > MAX_IMAGE_HEIGHT as usize {
        return Err(DecodeError::InvalidDimensions(format!(
            "{w}x{h} exceeds max {MAX_IMAGE_WIDTH}x{MAX_IMAGE_HEIGHT}"
        )));
    }

    let rgba = rgb_bytes_to_rgba(data);

    Ok(GraphicData {
        id: crate::graphics::GraphicId(0),
        width: w,
        height: h,
        color_type: ColorType::Rgba,
        pixels: rgba,
        is_opaque: true,
    })
}

/// Convert an RGB byte slice to RGBA (fully opaque).
pub fn rgb_bytes_to_rgba(rgb: &[u8]) -> Vec<u8> {
    let pixel_count = rgb.len() / 3;
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    for chunk in rgb.chunks_exact(3) {
        rgba.extend_from_slice(chunk);
        rgba.push(255);
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::kitty_parser::{Compression, Format, KittyCommand};

    // ── Direct medium tests (existing) ─────────────────────────────────

    #[test]
    fn decode_raw_rgba() {
        let pixels: Vec<u8> = vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let encoded = BASE64.encode(&pixels);
        let cmd = KittyCommand {
            format: Format::Rgba,
            width: 2,
            height: 2,
            payload: encoded.into_bytes(),
            ..Default::default()
        };
        let data = decode_payload(&cmd, &cmd.payload).unwrap();
        assert_eq!(data.width, 2);
        assert_eq!(data.height, 2);
        assert_eq!(data.pixels, pixels);
    }

    #[test]
    fn decode_raw_rgb() {
        let rgb_pixels: Vec<u8> = vec![255, 0, 0, 0, 255, 0];
        let encoded = BASE64.encode(&rgb_pixels);
        let cmd = KittyCommand {
            format: Format::Rgb,
            width: 2,
            height: 1,
            payload: encoded.into_bytes(),
            ..Default::default()
        };
        let data = decode_payload(&cmd, &cmd.payload).unwrap();
        assert_eq!(data.pixels, vec![255, 0, 0, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn decode_dimension_mismatch_too_short() {
        let encoded = BASE64.encode(&[0u8; 10]);
        let cmd = KittyCommand {
            format: Format::Rgba,
            width: 2,
            height: 2,
            payload: encoded.into_bytes(),
            ..Default::default()
        };
        assert!(decode_payload(&cmd, &cmd.payload).is_err());
    }

    #[test]
    fn decode_raw_truncates_slightly_over() {
        // Simulates chafa sending 1 extra byte beyond the expected size.
        // 2x2 RGBA = 16 bytes expected. Send 17 bytes — should truncate.
        let mut pixels: Vec<u8> = vec![
            255, 0, 0, 255, 0, 255, 0, 255,
            0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let expected = pixels.clone();
        pixels.push(0xDE); // 1 extra trailing byte

        let encoded = BASE64.encode(&pixels);
        let cmd = KittyCommand {
            format: Format::Rgba,
            width: 2,
            height: 2,
            payload: encoded.into_bytes(),
            ..Default::default()
        };
        let data = decode_payload(&cmd, &cmd.payload).unwrap();
        assert_eq!(data.width, 2);
        assert_eq!(data.height, 2);
        assert_eq!(data.pixels, expected, "should truncate extra trailing byte");
    }

    #[test]
    fn decode_raw_truncates_up_to_one_row_extra() {
        // Up to one extra row of data should be tolerated.
        // 4x4 RGBA = 64 bytes. One extra row = 16 bytes. Total = 80.
        let pixels = vec![0u8; 80];
        let encoded = BASE64.encode(&pixels);
        let cmd = KittyCommand {
            format: Format::Rgba,
            width: 4,
            height: 4,
            payload: encoded.into_bytes(),
            ..Default::default()
        };
        let data = decode_payload(&cmd, &cmd.payload).unwrap();
        assert_eq!(data.pixels.len(), 64, "should truncate to expected 64 bytes");
    }

    #[test]
    fn decode_raw_rejects_wildly_over() {
        // More than one extra row should be rejected.
        // 4x4 RGBA = 64 bytes. Two extra rows = 32 bytes. Total = 96.
        let pixels = vec![0u8; 97]; // expected + row*4 + 1 = too much
        let encoded = BASE64.encode(&pixels);
        let cmd = KittyCommand {
            format: Format::Rgba,
            width: 4,
            height: 4,
            payload: encoded.into_bytes(),
            ..Default::default()
        };
        assert!(decode_payload(&cmd, &cmd.payload).is_err());
    }

    #[test]
    fn decode_rgb_truncates_slightly_over() {
        // 2x1 RGB = 6 bytes. Send 7 — should truncate.
        let mut pixels: Vec<u8> = vec![255, 0, 0, 0, 255, 0];
        pixels.push(0xDE);
        let encoded = BASE64.encode(&pixels);
        let cmd = KittyCommand {
            format: Format::Rgb,
            width: 2,
            height: 1,
            payload: encoded.into_bytes(),
            ..Default::default()
        };
        let data = decode_payload(&cmd, &cmd.payload).unwrap();
        assert_eq!(data.pixels, vec![255, 0, 0, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn decode_missing_dimensions() {
        let encoded = BASE64.encode(&[0u8; 16]);
        let cmd = KittyCommand {
            format: Format::Rgba,
            width: 0,
            height: 0,
            payload: encoded.into_bytes(),
            ..Default::default()
        };
        assert!(decode_payload(&cmd, &cmd.payload).is_err());
    }

    #[test]
    fn decode_zlib_compressed() {
        use flate2::write::ZlibEncoder;
        use std::io::Write;
        let pixels: Vec<u8> = vec![255, 128, 64, 255];
        let mut encoder = ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&pixels).unwrap();
        let compressed = encoder.finish().unwrap();
        let encoded = BASE64.encode(&compressed);
        let cmd = KittyCommand {
            format: Format::Rgba,
            compression: Compression::Zlib,
            width: 1,
            height: 1,
            payload: encoded.into_bytes(),
            ..Default::default()
        };
        let data = decode_payload(&cmd, &cmd.payload).unwrap();
        assert_eq!(data.pixels, pixels);
    }

    #[test]
    fn decode_png_image() {
        let mut png_buf = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_buf, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[255, 0, 0, 255]).unwrap();
        }
        let encoded = BASE64.encode(&png_buf);
        let cmd = KittyCommand {
            format: Format::Png,
            payload: encoded.into_bytes(),
            ..Default::default()
        };
        let data = decode_payload(&cmd, &cmd.payload).unwrap();
        assert_eq!(data.pixels, vec![255, 0, 0, 255]);
    }

    #[test]
    fn rgb_to_rgba_conversion() {
        let rgb = vec![255, 0, 0, 0, 255, 0];
        let rgba = rgb_bytes_to_rgba(&rgb);
        assert_eq!(rgba, vec![255, 0, 0, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn decode_direct_with_padding_in_payload() {
        // Simulates chunked transfer where intermediate chunks include
        // trailing `=` padding (as chafa does). When chunks are merged,
        // `=` ends up in the middle of the base64 string.
        let pixels: Vec<u8> = vec![
            255, 0, 0, 255, 0, 255, 0, 255,
            0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let full_b64 = BASE64.encode(&pixels);

        // Split the base64 in the middle and add `=` padding to chunk 1,
        // simulating what a client like chafa would send across two APC
        // sequences that we then concatenate.
        let mid = full_b64.len() / 2;
        let chunk1 = &full_b64[..mid];
        let chunk2 = &full_b64[mid..];
        let merged_with_padding = format!("{chunk1}=={chunk2}");

        let cmd = KittyCommand {
            format: Format::Rgba, width: 2, height: 2,
            payload: merged_with_padding.into_bytes(), ..Default::default()
        };
        let data = decode_payload(&cmd, &cmd.payload).unwrap();
        assert_eq!(data.width, 2);
        assert_eq!(data.height, 2);
        assert_eq!(data.pixels, pixels);
    }

    #[test]
    fn decode_direct_with_trailing_padding() {
        // Standard base64 with trailing `=` padding should also work.
        let pixels: Vec<u8> = vec![255, 0, 0, 255, 0, 255, 0]; // 7 bytes → padded b64
        let b64_padded = BASE64.encode(&pixels);
        assert!(b64_padded.contains('='), "test expects padded base64");

        let cmd = KittyCommand {
            format: Format::Rgba, width: 0, height: 0,
            payload: b64_padded.into_bytes(), ..Default::default()
        };
        // Will fail on dimension check, but should NOT fail on base64 decode.
        let err = decode_payload(&cmd, &cmd.payload).unwrap_err();
        assert!(!err.to_string().contains("base64"), "should not be a base64 error, got: {err}");
    }

    // ── File medium tests ──────────────────────────────────────────────

    #[test]
    fn decode_file_medium_rgba() {
        use std::io::Write;

        // 2x1 RGBA pixel data: red + green.
        let pixels: Vec<u8> = vec![255, 0, 0, 255, 0, 255, 0, 255];

        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join(format!("kitty_test_file_{}", std::process::id()));
        {
            let mut f = std::fs::File::create(&file_path).unwrap();
            f.write_all(&pixels).unwrap();
        }

        let path_b64 = BASE64.encode(file_path.to_str().unwrap().as_bytes());
        let cmd = KittyCommand {
            format: Format::Rgba,
            medium: Medium::File,
            width: 2,
            height: 1,
            payload: path_b64.clone().into_bytes(),
            ..Default::default()
        };

        let data = decode_payload(&cmd, &cmd.payload).unwrap();
        assert_eq!(data.width, 2);
        assert_eq!(data.height, 1);
        assert_eq!(data.pixels, pixels);

        // Clean up (File medium does NOT delete the file).
        let _ = std::fs::remove_file(&file_path);
    }

    #[test]
    fn decode_file_medium_with_offset_and_size() {
        use std::io::Write;

        // Write 16 bytes: [header(4)] [pixels(8)] [trailer(4)]
        let header = [0xDE, 0xAD, 0xBE, 0xEF];
        let pixels: Vec<u8> = vec![255, 0, 0, 255, 0, 255, 0, 255];
        let trailer = [0xCA, 0xFE, 0xBA, 0xBE];

        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join(format!("kitty_test_offset_{}", std::process::id()));
        {
            let mut f = std::fs::File::create(&file_path).unwrap();
            f.write_all(&header).unwrap();
            f.write_all(&pixels).unwrap();
            f.write_all(&trailer).unwrap();
        }

        let path_b64 = BASE64.encode(file_path.to_str().unwrap().as_bytes());
        let cmd = KittyCommand {
            format: Format::Rgba,
            medium: Medium::File,
            width: 2,
            height: 1,
            data_offset: 4,
            data_size: 8,
            payload: path_b64.into_bytes(),
            ..Default::default()
        };

        let data = decode_payload(&cmd, &cmd.payload).unwrap();
        assert_eq!(data.pixels, pixels);

        let _ = std::fs::remove_file(&file_path);
    }

    #[test]
    fn decode_tempfile_medium_deletes_after_read() {
        use std::io::Write;

        let pixels: Vec<u8> = vec![255, 0, 0, 255, 0, 255, 0, 255];

        let temp_dir = std::env::temp_dir();
        let file_name = format!("kitty_test_tempfile_{}", std::process::id());
        let file_path = temp_dir.join(&file_name);
        {
            let mut f = std::fs::File::create(&file_path).unwrap();
            f.write_all(&pixels).unwrap();
        }

        assert!(file_path.exists(), "temp file should exist before decode");

        // For TempFile, the payload is the base64-encoded *relative* path (filename only).
        let path_b64 = BASE64.encode(file_name.as_bytes());
        let cmd = KittyCommand {
            format: Format::Rgba,
            medium: Medium::TempFile,
            width: 2,
            height: 1,
            payload: path_b64.into_bytes(),
            ..Default::default()
        };

        let data = decode_payload(&cmd, &cmd.payload).unwrap();
        assert_eq!(data.pixels, pixels);

        // TempFile medium should delete the file after reading.
        assert!(!file_path.exists(), "temp file should be deleted after decode");
    }

    #[test]
    fn decode_tempfile_rejects_parent_dir_traversal() {
        let evil_path = "../etc/passwd";
        let path_b64 = BASE64.encode(evil_path.as_bytes());
        let cmd = KittyCommand {
            format: Format::Rgba,
            medium: Medium::TempFile,
            width: 1,
            height: 1,
            payload: path_b64.into_bytes(),
            ..Default::default()
        };

        let err = decode_payload(&cmd, &cmd.payload).unwrap_err();
        assert!(
            matches!(err, DecodeError::PathTraversal(_)),
            "expected PathTraversal error, got: {err}"
        );
    }

    #[test]
    fn decode_tempfile_rejects_double_dot_in_middle() {
        let evil_path = "subdir/../../etc/passwd";
        let path_b64 = BASE64.encode(evil_path.as_bytes());
        let cmd = KittyCommand {
            format: Format::Rgba,
            medium: Medium::TempFile,
            width: 1,
            height: 1,
            payload: path_b64.into_bytes(),
            ..Default::default()
        };

        let err = decode_payload(&cmd, &cmd.payload).unwrap_err();
        assert!(
            matches!(err, DecodeError::PathTraversal(_)),
            "expected PathTraversal error, got: {err}"
        );
    }

    #[test]
    fn decode_tempfile_rejects_absolute_path() {
        let evil_path = "/etc/passwd";
        let path_b64 = BASE64.encode(evil_path.as_bytes());
        let cmd = KittyCommand {
            format: Format::Rgba,
            medium: Medium::TempFile,
            width: 1,
            height: 1,
            payload: path_b64.into_bytes(),
            ..Default::default()
        };

        let err = decode_payload(&cmd, &cmd.payload).unwrap_err();
        assert!(
            matches!(err, DecodeError::PathTraversal(_)),
            "expected PathTraversal error, got: {err}"
        );
    }

    #[test]
    fn decode_file_medium_with_zlib() {
        use flate2::write::ZlibEncoder;
        use std::io::Write;

        let pixels: Vec<u8> = vec![255, 0, 0, 255];
        let mut enc = ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&pixels).unwrap();
        let compressed = enc.finish().unwrap();

        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join(format!("kitty_test_zlib_{}", std::process::id()));
        {
            let mut f = std::fs::File::create(&file_path).unwrap();
            f.write_all(&compressed).unwrap();
        }

        let path_b64 = BASE64.encode(file_path.to_str().unwrap().as_bytes());
        let cmd = KittyCommand {
            format: Format::Rgba,
            medium: Medium::File,
            compression: Compression::Zlib,
            width: 1,
            height: 1,
            payload: path_b64.into_bytes(),
            ..Default::default()
        };

        let data = decode_payload(&cmd, &cmd.payload).unwrap();
        assert_eq!(data.pixels, pixels);

        let _ = std::fs::remove_file(&file_path);
    }

    #[test]
    fn decode_file_medium_png() {
        use std::io::Write;

        let mut png_buf = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_buf, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[0, 128, 255, 200]).unwrap();
        }

        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join(format!("kitty_test_png_{}", std::process::id()));
        {
            let mut f = std::fs::File::create(&file_path).unwrap();
            f.write_all(&png_buf).unwrap();
        }

        let path_b64 = BASE64.encode(file_path.to_str().unwrap().as_bytes());
        let cmd = KittyCommand {
            format: Format::Png,
            medium: Medium::File,
            payload: path_b64.into_bytes(),
            ..Default::default()
        };

        let data = decode_payload(&cmd, &cmd.payload).unwrap();
        assert_eq!(data.pixels, vec![0, 128, 255, 200]);
        assert_eq!(data.width, 1);
        assert_eq!(data.height, 1);

        let _ = std::fs::remove_file(&file_path);
    }

    // ── SharedMemory tests (unix only) ─────────────────────────────────

    #[cfg(unix)]
    mod shm_tests {
        use super::*;
        use std::ffi::CString;

        /// Helper: create a POSIX shared memory object with given data, return the name.
        fn create_shm(name: &str, data: &[u8]) -> CString {
            let c_name = CString::new(name).unwrap();
            unsafe {
                let fd = libc::shm_open(
                    c_name.as_ptr(),
                    libc::O_CREAT | libc::O_RDWR,
                    0o600,
                );
                assert!(fd >= 0, "shm_open failed: {}", std::io::Error::last_os_error());

                let ret = libc::ftruncate(fd, data.len() as libc::off_t);
                assert_eq!(ret, 0, "ftruncate failed");

                let ptr = libc::mmap(
                    std::ptr::null_mut(),
                    data.len(),
                    libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    fd,
                    0,
                );
                assert_ne!(ptr, libc::MAP_FAILED, "mmap failed");

                std::ptr::copy_nonoverlapping(data.as_ptr(), ptr as *mut u8, data.len());

                libc::munmap(ptr, data.len());
                libc::close(fd);
            }
            c_name
        }

        /// Check if a shm object still exists.
        fn shm_exists(name: &str) -> bool {
            let c_name = CString::new(name).unwrap();
            let fd = unsafe { libc::shm_open(c_name.as_ptr(), libc::O_RDONLY, 0) };
            if fd >= 0 {
                unsafe { libc::close(fd) };
                // Clean up the check-open by unlinking if it exists.
                // Actually, we just want to probe, not unlink. If it was
                // supposed to be gone, this tells us it's still there.
                true
            } else {
                false
            }
        }

        #[test]
        fn decode_shm_medium_rgba() {
            let shm_name = format!("/kitty_test_shm_{}", std::process::id());
            let pixels: Vec<u8> = vec![255, 0, 0, 255, 0, 255, 0, 255];

            create_shm(&shm_name, &pixels);

            let name_b64 = BASE64.encode(shm_name.as_bytes());
            let cmd = KittyCommand {
                format: Format::Rgba,
                medium: Medium::SharedMemory,
                width: 2,
                height: 1,
                // S= is required for shm: macOS allocates page-aligned regions,
                // so the fstat size may exceed the actual payload length.
                data_size: pixels.len() as u32,
                payload: name_b64.into_bytes(),
                ..Default::default()
            };

            let data = decode_payload(&cmd, &cmd.payload).unwrap();
            assert_eq!(data.width, 2);
            assert_eq!(data.height, 1);
            assert_eq!(data.pixels, pixels);

            // Verify shm was unlinked.
            assert!(!shm_exists(&shm_name), "shm should be unlinked after decode");
        }

        #[test]
        fn decode_shm_medium_with_offset_and_size() {
            let shm_name = format!("/kitty_test_shm_off_{}", std::process::id());

            // Write: [header(4)] [pixels(8)] [trailer(4)]
            let mut full_data = vec![0xDE, 0xAD, 0xBE, 0xEF];
            let pixels: Vec<u8> = vec![10, 20, 30, 255, 40, 50, 60, 255];
            full_data.extend_from_slice(&pixels);
            full_data.extend_from_slice(&[0xCA, 0xFE, 0xBA, 0xBE]);

            create_shm(&shm_name, &full_data);

            let name_b64 = BASE64.encode(shm_name.as_bytes());
            let cmd = KittyCommand {
                format: Format::Rgba,
                medium: Medium::SharedMemory,
                width: 2,
                height: 1,
                data_offset: 4,
                data_size: 8,
                payload: name_b64.into_bytes(),
                ..Default::default()
            };

            let data = decode_payload(&cmd, &cmd.payload).unwrap();
            assert_eq!(data.pixels, pixels);

            assert!(!shm_exists(&shm_name), "shm should be unlinked after decode");
        }

        #[test]
        fn decode_shm_medium_cleanup_on_success() {
            let shm_name = format!("/kitty_test_shm_clean_{}", std::process::id());
            let pixels: Vec<u8> = vec![100, 200, 50, 255];

            create_shm(&shm_name, &pixels);
            assert!(shm_exists(&shm_name), "shm should exist before decode");

            let name_b64 = BASE64.encode(shm_name.as_bytes());
            let cmd = KittyCommand {
                format: Format::Rgba,
                medium: Medium::SharedMemory,
                width: 1,
                height: 1,
                data_size: pixels.len() as u32,
                payload: name_b64.into_bytes(),
                ..Default::default()
            };

            let data = decode_payload(&cmd, &cmd.payload).unwrap();
            assert_eq!(data.pixels, pixels);

            // The shm should be fully cleaned up.
            assert!(!shm_exists(&shm_name), "shm must be cleaned up after decode");
        }

        #[test]
        fn decode_shm_nonexistent_returns_error() {
            let shm_name = "/kitty_test_shm_nonexistent_999999";

            let name_b64 = BASE64.encode(shm_name.as_bytes());
            let cmd = KittyCommand {
                format: Format::Rgba,
                medium: Medium::SharedMemory,
                width: 1,
                height: 1,
                payload: name_b64.into_bytes(),
                ..Default::default()
            };

            let err = decode_payload(&cmd, &cmd.payload);
            assert!(err.is_err(), "nonexistent shm should error");
        }
    }

    // ── validate_temp_path unit tests ──────────────────────────────────

    #[test]
    fn validate_temp_path_simple_filename() {
        assert!(validate_temp_path("image.png").is_ok());
    }

    #[test]
    fn validate_temp_path_subdir() {
        assert!(validate_temp_path("subdir/image.png").is_ok());
    }

    #[test]
    fn validate_temp_path_dot_component() {
        // Current-dir "." is harmless.
        assert!(validate_temp_path("./image.png").is_ok());
    }

    #[test]
    fn validate_temp_path_rejects_parent() {
        assert!(validate_temp_path("..").is_err());
        assert!(validate_temp_path("../image.png").is_err());
        assert!(validate_temp_path("a/../b").is_err());
    }

    #[test]
    fn validate_temp_path_rejects_absolute() {
        assert!(validate_temp_path("/etc/passwd").is_err());
        assert!(validate_temp_path("/tmp/file").is_err());
    }
}