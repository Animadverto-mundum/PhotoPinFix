//! Decode preview pixels in memory. No preview image is written to disk.
use std::{
    fs::{File, FileTimes, OpenOptions},
    io::{Cursor, Read, Seek, SeekFrom},
    path::Path,
    time::SystemTime,
};

use image::{DynamicImage, ImageDecoder, ImageReader, metadata::Orientation};

use crate::metadata::MetadataTool;

pub fn load_preview(
    path: &Path,
    tool: &MetadataTool,
    max_width: u32,
    max_height: u32,
) -> Result<DynamicImage, String> {
    let guard = AccessTimeGuard::new(path)?;
    let result = load_preview_inner(path, tool, max_width.max(1), max_height.max(1));
    match guard.restore() {
        Ok(()) => result,
        Err(error) => Err(format!("无法恢复照片访问时间：{error}")),
    }
}

fn load_preview_inner(
    path: &Path,
    tool: &MetadataTool,
    max_width: u32,
    max_height: u32,
) -> Result<DynamicImage, String> {
    let extension = path
        .extension()
        .and_then(|part| part.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(extension.as_str(), "heic" | "heif" | "hif") {
        return decode_heif(path, max_width, max_height);
    }
    let directly_decodable = matches!(
        extension.as_str(),
        "jpg" | "jpeg" | "png" | "tif" | "tiff" | "webp" | "avif" | "bmp" | "gif"
    );
    if directly_decodable && let Ok(image) = decode_file(path) {
        return Ok(image.thumbnail(max_width, max_height));
    }
    let embedded = tool.preview_bytes(path).and_then(|bytes| {
        let fallback = tool.orientation(path).ok().and_then(Orientation::from_exif);
        decode_bytes(&bytes, fallback)
    });
    match embedded {
        Ok(image) => Ok(image.thumbnail(max_width, max_height)),
        Err(embedded_error) if extension == "dng" => {
            // Some camera DNGs (including Leica M8) store a small rendered RGB
            // image directly in IFD0 instead of an embedded JPEG. Confirm the
            // reduced-resolution RGB tags so CFA/linear sensor data is never
            // displayed as if it were a developed photograph.
            if dng_has_rgb_thumbnail(path).unwrap_or(false) {
                decode_file(path)
                    .map(|image| image.thumbnail(max_width, max_height))
                    .map_err(|error| format!("DNG RGB 缩略图：{error}；{embedded_error}"))
            } else {
                Err(format!("DNG 没有可解码的内嵌预览：{embedded_error}"))
            }
        }
        Err(embedded_error) if extension == "avif" => decode_heif(path, max_width, max_height)
            .map_err(|error| format!("{error}；内嵌预览：{embedded_error}")),
        Err(error) => Err(format!("无法读取图片或内嵌预览：{error}")),
    }
}

fn dng_has_rgb_thumbnail(path: &Path) -> std::io::Result<bool> {
    let mut file = File::open(path)?;
    let mut header = [0u8; 8];
    file.read_exact(&mut header)?;
    let little_endian = match &header[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return Ok(false),
    };
    let short = |bytes: &[u8]| {
        let bytes = [bytes[0], bytes[1]];
        if little_endian {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        }
    };
    let long = |bytes: &[u8]| {
        let bytes = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if little_endian {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        }
    };
    if short(&header[2..4]) != 42 {
        return Ok(false);
    }
    let offset = long(&header[4..8]) as u64;
    let length = file.metadata()?.len();
    if offset < 8 || offset.saturating_add(2) > length {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut count = [0u8; 2];
    file.read_exact(&mut count)?;
    let count = short(&count) as u64;
    if count > 4096 || offset.saturating_add(2 + 12 * count) > length {
        return Ok(false);
    }
    let mut reduced = false;
    let mut rgb = false;
    for _ in 0..count {
        let mut entry = [0u8; 12];
        file.read_exact(&mut entry)?;
        if long(&entry[4..8]) != 1 {
            continue;
        }
        match (short(&entry[..2]), short(&entry[2..4])) {
            (254, 4) => reduced = long(&entry[8..12]) & 1 != 0,
            (255, 3) => reduced |= short(&entry[8..10]) == 2,
            (262, 3) => rgb = short(&entry[8..10]) == 2,
            _ => {}
        }
    }
    Ok(reduced && rgb)
}

fn decode_file(path: &Path) -> Result<DynamicImage, String> {
    let reader = ImageReader::open(path)
        .map_err(|error| format!("读取图片失败：{error}"))?
        .with_guessed_format()
        .map_err(|error| format!("识别图片失败：{error}"))?;
    let mut decoder = reader
        .into_decoder()
        .map_err(|error| format!("图片格式无法解码：{error}"))?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut image =
        DynamicImage::from_decoder(decoder).map_err(|error| format!("图片解码失败：{error}"))?;
    image.apply_orientation(orientation);
    Ok(image)
}

fn decode_bytes(bytes: &[u8], fallback: Option<Orientation>) -> Result<DynamicImage, String> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| format!("识别内嵌预览失败：{error}"))?;
    let mut decoder = reader
        .into_decoder()
        .map_err(|error| format!("内嵌预览格式无法解码：{error}"))?;
    let orientation = match decoder.orientation().unwrap_or(Orientation::NoTransforms) {
        Orientation::NoTransforms => fallback.unwrap_or(Orientation::NoTransforms),
        orientation => orientation,
    };
    let mut image = DynamicImage::from_decoder(decoder)
        .map_err(|error| format!("内嵌预览解码失败：{error}"))?;
    image.apply_orientation(orientation);
    Ok(image)
}

