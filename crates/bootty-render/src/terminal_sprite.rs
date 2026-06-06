mod families;

use crate::{geometry::SurfaceRect, paint_plan::PlanColor};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SpriteRegistry;

impl SpriteRegistry {
    pub fn prompt_graphics() -> Self {
        Self
    }

    pub fn glyph_for(self, ch: char) -> Option<SpriteGlyph> {
        families::family_for(ch).map(|family| SpriteGlyph { ch, family })
    }

    pub fn owns(self, ch: char) -> bool {
        self.glyph_for(ch).is_some()
    }

    pub fn commands_for(self, glyph: SpriteGlyph, rect: SurfaceRect) -> Vec<SpriteCommand> {
        families::commands_for(glyph, rect)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SpriteGlyph {
    pub ch: char,
    pub family: SpriteFamily,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpriteFamily {
    Powerline,
    ProgressIndicator,
    Separator,
    Block,
    Shade,
    BoxDrawing,
    Braille,
    LegacyComputing,
    LegacyComputingSupplement,
    Special,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SpriteCommand {
    FillRect {
        rect: SurfaceRect,
        alpha: f32,
    },
    FillPolygon {
        shape: SpriteShape,
        points: Vec<SpritePoint>,
        alpha: f32,
    },
    StrokePolyline {
        points: Vec<SpritePoint>,
        width: f32,
        alpha: f32,
    },
    ClearStrokePolyline {
        points: Vec<SpritePoint>,
        width: f32,
        alpha: f32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpriteShape {
    Triangle,
    Polygon,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpritePoint {
    pub x: f32,
    pub y: f32,
}

impl SpritePoint {
    pub(super) fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WgpuSpritePrimitives {
    pub vertices: Vec<WgpuSpriteVertex>,
    pub indices: Vec<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WgpuSpriteVertex {
    pub position: [f32; 2],
    pub color: [u8; 4],
}

pub struct WgpuSpriteBackend;

impl WgpuSpriteBackend {
    pub fn build_primitives(commands: &[SpriteCommand], color: PlanColor) -> WgpuSpritePrimitives {
        let mut primitives = WgpuSpritePrimitives::default();
        for command in commands {
            match command {
                SpriteCommand::FillRect { rect, alpha } => {
                    push_rect(&mut primitives, *rect, color_with_alpha(color, *alpha));
                }
                SpriteCommand::FillPolygon { points, alpha, .. } => {
                    push_polygon(&mut primitives, points, color_with_alpha(color, *alpha));
                }
                SpriteCommand::StrokePolyline {
                    points,
                    width,
                    alpha,
                } => {
                    push_polyline(
                        &mut primitives,
                        points,
                        *width,
                        color_with_alpha(color, *alpha),
                    );
                }
                SpriteCommand::ClearStrokePolyline { .. } => {}
            }
        }
        primitives
    }
}

fn push_rect(primitives: &mut WgpuSpritePrimitives, rect: SurfaceRect, color: [u8; 4]) {
    push_polygon(
        primitives,
        &[
            SpritePoint::new(rect.min_x, rect.min_y),
            SpritePoint::new(rect.max_x, rect.min_y),
            SpritePoint::new(rect.max_x, rect.max_y),
            SpritePoint::new(rect.min_x, rect.max_y),
        ],
        color,
    );
}

fn push_polygon(primitives: &mut WgpuSpritePrimitives, points: &[SpritePoint], color: [u8; 4]) {
    if points.len() < 3 {
        return;
    }
    let start = primitives.vertices.len() as u32;
    primitives
        .vertices
        .extend(points.iter().map(|point| WgpuSpriteVertex {
            position: [point.x, point.y],
            color,
        }));
    for offset in 1..(points.len() as u32 - 1) {
        primitives
            .indices
            .extend([start, start + offset, start + offset + 1]);
    }
}

fn push_polyline(
    primitives: &mut WgpuSpritePrimitives,
    points: &[SpritePoint],
    width: f32,
    color: [u8; 4],
) {
    for segment in points.windows(2) {
        push_stroke_segment(primitives, segment[0], segment[1], width, color);
    }
}

fn push_stroke_segment(
    primitives: &mut WgpuSpritePrimitives,
    start: SpritePoint,
    end: SpritePoint,
    width: f32,
    color: [u8; 4],
) {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len <= f32::EPSILON {
        return;
    }
    let half = width * 0.5;
    let nx = -dy / len * half;
    let ny = dx / len * half;
    push_polygon(
        primitives,
        &[
            SpritePoint::new(start.x + nx, start.y + ny),
            SpritePoint::new(end.x + nx, end.y + ny),
            SpritePoint::new(end.x - nx, end.y - ny),
            SpritePoint::new(start.x - nx, start.y - ny),
        ],
        color,
    );
}

fn color_with_alpha(color: PlanColor, alpha: f32) -> [u8; 4] {
    [
        color.r,
        color.g,
        color.b,
        ((f32::from(color.a) * alpha).round()).clamp(0.0, 255.0) as u8,
    ]
}
