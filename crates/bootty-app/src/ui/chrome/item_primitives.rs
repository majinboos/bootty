use bootty_extension::{ModuleColor, ModuleCoord, ModuleCornerRadius, ModulePrimitive};
use bootty_ui::{icons::paint_icon_slug, mix, readable_color};
use eframe::egui::{self, Pos2, Rect, Stroke, StrokeKind};

use crate::theme::module_color32;

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

fn corner_radius(value: ModuleCornerRadius) -> egui::CornerRadius {
    egui::CornerRadius {
        nw: value.nw,
        ne: value.ne,
        sw: value.sw,
        se: value.se,
    }
}

pub(super) fn paint_item_primitives(
    painter: &egui::Painter,
    item_rect: Rect,
    primitives: &[ModulePrimitive],
    default_color: egui::Color32,
    background: egui::Color32,
    // Sidebar session rows pick intentionally dim, hue-tinted colors; honor them verbatim instead of
    // running them through readable_color, whose AAA contrast gate flattens dim tints to white. The
    // status bar and footer keep the gate so module colors stay legible on varied backgrounds.
    respect_color: bool,
    // Fraction of each color to keep before blending the rest toward the background. 1.0 paints the
    // color as-is; unfocused session rows pass < 1.0 so every element dims in its own hue.
    keep: f32,
) {
    paint_item_primitives_inner(
        painter,
        item_rect,
        primitives,
        PrimitivePaintStyle {
            default_color,
            background,
            respect_color,
            keep,
            round_end: false,
            hover: None,
        },
    );
}

pub(super) struct PrimitivePaintStyle {
    pub default_color: egui::Color32,
    pub background: egui::Color32,
    pub respect_color: bool,
    pub keep: f32,
    pub round_end: bool,
    pub hover: Option<egui::Color32>,
}

pub(super) fn paint_item_primitives_inner(
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
            } => {
                let rect = primitive_rect(item_rect, *x, *y, *w, *h);
                let mut radius = corner_radius(*radius);
                if round_end {
                    radius.ne = 6;
                    radius.se = 6;
                }
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
                    ..
                } => {
                    painter.rect_filled(
                        primitive_rect(item_rect, *x, *y, *w, *h),
                        corner_radius(*radius),
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

pub(super) fn primitive_background(primitives: &[ModulePrimitive]) -> Option<egui::Color32> {
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
