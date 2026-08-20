use super::atlas::GlyphAtlasFormat;
use super::atlas::alpha_to_atlas_pixels;
#[cfg(windows)]
use super::clusters::windows_gdi_candidate;
use super::clusters::{
    ShapedCluster, ShapedGlyph, is_color_emoji_cluster, is_combining_mark, is_private_use,
    is_variation_selector,
};
use super::coretext;
use super::font_library::{FontLibrary, font_face_metrics};
#[cfg(windows)]
use super::windows_gdi;
use crate::terminal_font_face::{FontFaceMetrics, GlyphSize, terminal_glyph_constraint};
use crate::terminal_text::{FontStyle, ResolvedFontFace};
use ab_glyph::{Font, FontArc, GlyphId, PxScale, ScaleFont, point};

pub(super) struct RasterizeClusterRequest<'a> {
    pub(super) face: &'a ResolvedFontFace,
    pub(super) cluster: &'a ShapedCluster,
    pub(super) font_size: f32,
    pub(super) pixels_per_point: f32,
    pub(super) constraint_cells: u16,
    pub(super) tile: (u32, u32),
    pub(super) format: GlyphAtlasFormat,
}

pub(super) struct RasterizedCluster {
    pub(super) pixels: Vec<u8>,
    pub(super) color: bool,
}

pub(super) struct PositionedClusterGlyphRequest {
    ch: char,
    glyph_id: GlyphId,
    scale: PxScale,
    position: ab_glyph::Point,
    metrics: FontFaceMetrics,
    constraint_cells: u16,
    tile: (u32, u32),
}

pub(super) fn rasterize_cluster(
    fonts: &mut FontLibrary,
    request: RasterizeClusterRequest<'_>,
) -> RasterizedCluster {
    let RasterizeClusterRequest {
        face,
        cluster,
        font_size,
        pixels_per_point,
        constraint_cells,
        tile: (width, height),
        format,
    } = request;
    if cluster.is_whitespace {
        return RasterizedCluster {
            pixels: vec![0; (width * height * format.depth()) as usize],
            color: false,
        };
    }
    #[cfg(windows)]
    if windows_gdi_candidate(cluster)
        && let Some(family) =
            fonts.font_family_name_for_cluster(face, cluster, font_size * pixels_per_point)
        && let Some(alpha) = windows_gdi::rasterize_text_cluster(
            &family,
            face.style,
            cluster,
            font_size * pixels_per_point,
            width,
            height,
        )
    {
        return RasterizedCluster {
            pixels: alpha_to_atlas_pixels(format, alpha),
            color: false,
        };
    }
    // An explicit emoji-presentation cluster (VS16, or a default-emoji codepoint) renders as a
    // color emoji, even when the primary font carries a monochrome glyph for the base symbol —
    // otherwise ⚠️/❤️ draw as a theme-tinted text glyph, and the rendering flips with whatever
    // the shaper happened to produce. Skip the by-glyph path so it reaches the color path below.
    let prefer_color_emoji = format == GlyphAtlasFormat::Rgba && is_color_emoji_cluster(cluster);
    if !cluster.glyphs.is_empty()
        && !prefer_color_emoji
        && let Some(font) = fonts.font_for_face(face)
        && let Some(alpha) = rasterize_glyph_cluster(RasterizeGlyphClusterRequest {
            font: &font,
            glyphs: &cluster.glyphs,
            physical_font_size: font_size * pixels_per_point,
            pixels_per_point,
            style: face.style,
            constraint_cells,
            tile: (width, height),
        })
    {
        return RasterizedCluster {
            pixels: alpha_to_atlas_pixels(format, alpha),
            color: false,
        };
    }
    let scale = PxScale::from((font_size * pixels_per_point).max(1.0));
    let primary_metrics = fonts.font_for_face(face).map(|font| {
        fonts.font_face_metrics_for(face, &font, scale, constraint_cells, width, height)
    });
    if let Some(metrics) = primary_metrics {
        if format == GlyphAtlasFormat::Rgba
            && is_color_emoji_cluster(cluster)
            && let Some(pixels) = coretext::rasterize_color_cluster(
                face,
                cluster,
                font_size * pixels_per_point,
                metrics,
                constraint_cells,
                width,
                height,
            )
        {
            return RasterizedCluster {
                pixels,
                color: true,
            };
        }
        let cluster_uses_private_codepoint = cluster.text.chars().any(is_private_use);
        if !cluster_uses_private_codepoint
            && let Some(alpha) = coretext::rasterize_symbol_cluster(
                face,
                cluster,
                font_size * pixels_per_point,
                metrics,
                constraint_cells,
                width,
                height,
            )
        {
            return RasterizedCluster {
                pixels: alpha_to_atlas_pixels(format, alpha),
                color: false,
            };
        }
    }
    let Some(font) = fonts.font_for_cluster(face, cluster, font_size * pixels_per_point) else {
        return RasterizedCluster {
            pixels: alpha_to_atlas_pixels(format, fallback_cluster_mask(cluster, width, height)),
            color: false,
        };
    };
    let scaled = font.as_scaled(scale);
    let metrics = primary_metrics
        .unwrap_or_else(|| font_face_metrics(&font, scale, constraint_cells, width, height));
    let baseline = ((height as f32 - scaled.height()) * 0.5).max(0.0) + scaled.ascent();
    let mut pen_x = 0.0_f32;
    let mut alpha = vec![0; (width * height) as usize];

    for ch in cluster.text.chars() {
        if is_combining_mark(ch) || is_variation_selector(ch) {
            continue;
        }
        let glyph_id = scaled.glyph_id(ch);
        if glyph_id.0 == 0 {
            continue;
        }
        let glyph = positioned_cluster_glyph(
            &font,
            PositionedClusterGlyphRequest {
                ch,
                glyph_id,
                scale,
                position: point(pen_x, baseline),
                metrics,
                constraint_cells,
                tile: (width, height),
            },
        );
        let glyph_scaled = font.as_scaled(glyph.scale);
        draw_outline_glyph(&mut alpha, &glyph_scaled, glyph.clone(), width, height);
        if matches!(face.style, FontStyle::Bold | FontStyle::BoldItalic) {
            let glyph = glyph_id.with_scale_and_position(
                scale,
                point(
                    glyph.position.x + (pixels_per_point * 0.45).max(1.0),
                    glyph.position.y,
                ),
            );
            draw_outline_glyph(&mut alpha, &glyph_scaled, glyph, width, height);
        }
        pen_x += scaled.h_advance(glyph_id);
    }

    if alpha.iter().any(|value| *value > 0) {
        RasterizedCluster {
            pixels: alpha_to_atlas_pixels(format, alpha),
            color: false,
        }
    } else {
        RasterizedCluster {
            pixels: alpha_to_atlas_pixels(format, fallback_cluster_mask(cluster, width, height)),
            color: false,
        }
    }
}

