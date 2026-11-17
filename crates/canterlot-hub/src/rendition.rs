//! Уменьшенный показ картинок для ленты и просмотрщика: самая длинная сторона не больше
//! `RENDITION_LONG_SIDE`, показ всегда JPEG. GIF и WebP отдаются как есть (анимацию нельзя
//! ронять до первого кадра), повреждённые и слишком крупные файлы тоже.

/// Самая длинная сторона показа в пикселях: хватает для миниатюры ленты и фона просмотрщика.
pub const RENDITION_LONG_SIDE: u32 = 1024;
/// Больше не раскодируем: квадрат примерно 4096×4096, иначе память и процессор телефона страдают.
pub const MAX_RENDITION_PIXELS: u64 = 16_777_216;
/// Качество JPEG показа: текст скриншотов остаётся читаемым, файл в разы меньше оригинала.
pub const RENDITION_JPEG_QUALITY: u8 = 80;

/// Суффикс версии ETag показа: смена размера или кодека обязана сбросить кеш браузеров.
pub const RENDITION_ETAG_SUFFIX: &str = "preview1";

/// ETag показа: SHA-256 оригинала плюс версия показа, сам оригинал неизменен.
pub fn rendition_etag(sha256: &str) -> String {
    format!("\"{sha256}-{RENDITION_ETAG_SUFFIX}\"")
}

pub enum Rendition {
    /// Уменьшенный JPEG для показа.
    Scaled { bytes: Vec<u8>, mime: &'static str },
    /// Оригинал как есть: анимация, чужой тип, повреждение или слишком крупный файл.
    Original,
}

pub fn rendition(original: &[u8], mime: &str) -> Rendition {
    let format = match mime {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        _ => return Rendition::Original,
    };
    // Сначала дешёвый заголовок: огромную картинку не раскодируем вовсе.
    let dims = image::ImageReader::new(std::io::Cursor::new(original))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok());
    let Some((width, height)) = dims else {
        return Rendition::Original;
    };
    if width == 0
        || height == 0
        || u64::from(width) * u64::from(height) > MAX_RENDITION_PIXELS
    {
        return Rendition::Original;
    }
    let image = match image::load_from_memory_with_format(original, format) {
        Ok(image) => image,
        Err(_) => return Rendition::Original,
    };
    let longest = width.max(height);
    let scaled = if longest > RENDITION_LONG_SIDE {
        image.thumbnail(RENDITION_LONG_SIDE, RENDITION_LONG_SIDE)
    } else {
        image
    };
    let mut bytes = Vec::new();
    let encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, RENDITION_JPEG_QUALITY);
    match scaled.write_with_encoder(encoder) {
        Ok(()) => Rendition::Scaled {
            bytes,
            mime: "image/jpeg",
        },
        Err(_) => Rendition::Original,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 251) as u8, (y % 251) as u8, ((x + y) % 251) as u8])
        });
        let mut bytes = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut bytes);
        image::DynamicImage::ImageRgb8(image)
            .write_with_encoder(encoder)
            .unwrap();
        bytes
    }

    fn jpeg_dims(bytes: &[u8]) -> (u32, u32) {
        image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap()
    }

    #[test]
    fn wide_png_shrinks_to_long_side() {
        let bytes = png(2048, 1536);
        let Rendition::Scaled { bytes, mime } = rendition(&bytes, "image/png") else {
            panic!("wide png must scale");
        };
        assert_eq!(mime, "image/jpeg");
        assert_eq!(jpeg_dims(&bytes), (1024, 768));
        assert!(bytes.starts_with(&[0xff, 0xd8, 0xff]));
    }

    #[test]
    fn small_jpeg_becomes_jpeg_without_upscale() {
        let image = image::RgbImage::from_fn(640, 480, |x, y| {
            image::Rgb([(x % 251) as u8, (y % 251) as u8, 7])
        });
        let mut original = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut original),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let Rendition::Scaled { bytes, mime } = rendition(&original, "image/jpeg") else {
            panic!("small jpeg must still become a preview");
        };
        assert_eq!(mime, "image/jpeg");
        assert_eq!(jpeg_dims(&bytes), (640, 480));
    }

    #[test]
    fn animation_and_webp_pass_through() {
        let gif = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;".to_vec();
        assert!(matches!(
            rendition(&gif, "image/gif"),
            Rendition::Original
        ));
        let webp = png(4, 4);
        assert!(matches!(
            rendition(&webp, "image/webp"),
            Rendition::Original
        ));
    }

    #[test]
    fn huge_headers_and_broken_bytes_pass_through() {
        // Заголовок врёт про 10000×10000: раскодировать нельзя, отдаём оригинал.
        let mut huge = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec();
        huge.extend(10_000u32.to_be_bytes());
        huge.extend(10_000u32.to_be_bytes());
        assert!(matches!(
            rendition(&huge, "image/png"),
            Rendition::Original
        ));
        assert!(matches!(
            rendition(b"not an image", "image/png"),
            Rendition::Original
        ));
    }
}
