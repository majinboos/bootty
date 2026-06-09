use super::TerminalTextDraw;
use crate::{geometry::SurfaceRect, paint_plan::PlanColor};
use ab_glyph::{Font, FontArc, PxScale, ScaleFont, point};

pub(super) fn text_glyph_draws(
    ch: char,
    rect: SurfaceRect,
    color: PlanColor,
    font_size: f32,
    pixels_per_point: f32,
    font: Option<&FontArc>,
) -> Vec<TerminalTextDraw> {
    if let Some(font) = font {
        let draws = font_glyph_draws(font, ch, rect, color, font_size, pixels_per_point);
        if !draws.is_empty() {
            return draws;
        }
    }

    bitmap_glyph_draws(ch, rect, color, pixels_per_point)
}

fn font_glyph_draws(
    font: &FontArc,
    ch: char,
    rect: SurfaceRect,
    color: PlanColor,
    font_size: f32,
    pixels_per_point: f32,
) -> Vec<TerminalTextDraw> {
    let physical_rect = SurfaceRect {
        min_x: rect.min_x * pixels_per_point,
        min_y: rect.min_y * pixels_per_point,
        max_x: rect.max_x * pixels_per_point,
        max_y: rect.max_y * pixels_per_point,
    };
    let scale = PxScale::from((font_size * pixels_per_point).max(1.0));
    let scaled = font.as_scaled(scale);
    let glyph_id = scaled.glyph_id(ch);
    let advance = scaled.h_advance(glyph_id);
    let baseline =
        physical_rect.min_y + ((physical_rect.height() - scaled.height()) * 0.5) + scaled.ascent();
    let left = physical_rect.min_x + ((physical_rect.width() - advance) * 0.5).max(0.0);
    let glyph = glyph_id.with_scale_and_position(scale, point(left, baseline));
    let Some(outlined) = scaled.outline_glyph(glyph) else {
        return Vec::new();
    };
    let bounds = outlined.px_bounds();
    let mut draws = Vec::new();

    outlined.draw(|x, y, coverage| {
        if coverage <= 0.15 {
            return;
        }
        let physical_x = bounds.min.x + x as f32;
        let physical_y = bounds.min.y + y as f32;
        if physical_x < physical_rect.min_x
            || physical_x >= physical_rect.max_x
            || physical_y < physical_rect.min_y
            || physical_y >= physical_rect.max_y
        {
            return;
        }
        draws.push(TerminalTextDraw {
            ch,
            rect: SurfaceRect::from_min_size(
                physical_x / pixels_per_point,
                physical_y / pixels_per_point,
                1.0 / pixels_per_point,
                1.0 / pixels_per_point,
            ),
            color: PlanColor {
                a: (f32::from(color.a) * coverage).round() as u8,
                ..color
            },
        });
    });

    draws
}

fn bitmap_glyph_draws(
    ch: char,
    rect: SurfaceRect,
    color: PlanColor,
    pixels_per_point: f32,
) -> Vec<TerminalTextDraw> {
    let pattern = ascii_glyph_pattern(ch);
    let margin_x = rect.width() * 0.10;
    let margin_y = rect.height() * 0.14;
    let pixel_w = ((rect.width() - margin_x * 2.0) / 5.0).max(1.0 / pixels_per_point);
    let pixel_h = ((rect.height() - margin_y * 2.0) / 7.0).max(1.0 / pixels_per_point);
    let mut draws = Vec::new();

    for (row, bits) in pattern.iter().enumerate() {
        for (col, pixel) in bits.bytes().enumerate() {
            if pixel != b'#' {
                continue;
            }
            draws.push(TerminalTextDraw {
                ch,
                rect: SurfaceRect::from_min_size(
                    rect.min_x + margin_x + col as f32 * pixel_w,
                    rect.min_y + margin_y + row as f32 * pixel_h,
                    pixel_w,
                    pixel_h,
                ),
                color,
            });
        }
    }

    draws
}

