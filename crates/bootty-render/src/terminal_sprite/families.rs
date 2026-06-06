use crate::geometry::SurfaceRect;

use super::{SpriteCommand, SpriteFamily, SpriteGlyph, SpritePoint, SpriteShape};

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
        SpriteFamily::BoxDrawing => box_drawing_commands(glyph.ch, rect),
        SpriteFamily::Braille => braille_commands(glyph.ch, rect),
        SpriteFamily::LegacyComputing => legacy_computing_commands(glyph.ch, rect),
        SpriteFamily::LegacyComputingSupplement => {
            legacy_computing_supplement_commands(glyph.ch, rect)
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
        '❯' | '' => vec![
            SpritePoint::new(left, top),
            SpritePoint::new(right, center),
            SpritePoint::new(left, bottom),
        ],
        '❮' | '' => vec![
            SpritePoint::new(right, top),
            SpritePoint::new(left, center),
            SpritePoint::new(right, bottom),
        ],
        _ => Vec::new(),
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

fn box_drawing_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    if let Some(dashes) = box_dash_spec(ch) {
        return box_dash_commands(dashes, rect);
    }
    if let Some(lines) = box_line_spec(ch) {
        return box_line_commands(lines, rect);
    }
    if let Some(diagonals) = box_diagonal_spec(ch) {
        return box_diagonal_commands(diagonals, rect);
    }
    if let Some(corner) = box_rounded_corner_spec(ch) {
        return vec![box_rounded_corner_command(corner, rect)];
    }

    placeholder_commands(rect)
}

