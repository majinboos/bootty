use crate::geometry::SurfaceRect;
use crate::terminal_sprite::SpriteCommand;
use crate::terminal_sprite::families::primitives::{fill_rect, placeholder_commands};

pub(super) fn commands_for(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let dots = ch as u32 - 0x2800;
    if dots == 0 {
        return Vec::new();
    }
    let mut commands = Vec::with_capacity(8);
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
            commands.push(fill_rect(
                rect.min_x + layout.x[col],
                rect.min_y + layout.y[row],
                layout.dot_width,
                layout.dot_width,
            ));
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
