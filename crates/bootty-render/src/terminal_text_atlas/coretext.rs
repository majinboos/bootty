use super::ShapedCluster;
use crate::{terminal_font_face::FontFaceMetrics, terminal_text::ResolvedFontFace};

#[cfg(target_os = "macos")]
use super::{is_combining_mark, is_variation_selector, unpremultiply_rgba};
#[cfg(target_os = "macos")]
use crate::terminal_font_face::{GlyphConstraintSize, GlyphSize, terminal_glyph_constraint};

#[cfg(target_os = "macos")]
use std::ffi::{CStr, CString, c_char, c_void};

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct CFRange {
    location: isize,
    length: isize,
}

#[cfg(target_os = "macos")]
type CFStringRef = *const c_void;
#[cfg(target_os = "macos")]
type CTFontRef = *const c_void;
#[cfg(target_os = "macos")]
type CGContextRef = *mut c_void;
#[cfg(target_os = "macos")]
type CGColorSpaceRef = *mut c_void;
#[cfg(target_os = "macos")]
type CGGlyph = u16;
#[cfg(target_os = "macos")]
type UniChar = u16;

#[cfg(target_os = "macos")]
const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
#[cfg(target_os = "macos")]
const K_CG_IMAGE_ALPHA_PREMULTIPLIED_LAST: u32 = 1;

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
    fn CFStringCreateWithCString(
        alloc: *const c_void,
        c_str: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    fn CFStringGetCStringPtr(the_string: CFStringRef, encoding: u32) -> *const c_char;
    fn CFStringGetCString(
        the_string: CFStringRef,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> u8;
}

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
    fn CGColorSpaceCreateDeviceGray() -> CGColorSpaceRef;
    fn CGColorSpaceRelease(space: CGColorSpaceRef);
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: CGColorSpaceRef,
        bitmap_info: u32,
    ) -> CGContextRef;
    fn CGContextRelease(context: CGContextRef);
    fn CGContextSetGrayFillColor(context: CGContextRef, gray: f64, alpha: f64);
    fn CGContextFillRect(context: CGContextRef, rect: CGRect);
    fn CGContextSetAllowsFontSmoothing(context: CGContextRef, allows: bool);
    fn CGContextSetShouldSmoothFonts(context: CGContextRef, should: bool);
    fn CGContextSetAllowsFontSubpixelPositioning(context: CGContextRef, allows: bool);
    fn CGContextSetShouldSubpixelPositionFonts(context: CGContextRef, should: bool);
    fn CGContextSetAllowsFontSubpixelQuantization(context: CGContextRef, allows: bool);
    fn CGContextSetShouldSubpixelQuantizeFonts(context: CGContextRef, should: bool);
    fn CGContextSetAllowsAntialiasing(context: CGContextRef, allows: bool);
    fn CGContextSetShouldAntialias(context: CGContextRef, should: bool);
    fn CGContextTranslateCTM(context: CGContextRef, tx: f64, ty: f64);
    fn CGContextScaleCTM(context: CGContextRef, sx: f64, sy: f64);
}

#[cfg(target_os = "macos")]
#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    fn CTFontCreateWithName(name: CFStringRef, size: f64, matrix: *const c_void) -> CTFontRef;
    fn CTFontGetGlyphsForCharacters(
        font: CTFontRef,
        characters: *const UniChar,
        glyphs: *mut CGGlyph,
        count: isize,
    ) -> bool;
    fn CTFontGetBoundingRectsForGlyphs(
        font: CTFontRef,
        orientation: u32,
        glyphs: *const CGGlyph,
        bounding_rects: *mut CGRect,
        count: isize,
    ) -> CGRect;
    fn CTFontDrawGlyphs(
        font: CTFontRef,
        glyphs: *const CGGlyph,
        positions: *const CGPoint,
        count: usize,
        context: CGContextRef,
    );
    fn CTFontCreateForString(
        current_font: CTFontRef,
        string: CFStringRef,
        range: CFRange,
    ) -> CTFontRef;
    fn CTFontCopyFamilyName(font: CTFontRef) -> CFStringRef;
    fn CTFontCopyPostScriptName(font: CTFontRef) -> CFStringRef;
}

