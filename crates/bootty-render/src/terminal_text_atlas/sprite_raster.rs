use crate::{geometry::SurfaceRect, terminal_sprite::SpriteCommand};

pub(super) fn rasterize_sprite_commands(
    commands: &[SpriteCommand],
    rect: SurfaceRect,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let mut alpha = vec![0; (width * height) as usize];
    for command in commands {
        match command {
            SpriteCommand::FillRect {
                rect: fill,
                alpha: coverage,
            } => {
                fill_mask_rect(&mut alpha, rect, *fill, width, height, *coverage);
            }
            SpriteCommand::FillPolygon {
                points,
                alpha: coverage,
                ..
            } => {
                fill_mask_polygon(&mut alpha, rect, points, width, height, *coverage);
            }
            SpriteCommand::StrokePolyline {
                points,
                width: stroke_width,
                alpha: coverage,
            } => {
                for pair in points.windows(2) {
                    fill_mask_stroke_segment(
                        &mut alpha,
                        rect,
                        pair[0],
                        pair[1],
                        *stroke_width,
                        (width, height),
                        *coverage,
                    );
                }
            }
            SpriteCommand::ClearStrokePolyline {
                points,
                width: stroke_width,
                alpha: coverage,
            } => {
                for pair in points.windows(2) {
                    clear_mask_stroke_segment(
                        &mut alpha,
                        rect,
                        pair[0],
                        pair[1],
                        *stroke_width,
                        (width, height),
                        *coverage,
                    );
                }
            }
        }
    }
    alpha
}

fn fill_mask_rect(
    pixels: &mut [u8],
    cell: SurfaceRect,
    fill: SurfaceRect,
    width: u32,
    height: u32,
    coverage: f32,
) {
    let min_x = (((fill.min_x - cell.min_x) / cell.width().max(1.0)) * width as f32)
        .floor()
        .clamp(0.0, width as f32) as u32;
    let max_x = (((fill.max_x - cell.min_x) / cell.width().max(1.0)) * width as f32)
        .ceil()
        .clamp(0.0, width as f32) as u32;
    let min_y = (((fill.min_y - cell.min_y) / cell.height().max(1.0)) * height as f32)
        .floor()
        .clamp(0.0, height as f32) as u32;
    let max_y = (((fill.max_y - cell.min_y) / cell.height().max(1.0)) * height as f32)
        .ceil()
        .clamp(0.0, height as f32) as u32;
    let value = (coverage.clamp(0.0, 1.0) * 255.0).round() as u8;
    for y in min_y..max_y {
        for x in min_x..max_x {
            if let Some(dst) = pixels.get_mut((y * width + x) as usize) {
                *dst = (*dst).max(value);
            }
        }
    }
}

fn fill_mask_polygon(
    pixels: &mut [u8],
    cell: SurfaceRect,
    points: &[crate::terminal_sprite::SpritePoint],
    width: u32,
    height: u32,
    coverage: f32,
) {
    if points.len() < 3 {
        return;
    }
    let value = (coverage.clamp(0.0, 1.0) * 255.0).round() as u8;
    for y in 0..height {
        for x in 0..width {
            let px = cell.min_x + ((x as f32 + 0.5) / width as f32) * cell.width();
            let py = cell.min_y + ((y as f32 + 0.5) / height as f32) * cell.height();
            if point_in_polygon(px, py, points)
                && let Some(dst) = pixels.get_mut((y * width + x) as usize)
            {
                *dst = (*dst).max(value);
            }
        }
    }
}

fn fill_mask_stroke_segment(
    pixels: &mut [u8],
    cell: SurfaceRect,
    start: crate::terminal_sprite::SpritePoint,
    end: crate::terminal_sprite::SpritePoint,
    stroke_width: f32,
    size: (u32, u32),
    coverage: f32,
) {
    let (width, height) = size;
    let value = (coverage.clamp(0.0, 1.0) * 255.0).round() as u8;
    for y in 0..height {
        for x in 0..width {
            let px = cell.min_x + ((x as f32 + 0.5) / width as f32) * cell.width();
            let py = cell.min_y + ((y as f32 + 0.5) / height as f32) * cell.height();
            if distance_to_segment(px, py, start, end) <= stroke_width * 0.5
                && let Some(dst) = pixels.get_mut((y * width + x) as usize)
            {
                *dst = (*dst).max(value);
            }
        }
    }
}

fn clear_mask_stroke_segment(
    pixels: &mut [u8],
    cell: SurfaceRect,
    start: crate::terminal_sprite::SpritePoint,
    end: crate::terminal_sprite::SpritePoint,
    stroke_width: f32,
    size: (u32, u32),
    coverage: f32,
) {
    let (width, height) = size;
    let value = ((1.0 - coverage.clamp(0.0, 1.0)) * 255.0).round() as u8;
    for y in 0..height {
        for x in 0..width {
            let px = cell.min_x + ((x as f32 + 0.5) / width as f32) * cell.width();
            let py = cell.min_y + ((y as f32 + 0.5) / height as f32) * cell.height();
            if distance_to_segment(px, py, start, end) <= stroke_width * 0.5
                && let Some(dst) = pixels.get_mut((y * width + x) as usize)
            {
                *dst = (*dst).min(value);
            }
        }
    }
}

fn point_in_polygon(x: f32, y: f32, points: &[crate::terminal_sprite::SpritePoint]) -> bool {
    let mut inside = false;
    let mut previous = points.len() - 1;
    for current in 0..points.len() {
        let current_point = points[current];
        let previous_point = points[previous];
        if ((current_point.y > y) != (previous_point.y > y))
            && (x
                < (previous_point.x - current_point.x) * (y - current_point.y)
                    / (previous_point.y - current_point.y)
                    + current_point.x)
        {
            inside = !inside;
        }
        previous = current;
    }
    inside
}

fn distance_to_segment(
    x: f32,
    y: f32,
    start: crate::terminal_sprite::SpritePoint,
    end: crate::terminal_sprite::SpritePoint,
) -> f32 {
    let vx = end.x - start.x;
    let vy = end.y - start.y;
    let wx = x - start.x;
    let wy = y - start.y;
    let len_squared = vx * vx + vy * vy;
    if len_squared <= f32::EPSILON {
        return ((x - start.x).powi(2) + (y - start.y).powi(2)).sqrt();
    }
    let t = ((wx * vx + wy * vy) / len_squared).clamp(0.0, 1.0);
    let proj_x = start.x + t * vx;
    let proj_y = start.y + t * vy;
    ((x - proj_x).powi(2) + (y - proj_y).powi(2)).sqrt()
}
