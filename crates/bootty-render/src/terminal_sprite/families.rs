use crate::geometry::SurfaceRect;
use crate::terminal_sprite::{SpriteCommand, SpriteFamily, SpriteGlyph, SpritePoint, SpritePoints};

mod box_drawing;
mod braille;
mod legacy_computing;
mod legacy_computing_supplement;
mod primitives;

use self::primitives::{
    center_y, fill_block_rect, filled_polygon, flip_horizontal, heavy_line_width, left_bottom,
    left_top, line_width, placeholder_commands, points_from_array, right_bottom,
    right_round_points, right_top, stroke_commands, stroke_polyline, triangle_commands,
};

macro_rules! block_rect_specs {
    ($($ch:literal => ($row:literal, $col:literal, $rows:literal, $cols:literal),)+) => {
        fn block_rect_spec(ch: char) -> Option<(u8, u8, u8, u8)> {
            Some(match ch {
                $($ch => ($row, $col, $rows, $cols),)+
                _ => return None,
            })
        }
    };
}

macro_rules! shade_alphas {
    ($($ch:literal => $alpha:literal,)+) => {
        fn shade_alpha(ch: char) -> Option<f32> {
            Some(match ch {
                $($ch => $alpha,)+
                _ => return None,
            })
        }
    };
}

block_rect_specs! {
    '█' => (0, 0, 8, 8), '▁' => (7, 0, 1, 8), '▂' => (6, 0, 2, 8),
    '▃' => (5, 0, 3, 8), '▄' => (4, 0, 4, 8), '▅' => (3, 0, 5, 8),
    '▆' => (2, 0, 6, 8), '▇' => (1, 0, 7, 8), '▀' => (0, 0, 4, 8),
    '▔' => (0, 0, 1, 8), '▏' => (0, 0, 8, 1), '▎' => (0, 0, 8, 2),
    '▍' => (0, 0, 8, 3), '▌' => (0, 0, 8, 4), '▋' => (0, 0, 8, 5),
    '▊' => (0, 0, 8, 6), '▉' => (0, 0, 8, 7), '▐' => (0, 4, 8, 4),
    '▕' => (0, 7, 8, 1),
}

shade_alphas! {
    '░' => 0.25,
    '▒' => 0.50,
    '▓' => 0.75,
}

pub(super) fn family_for(ch: char) -> Option<SpriteFamily> {
    match ch {
        _ if is_powerline_sprite(ch) => Some(SpriteFamily::Powerline),
        _ if is_separator_sprite(ch) => Some(SpriteFamily::Separator),
        '\u{EE00}'..='\u{EE0B}' => Some(SpriteFamily::ProgressIndicator),
        _ if block_rect_spec(ch).is_some() => Some(SpriteFamily::Block),
        '▖'..='▟' => Some(SpriteFamily::Block),
        _ if shade_alpha(ch).is_some() => Some(SpriteFamily::Shade),
        '─'..='╿' => Some(SpriteFamily::BoxDrawing),
        '\u{2800}'..='\u{28FF}' => Some(SpriteFamily::Braille),
        '\u{1FB00}'..='\u{1FB67}'
        | '\u{1FB68}'..='\u{1FB6F}'
        | '\u{1FB70}'..='\u{1FB99}'
        | '\u{1FB9A}'..='\u{1FB9F}'
        | '\u{1FBA0}'..='\u{1FBAF}'
        | '\u{1FBBD}'..='\u{1FBBF}'
        | '\u{1FBCE}'..='\u{1FBCF}'
        | '\u{1FBD0}'..='\u{1FBDF}'
        | '\u{1FBE0}'..='\u{1FBEF}' => Some(SpriteFamily::LegacyComputing),
        '\u{1CC1B}'..='\u{1CC1E}'
        | '\u{1CC21}'..='\u{1CC2F}'
        | '\u{1CC30}'..='\u{1CC3F}'
        | '\u{1CD00}'..='\u{1CDE5}'
        | '\u{1CE00}'..='\u{1CE01}'
        | '\u{1CE0B}'..='\u{1CE0C}'
        | '\u{1CE16}'..='\u{1CE19}'
        | '\u{1CE51}'..='\u{1CE8F}'
        | '\u{1CE90}'..='\u{1CEAF}' => Some(SpriteFamily::LegacyComputingSupplement),
        _ => None,
    }
}

