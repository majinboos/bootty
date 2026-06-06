use libghostty_vt::{
    alloc::{Allocator, Bytes},
    kitty::graphics::{DecodePng, DecodedImage},
};

#[derive(Default)]
pub(crate) struct BoottyPngDecoder;

impl DecodePng for BoottyPngDecoder {
    fn decode_png<'alloc>(
        &mut self,
        alloc: &'alloc Allocator<'_>,
        data: &[u8],
    ) -> Option<DecodedImage<'alloc>> {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
        decoder.set_transformations(png::Transformations::ALPHA | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().ok()?;
        let mut buffer = vec![0; reader.output_buffer_size()?];
        let info = reader.next_frame(&mut buffer).ok()?;
        let rgba = png_frame_to_rgba8(
            &buffer[..info.buffer_size()],
            info.color_type,
            info.bit_depth,
        )?;
        let mut bytes = Bytes::new_with_alloc(alloc, rgba.len()).ok()?;
        bytes.copy_from_slice(&rgba);
        Some(DecodedImage {
            width: info.width,
            height: info.height,
            data: bytes,
        })
    }
}

pub(crate) fn png_frame_to_rgba8(
    data: &[u8],
    color_type: png::ColorType,
    bit_depth: png::BitDepth,
) -> Option<Vec<u8>> {
    match (color_type, bit_depth) {
        (png::ColorType::Rgba, png::BitDepth::Eight) => Some(data.to_vec()),
        (png::ColorType::Rgba, png::BitDepth::Sixteen) => {
            let mut rgba = Vec::with_capacity(data.len() / 2);
            for pixel in data.chunks_exact(8) {
                rgba.extend_from_slice(&[pixel[0], pixel[2], pixel[4], pixel[6]]);
            }
            Some(rgba)
        }
        (png::ColorType::Rgb, png::BitDepth::Eight) => {
            let mut rgba = Vec::with_capacity(data.len() / 3 * 4);
            for rgb in data.chunks_exact(3) {
                rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
            Some(rgba)
        }
        (png::ColorType::Rgb, png::BitDepth::Sixteen) => {
            let mut rgba = Vec::with_capacity(data.len() / 6 * 4);
            for rgb in data.chunks_exact(6) {
                rgba.extend_from_slice(&[rgb[0], rgb[2], rgb[4], 255]);
            }
            Some(rgba)
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
            Some(rgba)
        }
        (png::ColorType::GrayscaleAlpha, png::BitDepth::Sixteen) => {
            let mut rgba = Vec::with_capacity(data.len() / 4 * 4);
            for gray_alpha in data.chunks_exact(4) {
                rgba.extend_from_slice(&[
                    gray_alpha[0],
                    gray_alpha[0],
                    gray_alpha[0],
                    gray_alpha[2],
                ]);
            }
            Some(rgba)
        }
        (png::ColorType::Grayscale, png::BitDepth::Eight) => {
            let mut rgba = Vec::with_capacity(data.len() * 4);
            for gray in data {
                rgba.extend_from_slice(&[*gray, *gray, *gray, 255]);
            }
            Some(rgba)
        }
        (png::ColorType::Grayscale, png::BitDepth::Sixteen) => {
            let mut rgba = Vec::with_capacity(data.len() / 2 * 4);
            for gray in data.chunks_exact(2) {
                rgba.extend_from_slice(&[gray[0], gray[0], gray[0], 255]);
            }
            Some(rgba)
        }
        _ => None,
    }
}
