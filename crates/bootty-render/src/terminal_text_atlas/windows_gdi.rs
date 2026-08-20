use std::ptr::null_mut;

use windows_sys::Win32::{
    Foundation::COLORREF,
    Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BLACKNESS, CLEARTYPE_NATURAL_QUALITY, CLIP_DEFAULT_PRECIS,
        CreateCompatibleDC, CreateDIBSection, CreateFontW, DEFAULT_CHARSET, DEFAULT_PITCH,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GetTextMetricsW, HBITMAP, HDC, HFONT,
        OUT_TT_PRECIS, PatBlt, SelectObject, SetBkMode, SetTextColor, TEXTMETRICW, TRANSPARENT,
        TextOutW,
    },
};

use super::clusters::ShapedCluster;
use crate::terminal_text::FontStyle;

pub(super) fn rasterize_text_cluster(
    family: &str,
    style: FontStyle,
    cluster: &ShapedCluster,
    physical_font_size: f32,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    if cluster.text.is_empty() || width == 0 || height == 0 {
        return None;
    }
    let text = cluster.text.encode_utf16().collect::<Vec<_>>();
    let family = family
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let width_i32 = i32::try_from(width).ok()?;
    let height_i32 = i32::try_from(height).ok()?;
    let font_height = -(physical_font_size.round().max(1.0) as i32);

    let mut bits = null_mut();
    let mut bitmap_info = BITMAPINFO::default();
    bitmap_info.bmiHeader.biSize = std::mem::size_of_val(&bitmap_info.bmiHeader) as u32;
    bitmap_info.bmiHeader.biWidth = width_i32;
    bitmap_info.bmiHeader.biHeight = -height_i32;
    bitmap_info.bmiHeader.biPlanes = 1;
    bitmap_info.bmiHeader.biBitCount = 32;
    bitmap_info.bmiHeader.biCompression = BI_RGB;

    let dc = GdiDc::new()?;
    let bitmap = GdiBitmap::new(dc.0, &bitmap_info, &mut bits)?;
    let font = GdiFont::new(font_height, style, &family)?;

    // SAFETY: GDI handles are checked for null, selected only into the memory DC,
    // and restored/deleted by RAII guards before this function returns.
    unsafe {
        let old_bitmap = SelectObject(dc.0, bitmap.0.cast());
        if old_bitmap.is_null() {
            return None;
        }
        let old_font = SelectObject(dc.0, font.0.cast());
        if old_font.is_null() {
            SelectObject(dc.0, old_bitmap);
            return None;
        }

        PatBlt(dc.0, 0, 0, width_i32, height_i32, BLACKNESS);
        SetBkMode(dc.0, TRANSPARENT as i32);
        SetTextColor(dc.0, rgb(255, 255, 255));

        let mut metrics = TEXTMETRICW::default();
        if GetTextMetricsW(dc.0, &mut metrics) == 0 {
            SelectObject(dc.0, old_font);
            SelectObject(dc.0, old_bitmap);
            return None;
        }
        let y = ((height_i32 - metrics.tmHeight) / 2).max(0);
        let text_len = i32::try_from(text.len()).ok()?;
        if TextOutW(dc.0, 0, y, text.as_ptr(), text_len) == 0 {
            SelectObject(dc.0, old_font);
            SelectObject(dc.0, old_bitmap);
            return None;
        }

        let source = std::slice::from_raw_parts(bits.cast::<u8>(), (width * height * 4) as usize);
        let mut alpha = Vec::with_capacity((width * height) as usize);
        for pixel in source.chunks_exact(4) {
            let blue = u16::from(pixel[0]);
            let green = u16::from(pixel[1]);
            let red = u16::from(pixel[2]);
            alpha.push(((red + green + blue) / 3) as u8);
        }

        SelectObject(dc.0, old_font);
        SelectObject(dc.0, old_bitmap);
        alpha.iter().any(|value| *value > 0).then_some(alpha)
    }
}

struct GdiDc(HDC);

impl GdiDc {
    fn new() -> Option<Self> {
        // SAFETY: a null source DC creates a memory DC compatible with the current screen.
        let dc = unsafe { CreateCompatibleDC(null_mut()) };
        (!dc.is_null()).then_some(Self(dc))
    }
}

impl Drop for GdiDc {
    fn drop(&mut self) {
        // SAFETY: handle was returned by CreateCompatibleDC and is owned here.
        unsafe {
            DeleteDC(self.0);
        }
    }
}

struct GdiBitmap(HBITMAP);

impl GdiBitmap {
    fn new(dc: HDC, info: &BITMAPINFO, bits: &mut *mut std::ffi::c_void) -> Option<Self> {
        // SAFETY: info points to an initialized 32-bit top-down DIB description; bits is valid out-param.
        let bitmap = unsafe { CreateDIBSection(dc, info, DIB_RGB_COLORS, bits, null_mut(), 0) };
        (!bitmap.is_null() && !bits.is_null()).then_some(Self(bitmap))
    }
}

impl Drop for GdiBitmap {
    fn drop(&mut self) {
        // SAFETY: handle was returned by CreateDIBSection and is owned here.
        unsafe {
            DeleteObject(self.0.cast());
        }
    }
}

struct GdiFont(HFONT);

impl GdiFont {
    fn new(height: i32, style: FontStyle, family: &[u16]) -> Option<Self> {
        let weight = if matches!(style, FontStyle::Bold | FontStyle::BoldItalic) {
            700
        } else {
            400
        };
        let italic = u32::from(matches!(style, FontStyle::Italic | FontStyle::BoldItalic));
        // SAFETY: family is null-terminated and all other parameters are plain value settings.
        let font = unsafe {
            CreateFontW(
                height,
                0,
                0,
                0,
                weight,
                italic,
                0,
                0,
                u32::from(DEFAULT_CHARSET),
                u32::from(OUT_TT_PRECIS),
                u32::from(CLIP_DEFAULT_PRECIS),
                CLEARTYPE_NATURAL_QUALITY,
                u32::from(DEFAULT_PITCH),
                family.as_ptr(),
            )
        };
        (!font.is_null()).then_some(Self(font))
    }
}

impl Drop for GdiFont {
    fn drop(&mut self) {
        // SAFETY: handle was returned by CreateFontW and is owned here.
        unsafe {
            DeleteObject(self.0.cast());
        }
    }
}

const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    red as COLORREF | ((green as COLORREF) << 8) | ((blue as COLORREF) << 16)
}