#[derive(Clone, Copy)]
enum BoxDashAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy)]
struct BoxDashes {
    axis: BoxDashAxis,
    count: u8,
    style: BoxLineStyle,
    desired_gap: BoxLineStyle,
    min_gap: f32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BoxLineStyle {
    None,
    Light,
    Heavy,
    Double,
}

#[derive(Clone, Copy)]
struct BoxLines {
    up: BoxLineStyle,
    right: BoxLineStyle,
    down: BoxLineStyle,
    left: BoxLineStyle,
}

#[derive(Clone, Copy)]
struct BoxDiagonals {
    upper_left_to_lower_right: bool,
    upper_right_to_lower_left: bool,
}

#[derive(Clone, Copy)]
enum BoxRoundedCorner {
    UpperLeft,
    UpperRight,
    LowerRight,
    LowerLeft,
}

fn box_dash_spec(ch: char) -> Option<BoxDashes> {
    use BoxDashAxis::{Horizontal as HAxis, Vertical as VAxis};
    use BoxLineStyle::{Heavy as H, Light as L};
    let (axis, count, style, desired_gap, min_gap) = match ch as u32 {
        0x2504 => (HAxis, 3, L, L, 4.0),
        0x2505 => (HAxis, 3, H, L, 4.0),
        0x2506 => (VAxis, 3, L, L, 4.0),
        0x2507 => (VAxis, 3, H, L, 4.0),
        0x2508 => (HAxis, 4, L, L, 4.0),
        0x2509 => (HAxis, 4, H, L, 4.0),
        0x250A => (VAxis, 4, L, L, 4.0),
        0x250B => (VAxis, 4, H, L, 4.0),
        0x254C => (HAxis, 2, L, L, 0.0),
        0x254D => (HAxis, 2, H, L, 0.0),
        0x254E => (VAxis, 2, L, H, 0.0),
        0x254F => (VAxis, 2, H, H, 0.0),
        _ => return None,
    };
    Some(BoxDashes {
        axis,
        count,
        style,
        desired_gap,
        min_gap,
    })
}

fn box_line_spec(ch: char) -> Option<BoxLines> {
    use BoxLineStyle::{Double as D, Heavy as H, Light as L, None as N};
    let lines = match ch as u32 {
        0x2500 => (N, L, N, L),
        0x2501 => (N, H, N, H),
        0x2502 => (L, N, L, N),
        0x2503 => (H, N, H, N),
        0x250C => (N, L, L, N),
        0x250D => (N, H, L, N),
        0x250E => (N, L, H, N),
        0x250F => (N, H, H, N),
        0x2510 => (N, N, L, L),
        0x2511 => (N, N, L, H),
        0x2512 => (N, N, H, L),
        0x2513 => (N, N, H, H),
        0x2514 => (L, L, N, N),
        0x2515 => (L, H, N, N),
        0x2516 => (H, L, N, N),
        0x2517 => (H, H, N, N),
        0x2518 => (L, N, N, L),
        0x2519 => (L, N, N, H),
        0x251A => (H, N, N, L),
        0x251B => (H, N, N, H),
        0x251C => (L, L, L, N),
        0x251D => (L, H, L, N),
        0x251E => (H, L, L, N),
        0x251F => (L, L, H, N),
        0x2520 => (H, L, H, N),
        0x2521 => (H, H, L, N),
        0x2522 => (L, H, H, N),
        0x2523 => (H, H, H, N),
        0x2524 => (L, N, L, L),
        0x2525 => (L, N, L, H),
        0x2526 => (H, N, L, L),
        0x2527 => (L, N, H, L),
        0x2528 => (H, N, H, L),
        0x2529 => (H, N, L, H),
        0x252A => (L, N, H, H),
        0x252B => (H, N, H, H),
        0x252C => (N, L, L, L),
        0x252D => (N, L, L, H),
        0x252E => (N, H, L, L),
        0x252F => (N, H, L, H),
        0x2530 => (N, L, H, L),
        0x2531 => (N, L, H, H),
        0x2532 => (N, H, H, L),
        0x2533 => (N, H, H, H),
        0x2534 => (L, L, N, L),
        0x2535 => (L, L, N, H),
        0x2536 => (L, H, N, L),
        0x2537 => (L, H, N, H),
        0x2538 => (H, L, N, L),
        0x2539 => (H, L, N, H),
        0x253A => (H, H, N, L),
        0x253B => (H, H, N, H),
        0x253C => (L, L, L, L),
        0x253D => (L, L, L, H),
        0x253E => (L, H, L, L),
        0x253F => (L, H, L, H),
        0x2540 => (H, L, L, L),
        0x2541 => (L, L, H, L),
        0x2542 => (H, L, H, L),
        0x2543 => (H, L, L, H),
        0x2544 => (H, H, L, L),
        0x2545 => (L, L, H, H),
        0x2546 => (L, H, H, L),
        0x2547 => (H, H, L, H),
        0x2548 => (L, H, H, H),
        0x2549 => (H, L, H, H),
        0x254A => (H, H, H, L),
        0x254B => (H, H, H, H),
        0x2550 => (N, D, N, D),
        0x2551 => (D, N, D, N),
        0x2552 => (N, D, L, N),
        0x2553 => (N, L, D, N),
        0x2554 => (N, D, D, N),
        0x2555 => (N, N, L, D),
        0x2556 => (N, N, D, L),
        0x2557 => (N, N, D, D),
        0x2558 => (L, D, N, N),
        0x2559 => (D, L, N, N),
        0x255A => (D, D, N, N),
        0x255B => (L, N, N, D),
        0x255C => (D, N, N, L),
        0x255D => (D, N, N, D),
        0x255E => (L, D, L, N),
        0x255F => (D, L, D, N),
        0x2560 => (D, D, D, N),
        0x2561 => (L, N, L, D),
        0x2562 => (D, N, D, L),
        0x2563 => (D, N, D, D),
        0x2564 => (N, D, L, D),
        0x2565 => (N, L, D, L),
        0x2566 => (N, D, D, D),
        0x2567 => (L, D, N, D),
        0x2568 => (D, L, N, L),
        0x2569 => (D, D, N, D),
        0x256A => (L, D, L, D),
        0x256B => (D, L, D, L),
        0x256C => (D, D, D, D),
        0x2574 => (N, N, N, L),
        0x2575 => (L, N, N, N),
        0x2576 => (N, L, N, N),
        0x2577 => (N, N, L, N),
        0x2578 => (N, N, N, H),
        0x2579 => (H, N, N, N),
        0x257A => (N, H, N, N),
        0x257B => (N, N, H, N),
        0x257C => (N, H, N, L),
        0x257D => (L, N, H, N),
        0x257E => (N, L, N, H),
        0x257F => (H, N, L, N),
        _ => return None,
    };
    Some(BoxLines {
        up: lines.0,
        right: lines.1,
        down: lines.2,
        left: lines.3,
    })
}

fn box_diagonal_spec(ch: char) -> Option<BoxDiagonals> {
    Some(match ch as u32 {
        0x2571 => BoxDiagonals {
            upper_left_to_lower_right: false,
            upper_right_to_lower_left: true,
        },
        0x2572 => BoxDiagonals {
            upper_left_to_lower_right: true,
            upper_right_to_lower_left: false,
        },
        0x2573 => BoxDiagonals {
            upper_left_to_lower_right: true,
            upper_right_to_lower_left: true,
        },
        _ => return None,
    })
}

fn box_rounded_corner_spec(ch: char) -> Option<BoxRoundedCorner> {
    Some(match ch as u32 {
        0x256D => BoxRoundedCorner::UpperLeft,
        0x256E => BoxRoundedCorner::UpperRight,
        0x256F => BoxRoundedCorner::LowerRight,
        0x2570 => BoxRoundedCorner::LowerLeft,
        _ => return None,
    })
}

fn box_dash_commands(dashes: BoxDashes, rect: SurfaceRect) -> Vec<SpriteCommand> {
    match dashes.axis {
        BoxDashAxis::Horizontal => box_horizontal_dash_commands(dashes, rect),
        BoxDashAxis::Vertical => box_vertical_dash_commands(dashes, rect),
    }
}

fn box_horizontal_dash_commands(dashes: BoxDashes, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let count = f32::from(dashes.count);
    let gap_width = box_line_width(dashes.desired_gap, rect)
        .max(dashes.min_gap)
        .min((rect.width() / (2.0 * count)).floor());
    let total_gap_width = count * gap_width;
    let total_dash_width = rect.width() - total_gap_width;
    let dash_width = (total_dash_width / count).floor();
    let mut extra = total_dash_width % count;
    let y = rect.min_y + (rect.height() - box_line_width(dashes.style, rect)) * 0.5;
    let mut x = rect.min_x + (gap_width / 2.0).floor();
    let mut commands = Vec::new();

    for _ in 0..dashes.count {
        let mut width = dash_width;
        if extra > 0.0 {
            extra -= 1.0;
            width += 1.0;
        }
        commands.push(fill_rect(x, y, width, box_line_width(dashes.style, rect)));
        x += width + gap_width;
    }
    commands
}

fn box_vertical_dash_commands(dashes: BoxDashes, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let count = f32::from(dashes.count);
    let gap_height = box_line_width(dashes.desired_gap, rect)
        .max(dashes.min_gap)
        .min((rect.height() / (2.0 * count)).floor());
    let total_gap_height = count * gap_height;
    let total_dash_height = rect.height() - total_gap_height;
    let dash_height = (total_dash_height / count).floor();
    let mut extra = total_dash_height % count;
    let x = rect.min_x + (rect.width() - box_line_width(dashes.style, rect)) * 0.5;
    let mut y = rect.min_y;
    let mut commands = Vec::new();

    for _ in 0..dashes.count {
        let mut height = dash_height;
        if extra > 0.0 {
            extra -= 1.0;
            height += 1.0;
        }
        commands.push(fill_rect(x, y, box_line_width(dashes.style, rect), height));
        y += height + gap_height;
    }
    commands
}

fn box_diagonal_commands(diagonals: BoxDiagonals, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let slope_x = rect.width().min(rect.height()) / rect.height();
    let slope_y = rect.width().min(rect.height()) / rect.width();
    let mut commands = Vec::new();

    if diagonals.upper_right_to_lower_left {
        commands.push(SpriteCommand::StrokePolyline {
            points: vec![
                SpritePoint::new(rect.max_x + 0.5 * slope_x, rect.min_y - 0.5 * slope_y),
                SpritePoint::new(rect.min_x - 0.5 * slope_x, rect.max_y + 0.5 * slope_y),
            ],
            width: line_width(rect),
            alpha: 1.0,
        });
    }
    if diagonals.upper_left_to_lower_right {
        commands.push(SpriteCommand::StrokePolyline {
            points: vec![
                SpritePoint::new(rect.min_x - 0.5 * slope_x, rect.min_y - 0.5 * slope_y),
                SpritePoint::new(rect.max_x + 0.5 * slope_x, rect.max_y + 0.5 * slope_y),
            ],
            width: line_width(rect),
            alpha: 1.0,
        });
    }
    commands
}

fn box_rounded_corner_command(corner: BoxRoundedCorner, rect: SurfaceRect) -> SpriteCommand {
    let thick = line_width(rect);
    let center_x = rect.min_x + ((rect.width() - thick) * 0.5).floor() + thick * 0.5;
    let center_y = rect.min_y + ((rect.height() - thick) * 0.5).floor() + thick * 0.5;
    let radius = rect.width().min(rect.height()) * 0.5;
    let s = 0.25;
    let mut points = Vec::new();

    match corner {
        BoxRoundedCorner::UpperLeft => {
            points.push(SpritePoint::new(center_x, rect.max_y));
            points.push(SpritePoint::new(center_x, center_y + radius));
            sample_cubic(
                [
                    SpritePoint::new(center_x, center_y + radius),
                    SpritePoint::new(center_x, center_y + s * radius),
                    SpritePoint::new(center_x + s * radius, center_y),
                    SpritePoint::new(center_x + radius, center_y),
                ],
                &mut points,
            );
        }
        BoxRoundedCorner::UpperRight => {
            points.push(SpritePoint::new(center_x, rect.max_y));
            points.push(SpritePoint::new(center_x, center_y + radius));
            sample_cubic(
                [
                    SpritePoint::new(center_x, center_y + radius),
                    SpritePoint::new(center_x, center_y + s * radius),
                    SpritePoint::new(center_x - s * radius, center_y),
                    SpritePoint::new(center_x - radius, center_y),
                ],
                &mut points,
            );
        }
        BoxRoundedCorner::LowerRight => {
            points.push(SpritePoint::new(center_x, rect.min_y));
            points.push(SpritePoint::new(center_x, center_y - radius));
            sample_cubic(
                [
                    SpritePoint::new(center_x, center_y - radius),
                    SpritePoint::new(center_x, center_y - s * radius),
                    SpritePoint::new(center_x - s * radius, center_y),
                    SpritePoint::new(center_x - radius, center_y),
                ],
                &mut points,
            );
        }
        BoxRoundedCorner::LowerLeft => {
            points.push(SpritePoint::new(center_x, rect.min_y));
            points.push(SpritePoint::new(center_x, center_y - radius));
            sample_cubic(
                [
                    SpritePoint::new(center_x, center_y - radius),
                    SpritePoint::new(center_x, center_y - s * radius),
                    SpritePoint::new(center_x + s * radius, center_y),
                    SpritePoint::new(center_x + radius, center_y),
                ],
                &mut points,
            );
        }
    }

    SpriteCommand::StrokePolyline {
        points,
        width: thick,
        alpha: 1.0,
    }
}

fn box_line_commands(lines: BoxLines, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let light = line_width(rect);
    let heavy = heavy_line_width(rect);
    let center_x = rect.min_x + rect.width() * 0.5;
    let center_y = rect.min_y + rect.height() * 0.5;
    let h_light_top = center_y - light * 0.5;
    let h_light_bottom = center_y + light * 0.5;
    let h_heavy_top = center_y - heavy * 0.5;
    let h_heavy_bottom = center_y + heavy * 0.5;
    let h_double_top = h_light_top - light;
    let h_double_bottom = h_light_bottom + light;
    let v_light_left = center_x - light * 0.5;
    let v_light_right = center_x + light * 0.5;
    let v_heavy_left = center_x - heavy * 0.5;
    let v_heavy_right = center_x + heavy * 0.5;
    let v_double_left = v_light_left - light;
    let v_double_right = v_light_right + light;
    let horizontal_has_heavy =
        lines.left == BoxLineStyle::Heavy || lines.right == BoxLineStyle::Heavy;
    let horizontal_has_double =
        lines.left == BoxLineStyle::Double || lines.right == BoxLineStyle::Double;
    let horizontal_is_empty = lines.left == BoxLineStyle::None && lines.right == BoxLineStyle::None;
    let vertical_has_heavy = lines.up == BoxLineStyle::Heavy || lines.down == BoxLineStyle::Heavy;
    let vertical_has_double =
        lines.up == BoxLineStyle::Double || lines.down == BoxLineStyle::Double;
    let vertical_is_empty = lines.up == BoxLineStyle::None && lines.down == BoxLineStyle::None;

    let up_bottom = if horizontal_has_heavy {
        h_heavy_bottom
    } else if lines.left != lines.right || lines.down == lines.up {
        if horizontal_has_double {
            h_double_bottom
        } else {
            h_light_bottom
        }
    } else if horizontal_is_empty {
        h_light_bottom
    } else {
        h_light_top
    };
    let down_top = if horizontal_has_heavy {
        h_heavy_top
    } else if lines.left != lines.right || lines.up == lines.down {
        if horizontal_has_double {
            h_double_top
        } else {
            h_light_top
        }
    } else if horizontal_is_empty {
        h_light_top
    } else {
        h_light_bottom
    };
    let left_right = if vertical_has_heavy {
        v_heavy_right
    } else if lines.up != lines.down || lines.left == lines.right {
        if vertical_has_double {
            v_double_right
        } else {
            v_light_right
        }
    } else if vertical_is_empty {
        v_light_right
    } else {
        v_light_left
    };
    let right_left = if vertical_has_heavy {
        v_heavy_left
    } else if lines.up != lines.down || lines.right == lines.left {
        if vertical_has_double {
            v_double_left
        } else {
            v_light_left
        }
    } else if vertical_is_empty {
        v_light_left
    } else {
        v_light_right
    };

    let mut commands = Vec::new();
    match lines.up {
        BoxLineStyle::None => {}
        BoxLineStyle::Light | BoxLineStyle::Heavy => {
            commands.push(fill_rect(
                center_x - box_line_width(lines.up, rect) * 0.5,
                rect.min_y,
                box_line_width(lines.up, rect),
                up_bottom - rect.min_y,
            ));
        }
        BoxLineStyle::Double => {
            let left_bottom = if lines.left == BoxLineStyle::Double {
                h_light_top
            } else {
                up_bottom
            };
            let right_bottom = if lines.right == BoxLineStyle::Double {
                h_light_top
            } else {
                up_bottom
            };
            commands.push(fill_rect(
                v_double_left,
                rect.min_y,
                light,
                left_bottom - rect.min_y,
            ));
            commands.push(fill_rect(
                v_light_right,
                rect.min_y,
                light,
                right_bottom - rect.min_y,
            ));
        }
    }
    match lines.down {
        BoxLineStyle::None => {}
        BoxLineStyle::Light | BoxLineStyle::Heavy => {
            commands.push(fill_rect(
                center_x - box_line_width(lines.down, rect) * 0.5,
                down_top,
                box_line_width(lines.down, rect),
                rect.max_y - down_top,
            ));
        }
        BoxLineStyle::Double => {
            let left_top = if lines.left == BoxLineStyle::Double {
                h_light_bottom
            } else {
                down_top
            };
            let right_top = if lines.right == BoxLineStyle::Double {
                h_light_bottom
            } else {
                down_top
            };
            commands.push(fill_rect(
                v_double_left,
                left_top,
                light,
                rect.max_y - left_top,
            ));
            commands.push(fill_rect(
                v_light_right,
                right_top,
                light,
                rect.max_y - right_top,
            ));
        }
    }
    match lines.left {
        BoxLineStyle::None => {}
        BoxLineStyle::Light | BoxLineStyle::Heavy => {
            let width = left_right - rect.min_x;
            commands.push(fill_rect(
                rect.min_x,
                center_y - box_line_width(lines.left, rect) * 0.5,
                width,
                box_line_width(lines.left, rect),
            ));
        }
        BoxLineStyle::Double => {
            let top_right = if lines.up == BoxLineStyle::Double {
                v_light_left
            } else {
                left_right
            };
            let bottom_right = if lines.down == BoxLineStyle::Double {
                v_light_left
            } else {
                left_right
            };
            commands.push(fill_rect(
                rect.min_x,
                h_double_top,
                top_right - rect.min_x,
                light,
            ));
            commands.push(fill_rect(
                rect.min_x,
                h_light_bottom,
                bottom_right - rect.min_x,
                light,
            ));
        }
    }
    match lines.right {
        BoxLineStyle::None => {}
        BoxLineStyle::Light | BoxLineStyle::Heavy => {
            commands.push(fill_rect(
                right_left,
                center_y - box_line_width(lines.right, rect) * 0.5,
                rect.max_x - right_left,
                box_line_width(lines.right, rect),
            ));
        }
        BoxLineStyle::Double => {
            let top_left = if lines.up == BoxLineStyle::Double {
                v_light_right
            } else {
                right_left
            };
            let bottom_left = if lines.down == BoxLineStyle::Double {
                v_light_right
            } else {
                right_left
            };
            commands.push(fill_rect(
                top_left,
                h_double_top,
                rect.max_x - top_left,
                light,
            ));
            commands.push(fill_rect(
                bottom_left,
                h_light_bottom,
                rect.max_x - bottom_left,
                light,
            ));
        }
    }
    commands
}

fn box_line_width(style: BoxLineStyle, rect: SurfaceRect) -> f32 {
    match style {
        BoxLineStyle::None => 0.0,
        BoxLineStyle::Light => line_width(rect),
        BoxLineStyle::Heavy => heavy_line_width(rect),
        BoxLineStyle::Double => line_width(rect),
    }
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

fn braille_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let dots = ch as u32 - 0x2800;
    if dots == 0 {
        return Vec::new();
    }
    let mut commands = Vec::new();
    let layout = braille_dot_layout(rect);
    let positions = [
        (0, 0),
        (1, 0),
        (2, 0),
        (0, 1),
        (1, 1),
        (2, 1),
        (3, 0),
        (3, 1),
    ];
    for (bit, (row, col)) in positions.into_iter().enumerate() {
        if dots & (1 << bit) != 0 {
            commands.push(SpriteCommand::FillRect {
                rect: SurfaceRect::from_min_size(
                    rect.min_x + layout.x[col],
                    rect.min_y + layout.y[row],
                    layout.dot_width,
                    layout.dot_width,
                ),
                alpha: 1.0,
            });
        }
    }
    if commands.is_empty() {
        placeholder_commands(rect)
    } else {
        commands
    }
}

struct BrailleDotLayout {
    dot_width: f32,
    x: [f32; 2],
    y: [f32; 4],
}

fn braille_dot_layout(rect: SurfaceRect) -> BrailleDotLayout {
    let width = rect.width().round() as i32;
    let height = rect.height().round() as i32;

    let mut dot_width = (width / 4).min(height / 8);
    let mut x_spacing = width / 4;
    let mut y_spacing = height / 8;
    let mut x_margin = x_spacing / 2;
    let mut y_margin = y_spacing / 2;

    let mut x_px_left = width - 2 * x_margin - x_spacing - 2 * dot_width;
    let mut y_px_left = height - 2 * y_margin - 3 * y_spacing - 4 * dot_width;

    if x_px_left >= 2 && y_px_left >= 4 && dot_width == 0 {
        dot_width += 1;
        x_px_left -= 2;
        y_px_left -= 4;
    }

    if x_px_left >= 2 && x_margin == 0 {
        x_margin += 1;
        x_px_left -= 2;
    }
    if y_px_left >= 2 && y_margin == 0 {
        y_margin += 1;
        y_px_left -= 2;
    }

    if x_px_left >= 1 {
        x_spacing += 1;
        x_px_left -= 1;
    }
    if y_px_left >= 3 {
        y_spacing += 1;
        y_px_left -= 3;
    }

    if x_px_left >= 2 {
        x_margin += 1;
        x_px_left -= 2;
    }
    if y_px_left >= 2 {
        y_margin += 1;
        y_px_left -= 2;
    }

    if x_px_left >= 2 && y_px_left >= 4 {
        dot_width += 1;
    }

    let dot_width = dot_width.max(0) as f32;
    let x_margin = x_margin as f32;
    let y_margin = y_margin as f32;
    let x_spacing = x_spacing as f32;
    let y_spacing = y_spacing as f32;

    BrailleDotLayout {
        dot_width,
        x: [x_margin, x_margin + dot_width + x_spacing],
        y: [
            y_margin,
            y_margin + dot_width + y_spacing,
            y_margin + 2.0 * (dot_width + y_spacing),
            y_margin + 3.0 * (dot_width + y_spacing),
        ],
    }
}

fn legacy_computing_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    if ('\u{1FB00}'..='\u{1FB3B}').contains(&ch) {
        return sextant_commands(ch, rect);
    }
    if ('\u{1FB3C}'..='\u{1FB67}').contains(&ch) {
        return smooth_mosaic_commands(ch, rect);
    }
    if ('\u{1FB68}'..='\u{1FB6F}').contains(&ch) || ('\u{1FB9A}'..='\u{1FB9B}').contains(&ch) {
        return legacy_edge_triangle_commands(ch, rect);
    }
    if ('\u{1FB9C}'..='\u{1FB9F}').contains(&ch) {
        return legacy_corner_triangle_shade_commands(ch, rect);
    }
    if ('\u{1FB70}'..='\u{1FB97}').contains(&ch) {
        return legacy_block_extension_commands(ch, rect);
    }
    if ('\u{1FB98}'..='\u{1FB99}').contains(&ch) {
        return legacy_hatch_commands(ch, rect);
    }
    if ('\u{1FBA0}'..='\u{1FBAE}').contains(&ch) {
        return legacy_corner_diagonal_commands(ch, rect);
    }
    if ch == '\u{1FBAF}' {
        return legacy_mixed_box_connector_commands(rect);
    }
    if ('\u{1FBBD}'..='\u{1FBBF}').contains(&ch) {
        return legacy_inverse_diagonal_commands(ch, rect);
    }
    if ('\u{1FBCE}'..='\u{1FBCF}').contains(&ch) || ('\u{1FBE4}'..='\u{1FBE7}').contains(&ch) {
        return legacy_fractional_block_commands(ch, rect);
    }
    if ('\u{1FBD0}'..='\u{1FBDF}').contains(&ch) {
        return legacy_cell_diagonal_commands(ch, rect);
    }
    if ('\u{1FBE0}'..='\u{1FBEF}').contains(&ch) {
        return legacy_circle_commands(ch, rect);
    }