fn is_powerline_sprite(ch: char) -> bool {
    matches!(
        ch,
        '\u{E0B0}'
            | '\u{E0B1}'
            | '\u{E0B2}'
            | '\u{E0B3}'
            | '\u{E0B4}'
            | '\u{E0B5}'
            | '\u{E0B6}'
            | '\u{E0B7}'
            | '\u{E0B8}'
            | '\u{E0B9}'
            | '\u{E0BA}'
            | '\u{E0BB}'
            | '\u{E0BC}'
            | '\u{E0BD}'
            | '\u{E0BE}'
            | '\u{E0BF}'
            | '\u{E0D2}'
            | '\u{E0D4}'
    )
}

fn is_separator_sprite(ch: char) -> bool {
    matches!(ch, '❯' | '❮' | '' | '')
}

pub(super) fn commands_for(glyph: SpriteGlyph, rect: SurfaceRect) -> Vec<SpriteCommand> {
    match glyph.family {
        SpriteFamily::Powerline => powerline_commands(glyph.ch, rect),
        SpriteFamily::Separator => separator_commands(glyph.ch, rect),
        SpriteFamily::ProgressIndicator => progress_indicator_commands(glyph.ch, rect),
        SpriteFamily::Block => block_commands(glyph.ch, rect),
        SpriteFamily::Shade => shade_commands(glyph.ch, rect),
        SpriteFamily::BoxDrawing => box_drawing::commands_for(glyph.ch, rect),
        SpriteFamily::Braille => braille::commands_for(glyph.ch, rect),
        SpriteFamily::LegacyComputing => legacy_computing::commands_for(glyph.ch, rect),
        SpriteFamily::LegacyComputingSupplement => {
            legacy_computing_supplement::commands_for(glyph.ch, rect)
        }
        SpriteFamily::Special => placeholder_commands(rect),
    }
}

fn separator_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let left = rect.min_x + rect.width() * 0.28;
    let right = rect.min_x + rect.width() * 0.72;
    let top = rect.min_y + rect.height() * 0.18;
    let center = center_y(rect);
    let bottom = rect.min_y + rect.height() * 0.82;
    let width = match ch {
        '❯' | '❮' => heavy_line_width(rect),
        '' | '' => line_width(rect),
        _ => return Vec::new(),
    };
    let points = match ch {
        '❯' | '' => points_from_array([
            SpritePoint::new(left, top),
            SpritePoint::new(right, center),
            SpritePoint::new(left, bottom),
        ]),
        '❮' | '' => points_from_array([
            SpritePoint::new(right, top),
            SpritePoint::new(left, center),
            SpritePoint::new(right, bottom),
        ]),
        _ => SpritePoints::new(),
    };

    vec![SpriteCommand::StrokePolyline {
        points,
        width,
        alpha: 1.0,
    }]
}

fn progress_indicator_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let width = rect.width();
    let height = rect.height();
    let bar = |left: f32, top: f32, bar_width: f32, bar_height: f32| {
        vec![SpriteCommand::FillRect {
            rect: SurfaceRect::from_min_size(
                rect.min_x + width * left,
                rect.min_y + height * top,
                width * bar_width,
                height * bar_height,
            ),
            alpha: 1.0,
        }]
    };

    match ch {
        '\u{EE00}' | '\u{EE03}' => bar(0.13143872, 0.06866538, 0.8681172, 0.8626692),
        '\u{EE01}' | '\u{EE04}' => bar(0.0, 0.06866538, 1.0, 0.8626692),
        '\u{EE02}' | '\u{EE05}' => bar(0.0, 0.06866538, 0.86856127, 0.8626692),
        '\u{EE06}' => bar(0.1470292, 0.77654755, 0.7059416, 0.22345245),
        '\u{EE07}' => bar(0.5, 0.25012583, 0.5, 0.7498742),
        '\u{EE08}' => bar(0.37009063, 0.0, 0.6299094, 0.85354805),
        '\u{EE09}' => bar(0.0, 0.0, 1.0, 0.49974838),
        '\u{EE0A}' => bar(0.0, 0.0, 0.6299094, 0.85354805),
        '\u{EE0B}' => bar(0.0, 0.25012583, 0.5, 0.7498742),
        _ => Vec::new(),
    }
}

