use crate::{geometry::SurfaceRect, terminal_image::KittyImagePlacement};
use eframe::wgpu;
use libghostty_vt::kitty::graphics::ImageFormat;
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
    let inv_width = 1.0 / image.image_width as f32;
    let inv_height = 1.0 / image.image_height as f32;
    Some(SurfaceRect {
        min_x: (image.source.x as f32 + 0.5) * inv_width,
        min_y: (image.source.y as f32 + 0.5) * inv_height,
        max_x: (max_x as f32 - 0.5) * inv_width,
        max_y: (max_y as f32 - 0.5) * inv_height,
    })
}

pub(super) fn rgba_image_pixels(image: &KittyImagePlacement) -> Option<Cow<'_, [u8]>> {
    let pixels = image.image_width.checked_mul(image.image_height)? as usize;
    let channels = match image.image_format {
        ImageFormat::Rgba => 4,
        ImageFormat::Rgb => 3,
        ImageFormat::GrayAlpha => 2,
        ImageFormat::Gray => 1,
        ImageFormat::Png => return decode_png_rgba(image),
        _ => return None,
    };
    expand_rgba(&image.data, pixels, channels)
}

fn expand_rgba(data: &[u8], pixels: usize, channels: usize) -> Option<Cow<'_, [u8]>> {
    match channels {
        4 => Some(Cow::Borrowed(data.get(..pixels.checked_mul(4)?)?)),
        3 => Some(Cow::Owned(expand_pixels::<3>(
            data,
            pixels,
            |[r, g, b]| [r, g, b, 255],
        )?)),
        2 => Some(Cow::Owned(expand_pixels::<2>(data, pixels, |[g, a]| {
            [g, g, g, a]
        })?)),
        1 => Some(Cow::Owned(expand_pixels::<1>(data, pixels, |[g]| {
            [g, g, g, 255]
        })?)),
        _ => None,
    }
}

fn expand_pixels<const N: usize>(
    data: &[u8],
    pixels: usize,
    convert: impl Fn([u8; N]) -> [u8; 4],
) -> Option<Vec<u8>> {
    let data = data.get(..pixels.checked_mul(N)?)?;
    let mut rgba = Vec::with_capacity(pixels * 4);
    for pixel in data.as_chunks::<N>().0 {
        rgba.extend_from_slice(&convert(*pixel));
    }
    Some(rgba)
}

pub(super) fn rgba_image_texture_pixels(image: &KittyImagePlacement) -> Option<Cow<'_, [u8]>> {
    let pixels = rgba_image_pixels(image)?;
    if pixels
        .as_chunks::<4>()
        .0
        .iter()
        .all(|pixel| pixel[3] == 255)
    {
        return Some(pixels);
    }

    let mut premultiplied = Vec::with_capacity(pixels.len());
    for pixel in pixels.as_chunks::<4>().0 {
        premultiplied.extend_from_slice(&[
            premultiply_unorm_channel(pixel[0], pixel[3]),
            premultiply_unorm_channel(pixel[1], pixel[3]),
            premultiply_unorm_channel(pixel[2], pixel[3]),
            pixel[3],
        ]);
    }
    Some(Cow::Owned(premultiplied))
}

fn premultiply_unorm_channel(value: u8, alpha: u8) -> u8 {
    let value = u16::from(value);
    let alpha = u16::from(alpha);
    ((value * alpha + 127) / 255) as u8
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
    if info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let channels = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Grayscale => 1,
        _ => return None,
    };
    let pixels = image.image_width.checked_mul(image.image_height)? as usize;
    Some(Cow::Owned(
        expand_rgba(data, pixels, channels)?.into_owned(),
    ))
}