    placeholder_commands(rect)
}

fn smooth_mosaic_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let pattern = SMOOTH_MOSAIC_PATTERNS[ch as usize - 0x1FB3C];
    let mosaic = SmoothMosaic::from_pattern(pattern);
    let points = mosaic_polygon_points(mosaic, rect);
    if points.len() < 3 {
        Vec::new()
    } else {
        vec![filled_polygon(points)]
    }
}

fn legacy_block_extension_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let cp = ch as u32;
    if (0x1FB70..=0x1FB75).contains(&cp) {
        let slot = (cp - 0x1FB6F) as u8;
        return vec![fill_eighth_columns(rect, slot, slot + 1)];
    }
    if (0x1FB76..=0x1FB7B).contains(&cp) {
        let slot = (cp - 0x1FB75) as u8;
        return vec![fill_eighth_rows(rect, slot, slot + 1)];
    }

    match cp {
        0x1FB7C => vec![
            fill_eighth_columns(rect, 0, 1),
            fill_eighth_rows(rect, 7, 8),
        ],
        0x1FB7D => vec![
            fill_eighth_columns(rect, 0, 1),
            fill_eighth_rows(rect, 0, 1),
        ],
        0x1FB7E => vec![
            fill_eighth_columns(rect, 7, 8),
            fill_eighth_rows(rect, 0, 1),
        ],
        0x1FB7F => vec![
            fill_eighth_columns(rect, 7, 8),
            fill_eighth_rows(rect, 7, 8),
        ],
        0x1FB80 => vec![fill_eighth_rows(rect, 0, 1), fill_eighth_rows(rect, 7, 8)],
        0x1FB81 => vec![
            fill_eighth_rows(rect, 0, 1),
            fill_eighth_rows(rect, 2, 3),
            fill_eighth_rows(rect, 4, 5),
            fill_eighth_rows(rect, 7, 8),
        ],
        0x1FB82 => vec![fill_eighth_rows(rect, 0, 2)],
        0x1FB83 => vec![fill_eighth_rows(rect, 0, 3)],
        0x1FB84 => vec![fill_eighth_rows(rect, 0, 5)],
        0x1FB85 => vec![fill_eighth_rows(rect, 0, 6)],
        0x1FB86 => vec![fill_eighth_rows(rect, 0, 7)],
        0x1FB87 => vec![fill_eighth_columns(rect, 6, 8)],
        0x1FB88 => vec![fill_eighth_columns(rect, 5, 8)],
        0x1FB89 => vec![fill_eighth_columns(rect, 3, 8)],
        0x1FB8A => vec![fill_eighth_columns(rect, 2, 8)],
        0x1FB8B => vec![fill_eighth_columns(rect, 1, 8)],
        0x1FB8C => vec![shade_eighth_columns(rect, 0, 4, 0.5)],
        0x1FB8D => vec![shade_eighth_columns(rect, 4, 8, 0.5)],
        0x1FB8E => vec![shade_eighth_rows(rect, 0, 4, 0.5)],
        0x1FB8F => vec![shade_eighth_rows(rect, 4, 8, 0.5)],
        0x1FB90 => vec![shade_rect(rect, 0.5)],
        0x1FB91 => vec![shade_rect(rect, 0.5), fill_eighth_rows(rect, 0, 4)],
        0x1FB92 => vec![shade_rect(rect, 0.5), fill_eighth_rows(rect, 4, 8)],
        0x1FB93 => Vec::new(),
        0x1FB94 => vec![shade_rect(rect, 0.5), fill_eighth_columns(rect, 4, 8)],
        0x1FB95 => checkerboard_commands(rect, 0),
        0x1FB96 => checkerboard_commands(rect, 1),
        0x1FB97 => vec![fill_eighth_rows(rect, 2, 4), fill_eighth_rows(rect, 6, 8)],
        _ => placeholder_commands(rect),
    }
}

fn legacy_hatch_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let line_count = (rect.width() / (2.0 * line_width(rect))).floor().max(1.0) as i32;
    let stride = (rect.width() / line_count as f32).round();
    (-line_count..=line_count)
        .map(|i| clipped_hatch_line(rect, i as f32 * stride, ch == '\u{1FB99}'))
        .collect()
}

