//! Painting a published item's primitives: rectangles, polygons, text and icons placed in the
//! item's own rect. The vocabulary is the shared item schema, so this knows nothing about who
//! published the item.

use bootty_item::{ModuleColor, ModuleCoord, ModuleCornerRadius, ModulePrimitive};
use eframe::egui::{self, Pos2, Rect, Stroke, StrokeKind};

use crate::{icons::paint_icon_slug, mix, readable_color};

/// A module color as an egui color.
#[must_use]
pub fn module_color32(color: ModuleColor) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(color.r, color.g, color.b, color.a)
}

fn coord(origin: f32, length: f32, value: ModuleCoord) -> f32 {
    origin + length * value.frac + value.px
}

fn primitive_pos(rect: Rect, x: ModuleCoord, y: ModuleCoord) -> Pos2 {
    Pos2::new(
        coord(rect.min.x, rect.width(), x),
        coord(rect.min.y, rect.height(), y),
    )
}

fn primitive_rect(
    rect: Rect,
    x: ModuleCoord,
    y: ModuleCoord,
    w: ModuleCoord,
    h: ModuleCoord,
) -> Rect {
    Rect::from_min_size(
        primitive_pos(rect, x, y),
        egui::vec2(coord(0.0, rect.width(), w), coord(0.0, rect.height(), h)),
    )
}

fn primitive_points(rect: Rect, points: &[(ModuleCoord, ModuleCoord)]) -> Vec<Pos2> {
    points
        .iter()
        .map(|&(x, y)| primitive_pos(rect, x, y))
        .collect()
}

/// The egui radius a declared module corner radius resolves to.
pub fn corner_radius(value: ModuleCornerRadius) -> egui::CornerRadius {
    egui::CornerRadius {
        nw: value.nw,
        ne: value.ne,
        sw: value.sw,
        se: value.se,
    }
}

pub struct PrimitivePaintStyle {
    pub default_color: egui::Color32,
    pub background: egui::Color32,
    /// Sidebar session rows pick intentionally dim, hue-tinted colors; honor them verbatim instead
    /// of running them through `readable_color`, whose AAA contrast gate flattens dim tints to
    /// white. The status bar and footer keep the gate so module colors stay legible on varied
    /// backgrounds.
    pub respect_color: bool,
    /// Fraction of each color to keep before blending the rest toward the background. 1.0 paints
    /// the color as-is; unfocused session rows pass < 1.0 so every element dims in its own hue.
    pub keep: f32,
    pub round_end: bool,
    pub hover: Option<egui::Color32>,
    /// Frame time in seconds, driving any sweeping primitive.
    pub time: f64,
}

pub fn paint_item_primitives(
    painter: &egui::Painter,
    item_rect: Rect,
    primitives: &[ModulePrimitive],
    style: PrimitivePaintStyle,
) {
    let PrimitivePaintStyle {
        default_color,
        background,
        respect_color,
        keep,
        round_end,
        hover,
        time,
    } = style;
    let dim = |color: egui::Color32| mix(background, color, keep);
    let resolve = |value: &Option<ModuleColor>| {
        let value = value.map_or(default_color, module_color32);
        let value = if respect_color {
            value
        } else {
            readable_color(background, value)
        };
        dim(value)
    };
    for primitive in primitives {
        match primitive {
            ModulePrimitive::Rect {
                fill,
                stroke,
                x,
                y,
                w,
                h,
                radius,
                sweep,
            } => {
                let rect = primitive_rect(item_rect, sweep_x(*x, *w, *sweep, time), *y, *w, *h);
                let radius = rect_radius(*radius, round_end);
                if let Some(fill) = fill {
                    painter.rect_filled(rect, radius, dim(module_color32(*fill)));
                }
                if let Some(stroke) = stroke {
                    painter.rect_stroke(
                        rect,
                        radius,
                        Stroke::new(1.0, dim(module_color32(*stroke))),
                        StrokeKind::Inside,
                    );
                }
            }
            ModulePrimitive::Polygon {
                fill,
                stroke,
                points,
            } => {
                let points = primitive_points(item_rect, points);
                if points.len() >= 3 {
                    if let Some(fill) = fill {
                        painter.add(egui::Shape::convex_polygon(
                            points.clone(),
                            dim(module_color32(*fill)),
                            Stroke::NONE,
                        ));
                    }
                    if let Some(stroke) = stroke {
                        painter.add(egui::Shape::closed_line(
                            points,
                            Stroke::new(1.0, dim(module_color32(*stroke))),
                        ));
                    }
                }
            }
            ModulePrimitive::Text {
                text,
                color,
                x,
                y,
                size,
                align,
                min_width,
            } => {
                if min_width.is_some_and(|min_width| item_rect.width() < min_width) {
                    continue;
                }
                painter.text(
                    primitive_pos(item_rect, *x, *y),
                    primitive_align(align),
                    text,
                    egui::FontId::monospace(*size),
                    resolve(color),
                );
            }
            ModulePrimitive::Icon {
                icon,
                color,
                x,
                y,
                size,
                min_width,
            } => {
                if min_width.is_some_and(|min_width| item_rect.width() < min_width) {
                    continue;
                }
                paint_icon_slug(
                    painter,
                    icon,
                    primitive_pos(item_rect, *x, *y),
                    *size,
                    resolve(color),
                );
            }
        }
    }
    if let Some(color) = hover {
        for primitive in primitives {
            match primitive {
                ModulePrimitive::Rect {
                    fill: Some(_),
                    x,
                    y,
                    w,
                    h,
                    radius,
                    sweep,
                    ..
                } => {
                    // Same rounding as the base pass, or the hover fill squares off a rounded pill.
                    painter.rect_filled(
                        primitive_rect(item_rect, sweep_x(*x, *w, *sweep, time), *y, *w, *h),
                        rect_radius(*radius, round_end),
                        color,
                    );
                }
                ModulePrimitive::Polygon {
                    fill: Some(_),
                    points,
                    ..
                } => {
                    let points = primitive_points(item_rect, points);
                    if points.len() >= 3 {
                        painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
                    }
                }
                _ => {}
            }
        }
    }
}

