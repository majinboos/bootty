use crate::geometry::SurfaceRect;
use crate::terminal_sprite::{SpriteCommand, SpritePoint, SpritePoints, SpriteShape};

pub(super) fn placeholder_commands(rect: SurfaceRect) -> Vec<SpriteCommand> {
    vec![SpriteCommand::FillRect { rect, alpha: 1.0 }]
}

pub(super) fn sixel_grid_commands(
    pattern: u8,
    rect: SurfaceRect,
    rows: u8,
    cols: u8,
) -> Vec<SpriteCommand> {
    let cell_width = rect.width() / f32::from(cols);
    let cell_height = rect.height() / f32::from(rows);
    let mut commands = Vec::with_capacity(usize::from(rows) * usize::from(cols));

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

pub(super) fn fill_rect(x: f32, y: f32, width: f32, height: f32) -> SpriteCommand {
    SpriteCommand::FillRect {
        rect: SurfaceRect::from_min_size(x, y, width, height),
        alpha: 1.0,
    }
}

pub(super) fn triangle_commands(
    a: SpritePoint,
    b: SpritePoint,
    c: SpritePoint,
) -> Vec<SpriteCommand> {
    vec![filled_triangle([a, b, c])]
}

pub(super) fn stroke_commands(
    pairs: &[(SpritePoint, SpritePoint)],
    rect: SurfaceRect,
) -> Vec<SpriteCommand> {
    let mut commands = Vec::with_capacity(pairs.len());
    for (start, end) in pairs {
        commands.push(stroke_segment(*start, *end, rect));
    }
    commands
}

pub(super) fn filled_triangle(points: [SpritePoint; 3]) -> SpriteCommand {
    SpriteCommand::FillPolygon {
        shape: SpriteShape::Triangle,
        points: points_from_array(points),
        alpha: 1.0,
    }
}

pub(super) fn filled_polygon(points: Vec<SpritePoint>) -> SpriteCommand {
    SpriteCommand::FillPolygon {
        shape: SpriteShape::Polygon,
        points: points_from_vec(points),
        alpha: 1.0,
    }
}

pub(super) fn stroke_polyline(points: Vec<SpritePoint>, rect: SurfaceRect) -> SpriteCommand {
    SpriteCommand::StrokePolyline {
        points: points_from_vec(points),
        width: soft_powerline_width(rect),
        alpha: 1.0,
    }
}

pub(super) fn stroke_segment(
    start: SpritePoint,
    end: SpritePoint,
    rect: SurfaceRect,
) -> SpriteCommand {
    SpriteCommand::StrokePolyline {
        points: points_from_array([start, end]),
        width: soft_powerline_width(rect),
        alpha: 1.0,
    }
}

pub(super) fn clear_stroke_segment(
    start: SpritePoint,
    end: SpritePoint,
    rect: SurfaceRect,
) -> SpriteCommand {
    SpriteCommand::ClearStrokePolyline {
        points: points_from_array([start, end]),
        width: line_width(rect),
        alpha: 1.0,
    }
}

pub(super) fn points_from_array<const N: usize>(points: [SpritePoint; N]) -> SpritePoints {
    points.into_iter().collect()
}

pub(super) fn points_from_vec(points: Vec<SpritePoint>) -> SpritePoints {
    SpritePoints::from_vec(points)
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

pub(super) fn fill_block_rect(
    rect: SurfaceRect,
    row: u8,
    col: u8,
    rows: u8,
    cols: u8,
) -> SpriteCommand {
    SpriteCommand::FillRect {
        rect: block_rect(rect, row, col, rows, cols),
        alpha: 1.0,
    }
}

pub(super) fn line_width(rect: SurfaceRect) -> f32 {
    (rect.width().min(rect.height()) / 8.0)
        .round()
        .clamp(1.0, 2.0)
}

pub(super) fn heavy_line_width(rect: SurfaceRect) -> f32 {
    (line_width(rect) * 2.0).clamp(2.0, 4.0)
}

pub(super) fn soft_powerline_width(rect: SurfaceRect) -> f32 {
    line_width(rect).max(1.5)
}

pub(super) fn right_round_points(rect: SurfaceRect) -> Vec<SpritePoint> {
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

pub(super) fn sample_cubic(points: [SpritePoint; 4], out: &mut Vec<SpritePoint>) {
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

pub(super) fn flip_horizontal(points: &[SpritePoint], rect: SurfaceRect) -> Vec<SpritePoint> {
    points
        .iter()
        .map(|point| SpritePoint::new(rect.min_x + rect.max_x - point.x, point.y))
        .collect()
}

pub(super) fn left_top(rect: SurfaceRect) -> SpritePoint {
    SpritePoint::new(rect.min_x, rect.min_y)
}

pub(super) fn left_bottom(rect: SurfaceRect) -> SpritePoint {
    SpritePoint::new(rect.min_x, rect.max_y)
}

pub(super) fn right_top(rect: SurfaceRect) -> SpritePoint {
    SpritePoint::new(rect.max_x, rect.min_y)
}

pub(super) fn right_bottom(rect: SurfaceRect) -> SpritePoint {
    SpritePoint::new(rect.max_x, rect.max_y)
}

pub(super) fn center_y(rect: SurfaceRect) -> f32 {
    rect.min_y + rect.height() * 0.5
}