fn clipped_hatch_line(rect: SurfaceRect, offset: f32, descending: bool) -> SpriteCommand {
    let w = rect.width();
    let h = rect.height();
    let mut points = Vec::new();
    let add_unique = |points: &mut Vec<SpritePoint>, x: f32, y: f32| {
        let point = SpritePoint::new(x, y);
        if !points.iter().any(|existing| {
            (existing.x - point.x).abs() < 0.001 && (existing.y - point.y).abs() < 0.001
        }) {
            points.push(point);
        }
    };

    if descending {
        let top_x = w + offset;
        let bottom_x = offset;
        if (0.0..=w).contains(&top_x) {
            add_unique(&mut points, rect.min_x + top_x, rect.min_y);
        }
        if (0.0..=w).contains(&bottom_x) {
            add_unique(&mut points, rect.min_x + bottom_x, rect.max_y);
        }
        let left_y = h * (w + offset) / w;
        if (0.0..=h).contains(&left_y) {
            add_unique(&mut points, rect.min_x, rect.min_y + left_y);
        }
        let right_y = h * offset / w;
        if (0.0..=h).contains(&right_y) {
            add_unique(&mut points, rect.max_x, rect.min_y + right_y);
        }
    } else {
        let top_x = offset;
        let bottom_x = w + offset;
        if (0.0..=w).contains(&top_x) {
            add_unique(&mut points, rect.min_x + top_x, rect.min_y);
        }
        if (0.0..=w).contains(&bottom_x) {
            add_unique(&mut points, rect.min_x + bottom_x, rect.max_y);
        }
        let left_y = -offset * h / w;
        if (0.0..=h).contains(&left_y) {
            add_unique(&mut points, rect.min_x, rect.min_y + left_y);
        }
        let right_y = (w - offset) * h / w;
        if (0.0..=h).contains(&right_y) {
            add_unique(&mut points, rect.max_x, rect.min_y + right_y);
        }
    }

    stroke_polyline(points, rect)
}

fn legacy_mixed_box_connector_commands(rect: SurfaceRect) -> Vec<SpriteCommand> {
    let light = line_width(rect);
    let heavy = heavy_line_width(rect);
    let h_light_top = rect.min_y + ((rect.height() - light) / 2.0).floor();
    let h_light_bottom = h_light_top + light;
    let v_heavy_left = rect.min_x + ((rect.width() - heavy) / 2.0).floor();

    vec![
        SpriteCommand::FillRect {
            rect: SurfaceRect::from_min_size(
                v_heavy_left,
                rect.min_y,
                heavy,
                h_light_bottom - rect.min_y,
            ),
            alpha: 1.0,
        },
        SpriteCommand::FillRect {
            rect: SurfaceRect::from_min_size(
                v_heavy_left,
                h_light_top,
                heavy,
                rect.max_y - h_light_top,
            ),
            alpha: 1.0,
        },
        SpriteCommand::FillRect {
            rect: SurfaceRect::from_min_size(rect.min_x, h_light_top, rect.width(), light),
            alpha: 1.0,
        },
    ]
}

fn legacy_inverse_diagonal_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let mut commands = vec![SpriteCommand::FillRect { rect, alpha: 1.0 }];
    match ch as u32 {
        0x1FBBD => commands.extend(light_diagonal_cross_clear_commands(rect)),
        0x1FBBE => {
            let (from, to) = legacy_corner_diagonal_segment(LegacyCorner::LowerRight, rect);
            commands.push(clear_stroke_polyline(vec![from, to], rect));
        }
        0x1FBBF => {
            commands.extend(
                [
                    LegacyCorner::UpperLeft,
                    LegacyCorner::UpperRight,
                    LegacyCorner::LowerLeft,
                    LegacyCorner::LowerRight,
                ]
                .into_iter()
                .map(|corner| {
                    let (from, to) = legacy_corner_diagonal_segment(corner, rect);
                    clear_stroke_polyline(vec![from, to], rect)
                }),
            );
        }
        _ => return placeholder_commands(rect),
    }
    commands
}

fn light_diagonal_cross_clear_commands(rect: SurfaceRect) -> Vec<SpriteCommand> {
    let slope_x = rect.width().min(rect.height()) / rect.height().max(1.0);
    let slope_y = rect.height().min(rect.width()) / rect.width().max(1.0);
    vec![
        clear_stroke_polyline(
            vec![
                SpritePoint::new(rect.max_x + 0.5 * slope_x, rect.min_y - 0.5 * slope_y),
                SpritePoint::new(rect.min_x - 0.5 * slope_x, rect.max_y + 0.5 * slope_y),
            ],
            rect,
        ),
        clear_stroke_polyline(
            vec![
                SpritePoint::new(rect.min_x - 0.5 * slope_x, rect.min_y - 0.5 * slope_y),
                SpritePoint::new(rect.max_x + 0.5 * slope_x, rect.max_y + 0.5 * slope_y),
            ],
            rect,
        ),
    ]
}

fn legacy_circle_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    match ch as u32 {
        0x1FBE0 => vec![circle_arc_command(rect, LegacyCirclePosition::Top)],
        0x1FBE1 => vec![circle_arc_command(rect, LegacyCirclePosition::Right)],
        0x1FBE2 => vec![circle_arc_command(rect, LegacyCirclePosition::Bottom)],
        0x1FBE3 => vec![circle_arc_command(rect, LegacyCirclePosition::Left)],
        0x1FBE8 => vec![filled_circle_sector(rect, LegacyCirclePosition::Top)],
        0x1FBE9 => vec![filled_circle_sector(rect, LegacyCirclePosition::Right)],
        0x1FBEA => vec![filled_circle_sector(rect, LegacyCirclePosition::Bottom)],
        0x1FBEB => vec![filled_circle_sector(rect, LegacyCirclePosition::Left)],
        0x1FBEC => vec![filled_circle_sector(rect, LegacyCirclePosition::TopRight)],
        0x1FBED => vec![filled_circle_sector(rect, LegacyCirclePosition::BottomLeft)],
        0x1FBEE => vec![filled_circle_sector(
            rect,
            LegacyCirclePosition::BottomRight,
        )],
        0x1FBEF => vec![filled_circle_sector(rect, LegacyCirclePosition::TopLeft)],
        _ => placeholder_commands(rect),
    }
}

fn legacy_edge_triangle_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    match ch as u32 {
        0x1FB68 => inverted_edge_triangle_commands(rect, LegacyEdge::Left),
        0x1FB69 => inverted_edge_triangle_commands(rect, LegacyEdge::Top),
        0x1FB6A => inverted_edge_triangle_commands(rect, LegacyEdge::Right),
        0x1FB6B => inverted_edge_triangle_commands(rect, LegacyEdge::Bottom),
        0x1FB6C => vec![edge_triangle_command(rect, LegacyEdge::Left)],
        0x1FB6D => vec![edge_triangle_command(rect, LegacyEdge::Top)],
        0x1FB6E => vec![edge_triangle_command(rect, LegacyEdge::Right)],
        0x1FB6F => vec![edge_triangle_command(rect, LegacyEdge::Bottom)],
        0x1FB9A => vec![
            edge_triangle_command(rect, LegacyEdge::Top),
            edge_triangle_command(rect, LegacyEdge::Bottom),
        ],
        0x1FB9B => vec![
            edge_triangle_command(rect, LegacyEdge::Left),
            edge_triangle_command(rect, LegacyEdge::Right),
        ],
        _ => placeholder_commands(rect),
    }
}

fn legacy_corner_triangle_shade_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let points = match ch as u32 {
        0x1FB9C => [left_top(rect), right_top(rect), left_bottom(rect)],
        0x1FB9D => [left_top(rect), right_top(rect), right_bottom(rect)],
        0x1FB9E => [right_top(rect), right_bottom(rect), left_bottom(rect)],
        0x1FB9F => [left_top(rect), left_bottom(rect), right_bottom(rect)],
        _ => return placeholder_commands(rect),
    };
    vec![SpriteCommand::FillPolygon {
        shape: SpriteShape::Triangle,
        points: points.to_vec(),
        alpha: 0.5,
    }]
}

fn legacy_fractional_block_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    match ch as u32 {
        0x1FBCE => vec![fill_fractional_rect(rect, 0.0, 0.0, 2.0 / 3.0, 1.0)],
        0x1FBCF => vec![fill_fractional_rect(rect, 0.0, 0.0, 1.0 / 3.0, 1.0)],
        0x1FBE4 => vec![fill_fractional_rect(rect, 0.25, 0.0, 0.5, 0.5)],
        0x1FBE5 => vec![fill_fractional_rect(rect, 0.25, 0.5, 0.5, 0.5)],
        0x1FBE6 => vec![fill_fractional_rect(rect, 0.0, 0.25, 0.5, 0.5)],
        0x1FBE7 => vec![fill_fractional_rect(rect, 0.5, 0.25, 0.5, 0.5)],
        _ => placeholder_commands(rect),
    }
}

#[derive(Clone, Copy)]
enum LegacyCirclePosition {
    Top,
    Right,
    Bottom,
    Left,
    TopRight,
    BottomLeft,
    BottomRight,
    TopLeft,
}

fn circle_arc_command(rect: SurfaceRect, position: LegacyCirclePosition) -> SpriteCommand {
    SpriteCommand::StrokePolyline {
        points: circle_arc_points(rect, position),
        width: line_width(rect),
        alpha: 1.0,
    }
}

fn filled_circle_sector(rect: SurfaceRect, position: LegacyCirclePosition) -> SpriteCommand {
    let mut points = vec![circle_center(rect, position)];
    points.extend(circle_arc_points(rect, position));
    filled_polygon(points)
}

fn circle_arc_points(rect: SurfaceRect, position: LegacyCirclePosition) -> Vec<SpritePoint> {
    let (start, end) = circle_angles(position);
    let center = circle_center(rect, position);
    let radius = rect.width().min(rect.height()) * 0.5;
    let steps = if (end - start).abs() > std::f32::consts::FRAC_PI_2 {
        8
    } else {
        4
    };

    (0..=steps)
        .map(|step| {
            let t = step as f32 / steps as f32;
            let angle = start + (end - start) * t;
            SpritePoint::new(
                center.x + radius * angle.cos(),
                center.y + radius * angle.sin(),
            )
        })
        .collect()
}