fn draw_outline_glyph<F: Font>(
    alpha: &mut [u8],
    font: &impl ScaleFont<F>,
    glyph: ab_glyph::Glyph,
    width: u32,
    height: u32,
) {
    if let Some(outlined) = font.outline_glyph(glyph) {
        let bounds = outlined.px_bounds();
        outlined.draw(|x, y, coverage| {
            let px = bounds.min.x + x as f32;
            let py = bounds.min.y + y as f32;
            if px < 0.0 || py < 0.0 || px >= width as f32 || py >= height as f32 {
                return;
            }
            let index = py as u32 * width + px as u32;
            if let Some(dst) = alpha.get_mut(index as usize) {
                *dst = (*dst).max((coverage * 255.0).round() as u8);
            }
        });
    }
}

struct RasterizeGlyphClusterRequest<'a> {
    font: &'a FontArc,
    glyphs: &'a [ShapedGlyph],
    physical_font_size: f32,
    pixels_per_point: f32,
    style: FontStyle,
    constraint_cells: u16,
    tile: (u32, u32),
}

/// Draws a shaped cluster from its glyph ids into an alpha mask. Glyph offsets
/// arrive in logical pixels (shaped at the logical font size) and scale up by
/// `pixels_per_point` to device pixels. Bold faces get the same synthetic-weight
/// double strike as the per-character path.
fn rasterize_glyph_cluster(request: RasterizeGlyphClusterRequest<'_>) -> Option<Vec<u8>> {
    let RasterizeGlyphClusterRequest {
        font,
        glyphs,
        physical_font_size,
        pixels_per_point,
        style,
        constraint_cells,
        tile: (width, height),
    } = request;
    let scale = PxScale::from(physical_font_size.max(1.0));
    let scaled = font.as_scaled(scale);
    let baseline = ((height as f32 - scaled.height()) * 0.5).max(0.0) + scaled.ascent();
    let synthesize_bold = matches!(style, FontStyle::Bold | FontStyle::BoldItalic);
    let bold_offset = (pixels_per_point * 0.45).max(1.0);

    let cluster_start = glyphs.iter().map(|glyph| glyph.cluster).min().unwrap_or(0);
    let cell_width = width as f32 / f32::from(constraint_cells.max(1));
    let mut alpha = vec![0_u8; (width * height) as usize];

    // A single substituted glyph (e.g. a font's GSUB stylistic-alternate circle) can be drawn
    // wider than its cell. Fit it to the tile and center it so it doesn't hard-clip; this only
    // diverts when the ink actually overflows, leaving normal glyphs and multi-glyph ligatures on
    // the shaped-position path below.
    if let [only] = glyphs
        && let Some(outlined) = scaled
            .outline_glyph(GlyphId(only.glyph_id).with_scale_and_position(scale, point(0.0, 0.0)))
    {
        let bounds = outlined.px_bounds();
        let fit = (width as f32 / bounds.width())
            .min(height as f32 / bounds.height())
            .min(1.0);
        if fit < 1.0 {
            let fitted = PxScale {
                x: scale.x * fit,
                y: scale.y * fit,
            };
            let fitted_scaled = font.as_scaled(fitted);
            let glyph_id = GlyphId(only.glyph_id);
            if let Some(outlined) = fitted_scaled
                .outline_glyph(glyph_id.with_scale_and_position(fitted, point(0.0, 0.0)))
            {
                let bounds = outlined.px_bounds();
                let dx = ((width as f32 - bounds.width()) * 0.5) - bounds.min.x;
                let dy = ((height as f32 - bounds.height()) * 0.5) - bounds.min.y;
                draw_outline_glyph(
                    &mut alpha,
                    &fitted_scaled,
                    glyph_id.with_scale_and_position(fitted, point(dx, dy)),
                    width,
                    height,
                );
                if synthesize_bold {
                    draw_outline_glyph(
                        &mut alpha,
                        &fitted_scaled,
                        glyph_id.with_scale_and_position(fitted, point(dx + bold_offset, dy)),
                        width,
                        height,
                    );
                }
                return alpha.iter().any(|value| *value > 0).then_some(alpha);
            }
        }
    }

    for glyph in glyphs {
        let glyph_id = GlyphId(glyph.glyph_id);
        let cell_offset = glyph.cluster.saturating_sub(cluster_start) as f32 * cell_width;
        let x = cell_offset + glyph.x_offset * pixels_per_point;
        let y = baseline - glyph.y_offset * pixels_per_point;
        draw_outline_glyph(
            &mut alpha,
            &scaled,
            glyph_id.with_scale_and_position(scale, point(x, y)),
            width,
            height,
        );
        if synthesize_bold {
            draw_outline_glyph(
                &mut alpha,
                &scaled,
                glyph_id.with_scale_and_position(scale, point(x + bold_offset, y)),
                width,
                height,
            );
        }
    }
    alpha.iter().any(|value| *value > 0).then_some(alpha)
}