fn primitive_align(value: &str) -> egui::Align2 {
    match value {
        "left_top" => egui::Align2::LEFT_TOP,
        "left_center" => egui::Align2::LEFT_CENTER,
        "left_bottom" => egui::Align2::LEFT_BOTTOM,
        "center_top" => egui::Align2::CENTER_TOP,
        "center_center" | "center" => egui::Align2::CENTER_CENTER,
        "center_bottom" => egui::Align2::CENTER_BOTTOM,
        "right_top" => egui::Align2::RIGHT_TOP,
        "right_center" => egui::Align2::RIGHT_CENTER,
        "right_bottom" => egui::Align2::RIGHT_BOTTOM,
        _ => egui::Align2::LEFT_CENTER,
    }
}

pub fn primitive_background(primitives: &[ModulePrimitive]) -> Option<egui::Color32> {
    let mut rect = None;
    let mut polygon = None;
    for primitive in primitives.iter().rev() {
        match primitive {
            ModulePrimitive::Rect {
                fill: Some(fill),
                x,
                y,
                w,
                h,
                ..
            } if [x, y]
                .iter()
                .all(|coord| coord.frac == 0.0 && coord.px == 0.0)
                && [w, h]
                    .iter()
                    .all(|coord| coord.frac == 1.0 && coord.px == 0.0) =>
            {
                return Some(module_color32(*fill));
            }
            ModulePrimitive::Rect { fill, .. } if rect.is_none() => {
                rect = fill.map(module_color32);
            }
            ModulePrimitive::Polygon { fill, .. } if polygon.is_none() => {
                polygon = fill.map(module_color32);
            }
            ModulePrimitive::Rect { .. }
            | ModulePrimitive::Polygon { .. }
            | ModulePrimitive::Text { .. }
            | ModulePrimitive::Icon { .. } => {}
        }
    }
    rect.or(polygon)
}

/// A rect primitive's corners, with the trailing pair rounded off when the item closes a run of
/// tabs. Both the base fill and the hover overlay resolve it here so they cannot disagree.
/// The corner radius a run member draws with: only the last one rounds its trailing corners.
pub fn rect_radius(radius: ModuleCornerRadius, round_end: bool) -> egui::CornerRadius {
    let mut radius = corner_radius(radius);
    if round_end {
        radius.ne = RUN_END_RADIUS;
        radius.se = RUN_END_RADIUS;
    }
    radius
}

/// Corner radius applied to the trailing edge of a tab run.
/// The radius the trailing corners of a run take.
pub const RUN_END_RADIUS: u8 = 6;

/// A sweeping rect's left edge: a triangle wave over the width its own `w` leaves free, so the fill
/// travels to the far edge and back. A non-sweeping rect keeps its declared `x`.
/// Where a sweeping rect sits at `time`: it travels the space its own width leaves free and
/// returns, so an indeterminate bar needs no state of its own.
pub fn sweep_x(x: ModuleCoord, w: ModuleCoord, sweep: bool, time: f64) -> ModuleCoord {
    if !sweep {
        return x;
    }
    let phase = ((time % SWEEP_PERIOD) / SWEEP_PERIOD) as f32;
    let travel = (1.0_f32 - w.frac).max(0.0);
    ModuleCoord {
        frac: travel * (1.0 - (2.0 * phase - 1.0).abs()),
        px: x.px,
    }
}

/// Seconds for one there-and-back sweep.
pub const SWEEP_PERIOD: f64 = 1.5;