#[cfg(target_os = "macos")]
pub(super) fn rasterize_symbol_cluster(
    face: &ResolvedFontFace,
    cluster: &ShapedCluster,
    physical_font_size: f32,
    metrics: FontFaceMetrics,
    constraint_cells: u16,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    let ch = cluster.text.chars().next()?;
    if cluster.text.chars().nth(1).is_some()
        || !terminal_glyph_constraint(ch as u32).does_anything()
    {
        return None;
    }

    let mut families = Vec::with_capacity(face.fallback_families.len() + 2);
    families.push(face.family.as_str());
    families.extend(face.fallback_families.iter().map(String::as_str));

    for family in families {
        if let Some(alpha) = rasterize_symbol_with_family(
            family,
            ch,
            physical_font_size,
            metrics,
            constraint_cells,
            width,
            height,
        ) {
            return Some(alpha);
        }
    }

    let names = fallback_names(&face.family, ch, physical_font_size)?;
    rasterize_symbol_with_family(
        &names.postscript,
        ch,
        physical_font_size,
        metrics,
        constraint_cells,
        width,
        height,
    )
}

#[cfg(not(target_os = "macos"))]
pub(super) fn rasterize_symbol_cluster(
    _face: &ResolvedFontFace,
    _cluster: &ShapedCluster,
    _physical_font_size: f32,
    _metrics: FontFaceMetrics,
    _constraint_cells: u16,
    _width: u32,
    _height: u32,
) -> Option<Vec<u8>> {
    None
}

#[cfg(target_os = "macos")]
pub(super) fn rasterize_color_cluster(
    face: &ResolvedFontFace,
    cluster: &ShapedCluster,
    physical_font_size: f32,
    _metrics: FontFaceMetrics,
    _constraint_cells: u16,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    let ch = cluster
        .text
        .chars()
        .find(|ch| !is_combining_mark(*ch) && !is_variation_selector(*ch))?;
    let mut families = Vec::with_capacity(face.fallback_families.len() + 4);
    families.push(face.family.clone());
    families.extend(face.fallback_families.iter().cloned());

    if let Some(names) = fallback_names(&face.family, ch, physical_font_size) {
        families.push(names.postscript);
        families.push(names.family);
    }
    families.push("Apple Color Emoji".to_owned());

    for family in &families {
        if let Some(rgba) =
            rasterize_color_with_family(family, cluster, physical_font_size, width, height)
        {
            return Some(rgba);
        }
    }

    None
}

#[cfg(not(target_os = "macos"))]
pub(super) fn rasterize_color_cluster(
    _face: &ResolvedFontFace,
    _cluster: &ShapedCluster,
    _physical_font_size: f32,
    _metrics: FontFaceMetrics,
    _constraint_cells: u16,
    _width: u32,
    _height: u32,
) -> Option<Vec<u8>> {
    None
}