fn circle_center(rect: SurfaceRect, position: LegacyCirclePosition) -> SpritePoint {
    let x = match position {
        LegacyCirclePosition::Left
        | LegacyCirclePosition::TopLeft
        | LegacyCirclePosition::BottomLeft => rect.min_x,
        LegacyCirclePosition::Right
        | LegacyCirclePosition::TopRight
        | LegacyCirclePosition::BottomRight => rect.max_x,
        LegacyCirclePosition::Top | LegacyCirclePosition::Bottom => rect.min_x + rect.width() * 0.5,
    };
    let y = match position {
        LegacyCirclePosition::Top
        | LegacyCirclePosition::TopLeft
        | LegacyCirclePosition::TopRight => rect.min_y,
        LegacyCirclePosition::Bottom
        | LegacyCirclePosition::BottomLeft
        | LegacyCirclePosition::BottomRight => rect.max_y,
        LegacyCirclePosition::Left | LegacyCirclePosition::Right => {
            rect.min_y + rect.height() * 0.5
        }
    };
    SpritePoint::new(x, y)
}

fn circle_angles(position: LegacyCirclePosition) -> (f32, f32) {
    let pi = std::f32::consts::PI;
    let half = std::f32::consts::FRAC_PI_2;
    match position {
        LegacyCirclePosition::Top => (0.0, pi),
        LegacyCirclePosition::Right => (half, pi + half),
        LegacyCirclePosition::Bottom => (pi, 2.0 * pi),
        LegacyCirclePosition::Left => (-half, half),
        LegacyCirclePosition::TopRight => (half, pi),
        LegacyCirclePosition::BottomLeft => (-half, 0.0),
        LegacyCirclePosition::BottomRight => (pi, pi + half),
        LegacyCirclePosition::TopLeft => (0.0, half),
    }
}

#[derive(Clone, Copy)]
enum LegacyEdge {
    Top,
    Left,
    Bottom,
    Right,
}

fn edge_triangle_command(rect: SurfaceRect, edge: LegacyEdge) -> SpriteCommand {
    let center = SpritePoint::new(
        rect.min_x + rect.width() * 0.5,
        rect.min_y + rect.height() * 0.5,
    );
    let (a, b) = edge_span(edge, rect);
    filled_triangle([center, a, b])
}

fn inverted_edge_triangle_commands(rect: SurfaceRect, edge: LegacyEdge) -> Vec<SpriteCommand> {
    let center = SpritePoint::new(
        rect.min_x + rect.width() * 0.5,
        rect.min_y + rect.height() * 0.5,
    );
    match edge {
        LegacyEdge::Left => vec![
            filled_triangle([left_top(rect), right_top(rect), center]),
            filled_triangle([center, right_bottom(rect), left_bottom(rect)]),
        ],
        LegacyEdge::Top => vec![
            filled_triangle([left_top(rect), left_bottom(rect), center]),
            filled_triangle([center, right_bottom(rect), right_top(rect)]),
        ],
        LegacyEdge::Right => vec![
            filled_triangle([right_top(rect), left_top(rect), center]),
            filled_triangle([center, left_bottom(rect), right_bottom(rect)]),
        ],
        LegacyEdge::Bottom => vec![
            filled_triangle([left_bottom(rect), left_top(rect), center]),
            filled_triangle([center, right_top(rect), right_bottom(rect)]),
        ],
    }
}

fn edge_span(edge: LegacyEdge, rect: SurfaceRect) -> (SpritePoint, SpritePoint) {
    match edge {
        LegacyEdge::Top => (right_top(rect), left_top(rect)),
        LegacyEdge::Left => (left_top(rect), left_bottom(rect)),
        LegacyEdge::Bottom => (left_bottom(rect), right_bottom(rect)),
        LegacyEdge::Right => (right_bottom(rect), right_top(rect)),
    }
}

fn legacy_corner_diagonal_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let cp = ch as u32;
    let corners: &[LegacyCorner] = match cp {
        0x1FBA0 => &[LegacyCorner::UpperLeft],
        0x1FBA1 => &[LegacyCorner::UpperRight],
        0x1FBA2 => &[LegacyCorner::LowerLeft],
        0x1FBA3 => &[LegacyCorner::LowerRight],
        0x1FBA4 => &[LegacyCorner::UpperLeft, LegacyCorner::LowerLeft],
        0x1FBA5 => &[LegacyCorner::UpperRight, LegacyCorner::LowerRight],
        0x1FBA6 => &[LegacyCorner::LowerLeft, LegacyCorner::LowerRight],
        0x1FBA7 => &[LegacyCorner::UpperLeft, LegacyCorner::UpperRight],
        0x1FBA8 => &[LegacyCorner::UpperLeft, LegacyCorner::LowerRight],
        0x1FBA9 => &[LegacyCorner::UpperRight, LegacyCorner::LowerLeft],
        0x1FBAA => &[
            LegacyCorner::UpperRight,
            LegacyCorner::LowerLeft,
            LegacyCorner::LowerRight,
        ],
        0x1FBAB => &[
            LegacyCorner::UpperLeft,
            LegacyCorner::LowerLeft,
            LegacyCorner::LowerRight,
        ],
        0x1FBAC => &[
            LegacyCorner::UpperLeft,
            LegacyCorner::UpperRight,
            LegacyCorner::LowerRight,
        ],
        0x1FBAD => &[
            LegacyCorner::UpperLeft,
            LegacyCorner::UpperRight,
            LegacyCorner::LowerLeft,
        ],
        0x1FBAE => &[
            LegacyCorner::UpperLeft,
            LegacyCorner::UpperRight,
            LegacyCorner::LowerLeft,
            LegacyCorner::LowerRight,
        ],
        _ => return placeholder_commands(rect),
    };

    corners
        .iter()
        .map(|corner| {
            let (from, to) = legacy_corner_diagonal_segment(*corner, rect);
            stroke_polyline(vec![from, to], rect)
        })
        .collect()
}

fn fill_fractional_rect(
    rect: SurfaceRect,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> SpriteCommand {
    SpriteCommand::FillRect {
        rect: SurfaceRect::from_min_size(
            rect.min_x + rect.width() * x,
            rect.min_y + rect.height() * y,
            rect.width() * width,
            rect.height() * height,
        ),
        alpha: 1.0,
    }
}

fn legacy_cell_diagonal_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let cp = ch as u32;
    let pairs: &[(LegacyAlignment, LegacyAlignment)] = match cp {
        0x1FBD0 => &[(LegacyAlignment::MiddleRight, LegacyAlignment::LowerLeft)],
        0x1FBD1 => &[(LegacyAlignment::UpperRight, LegacyAlignment::MiddleLeft)],
        0x1FBD2 => &[(LegacyAlignment::UpperLeft, LegacyAlignment::MiddleRight)],
        0x1FBD3 => &[(LegacyAlignment::MiddleLeft, LegacyAlignment::LowerRight)],
        0x1FBD4 => &[(LegacyAlignment::UpperLeft, LegacyAlignment::LowerCenter)],
        0x1FBD5 => &[(LegacyAlignment::UpperCenter, LegacyAlignment::LowerRight)],
        0x1FBD6 => &[(LegacyAlignment::UpperRight, LegacyAlignment::LowerCenter)],
        0x1FBD7 => &[(LegacyAlignment::UpperCenter, LegacyAlignment::LowerLeft)],
        0x1FBD8 => &[
            (LegacyAlignment::UpperLeft, LegacyAlignment::MiddleCenter),
            (LegacyAlignment::MiddleCenter, LegacyAlignment::UpperRight),
        ],
        0x1FBD9 => &[
            (LegacyAlignment::UpperRight, LegacyAlignment::MiddleCenter),
            (LegacyAlignment::MiddleCenter, LegacyAlignment::LowerRight),
        ],
        0x1FBDA => &[
            (LegacyAlignment::LowerLeft, LegacyAlignment::MiddleCenter),
            (LegacyAlignment::MiddleCenter, LegacyAlignment::LowerRight),
        ],
        0x1FBDB => &[
            (LegacyAlignment::UpperLeft, LegacyAlignment::MiddleCenter),
            (LegacyAlignment::MiddleCenter, LegacyAlignment::LowerLeft),
        ],
        0x1FBDC => &[
            (LegacyAlignment::UpperLeft, LegacyAlignment::LowerCenter),
            (LegacyAlignment::LowerCenter, LegacyAlignment::UpperRight),
        ],
        0x1FBDD => &[
            (LegacyAlignment::UpperRight, LegacyAlignment::MiddleLeft),
            (LegacyAlignment::MiddleLeft, LegacyAlignment::LowerRight),
        ],
        0x1FBDE => &[
            (LegacyAlignment::LowerLeft, LegacyAlignment::UpperCenter),
            (LegacyAlignment::UpperCenter, LegacyAlignment::LowerRight),
        ],
        0x1FBDF => &[
            (LegacyAlignment::UpperLeft, LegacyAlignment::MiddleRight),
            (LegacyAlignment::MiddleRight, LegacyAlignment::LowerLeft),
        ],
        _ => return placeholder_commands(rect),
    };

    pairs
        .iter()
        .map(|(from, to)| {
            stroke_polyline(
                vec![
                    legacy_alignment_point(*from, rect),
                    legacy_alignment_point(*to, rect),
                ],
                rect,
            )
        })
        .collect()
}

#[derive(Clone, Copy)]
enum LegacyCorner {
    UpperLeft,
    UpperRight,
    LowerLeft,
    LowerRight,
}

fn legacy_corner_diagonal_segment(
    corner: LegacyCorner,
    rect: SurfaceRect,
) -> (SpritePoint, SpritePoint) {
    let center_x = rect.min_x + rect.width() * 0.5;
    let center_y = rect.min_y + rect.height() * 0.5;
    match corner {
        LegacyCorner::UpperLeft => (
            SpritePoint::new(center_x, rect.min_y),
            SpritePoint::new(rect.min_x, center_y),
        ),
        LegacyCorner::UpperRight => (
            SpritePoint::new(center_x, rect.min_y),
            SpritePoint::new(rect.max_x, center_y),
        ),
        LegacyCorner::LowerLeft => (
            SpritePoint::new(center_x, rect.max_y),
            SpritePoint::new(rect.min_x, center_y),
        ),
        LegacyCorner::LowerRight => (
            SpritePoint::new(center_x, rect.max_y),
            SpritePoint::new(rect.max_x, center_y),
        ),
    }
}

#[derive(Clone, Copy)]
enum LegacyAlignment {
    UpperLeft,
    UpperCenter,
    UpperRight,
    MiddleLeft,
    MiddleCenter,
    MiddleRight,
    LowerLeft,
    LowerCenter,
    LowerRight,
}

