use crate::{geometry::SurfaceRect, terminal_image::KittyImagePlacement};
use eframe::wgpu;
use std::borrow::Cow;

const MAX_IMAGE_UPLOAD_BYTES: u64 = 64 * 1024 * 1024;

pub(super) fn image_fits_device_limits(device: &wgpu::Device, image: &KittyImagePlacement) -> bool {
    let limits = device.limits();
    if image.image_width == 0
        || image.image_height == 0
        || image.image_width > limits.max_texture_dimension_2d
        || image.image_height > limits.max_texture_dimension_2d
    {
        return false;
    }
    let Some(bytes) = image
        .image_width
        .checked_mul(image.image_height)
        .and_then(|pixels| pixels.checked_mul(4))
        .map(u64::from)
    else {
        return false;
    };
    bytes <= MAX_IMAGE_UPLOAD_BYTES && source_uv_rect(image).is_some()
}

pub(super) fn source_uv_rect(image: &KittyImagePlacement) -> Option<SurfaceRect> {
    if image.source.width == 0 || image.source.height == 0 {
        return None;
    }
    let max_x = image.source.x.checked_add(image.source.width)?;
    let max_y = image.source.y.checked_add(image.source.height)?;
    if max_x > image.image_width || max_y > image.image_height {
        return None;
    }
    let inv_width = 1.0 / image.image_width.max(1) as f32;
    let inv_height = 1.0 / image.image_height.max(1) as f32;
    Some(SurfaceRect {
        min_x: (image.source.x as f32 + 0.5) * inv_width,
        min_y: (image.source.y as f32 + 0.5) * inv_height,
        max_x: (max_x as f32 - 0.5) * inv_width,
        max_y: (max_y as f32 - 0.5) * inv_height,
    })
}

pub(super) fn rgba_image_pixels(image: &KittyImagePlacement) -> Option<Cow<'_, [u8]>> {
    let pixels = image.image_width.checked_mul(image.image_height)? as usize;
    match image.image_format {
        libghostty_vt::kitty::graphics::ImageFormat::Rgba => {
            let expected = pixels.checked_mul(4)?;
            if image.data.len() < expected {
                return None;
            }
            Some(Cow::Borrowed(&image.data[..expected]))
        }
        libghostty_vt::kitty::graphics::ImageFormat::Rgb => {
            let expected = pixels.checked_mul(3)?;
            if image.data.len() < expected {
                return None;
            }
            let mut rgba = Vec::with_capacity(pixels * 4);
            for rgb in image.data[..expected].chunks_exact(3) {
                rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
            Some(Cow::Owned(rgba))
        }
        libghostty_vt::kitty::graphics::ImageFormat::GrayAlpha => {
            let expected = pixels.checked_mul(2)?;
            if image.data.len() < expected {
                return None;
            }
            let mut rgba = Vec::with_capacity(pixels * 4);
            for gray_alpha in image.data[..expected].chunks_exact(2) {
                rgba.extend_from_slice(&[
                    gray_alpha[0],
                    gray_alpha[0],
                    gray_alpha[0],
                    gray_alpha[1],
                ]);
            }
            Some(Cow::Owned(rgba))
        }
        libghostty_vt::kitty::graphics::ImageFormat::Gray => {
            if image.data.len() < pixels {
                return None;
            }
            let mut rgba = Vec::with_capacity(pixels * 4);
            for gray in &image.data[..pixels] {
                rgba.extend_from_slice(&[*gray, *gray, *gray, 255]);
            }
            Some(Cow::Owned(rgba))
        }
        libghostty_vt::kitty::graphics::ImageFormat::Png => decode_png_rgba(image),
        _ => None,
    }
}

fn decode_png_rgba(image: &KittyImagePlacement) -> Option<Cow<'_, [u8]>> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(image.data.as_slice()));
    decoder.set_transformations(png::Transformations::ALPHA | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buffer).ok()?;
    if info.width != image.image_width || info.height != image.image_height {
        return None;
    }
    let data = &buffer[..info.buffer_size()];
    match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgba, png::BitDepth::Eight) => Some(Cow::Owned(data.to_vec())),
        (png::ColorType::Rgb, png::BitDepth::Eight) => {
            let mut rgba = Vec::with_capacity(data.len() / 3 * 4);
            for rgb in data.chunks_exact(3) {
                rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
            Some(Cow::Owned(rgba))
        }
        (png::ColorType::GrayscaleAlpha, png::BitDepth::Eight) => {
            let mut rgba = Vec::with_capacity(data.len() / 2 * 4);
            for gray_alpha in data.chunks_exact(2) {
                rgba.extend_from_slice(&[
                    gray_alpha[0],
                    gray_alpha[0],
                    gray_alpha[0],
                    gray_alpha[1],
                ]);
            }
            Some(Cow::Owned(rgba))
        }
        (png::ColorType::Grayscale, png::BitDepth::Eight) => {
            let mut rgba = Vec::with_capacity(data.len() * 4);
            for gray in data {
                rgba.extend_from_slice(&[*gray, *gray, *gray, 255]);
            }
            Some(Cow::Owned(rgba))
        }
        _ => None,
    }
}