#[cfg(target_os = "macos")]
pub(super) fn rasterize_color_with_family(
    family: &str,
    cluster: &ShapedCluster,
    physical_font_size: f32,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    let family = CString::new(family).ok()?;
    let text = cluster
        .text
        .chars()
        .filter(|ch| !is_variation_selector(*ch))
        .collect::<String>();
    let utf16 = text.encode_utf16().collect::<Vec<_>>();
    if utf16.is_empty() || width == 0 || height == 0 {
        return None;
    }

    unsafe {
        let family_ref =
            CFStringCreateWithCString(std::ptr::null(), family.as_ptr(), K_CF_STRING_ENCODING_UTF8);
        if family_ref.is_null() {
            return None;
        }
        let font = CTFontCreateWithName(
            family_ref,
            f64::from(physical_font_size.max(1.0)),
            std::ptr::null(),
        );
        CFRelease(family_ref);
        if font.is_null() {
            return None;
        }

        let mut glyphs = vec![0_u16; utf16.len()];
        let supports = CTFontGetGlyphsForCharacters(
            font,
            utf16.as_ptr(),
            glyphs.as_mut_ptr(),
            glyphs.len() as isize,
        );
        let Some(glyph) = glyphs.into_iter().find(|glyph| *glyph != 0) else {
            CFRelease(font);
            return None;
        };
        if !supports {
            CFRelease(font);
            return None;
        }

        let mut rect = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize {
                width: 0.0,
                height: 0.0,
            },
        };
        CTFontGetBoundingRectsForGlyphs(font, 0, &glyph, &mut rect, 1);
        if rect.size.width <= 0.0 || rect.size.height <= 0.0 {
            CFRelease(font);
            return None;
        }

        let scratch_width = (rect.size.width.ceil() as u32).saturating_add(2).max(1);
        let scratch_height = (rect.size.height.ceil() as u32).saturating_add(2).max(1);
        if scratch_width > width.saturating_mul(2) || scratch_height > height.saturating_mul(2) {
            CFRelease(font);
            return None;
        }

        let mut scratch = vec![0_u8; (scratch_width * scratch_height * 4) as usize];
        let color_space = CGColorSpaceCreateDeviceRGB();
        if color_space.is_null() {
            CFRelease(font);
            return None;
        }
        let context = CGBitmapContextCreate(
            scratch.as_mut_ptr().cast(),
            scratch_width as usize,
            scratch_height as usize,
            8,
            (scratch_width * 4) as usize,
            color_space,
            K_CG_IMAGE_ALPHA_PREMULTIPLIED_LAST,
        );
        CGColorSpaceRelease(color_space);
        if context.is_null() {
            CFRelease(font);
            return None;
        }

        CGContextSetAllowsFontSmoothing(context, true);
        CGContextSetShouldSmoothFonts(context, true);
        CGContextSetAllowsFontSubpixelPositioning(context, true);
        CGContextSetShouldSubpixelPositionFonts(context, true);
        CGContextSetAllowsFontSubpixelQuantization(context, false);
        CGContextSetShouldSubpixelQuantizeFonts(context, false);
        CGContextSetAllowsAntialiasing(context, true);
        CGContextSetShouldAntialias(context, true);
        CTFontDrawGlyphs(
            font,
            &glyph,
            &CGPoint {
                x: 1.0 - rect.origin.x,
                y: 1.0 - rect.origin.y,
            },
            1,
            context,
        );
        CGContextRelease(context);
        CFRelease(font);

        unpremultiply_rgba(&mut scratch);
        let mut rgba = vec![0_u8; (width * height * 4) as usize];
        let dst_origin_x = width.saturating_sub(scratch_width) / 2;
        let dst_origin_y = height.saturating_sub(scratch_height) / 2;
        for src_y in 0..scratch_height.min(height) {
            for src_x in 0..scratch_width.min(width) {
                let src = ((src_y * scratch_width + src_x) * 4) as usize;
                let dst = (((dst_origin_y + src_y) * width + dst_origin_x + src_x) * 4) as usize;
                rgba[dst..dst + 4].copy_from_slice(&scratch[src..src + 4]);
            }
        }

        rgba.chunks_exact(4)
            .any(|pixel| pixel[3] > 0)
            .then_some(rgba)
    }
}

