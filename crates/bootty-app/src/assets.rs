use std::io::Cursor;

#[cfg(not(target_os = "macos"))]
use std::sync::OnceLock;

use eframe::egui;

#[cfg(not(target_os = "macos"))]
const NATIVE_APP_ICON_PNG: &[u8] = MASCOT_ICON_PNG;
const MASCOT_ICON_PNG: &[u8] = include_bytes!("../assets/bootty-mascot.png");

pub(crate) fn title_icon_color_image() -> egui::ColorImage {
    let icon = decode_png(MASCOT_ICON_PNG);
    egui::ColorImage::from_rgba_unmultiplied(
        [icon.width as usize, icon.height as usize],
        &icon.rgba,
    )
}

#[cfg(not(target_os = "macos"))]
const NATIVE_APP_ICON_SIZE: u32 = 256;

#[cfg(not(target_os = "macos"))]
pub(crate) fn native_app_icon_data() -> egui::IconData {
    static ICON: OnceLock<egui::IconData> = OnceLock::new();
    ICON.get_or_init(|| {
        let icon = decode_png(NATIVE_APP_ICON_PNG);
        let rgba =
            downscale_rgba_to_square(&icon.rgba, icon.width, icon.height, NATIVE_APP_ICON_SIZE);
        egui::IconData {
            rgba,
            width: NATIVE_APP_ICON_SIZE,
            height: NATIVE_APP_ICON_SIZE,
        }
    })
    .clone()
}

/// Box-filter downscale with alpha-weighted color averaging, so transparent
/// source pixels do not darken the edges of the icon.
#[cfg(not(target_os = "macos"))]
fn downscale_rgba_to_square(rgba: &[u8], width: u32, height: u32, target: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((target * target * 4) as usize);
    for target_y in 0..target {
        let y0 = (target_y * height / target) as usize;
        let y1 = ((target_y + 1) * height)
            .div_ceil(target)
            .max(y0 as u32 + 1) as usize;
        for target_x in 0..target {
            let x0 = (target_x * width / target) as usize;
            let x1 = ((target_x + 1) * width).div_ceil(target).max(x0 as u32 + 1) as usize;
            let mut weighted_rgb = [0_u64; 3];
            let mut alpha_sum = 0_u64;
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * width as usize + x) * 4;
                    let alpha = u64::from(rgba[i + 3]);
                    for (sum, channel) in weighted_rgb.iter_mut().zip(&rgba[i..i + 3]) {
                        *sum += u64::from(*channel) * alpha;
                    }
                    alpha_sum += alpha;
                }
            }
            if alpha_sum == 0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                let pixel_count = ((y1 - y0) * (x1 - x0)) as u64;
                out.extend(weighted_rgb.map(|sum| (sum / alpha_sum) as u8));
                out.push((alpha_sum / pixel_count) as u8);
            }
        }
    }
    out
}

struct AppIconPng {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

fn decode_png(png_bytes: &[u8]) -> AppIconPng {
    let mut decoder = png::Decoder::new(Cursor::new(png_bytes));
    decoder.set_transformations(png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .expect("embedded Bootty icon PNG header decodes");
    let output_size = reader
        .output_buffer_size()
        .expect("embedded Bootty icon PNG output size is known");
    let mut output = vec![0; output_size];
    let info = reader
        .next_frame(&mut output)
        .expect("embedded Bootty icon PNG frame decodes");
    let bytes = &output[..info.buffer_size()];
    let rgba = rgba_from_png(bytes, info.color_type);
    assert_eq!(
        rgba.len(),
        (info.width * info.height * 4) as usize,
        "embedded Bootty icon decodes to rgba8"
    );
    AppIconPng {
        rgba,
        width: info.width,
        height: info.height,
    }
}

fn rgba_from_png(bytes: &[u8], color_type: png::ColorType) -> Vec<u8> {
    match color_type {
        png::ColorType::Rgba => bytes.to_vec(),
        png::ColorType::Rgb => bytes
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|rgb| [rgb[0], rgb[1], rgb[2], 255])
            .collect(),
        png::ColorType::Grayscale => bytes
            .iter()
            .flat_map(|gray| [*gray, *gray, *gray, 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => bytes
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|gray| [gray[0], gray[0], gray[0], gray[1]])
            .collect(),
        png::ColorType::Indexed => panic!("indexed Bootty icon PNG is unsupported"),
    }
}