fn decode_heif(path: &Path, max_width: u32, max_height: u32) -> Result<DynamicImage, String> {
    use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma, StreamReader};

    let decoder = LibHeif::new();
    let file = File::open(path).map_err(|error| format!("读取 HEIF 失败：{error}"))?;
    let length = file.metadata().map_err(|error| error.to_string())?.len();
    // Rust opens the file, preserving Unicode/non-UTF8 pathname support without
    // passing paths through a C narrow-string API. No process or temp image.
    let reader = StreamReader::new(std::io::BufReader::new(file), length);
    let mut context = HeifContext::read_from_reader(Box::new(reader))
        .map_err(|error| format!("HEIF 容器读取失败：{error}"))?;
    context.set_max_decoding_threads(1);
    let handle = context
        .primary_image_handle()
        .map_err(|error| format!("HEIF 主图读取失败：{error}"))?;
    let pixels = u64::from(handle.width()) * u64::from(handle.height());
    if pixels == 0 || pixels > 128 * 1024 * 1024 {
        return Err("HEIF 主图尺寸无效或超出预览内存上限".into());
    }
    // libheif applies HEIF rotation/mirroring properties by default. Asking for
    // interleaved RGBA also converts 10/12-bit HEVC to displayable 8-bit pixels.
    let decoded = decoder
        .decode(&handle, ColorSpace::Rgb(RgbChroma::Rgba), None)
        .map_err(|error| format!("HEIF 原生解码失败：{error}"))?;
    let planes = decoded.planes();
    let plane = planes.interleaved.ok_or("HEIF 解码器没有返回 RGB 像素")?;
    let width = plane.width;
    let height = plane.height;
    let row_bytes = (width as usize).checked_mul(4).ok_or("HEIF 行宽溢出")?;
    let required = row_bytes
        .checked_mul(height as usize)
        .ok_or("HEIF 像素尺寸溢出")?;
    if plane.stride < row_bytes || plane.data.len() < plane.stride.saturating_mul(height as usize) {
        return Err("HEIF 解码器返回的像素缓冲区不完整".into());
    }
    let mut rgba = Vec::with_capacity(required);
    for row in plane.data.chunks(plane.stride).take(height as usize) {
        rgba.extend_from_slice(&row[..row_bytes]);
    }
    let image = image::RgbaImage::from_raw(width, height, rgba).ok_or("无法建立 HEIF 预览像素")?;
    Ok(DynamicImage::ImageRgba8(image).thumbnail(max_width, max_height))
}

/// Holding the original handle also avoids restoring timestamps on an unrelated
/// replacement path. FileTimes changes only atime; mtime remains untouched.
struct AccessTimeGuard {
    file: File,
    accessed: SystemTime,
}

impl AccessTimeGuard {
    fn new(path: &Path) -> Result<Self, String> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // GENERIC_READ | FILE_WRITE_ATTRIBUTES, without asking for data writes.
            options.access_mode(0x8000_0000 | 0x0000_0100);
        }
        let file = options
            .open(path)
            .map_err(|error| format!("读取照片失败：{error}"))?;
        let accessed = file
            .metadata()
            .and_then(|metadata| metadata.accessed())
            .map_err(|error| format!("读取访问时间失败：{error}"))?;
        Ok(Self { file, accessed })
    }

    fn restore(&self) -> std::io::Result<()> {
        self.file
            .set_times(FileTimes::new().set_accessed(self.accessed))
    }
}

impl Drop for AccessTimeGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