fn ascii_glyph_pattern(ch: char) -> [&'static str; 7] {
    // Last-resort fallback for environments where fontdb cannot discover a
    // monospace system font. The primary path above still rasterizes a real
    // configured terminal font.
    match ch.to_ascii_lowercase() {
        '0' => [
            " ### ", "#   #", "#  ##", "# # #", "##  #", "#   #", " ### ",
        ],
        '1' => [
            "  #  ", " ##  ", "# #  ", "  #  ", "  #  ", "  #  ", "#####",
        ],
        '2' => [
            " ### ", "#   #", "    #", "   # ", "  #  ", " #   ", "#####",
        ],
        '3' => [
            "#### ", "    #", "    #", " ### ", "    #", "    #", "#### ",
        ],
        '4' => [
            "#   #", "#   #", "#   #", "#####", "    #", "    #", "    #",
        ],
        '5' => [
            "#####", "#    ", "#    ", "#### ", "    #", "    #", "#### ",
        ],
        '6' => [
            " ### ", "#    ", "#    ", "#### ", "#   #", "#   #", " ### ",
        ],
        '7' => [
            "#####", "    #", "   # ", "  #  ", " #   ", " #   ", " #   ",
        ],
        '8' => [
            " ### ", "#   #", "#   #", " ### ", "#   #", "#   #", " ### ",
        ],
        '9' => [
            " ### ", "#   #", "#   #", " ####", "    #", "    #", " ### ",
        ],
        'a' => [
            " ### ", "#   #", "#   #", "#####", "#   #", "#   #", "#   #",
        ],
        'b' => [
            "#### ", "#   #", "#   #", "#### ", "#   #", "#   #", "#### ",
        ],
        'c' => [
            " ### ", "#   #", "#    ", "#    ", "#    ", "#   #", " ### ",
        ],
        'd' => [
            "#### ", "#   #", "#   #", "#   #", "#   #", "#   #", "#### ",
        ],
        'e' => [
            "#####", "#    ", "#    ", "#### ", "#    ", "#    ", "#####",
        ],
        'f' => [
            "#####", "#    ", "#    ", "#### ", "#    ", "#    ", "#    ",
        ],
        'g' => [
            " ### ", "#   #", "#    ", "#  ##", "#   #", "#   #", " ### ",
        ],
        'h' => [
            "#   #", "#   #", "#   #", "#####", "#   #", "#   #", "#   #",
        ],
        'i' => [
            "#####", "  #  ", "  #  ", "  #  ", "  #  ", "  #  ", "#####",
        ],
        'j' => [
            "#####", "    #", "    #", "    #", "    #", "#   #", " ### ",
        ],
        'k' => [
            "#   #", "#  # ", "# #  ", "##   ", "# #  ", "#  # ", "#   #",
        ],
        'l' => [
            "#    ", "#    ", "#    ", "#    ", "#    ", "#    ", "#####",
        ],
        'm' => [
            "#   #", "## ##", "# # #", "#   #", "#   #", "#   #", "#   #",
        ],
        'n' => [
            "#   #", "##  #", "# # #", "#  ##", "#   #", "#   #", "#   #",
        ],
        'o' => [
            " ### ", "#   #", "#   #", "#   #", "#   #", "#   #", " ### ",
        ],
        'p' => [
            "#### ", "#   #", "#   #", "#### ", "#    ", "#    ", "#    ",
        ],
        'q' => [
            " ### ", "#   #", "#   #", "#   #", "# # #", "#  # ", " ## #",
        ],
        'r' => [
            "#### ", "#   #", "#   #", "#### ", "# #  ", "#  # ", "#   #",
        ],
        's' => [
            " ####", "#    ", "#    ", " ### ", "    #", "    #", "#### ",
        ],
        't' => [
            "#####", "  #  ", "  #  ", "  #  ", "  #  ", "  #  ", "  #  ",
        ],
        'u' => [
            "#   #", "#   #", "#   #", "#   #", "#   #", "#   #", " ### ",
        ],
        'v' => [
            "#   #", "#   #", "#   #", "#   #", "#   #", " # # ", "  #  ",
        ],
        'w' => [
            "#   #", "#   #", "#   #", "#   #", "# # #", "## ##", "#   #",
        ],
        'x' => [
            "#   #", "#   #", " # # ", "  #  ", " # # ", "#   #", "#   #",
        ],
        'y' => [
            "#   #", "#   #", "#   #", " ####", "    #", "#   #", " ### ",
        ],
        'z' => [
            "#####", "    #", "   # ", "  #  ", " #   ", "#    ", "#####",
        ],
        '.' => [
            "     ", "     ", "     ", "     ", "     ", " ##  ", " ##  ",
        ],
        ',' => [
            "     ", "     ", "     ", "     ", " ##  ", " ##  ", " #   ",
        ],
        ':' => [
            "     ", " ##  ", " ##  ", "     ", " ##  ", " ##  ", "     ",
        ],
        ';' => [
            "     ", " ##  ", " ##  ", "     ", " ##  ", " ##  ", " #   ",
        ],
        '-' => [
            "     ", "     ", "     ", "#####", "     ", "     ", "     ",
        ],
        '_' => [
            "     ", "     ", "     ", "     ", "     ", "     ", "#####",
        ],
        '/' => [
            "    #", "   # ", "   # ", "  #  ", " #   ", " #   ", "#    ",
        ],
        '\\' => [
            "#    ", " #   ", " #   ", "  #  ", "   # ", "   # ", "    #",
        ],
        '|' => [
            "  #  ", "  #  ", "  #  ", "  #  ", "  #  ", "  #  ", "  #  ",
        ],
        '+' => [
            "     ", "  #  ", "  #  ", "#####", "  #  ", "  #  ", "     ",
        ],
        '=' => [
            "     ", "     ", "#####", "     ", "#####", "     ", "     ",
        ],
        '<' => [
            "   # ", "  #  ", " #   ", "#    ", " #   ", "  #  ", "   # ",
        ],
        '>' => [
            " #   ", "  #  ", "   # ", "    #", "   # ", "  #  ", " #   ",
        ],
        '[' => [
            " ### ", " #   ", " #   ", " #   ", " #   ", " #   ", " ### ",
        ],
        ']' => [
            " ### ", "   # ", "   # ", "   # ", "   # ", "   # ", " ### ",
        ],
        '(' => [
            "   # ", "  #  ", " #   ", " #   ", " #   ", "  #  ", "   # ",
        ],
        ')' => [
            " #   ", "  #  ", "   # ", "   # ", "   # ", "  #  ", " #   ",
        ],
        '$' => [
            " ####", "# #  ", "# #  ", " ### ", "  # #", "  # #", "#### ",
        ],
        '#' => [
            " # # ", " # # ", "#####", " # # ", "#####", " # # ", " # # ",
        ],
        '!' => [
            "  #  ", "  #  ", "  #  ", "  #  ", "  #  ", "     ", "  #  ",
        ],
        '?' => [
            " ### ", "#   #", "    #", "   # ", "  #  ", "     ", "  #  ",
        ],
        _ => [
            "#####", "#   #", "#  ##", "# # #", "##  #", "#   #", "#####",
        ],
    }
}
