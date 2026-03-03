use base64::engine::{Engine, general_purpose::STANDARD};

/// Default maximum dimension (longest edge) for VLM images.
pub const DEFAULT_MAX_DIMENSION: u32 = 1280;

/// Downscale and JPEG-encode raw image bytes for VLM consumption.
///
/// - Decodes from any format the `image` crate supports (PNG, JPEG, etc.)
/// - Downscales proportionally so the longest edge ≤ `max_dimension`
///   (no-op if already within bounds)
/// - Re-encodes as JPEG
/// - Returns `(base64_data, "image/jpeg")`
///
/// Returns `None` if the image cannot be decoded.
pub fn prepare_image_for_vlm(image_bytes: &[u8], max_dimension: u32) -> Option<(String, String)> {
    let img = image::load_from_memory(image_bytes).ok()?;
    Some(prepare_dynimage_for_vlm(img, max_dimension))
}

/// Downscale and JPEG-encode an already-decoded image for VLM consumption.
///
/// Use this when you already have a `DynamicImage` in memory to avoid a
/// redundant encode→decode round-trip through `prepare_image_for_vlm`.
pub fn prepare_dynimage_for_vlm(img: image::DynamicImage, max_dimension: u32) -> (String, String) {
    let (w, h) = (img.width(), img.height());
    let longest = w.max(h);

    let img = if longest > max_dimension {
        let scale = max_dimension as f64 / longest as f64;
        let new_w = (w as f64 * scale).round() as u32;
        let new_h = (h as f64 * scale).round() as u32;
        image::DynamicImage::ImageRgba8(image::imageops::resize(
            &img,
            new_w,
            new_h,
            image::imageops::FilterType::Triangle,
        ))
    } else {
        img
    };

    let mut buf = std::io::Cursor::new(Vec::new());
    // JPEG encode cannot fail for valid DynamicImage buffers.
    img.write_to(&mut buf, image::ImageFormat::Jpeg)
        .expect("JPEG encoding failed");

    (STANDARD.encode(buf.into_inner()), "image/jpeg".to_string())
}

/// Convenience wrapper: decode base64 image data, prepare for VLM.
/// Use when you already have base64-encoded image data (e.g. from MCP results).
pub fn prepare_base64_image_for_vlm(
    base64_data: &str,
    max_dimension: u32,
) -> Option<(String, String)> {
    let bytes = STANDARD.decode(base64_data).ok()?;
    prepare_image_for_vlm(&bytes, max_dimension)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_png(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([0, 128, 255, 255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        buf.into_inner()
    }

    #[test]
    fn downscales_large_image_to_max_dimension() {
        let png = make_test_png(2560, 1440);
        let (b64, mime) = prepare_image_for_vlm(&png, 1280).unwrap();
        assert_eq!(mime, "image/jpeg");

        let decoded = STANDARD.decode(&b64).unwrap();
        let img = image::load_from_memory(&decoded).unwrap();
        assert_eq!(img.width(), 1280);
        assert_eq!(img.height(), 720);
    }

    #[test]
    fn preserves_small_image_dimensions() {
        let png = make_test_png(800, 600);
        let (b64, mime) = prepare_image_for_vlm(&png, 1280).unwrap();
        assert_eq!(mime, "image/jpeg");

        let decoded = STANDARD.decode(&b64).unwrap();
        let img = image::load_from_memory(&decoded).unwrap();
        assert_eq!(img.width(), 800);
        assert_eq!(img.height(), 600);
    }

    #[test]
    fn tall_image_scales_by_height() {
        let png = make_test_png(720, 2560);
        let (b64, _) = prepare_image_for_vlm(&png, 1280).unwrap();
        let decoded = STANDARD.decode(&b64).unwrap();
        let img = image::load_from_memory(&decoded).unwrap();
        assert_eq!(img.height(), 1280);
        assert_eq!(img.width(), 360);
    }

    #[test]
    fn base64_convenience_wrapper_works() {
        let png = make_test_png(2560, 1440);
        let b64_input = STANDARD.encode(&png);
        let (b64_out, mime) = prepare_base64_image_for_vlm(&b64_input, 1280).unwrap();
        assert_eq!(mime, "image/jpeg");
        assert!(!b64_out.is_empty());
    }

    #[test]
    fn returns_none_for_invalid_bytes() {
        assert!(prepare_image_for_vlm(b"not an image", 1280).is_none());
    }
}
