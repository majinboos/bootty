use crate::geometry::SurfaceRect;
use crate::terminal_sprite::families::primitives::{
    fill_rect, heavy_line_width, line_width, placeholder_commands, points_from_array,
    points_from_vec, sample_cubic,
};
use crate::terminal_sprite::{SpriteCommand, SpritePoint};

pub(super) fn commands_for(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
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
    let mut commands = Vec::with_capacity(usize::from(dashes.count));

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
    let mut commands = Vec::with_capacity(usize::from(dashes.count));

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
    let mut commands = Vec::with_capacity(2);

    if diagonals.upper_right_to_lower_left {
        commands.push(SpriteCommand::StrokePolyline {
            points: points_from_array([
                SpritePoint::new(rect.max_x + 0.5 * slope_x, rect.min_y - 0.5 * slope_y),
                SpritePoint::new(rect.min_x - 0.5 * slope_x, rect.max_y + 0.5 * slope_y),
            ]),
            width: line_width(rect),
            alpha: 1.0,
        });
    }
    if diagonals.upper_left_to_lower_right {
        commands.push(SpriteCommand::StrokePolyline {
            points: points_from_array([
                SpritePoint::new(rect.min_x - 0.5 * slope_x, rect.min_y - 0.5 * slope_y),
                SpritePoint::new(rect.max_x + 0.5 * slope_x, rect.max_y + 0.5 * slope_y),
            ]),
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
        points: points_from_vec(points),
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

    let mut commands = Vec::with_capacity(8);
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