#[cfg(target_os = "macos")]
pub(super) fn rasterize_symbol_with_family(
    family: &str,
    ch: char,
    physical_font_size: f32,
    metrics: FontFaceMetrics,
    constraint_cells: u16,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    let family = CString::new(family).ok()?;
    let mut utf16 = [0_u16; 2];
    let encoded = ch.encode_utf16(&mut utf16);
    if encoded.len() != 1 {
        return None;
    }

    unsafe {
        let family_ref =
            CFStringCreateWithCString(std::ptr::null(), family.as_ptr(), K_CF_STRING_ENCODING_UTF8);
        if family_ref.is_null() {
            return None;
        }
        let font = CTFontCreateWithName(
            family_ref,
            f64::from(physical_font_size.max(1.0)),
            std::ptr::null(),
        );
        CFRelease(family_ref);
        if font.is_null() {
            return None;
        }

        let mut glyph = 0_u16;
        let supports = CTFontGetGlyphsForCharacters(font, utf16.as_ptr(), &mut glyph, 1);
        if !supports || glyph == 0 {
            CFRelease(font);
            return None;
        }

        let mut rect = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize {
                width: 0.0,
                height: 0.0,
            },
        };
        CTFontGetBoundingRectsForGlyphs(font, 0, &glyph, &mut rect, 1);
        if rect.size.width <= 0.0 || rect.size.height <= 0.0 {
            CFRelease(font);
            return None;
        }

        let constraint = terminal_glyph_constraint(ch as u32);
        let constrained = constraint.constrain(
            GlyphSize {
                width: rect.size.width,
                height: rect.size.height,
                x: rect.origin.x,
                y: rect.origin.y + f64::from(metrics.cell_baseline),
            },
            metrics,
            constraint_cells.min(u16::from(u8::MAX)) as u8,
        );

        let mut x = constrained.x;
        let y = constrained.y;
        let constrained_width = constrained.width;
        let constrained_height = constrained.height;

        if constraint.size != GlyphConstraintSize::Stretch {
            let dx = (f64::from(metrics.cell_width) - metrics.face_width) / 2.0;
            x += dx;
            if dx < 0.0 {
                x -= dx.trunc();
            }
        }

        let px_x = x.floor() as i32;
        let px_y = y.floor() as i32;
        let frac_x = x - x.floor();
        let frac_y = y - y.floor();
        let px_width = (constrained_width + frac_x).ceil().max(1.0) as u32;
        let px_height = (constrained_height + frac_y).ceil().max(1.0) as u32;
        if px_width > width.saturating_mul(2) || px_height > height.saturating_mul(2) {
            CFRelease(font);
            return None;
        }

        let mut glyph_mask = vec![0_u8; (px_width * px_height) as usize];
        let color_space = CGColorSpaceCreateDeviceGray();
        if color_space.is_null() {
            CFRelease(font);
            return None;
        }
        let context = CGBitmapContextCreate(
            glyph_mask.as_mut_ptr().cast(),
            px_width as usize,
            px_height as usize,
            8,
            px_width as usize,
            color_space,
            0,
        );
        CGColorSpaceRelease(color_space);
        if context.is_null() {
            CFRelease(font);
            return None;
        }

        CGContextSetGrayFillColor(context, 0.0, 1.0);
        CGContextFillRect(
            context,
            CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: f64::from(px_width),
                    height: f64::from(px_height),
                },
            },
        );
        CGContextSetAllowsFontSmoothing(context, true);
        CGContextSetShouldSmoothFonts(context, false);
        CGContextSetAllowsFontSubpixelPositioning(context, true);
        CGContextSetShouldSubpixelPositionFonts(context, true);
        CGContextSetAllowsFontSubpixelQuantization(context, false);
        CGContextSetShouldSubpixelQuantizeFonts(context, false);
        CGContextSetAllowsAntialiasing(context, true);
        CGContextSetShouldAntialias(context, true);
        CGContextSetGrayFillColor(context, 1.0, 1.0);
        CGContextTranslateCTM(context, frac_x, frac_y);
        CGContextScaleCTM(
            context,
            constrained_width / rect.size.width,
            constrained_height / rect.size.height,
        );
        CTFontDrawGlyphs(
            font,
            &glyph,
            &CGPoint {
                x: -rect.origin.x,
                y: -rect.origin.y,
            },
            1,
            context,
        );
        CGContextRelease(context);
        CFRelease(font);

        let top_y = i32::try_from(height).ok()? - (px_y + i32::try_from(px_height).ok()?);
        let mut alpha = vec![0_u8; (width * height) as usize];
        for src_y in 0..px_height {
            for src_x in 0..px_width {
                let dst_x = px_x + i32::try_from(src_x).ok()?;
                let dst_y = top_y + i32::try_from(src_y).ok()?;
                if dst_x < 0
                    || dst_y < 0
                    || dst_x >= i32::try_from(width).ok()?
                    || dst_y >= i32::try_from(height).ok()?
                {
                    continue;
                }
                let src = glyph_mask[(src_y * px_width + src_x) as usize];
                alpha[(u32::try_from(dst_y).ok()? * width + u32::try_from(dst_x).ok()?) as usize] =
                    src;
            }
        }

        alpha.iter().any(|value| *value > 0).then_some(alpha)
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CoreTextFallbackNames {
    pub(super) family: String,
    pub(super) postscript: String,
}