fn legacy_alignment_point(alignment: LegacyAlignment, rect: SurfaceRect) -> SpritePoint {
    let x = match alignment {
        LegacyAlignment::UpperLeft | LegacyAlignment::MiddleLeft | LegacyAlignment::LowerLeft => {
            rect.min_x
        }
        LegacyAlignment::UpperRight
        | LegacyAlignment::MiddleRight
        | LegacyAlignment::LowerRight => rect.max_x,
        LegacyAlignment::UpperCenter
        | LegacyAlignment::MiddleCenter
        | LegacyAlignment::LowerCenter => rect.min_x + rect.width() * 0.5,
    };
    let y = match alignment {
        LegacyAlignment::UpperLeft | LegacyAlignment::UpperCenter | LegacyAlignment::UpperRight => {
            rect.min_y
        }
        LegacyAlignment::LowerLeft | LegacyAlignment::LowerCenter | LegacyAlignment::LowerRight => {
            rect.max_y
        }
        LegacyAlignment::MiddleLeft
        | LegacyAlignment::MiddleCenter
        | LegacyAlignment::MiddleRight => rect.min_y + rect.height() * 0.5,
    };

    SpritePoint::new(x, y)
}

#[derive(Clone, Copy)]
struct SmoothMosaic {
    tl: bool,
    ul: bool,
    ll: bool,
    bl: bool,
    bc: bool,
    br: bool,
    lr: bool,
    ur: bool,
    tr: bool,
    tc: bool,
}

impl SmoothMosaic {
    fn from_pattern(pattern: &[u8; 12]) -> Self {
        Self {
            tl: pattern[0] == b'#',
            ul: pattern[3] == b'#' && (pattern[0] != b'#' || pattern[6] != b'#'),
            ll: pattern[6] == b'#' && (pattern[3] != b'#' || pattern[9] != b'#'),
            bl: pattern[9] == b'#',
            bc: pattern[10] == b'#' && (pattern[9] != b'#' || pattern[11] != b'#'),
            br: pattern[11] == b'#',
            lr: pattern[8] == b'#' && (pattern[11] != b'#' || pattern[5] != b'#'),
            ur: pattern[5] == b'#' && (pattern[8] != b'#' || pattern[2] != b'#'),
            tr: pattern[2] == b'#',
            tc: pattern[1] == b'#' && (pattern[2] != b'#' || pattern[0] != b'#'),
        }
    }
}

fn mosaic_polygon_points(mosaic: SmoothMosaic, rect: SurfaceRect) -> Vec<SpritePoint> {
    let upper = rect.min_y + rect.height() / 3.0;
    let lower = rect.min_y + rect.height() * 2.0 / 3.0;
    let center = rect.min_x + rect.width() * 0.5;
    let mut points = Vec::new();

    if mosaic.tl {
        points.push(SpritePoint::new(rect.min_x, rect.min_y));
    }
    if mosaic.ul {
        points.push(SpritePoint::new(rect.min_x, upper));
    }
    if mosaic.ll {
        points.push(SpritePoint::new(rect.min_x, lower));
    }
    if mosaic.bl {
        points.push(SpritePoint::new(rect.min_x, rect.max_y));
    }
    if mosaic.bc {
        points.push(SpritePoint::new(center, rect.max_y));
    }
    if mosaic.br {
        points.push(SpritePoint::new(rect.max_x, rect.max_y));
    }
    if mosaic.lr {
        points.push(SpritePoint::new(rect.max_x, lower));
    }
    if mosaic.ur {
        points.push(SpritePoint::new(rect.max_x, upper));
    }
    if mosaic.tr {
        points.push(SpritePoint::new(rect.max_x, rect.min_y));
    }
    if mosaic.tc {
        points.push(SpritePoint::new(center, rect.min_y));
    }

    points
}

fn sextant_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let cp = ch as u32;
    let idx = cp - 0x1FB00;
    let pattern = idx + (idx / 0x14) + 1;

    sixel_grid_commands(pattern as u8, rect, 3, 2)
}

fn legacy_computing_supplement_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    if ('\u{1CC1B}'..='\u{1CC1E}').contains(&ch) {
        return supplement_horizontal_corner_commands(ch, rect);
    }
    if ('\u{1CC21}'..='\u{1CC2F}').contains(&ch) {
        return separated_quadrant_commands(ch, rect);
    }
    if ('\u{1CC30}'..='\u{1CC3F}').contains(&ch) {
        return supplement_circle_piece_commands(ch, rect);
    }
    if ('\u{1CD00}'..='\u{1CDE5}').contains(&ch) {
        return octant_commands(ch, rect);
    }
    if ('\u{1CE00}'..='\u{1CE01}').contains(&ch) {
        return supplement_split_circle_commands(ch, rect);
    }
    if ('\u{1CE0B}'..='\u{1CE0C}').contains(&ch) {
        return supplement_ellipse_commands(ch, rect);
    }
    if ('\u{1CE16}'..='\u{1CE19}').contains(&ch) {
        return supplement_vertical_corner_commands(ch, rect);
    }
    if ('\u{1CE51}'..='\u{1CE8F}').contains(&ch) {
        return separated_sextant_commands(ch, rect);
    }
    if ('\u{1CE90}'..='\u{1CEAF}').contains(&ch) {
        return sixteenth_block_commands(ch, rect);
    }

    placeholder_commands(rect)
}

fn supplement_circle_piece_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let (x, y, width, height, corner) = match ch as u32 {
        0x1CC30 => (0.0, 0.0, 2.0, 2.0, LegacyCorner::UpperLeft),
        0x1CC31 => (1.0, 0.0, 2.0, 2.0, LegacyCorner::UpperLeft),
        0x1CC32 => (2.0, 0.0, 2.0, 2.0, LegacyCorner::UpperRight),
        0x1CC33 => (3.0, 0.0, 2.0, 2.0, LegacyCorner::UpperRight),
        0x1CC34 => (0.0, 1.0, 2.0, 2.0, LegacyCorner::UpperLeft),
        0x1CC35 => (0.0, 0.0, 1.0, 1.0, LegacyCorner::UpperLeft),
        0x1CC36 => (1.0, 0.0, 1.0, 1.0, LegacyCorner::UpperRight),
        0x1CC37 => (3.0, 1.0, 2.0, 2.0, LegacyCorner::UpperRight),
        0x1CC38 => (0.0, 2.0, 2.0, 2.0, LegacyCorner::LowerLeft),
        0x1CC39 => (0.0, 1.0, 1.0, 1.0, LegacyCorner::LowerLeft),
        0x1CC3A => (1.0, 1.0, 1.0, 1.0, LegacyCorner::LowerRight),
        0x1CC3B => (3.0, 2.0, 2.0, 2.0, LegacyCorner::LowerRight),
        0x1CC3C => (0.0, 3.0, 2.0, 2.0, LegacyCorner::LowerLeft),
        0x1CC3D => (1.0, 3.0, 2.0, 2.0, LegacyCorner::LowerLeft),
        0x1CC3E => (2.0, 3.0, 2.0, 2.0, LegacyCorner::LowerRight),
        0x1CC3F => (3.0, 3.0, 2.0, 2.0, LegacyCorner::LowerRight),
        _ => return Vec::new(),
    };
    vec![supplement_circle_piece_command(
        rect, x, y, width, height, corner,
    )]
}

fn supplement_split_circle_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    match ch as u32 {
        0x1CE00 => vec![
            circle_arc_command(rect, LegacyCirclePosition::Left),
            circle_arc_command(rect, LegacyCirclePosition::Right),
        ],
        0x1CE01 => vec![
            circle_arc_command(rect, LegacyCirclePosition::Top),
            circle_arc_command(rect, LegacyCirclePosition::Bottom),
        ],
        _ => Vec::new(),
    }
}

fn supplement_ellipse_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let specs: &[(f32, f32, f32, f32, LegacyCorner)] = match ch as u32 {
        0x1CE0B => &[
            (0.0, 0.0, 1.0, 0.5, LegacyCorner::UpperLeft),
            (0.0, 0.0, 1.0, 0.5, LegacyCorner::LowerLeft),
        ],
        0x1CE0C => &[
            (1.0, 0.0, 1.0, 0.5, LegacyCorner::UpperRight),
            (1.0, 0.0, 1.0, 0.5, LegacyCorner::LowerRight),
        ],
        _ => return Vec::new(),
    };

    specs
        .iter()
        .map(|(x, y, width, height, corner)| {
            supplement_circle_piece_command(rect, *x, *y, *width, *height, *corner)
        })
        .collect()
}

fn supplement_circle_piece_command(
    rect: SurfaceRect,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    corner: LegacyCorner,
) -> SpriteCommand {
    let wdth = rect.width() * width;
    let hght = rect.height() * height;
    let xp = rect.width() * x;
    let yp = rect.height() * y;
    let c = (std::f32::consts::SQRT_2 - 1.0) * 4.0 / 3.0;
    let cw = c * wdth;
    let ch = c * hght;
    let ht = line_width(rect) * 0.5;
    let point = |px: f32, py: f32| SpritePoint::new(rect.min_x + px, rect.min_y + py);
    let mut points = match corner {
        LegacyCorner::UpperLeft => {
            let mut points = vec![point(wdth - xp, ht - yp)];
            sample_cubic(
                [
                    point(wdth - xp, ht - yp),
                    point(wdth - cw - xp, ht - yp),
                    point(ht - xp, hght - ch - yp),
                    point(ht - xp, hght - yp),
                ],
                &mut points,
            );
            points
        }
        LegacyCorner::UpperRight => {
            let mut points = vec![point(wdth - xp, ht - yp)];
            sample_cubic(
                [
                    point(wdth - xp, ht - yp),
                    point(wdth + cw - xp, ht - yp),
                    point(wdth * 2.0 - ht - xp, hght - ch - yp),
                    point(wdth * 2.0 - ht - xp, hght - yp),
                ],
                &mut points,
            );
            points
        }
        LegacyCorner::LowerLeft => {
            let mut points = vec![point(ht - xp, hght - yp)];
            sample_cubic(
                [
                    point(ht - xp, hght - yp),
                    point(ht - xp, hght + ch - yp),
                    point(wdth - cw - xp, hght * 2.0 - ht - yp),
                    point(wdth - xp, hght * 2.0 - ht - yp),
                ],
                &mut points,
            );
            points
        }
        LegacyCorner::LowerRight => {
            let mut points = vec![point(wdth * 2.0 - ht - xp, hght - yp)];
            sample_cubic(
                [
                    point(wdth * 2.0 - ht - xp, hght - yp),
                    point(wdth * 2.0 - ht - xp, hght + ch - yp),
                    point(wdth + cw - xp, hght * 2.0 - ht - yp),
                    point(wdth - xp, hght * 2.0 - ht - yp),
                ],
                &mut points,
            );
            points
        }
    };
    points.retain(|point| {
        point.x >= rect.min_x
            && point.x <= rect.max_x
            && point.y >= rect.min_y
            && point.y <= rect.max_y
    });
    SpriteCommand::StrokePolyline {
        points,
        width: line_width(rect),
        alpha: 1.0,
    }
}