fn positioned_cluster_glyph(
    font: &FontArc,
    request: PositionedClusterGlyphRequest,
) -> ab_glyph::Glyph {
    let PositionedClusterGlyphRequest {
        ch,
        glyph_id,
        scale,
        position,
        metrics,
        constraint_cells,
        tile,
    } = request;
    let glyph = glyph_id.with_scale_and_position(scale, position);
    let scaled = font.as_scaled(scale);
    let Some(outlined) = scaled.outline_glyph(glyph.clone()) else {
        return glyph;
    };
    let bounds = outlined.px_bounds();
    let tile_width = tile.0 as f32;
    let tile_height = tile.1 as f32;

    let constraint = terminal_glyph_constraint(ch as u32);
    if constraint.does_anything() {
        let constrained = constraint.constrain(
            GlyphSize {
                width: f64::from(bounds.width()),
                height: f64::from(bounds.height()),
                x: f64::from(bounds.min.x),
                y: f64::from(bounds.min.y),
            },
            metrics,
            constraint_cells.min(u16::from(u8::MAX)) as u8,
        );
        let scale_factor = (constrained.width as f32 / bounds.width()).max(0.01);
        let scale = PxScale {
            x: scale.x * scale_factor,
            y: scale.y * scale_factor,
        };
        let scaled = font.as_scaled(scale);
        let glyph = glyph_id.with_scale_and_position(scale, point(0.0, 0.0));
        let Some(outlined) = scaled.outline_glyph(glyph.clone()) else {
            return glyph;
        };
        let bounds = outlined.px_bounds();
        return glyph_id.with_scale_and_position(
            scale,
            point(
                constrained.x as f32 - bounds.min.x,
                constrained.y as f32 - bounds.min.y,
            ),
        );
    }

    // No symbol constraint: keep the glyph at its natural size and baseline, EXCEPT when its ink
    // overflows the cell. Oversized symbols a font draws wider than one cell (circles, bullets,
    // shapes outside the symbol-fit ranges) would otherwise be hard-clipped by draw_outline_glyph.
    // Private-use icons always fit-and-center. The fit caps at 1.0, so well-behaved text glyphs
    // (ink within the cell) are returned untouched.
    let fit = (tile_width / bounds.width())
        .min(tile_height / bounds.height())
        .min(1.0);
    if fit >= 1.0 && !is_private_use(ch) {
        return glyph;
    }
    let scale = PxScale {
        x: scale.x * fit,
        y: scale.y * fit,
    };
    let scaled = font.as_scaled(scale);
    let baseline = ((tile_height - scaled.height()) * 0.5).max(0.0) + scaled.ascent();
    let glyph = glyph_id.with_scale_and_position(scale, point(position.x, baseline));
    let Some(outlined) = scaled.outline_glyph(glyph.clone()) else {
        return glyph;
    };
    let bounds = outlined.px_bounds();
    let dx = ((tile_width - bounds.width()) * 0.5) - bounds.min.x;
    let dy = ((tile_height - bounds.height()) * 0.5) - bounds.min.y;
    glyph_id.with_scale_and_position(scale, point(position.x + dx, baseline + dy))
}

