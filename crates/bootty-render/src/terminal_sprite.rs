mod families;

use crate::geometry::SurfaceRect;
use smallvec::SmallVec;

impl SpriteGlyph {
    pub fn from_char(ch: char) -> Option<Self> {
        families::family_for(ch).map(|family| SpriteGlyph { ch, family })
    }

    pub fn commands_for(self, rect: SurfaceRect) -> Vec<SpriteCommand> {
        families::commands_for(self, rect)
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
        points: SpritePoints,
        alpha: f32,
    },
    StrokePolyline {
        points: SpritePoints,
        width: f32,
        alpha: f32,
    },
    ClearStrokePolyline {
        points: SpritePoints,
        width: f32,
        alpha: f32,
    },
}

pub type SpritePoints = SmallVec<[SpritePoint; 4]>;

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