fn supplement_horizontal_corner_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let width = line_width(rect);
    let center_y = rect.min_y + (rect.height() - width) * 0.5;
    let half_y = rect.min_y + rect.height() * 0.5;
    match ch {
        '\u{1CC1B}' => vec![
            fill_rect(rect.min_x, center_y, rect.width(), width),
            fill_rect(rect.max_x - width, rect.min_y, width, rect.height() * 0.5),
        ],
        '\u{1CC1C}' => vec![
            fill_rect(rect.min_x, center_y, rect.width(), width),
            fill_rect(rect.max_x - width, half_y, width, rect.height() * 0.5),
        ],
        '\u{1CC1D}' => vec![
            fill_rect(rect.min_x, rect.min_y, rect.width(), width),
            fill_rect(rect.min_x, rect.min_y, width, rect.height() * 0.5),
        ],
        '\u{1CC1E}' => vec![
            fill_rect(rect.min_x, rect.max_y - width, rect.width(), width),
            fill_rect(rect.min_x, half_y, width, rect.height() * 0.5),
        ],
        _ => Vec::new(),
    }
}

fn supplement_vertical_corner_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let width = line_width(rect);
    let center_x = rect.min_x + (rect.width() - width) * 0.5;
    let half_x = rect.min_x + rect.width() * 0.5;
    match ch {
        '\u{1CE16}' => vec![
            fill_rect(center_x, rect.min_y, width, rect.height()),
            fill_rect(half_x, rect.min_y, rect.width() * 0.5, width),
        ],
        '\u{1CE17}' => vec![
            fill_rect(center_x, rect.min_y, width, rect.height()),
            fill_rect(half_x, rect.max_y - width, rect.width() * 0.5, width),
        ],
        '\u{1CE18}' => vec![
            fill_rect(center_x, rect.min_y, width, rect.height()),
            fill_rect(rect.min_x, rect.min_y, rect.width() * 0.5, width),
        ],
        '\u{1CE19}' => vec![
            fill_rect(center_x, rect.min_y, width, rect.height()),
            fill_rect(rect.min_x, rect.max_y - width, rect.width() * 0.5, width),
        ],
        _ => Vec::new(),
    }
}

fn octant_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    sixel_grid_commands(OCTANT_PATTERNS[ch as usize - 0x1CD00], rect, 4, 2)
}

fn separated_quadrant_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let pattern = ((ch as u32) - 0x1CC20) as u8;
    let gap = (rect.width() / 12.0).floor().max(1.0);
    let mid_gap_x = gap * 2.0 + (rect.width().round() % 2.0);
    let mid_gap_y = gap * 2.0 + (rect.height().round() % 2.0);
    let quad_width = (rect.width() - gap * 2.0 - mid_gap_x) / 2.0;
    let quad_height = (rect.height() - gap * 2.0 - mid_gap_y) / 2.0;
    let positions = [
        (rect.min_x + gap, rect.min_y + gap),
        (rect.min_x + gap + quad_width + mid_gap_x, rect.min_y + gap),
        (rect.min_x + gap, rect.min_y + gap + quad_height + mid_gap_y),
        (
            rect.min_x + gap + quad_width + mid_gap_x,
            rect.min_y + gap + quad_height + mid_gap_y,
        ),
    ];

    positions
        .into_iter()
        .enumerate()
        .filter_map(|(bit, (x, y))| {
            if pattern & (1 << bit) == 0 {
                return None;
            }
            Some(SpriteCommand::FillRect {
                rect: SurfaceRect::from_min_size(x, y, quad_width, quad_height),
                alpha: 1.0,
            })
        })
        .collect()
}

fn separated_sextant_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let pattern = ((ch as u32) - 0x1CE50) as u8;
    let gap = (rect.width() / 12.0).floor().max(1.0);
    let mid_gap_x = gap * 2.0 + (rect.width().round() % 2.0);
    let y_extra = rect.height().round() % 3.0;
    let mid_gap_y = gap * 2.0 + (y_extra / 2.0).floor();
    let cell_width = (rect.width() - gap * 2.0 - mid_gap_x) / 2.0;
    let cell_height = ((rect.height() - gap * 2.0 - mid_gap_y * 2.0) / 3.0).floor();
    let middle_height = rect.height() - gap * 2.0 - mid_gap_y * 2.0 - cell_height * 2.0;
    let positions = [
        (rect.min_x + gap, rect.min_y + gap, cell_height),
        (
            rect.min_x + gap + cell_width + mid_gap_x,
            rect.min_y + gap,
            cell_height,
        ),
        (
            rect.min_x + gap,
            rect.min_y + gap + cell_height + mid_gap_y,
            middle_height,
        ),
        (
            rect.min_x + gap + cell_width + mid_gap_x,
            rect.min_y + gap + cell_height + mid_gap_y,
            middle_height,
        ),
        (
            rect.min_x + gap,
            rect.min_y + gap + cell_height + mid_gap_y + middle_height + mid_gap_y,
            cell_height,
        ),
        (
            rect.min_x + gap + cell_width + mid_gap_x,
            rect.min_y + gap + cell_height + mid_gap_y + middle_height + mid_gap_y,
            cell_height,
        ),
    ];

    positions
        .into_iter()
        .enumerate()
        .filter_map(|(bit, (x, y, height))| {
            if pattern & (1 << bit) == 0 {
                return None;
            }
            Some(SpriteCommand::FillRect {
                rect: SurfaceRect::from_min_size(x, y, cell_width, height),
                alpha: 1.0,
            })
        })
        .collect()
}

fn sixteenth_block_commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let q = |slot: u8, total: f32| total * f32::from(slot) / 4.0;
    let fill_quarters = |left: u8, right: u8, top: u8, bottom: u8| SpriteCommand::FillRect {
        rect: SurfaceRect::from_min_size(
            rect.min_x + q(left, rect.width()),
            rect.min_y + q(top, rect.height()),
            q(right - left, rect.width()),
            q(bottom - top, rect.height()),
        ),
        alpha: 1.0,
    };

    let cp = ch as u32;
    if (0x1CE90..=0x1CE9F).contains(&cp) {
        let index = (cp - 0x1CE90) as u8;
        let row = index / 4;
        let col = index % 4;
        return vec![fill_quarters(col, col + 1, row, row + 1)];
    }

    let spec = match cp {
        0x1CEA0 => (2, 4, 3, 4),
        0x1CEA1 => (1, 4, 3, 4),
        0x1CEA2 => (0, 3, 3, 4),
        0x1CEA3 => (0, 2, 3, 4),
        0x1CEA4 => (0, 1, 2, 4),
        0x1CEA5 => (0, 1, 1, 4),
        0x1CEA6 => (0, 1, 0, 3),
        0x1CEA7 => (0, 1, 0, 2),
        0x1CEA8 => (0, 2, 0, 1),
        0x1CEA9 => (0, 3, 0, 1),
        0x1CEAA => (1, 4, 0, 1),
        0x1CEAB => (2, 4, 0, 1),
        0x1CEAC => (3, 4, 0, 2),
        0x1CEAD => (3, 4, 0, 3),
        0x1CEAE => (3, 4, 1, 4),
        0x1CEAF => (3, 4, 2, 4),
        _ => return Vec::new(),
    };

    vec![fill_quarters(spec.0, spec.1, spec.2, spec.3)]
}

fn sixel_grid_commands(pattern: u8, rect: SurfaceRect, rows: u8, cols: u8) -> Vec<SpriteCommand> {
    let cell_width = rect.width() / f32::from(cols);
    let cell_height = rect.height() / f32::from(rows);
    let mut commands = Vec::new();

    for row in 0..rows {
        for col in 0..cols {
            let bit = row * cols + col;
            if pattern & (1 << bit) == 0 {
                continue;
            }
            commands.push(SpriteCommand::FillRect {
                rect: SurfaceRect::from_min_size(
                    rect.min_x + f32::from(col) * cell_width,
                    rect.min_y + f32::from(row) * cell_height,
                    cell_width,
                    cell_height,
                ),
                alpha: 1.0,
            });
        }
    }

    commands
}

fn fill_rect(x: f32, y: f32, width: f32, height: f32) -> SpriteCommand {
    SpriteCommand::FillRect {
        rect: SurfaceRect::from_min_size(x, y, width, height),
        alpha: 1.0,
    }
}

fn fill_eighth_columns(rect: SurfaceRect, start: u8, end: u8) -> SpriteCommand {
    let column_width = rect.width() / 8.0;
    SpriteCommand::FillRect {
        rect: SurfaceRect::from_min_size(
            rect.min_x + f32::from(start) * column_width,
            rect.min_y,
            f32::from(end - start) * column_width,
            rect.height(),
        ),
        alpha: 1.0,
    }
}

fn fill_eighth_rows(rect: SurfaceRect, start: u8, end: u8) -> SpriteCommand {
    let row_height = rect.height() / 8.0;
    SpriteCommand::FillRect {
        rect: SurfaceRect::from_min_size(
            rect.min_x,
            rect.min_y + f32::from(start) * row_height,
            rect.width(),
            f32::from(end - start) * row_height,
        ),
        alpha: 1.0,
    }
}

fn shade_eighth_columns(rect: SurfaceRect, start: u8, end: u8, alpha: f32) -> SpriteCommand {
    let column_width = rect.width() / 8.0;
    SpriteCommand::FillRect {
        rect: SurfaceRect::from_min_size(
            rect.min_x + f32::from(start) * column_width,
            rect.min_y,
            f32::from(end - start) * column_width,
            rect.height(),
        ),
        alpha,
    }
}

fn shade_eighth_rows(rect: SurfaceRect, start: u8, end: u8, alpha: f32) -> SpriteCommand {
    let row_height = rect.height() / 8.0;
    SpriteCommand::FillRect {
        rect: SurfaceRect::from_min_size(
            rect.min_x,
            rect.min_y + f32::from(start) * row_height,
            rect.width(),
            f32::from(end - start) * row_height,
        ),
        alpha,
    }
}