fn fallback_cluster_mask(cluster: &ShapedCluster, width: u32, height: u32) -> Vec<u8> {
    let mut alpha = vec![0; (width * height) as usize];
    if cluster.is_whitespace {
        return alpha;
    }
    if let Some(ch) = cluster.text.chars().next()
        && draw_fallback_arrow(&mut alpha, ch, width, height)
    {
        return alpha;
    }
    let seed = cluster.text.chars().next().unwrap_or(' ') as u32;
    let margin_x = (width / 6).min(width.saturating_sub(1));
    let margin_y = (height / 6).min(height.saturating_sub(1));
    for y in margin_y..height.saturating_sub(margin_y) {
        for x in margin_x..width.saturating_sub(margin_x) {
            let pattern = (x + y + seed).is_multiple_of(3);
            if pattern || cluster.text != " " {
                alpha[(y * width + x) as usize] = 220;
            }
        }
    }
    alpha
}

fn draw_fallback_arrow(alpha: &mut [u8], ch: char, width: u32, height: u32) -> bool {
    let direction = match ch {
        '\u{21e1}' | '\u{2191}' | '\u{21e7}' => ArrowDirection::Up,
        '\u{21e3}' | '\u{2193}' | '\u{21e9}' => ArrowDirection::Down,
        _ => return false,
    };
    let stroke = (width / 6).max(1);
    let center_x = width / 2;
    let top = height / 4;
    let bottom = height - height / 4;

    match direction {
        ArrowDirection::Up => {
            fill_pixel_rect(
                alpha,
                width,
                center_x.saturating_sub(stroke / 2),
                top + height / 8,
                stroke,
                bottom.saturating_sub(top + height / 8),
            );
            for offset in 0..=(width / 4).max(1) {
                fill_pixel_rect(
                    alpha,
                    width,
                    center_x.saturating_sub(offset),
                    top + offset,
                    stroke,
                    stroke,
                );
                fill_pixel_rect(
                    alpha,
                    width,
                    center_x + offset,
                    top + offset,
                    stroke,
                    stroke,
                );
            }
        }
        ArrowDirection::Down => {
            fill_pixel_rect(
                alpha,
                width,
                center_x.saturating_sub(stroke / 2),
                top,
                stroke,
                bottom.saturating_sub(top + height / 8),
            );
            for offset in 0..=(width / 4).max(1) {
                fill_pixel_rect(
                    alpha,
                    width,
                    center_x.saturating_sub(offset),
                    bottom.saturating_sub(offset),
                    stroke,
                    stroke,
                );
                fill_pixel_rect(
                    alpha,
                    width,
                    center_x + offset,
                    bottom.saturating_sub(offset),
                    stroke,
                    stroke,
                );
            }
        }
    }
    true
}

#[derive(Clone, Copy)]
enum ArrowDirection {
    Up,
    Down,
}

fn fill_pixel_rect(alpha: &mut [u8], width: u32, x: u32, y: u32, rect_width: u32, height: u32) {
    let total_height = alpha.len() as u32 / width.max(1);
    for py in y..(y + height).min(total_height) {
        for px in x..(x + rect_width).min(width) {
            alpha[(py * width + px) as usize] = 220;
        }
    }
}