fn powerline_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let left_center = SpritePoint::new(rect.min_x, center_y(rect));
    let right_center = SpritePoint::new(rect.max_x, center_y(rect));
    macro_rules! tri {
        ($a:expr, $b:expr, $c:expr) => {
            triangle_commands($a, $b, $c)
        };
    }
    macro_rules! strokes {
        ($(($start:expr, $end:expr)),+ $(,)?) => {
            stroke_commands(&[$(($start, $end)),+], rect)
        };
    }

    match ch {
        '\u{E0B0}' => tri!(left_top(rect), right_center, left_bottom(rect)),
        '\u{E0B1}' => strokes!(
            (left_top(rect), right_center),
            (left_bottom(rect), right_center)
        ),
        '\u{E0B2}' => tri!(right_top(rect), left_center, right_bottom(rect)),
        '\u{E0B3}' => strokes!(
            (right_top(rect), left_center),
            (right_bottom(rect), left_center)
        ),
        '\u{E0B4}' => vec![filled_polygon(right_round_points(rect))],
        '\u{E0B5}' => vec![stroke_polyline(right_round_points(rect), rect)],
        '\u{E0B6}' => vec![filled_polygon(flip_horizontal(
            &right_round_points(rect),
            rect,
        ))],
        '\u{E0B7}' => vec![stroke_polyline(
            flip_horizontal(&right_round_points(rect), rect),
            rect,
        )],
        '\u{E0B8}' => tri!(left_top(rect), left_bottom(rect), right_bottom(rect)),
        '\u{E0B9}' | '\u{E0BF}' => strokes!((left_top(rect), right_bottom(rect))),
        '\u{E0BA}' => tri!(right_top(rect), right_bottom(rect), left_bottom(rect)),
        '\u{E0BB}' | '\u{E0BD}' => strokes!((left_bottom(rect), right_top(rect))),
        '\u{E0BC}' => tri!(left_top(rect), right_top(rect), left_bottom(rect)),
        '\u{E0BE}' => tri!(left_top(rect), right_top(rect), right_bottom(rect)),
        '\u{E0D2}' => powerline_split_commands(rect, false),
        '\u{E0D4}' => powerline_split_commands(rect, true),
        _ => Vec::new(),
    }
}

fn powerline_split_commands(rect: SurfaceRect, mirrored: bool) -> Vec<SpriteCommand> {
    let thickness = line_width(rect);
    let mid_x = rect.min_x + rect.width() * 0.5;
    let upper_mid_y = center_y(rect) - thickness * 0.5;
    let lower_mid_y = center_y(rect) + thickness * 0.5;

    let top = [
        SpritePoint::new(rect.min_x, rect.min_y),
        SpritePoint::new(rect.max_x, rect.min_y),
        SpritePoint::new(mid_x, upper_mid_y),
        SpritePoint::new(rect.min_x, upper_mid_y),
    ];
    let bottom = [
        SpritePoint::new(rect.min_x, rect.max_y),
        SpritePoint::new(rect.max_x, rect.max_y),
        SpritePoint::new(mid_x, lower_mid_y),
        SpritePoint::new(rect.min_x, lower_mid_y),
    ];

    let polygons = if mirrored {
        vec![flip_horizontal(&top, rect), flip_horizontal(&bottom, rect)]
    } else {
        vec![top.to_vec(), bottom.to_vec()]
    };

    polygons.into_iter().map(filled_polygon).collect()
}

fn block_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    if let Some((row, col, rows, cols)) = block_rect_spec(ch) {
        return vec![fill_block_rect(rect, row, col, rows, cols)];
    }

    quadrant_rect_specs(ch)
        .map(|specs| {
            specs
                .iter()
                .map(|(row, col, rows, cols)| fill_block_rect(rect, *row, *col, *rows, *cols))
                .collect()
        })
        .unwrap_or_else(|| placeholder_commands(rect))
}

fn shade_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let Some(alpha) = shade_alpha(ch) else {
        return Vec::new();
    };
    vec![SpriteCommand::FillRect { rect, alpha }]
}

fn quadrant_rect_specs(ch: char) -> Option<&'static [(u8, u8, u8, u8)]> {
    const TL: (u8, u8, u8, u8) = (0, 0, 4, 4);
    const TR: (u8, u8, u8, u8) = (0, 4, 4, 4);
    const BL: (u8, u8, u8, u8) = (4, 0, 4, 4);
    const BR: (u8, u8, u8, u8) = (4, 4, 4, 4);

    Some(match ch {
        '▖' => &[BL],
        '▗' => &[BR],
        '▘' => &[TL],
        '▙' => &[TL, BL, BR],
        '▚' => &[TL, BR],
        '▛' => &[TL, TR, BL],
        '▜' => &[TL, TR, BR],
        '▝' => &[TR],
        '▞' => &[TR, BL],
        '▟' => &[TR, BL, BR],
        _ => return None,
    })
}