fn shade_rect(rect: SurfaceRect, alpha: f32) -> SpriteCommand {
    SpriteCommand::FillRect { rect, alpha }
}

fn checkerboard_commands(rect: SurfaceRect, parity: usize) -> Vec<SpriteCommand> {
    let x_cells = 4usize;
    let y_cells = (4.0 * (rect.height() / rect.width())).round().max(1.0) as usize;
    let cell_width = rect.width() / x_cells as f32;
    let cell_height = rect.height() / y_cells as f32;
    let mut commands = Vec::new();

    for x in 0..x_cells {
        for y in 0..y_cells {
            if (x + y) % 2 != parity {
                continue;
            }
            commands.push(SpriteCommand::FillRect {
                rect: SurfaceRect::from_min_size(
                    rect.min_x + x as f32 * cell_width,
                    rect.min_y + y as f32 * cell_height,
                    cell_width,
                    cell_height,
                ),
                alpha: 1.0,
            });
        }
    }

    commands
}

const OCTANT_PATTERNS: [u8; 230] = [
    0x04, 0x06, 0x07, 0x08, 0x09, 0x0B, 0x0C, 0x0D, 0x0E, 0x10, 0x11, 0x12, 0x13, 0x15, 0x16, 0x17,
    0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27,
    0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38,
    0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A,
    0x4B, 0x4C, 0x4D, 0x4E, 0x4F, 0x51, 0x52, 0x53, 0x54, 0x56, 0x57, 0x58, 0x59, 0x5B, 0x5C, 0x5D,
    0x5E, 0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E,
    0x6F, 0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x7B, 0x7C, 0x7D, 0x7E,
    0x7F, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x8B, 0x8C, 0x8D, 0x8E, 0x8F,
    0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0x9B, 0x9C, 0x9D, 0x9E, 0x9F,
    0xA1, 0xA2, 0xA3, 0xA4, 0xA6, 0xA7, 0xA8, 0xA9, 0xAB, 0xAC, 0xAD, 0xAE, 0xB0, 0xB1, 0xB2, 0xB3,
    0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xBB, 0xBC, 0xBD, 0xBE, 0xBF, 0xC1, 0xC2, 0xC3, 0xC4,
    0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xCB, 0xCC, 0xCD, 0xCE, 0xCF, 0xD0, 0xD1, 0xD2, 0xD3, 0xD4,
    0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xDB, 0xDC, 0xDD, 0xDE, 0xDF, 0xE0, 0xE1, 0xE2, 0xE3, 0xE4,
    0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xEB, 0xEC, 0xED, 0xEE, 0xEF, 0xF1, 0xF2, 0xF3, 0xF4, 0xF6,
    0xF7, 0xF8, 0xF9, 0xFB, 0xFD, 0xFE,
];

const SMOOTH_MOSAIC_PATTERNS: [&[u8; 12]; 44] = [
    b"......#..##.",
    b"......#\\.###",
    b"...#..#\\.##.",
    b"...#..##.###",
    b"#..#..##.##.",
    b"/###########",
    b"./##########",
    b".##.########",
    b"..#.########",
    b".##.##.#####",
    b"..../#######",
    b"........#.##",
    b"......./####",
    b".....#./#.##",
    b".....#.#####",
    b"..#..#.##.##",
    b"##\\#########",
    b"#\\.#########",
    b"##.##.######",
    b"#..##.######",
    b"##.##.##.###",
    b"...#\\.######",
    b"#########\\##",
    b"#########.\\#",
    b"######.##.##",
    b"######.##..#",
    b"###.##.##.##",
    b"##.#........",
    b"####/.......",
    b"##.#/.#.....",
    b"#####.#.....",
    b"##.##.#..#..",
    b"#######/....",
    b"###########/",
    b"##########/.",
    b"########.##.",
    b"########.#..",
    b"#####.##.##.",
    b".##..#......",
    b"###.\\#......",
    b".##.\\#..#...",
    b"###.##..#...",
    b".##.##..#..#",
    b"######.\\#...",
];

fn placeholder_commands(rect: SurfaceRect) -> Vec<SpriteCommand> {
    vec![SpriteCommand::FillRect { rect, alpha: 1.0 }]
}

fn triangle_commands(a: SpritePoint, b: SpritePoint, c: SpritePoint) -> Vec<SpriteCommand> {
    vec![filled_triangle([a, b, c])]
}

fn stroke_commands(pairs: &[(SpritePoint, SpritePoint)], rect: SurfaceRect) -> Vec<SpriteCommand> {
    pairs
        .iter()
        .map(|(start, end)| stroke_polyline(vec![*start, *end], rect))
        .collect()
}

fn filled_triangle(points: [SpritePoint; 3]) -> SpriteCommand {
    SpriteCommand::FillPolygon {
        shape: SpriteShape::Triangle,
        points: points.to_vec(),
        alpha: 1.0,
    }
}

fn filled_polygon(points: Vec<SpritePoint>) -> SpriteCommand {
    SpriteCommand::FillPolygon {
        shape: SpriteShape::Polygon,
        points,
        alpha: 1.0,
    }
}

fn stroke_polyline(points: Vec<SpritePoint>, rect: SurfaceRect) -> SpriteCommand {
    SpriteCommand::StrokePolyline {
        points,
        width: soft_powerline_width(rect),
        alpha: 1.0,
    }
}

fn clear_stroke_polyline(points: Vec<SpritePoint>, rect: SurfaceRect) -> SpriteCommand {
    SpriteCommand::ClearStrokePolyline {
        points,
        width: line_width(rect),
        alpha: 1.0,
    }
}

fn block_rect(rect: SurfaceRect, row: u8, col: u8, rows: u8, cols: u8) -> SurfaceRect {
    let eighth_w = rect.width() / 8.0;
    let eighth_h = rect.height() / 8.0;
    SurfaceRect::from_min_size(
        rect.min_x + f32::from(col) * eighth_w,
        rect.min_y + f32::from(row) * eighth_h,
        f32::from(cols) * eighth_w,
        f32::from(rows) * eighth_h,
    )
}

fn fill_block_rect(rect: SurfaceRect, row: u8, col: u8, rows: u8, cols: u8) -> SpriteCommand {
    SpriteCommand::FillRect {
        rect: block_rect(rect, row, col, rows, cols),
        alpha: 1.0,
    }
}

fn line_width(rect: SurfaceRect) -> f32 {
    (rect.width().min(rect.height()) / 8.0)
        .round()
        .clamp(1.0, 2.0)
}

fn heavy_line_width(rect: SurfaceRect) -> f32 {
    (line_width(rect) * 2.0).clamp(2.0, 4.0)
}

fn soft_powerline_width(rect: SurfaceRect) -> f32 {
    line_width(rect).max(1.5)
}

fn right_round_points(rect: SurfaceRect) -> Vec<SpritePoint> {
    let radius = rect.width().min(rect.height() * 0.5);
    let c = (std::f32::consts::SQRT_2 - 1.0) * 4.0 / 3.0;
    let x0 = rect.min_x;
    let y0 = rect.min_y;
    let y1 = rect.max_y;
    let r = radius;
    let mut points = Vec::with_capacity(18);
    points.push(SpritePoint::new(x0, y0));
    sample_cubic(
        [
            SpritePoint::new(x0, y0),
            SpritePoint::new(x0 + r * c, y0),
            SpritePoint::new(x0 + r, y0 + r - r * c),
            SpritePoint::new(x0 + r, y0 + r),
        ],
        &mut points,
    );
    points.push(SpritePoint::new(x0 + r, y1 - r));
    sample_cubic(
        [
            SpritePoint::new(x0 + r, y1 - r),
            SpritePoint::new(x0 + r, y1 - r + r * c),
            SpritePoint::new(x0 + r * c, y1),
            SpritePoint::new(x0, y1),
        ],
        &mut points,
    );
    points
}

fn sample_cubic(points: [SpritePoint; 4], out: &mut Vec<SpritePoint>) {
    for step in 1..=8 {
        let t = step as f32 / 8.0;
        let mt = 1.0 - t;
        out.push(SpritePoint::new(
            mt.powi(3) * points[0].x
                + 3.0 * mt.powi(2) * t * points[1].x
                + 3.0 * mt * t.powi(2) * points[2].x
                + t.powi(3) * points[3].x,
            mt.powi(3) * points[0].y
                + 3.0 * mt.powi(2) * t * points[1].y
                + 3.0 * mt * t.powi(2) * points[2].y
                + t.powi(3) * points[3].y,
        ));
    }
}

fn flip_horizontal(points: &[SpritePoint], rect: SurfaceRect) -> Vec<SpritePoint> {
    points
        .iter()
        .map(|point| SpritePoint::new(rect.min_x + rect.max_x - point.x, point.y))
        .collect()
}

fn left_top(rect: SurfaceRect) -> SpritePoint {
    SpritePoint::new(rect.min_x, rect.min_y)
}

fn left_bottom(rect: SurfaceRect) -> SpritePoint {
    SpritePoint::new(rect.min_x, rect.max_y)
}

fn right_top(rect: SurfaceRect) -> SpritePoint {
    SpritePoint::new(rect.max_x, rect.min_y)
}

fn right_bottom(rect: SurfaceRect) -> SpritePoint {
    SpritePoint::new(rect.max_x, rect.max_y)
}

fn center_y(rect: SurfaceRect) -> f32 {
    rect.min_y + rect.height() * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_separator_points_stay_inside_cell() {
        let rect = SurfaceRect::from_min_size(10.0, 20.0, 8.0, 24.0);
        let points = right_round_points(rect);

        assert_eq!(points.first().copied(), Some(left_top(rect)));
        assert_eq!(points.last().copied(), Some(left_bottom(rect)));
        assert!(points.iter().all(|point| point.x >= rect.min_x));
        assert!(points.iter().all(|point| point.x <= rect.max_x));
        assert!(points.iter().any(|point| point.x == rect.max_x));
    }

    #[test]
    fn block_commands_use_eighth_cell_fractions() {
        let rect = SurfaceRect::from_min_size(10.0, 20.0, 16.0, 24.0);
        let commands = block_commands('▂', rect);

        assert_eq!(
            commands,
            vec![SpriteCommand::FillRect {
                rect: SurfaceRect::from_min_size(10.0, 38.0, 16.0, 6.0),
                alpha: 1.0,
            }]
        );
    }
}
