//! The drawable-item schema: what a module publishes and a widget paints.
//!
//! Its own crate because both sides need it and neither should depend on the other — the Luau
//! runtime has no business pulling in a rendering stack, and the widget library has no business
//! knowing about extensions.

/// An opaque RGBA color for extension-owned values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModuleColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl ModuleColor {
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ModuleCoord {
    pub frac: f32,
    pub px: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModuleCornerRadius {
    pub nw: u8,
    pub ne: u8,
    pub sw: u8,
    pub se: u8,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModuleItem {
    pub text: String,
    pub fg: Option<ModuleColor>,
    pub bg: Option<ModuleColor>,
    pub stroke: Option<ModuleColor>,
    pub icon: Option<String>,
    pub gauge: Option<f32>,
    pub primitives: Vec<ModulePrimitive>,
    pub pad_left: f32,
    pub pad_right: f32,
    pub join: Option<bool>,
    pub gap: Option<bool>,
    pub action: Option<String>,
    pub key: Option<String>,
    pub kind: Option<String>,
    pub number: Option<usize>,
    pub indent: Option<u16>,
    pub tree: Option<String>,
    pub selectable: Option<bool>,
    pub session_id: Option<String>,
    pub reorder_anchor: Option<String>,
    pub current: Option<bool>,
    pub active: Option<bool>,
    pub dim_fg: Option<ModuleColor>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ModulePrimitive {
    Rect {
        fill: Option<ModuleColor>,
        stroke: Option<ModuleColor>,
        x: ModuleCoord,
        y: ModuleCoord,
        w: ModuleCoord,
        h: ModuleCoord,
        radius: ModuleCornerRadius,
        /// Sweep this rect back and forth across the space its width leaves free, ignoring `x`. The
        /// painter drives it off the frame clock, so an indeterminate bar animates at the frame rate
        /// instead of at the producing module's render interval.
        sweep: bool,
    },
    Polygon {
        fill: Option<ModuleColor>,
        stroke: Option<ModuleColor>,
        points: Vec<(ModuleCoord, ModuleCoord)>,
    },
    Text {
        text: String,
        color: Option<ModuleColor>,
        x: ModuleCoord,
        y: ModuleCoord,
        size: f32,
        align: String,
        min_width: Option<f32>,
    },
    Icon {
        icon: String,
        color: Option<ModuleColor>,
        x: ModuleCoord,
        y: ModuleCoord,
        size: f32,
        min_width: Option<f32>,
    },
}
