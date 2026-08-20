use crate::geometry::SurfaceRect;
use crate::terminal_sprite::families::legacy_computing::{
    LegacyCirclePosition, LegacyCorner, circle_arc_command,
};
use crate::terminal_sprite::families::primitives::{
    fill_rect, line_width, placeholder_commands, points_from_vec, sample_cubic, sixel_grid_commands,
};
use crate::terminal_sprite::{SpriteCommand, SpritePoint};

pub(super) fn commands_for(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
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
        points: points_from_vec(points),
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