#[cfg(target_os = "macos")]
pub(super) fn fallback_names(
    base_family: &str,
    ch: char,
    physical_font_size: f32,
) -> Option<CoreTextFallbackNames> {
    unsafe fn cfstring_to_string(value: CFStringRef) -> Option<String> {
        if value.is_null() {
            return None;
        }
        let ptr = unsafe { CFStringGetCStringPtr(value, K_CF_STRING_ENCODING_UTF8) };
        if !ptr.is_null() {
            return unsafe { CStr::from_ptr(ptr) }
                .to_str()
                .ok()
                .map(str::to_owned);
        }

        let mut buffer = vec![0 as c_char; 1024];
        if unsafe {
            CFStringGetCString(
                value,
                buffer.as_mut_ptr(),
                buffer.len() as isize,
                K_CF_STRING_ENCODING_UTF8,
            )
        } == 0
        {
            return None;
        }
        unsafe { CStr::from_ptr(buffer.as_ptr()) }
            .to_str()
            .ok()
            .map(str::to_owned)
    }

    let base_family = CString::new(base_family).ok()?;
    let ch_string = CString::new(ch.to_string()).ok()?;
    unsafe {
        let base_family_ref = CFStringCreateWithCString(
            std::ptr::null(),
            base_family.as_ptr(),
            K_CF_STRING_ENCODING_UTF8,
        );
        if base_family_ref.is_null() {
            return None;
        }
        let base_font = CTFontCreateWithName(
            base_family_ref,
            f64::from(physical_font_size.max(1.0)),
            std::ptr::null(),
        );
        CFRelease(base_family_ref);
        if base_font.is_null() {
            return None;
        }

        let string_ref = CFStringCreateWithCString(
            std::ptr::null(),
            ch_string.as_ptr(),
            K_CF_STRING_ENCODING_UTF8,
        );
        if string_ref.is_null() {
            CFRelease(base_font);
            return None;
        }

        let fallback_font = CTFontCreateForString(
            base_font,
            string_ref,
            CFRange {
                location: 0,
                length: ch.len_utf16() as isize,
            },
        );
        CFRelease(string_ref);
        CFRelease(base_font);
        if fallback_font.is_null() {
            return None;
        }

        let family_ref = CTFontCopyFamilyName(fallback_font);
        let postscript_ref = CTFontCopyPostScriptName(fallback_font);
        let family = cfstring_to_string(family_ref);
        let postscript = cfstring_to_string(postscript_ref);
        if !family_ref.is_null() {
            CFRelease(family_ref);
        }
        if !postscript_ref.is_null() {
            CFRelease(postscript_ref);
        }
        CFRelease(fallback_font);

        let family = family?;
        let postscript = postscript?;
        if postscript == "LastResort" {
            return None;
        }
        Some(CoreTextFallbackNames { family, postscript })
    }
}
