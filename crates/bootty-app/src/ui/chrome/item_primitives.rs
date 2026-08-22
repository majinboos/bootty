use bootty_extension::{ModuleColor, ModuleCoord, ModuleCornerRadius, ModulePrimitive};
use bootty_ui::{icons::paint_icon_slug, readable_color};
use eframe::egui::{self, Pos2, Rect, Stroke, StrokeKind};

use crate::theme::module_color32;

fn coord_x(rect: Rect, coord: ModuleCoord) -> f32 {
    rect.min.x + rect.width() * coord.frac + coord.px
}

fn coord_y(rect: Rect, coord: ModuleCoord) -> f32 {
    rect.min.y + rect.height() * coord.frac + coord.px
}

fn coord_w(rect: Rect, coord: ModuleCoord) -> f32 {
    rect.width() * coord.frac + coord.px
}

fn coord_h(rect: Rect, coord: ModuleCoord) -> f32 {
    rect.height() * coord.frac + coord.px
}

fn corner_radius(value: ModuleCornerRadius) -> egui::CornerRadius {
    egui::CornerRadius {
        nw: value.nw,
        ne: value.ne,
        sw: value.sw,
        se: value.se,
    }
}

fn blend_toward(color: egui::Color32, background: egui::Color32, keep: f32) -> egui::Color32 {
    if keep >= 1.0 {
        return color;
    }
    if keep <= 0.0 {
        return background;
    }
    let mix = |fg: u8, bg: u8| (bg as f32 + (fg as f32 - bg as f32) * keep).round() as u8;
    egui::Color32::from_rgb(
        mix(color.r(), background.r()),
        mix(color.g(), background.g()),
        mix(color.b(), background.b()),
    )
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
    let dim = |color: egui::Color32| blend_toward(color, background, keep);
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
                let rect = Rect::from_min_size(
                    Pos2::new(coord_x(item_rect, *x), coord_y(item_rect, *y)),
                    egui::vec2(coord_w(item_rect, *w), coord_h(item_rect, *h)),
                );
                if let Some(fill) = fill {
                    painter.rect_filled(rect, corner_radius(*radius), dim(module_color32(*fill)));
                }
                if let Some(stroke) = stroke {
                    painter.rect_stroke(
                        rect,
                        corner_radius(*radius),
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
                let points = points
                    .iter()
                    .map(|(x, y)| Pos2::new(coord_x(item_rect, *x), coord_y(item_rect, *y)))
                    .collect::<Vec<_>>();
                if points.len() >= 3 {
                    if let Some(fill) = fill {
                        painter.add(egui::Shape::convex_polygon(
                            points.clone(),
                            dim(module_color32(*fill)),
                            Stroke::new(0.0, egui::Color32::TRANSPARENT),
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
                    Pos2::new(coord_x(item_rect, *x), coord_y(item_rect, *y)),
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
                    Pos2::new(coord_x(item_rect, *x), coord_y(item_rect, *y)),
                    *size,
                    resolve(color),
                );
            }
        }
    }
}

pub(super) fn paint_item_hover_overlay(
    painter: &egui::Painter,
    item_rect: Rect,
    primitives: &[ModulePrimitive],
    color: egui::Color32,
) {
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
                let rect = Rect::from_min_size(
                    Pos2::new(coord_x(item_rect, *x), coord_y(item_rect, *y)),
                    egui::vec2(coord_w(item_rect, *w), coord_h(item_rect, *h)),
                );
                painter.rect_filled(rect, corner_radius(*radius), color);
            }
            ModulePrimitive::Polygon {
                fill: Some(_),
                points,
                ..
            } => {
                let points = points
                    .iter()
                    .map(|(x, y)| Pos2::new(coord_x(item_rect, *x), coord_y(item_rect, *y)))
                    .collect::<Vec<_>>();
                if points.len() >= 3 {
                    painter.add(egui::Shape::convex_polygon(
                        points,
                        color,
                        Stroke::new(0.0, egui::Color32::TRANSPARENT),
                    ));
                }
            }
            ModulePrimitive::Rect { fill: None, .. }
            | ModulePrimitive::Polygon { fill: None, .. }
            | ModulePrimitive::Text { .. }
            | ModulePrimitive::Icon { .. } => {}
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
    primitives
        .iter()
        .rev()
        .find_map(|primitive| match primitive {
            ModulePrimitive::Rect {
                fill: Some(fill),
                x,
                y,
                w,
                h,
                ..
            } if x.frac == 0.0
                && x.px == 0.0
                && y.frac == 0.0
                && y.px == 0.0
                && w.frac == 1.0
                && w.px == 0.0
                && h.frac == 1.0
                && h.px == 0.0 =>
            {
                Some(module_color32(*fill))
            }
            ModulePrimitive::Rect { .. }
            | ModulePrimitive::Polygon { .. }
            | ModulePrimitive::Text { .. }
            | ModulePrimitive::Icon { .. } => None,
        })
        .or_else(|| {
            primitives
                .iter()
                .rev()
                .find_map(|primitive| match primitive {
                    ModulePrimitive::Rect { fill, .. } => fill.map(module_color32),
                    ModulePrimitive::Polygon { .. }
                    | ModulePrimitive::Text { .. }
                    | ModulePrimitive::Icon { .. } => None,
                })
        })
        .or_else(|| {
            primitives
                .iter()
                .rev()
                .find_map(|primitive| match primitive {
                    ModulePrimitive::Polygon { fill, .. } => fill.map(module_color32),
                    ModulePrimitive::Rect { .. }
                    | ModulePrimitive::Text { .. }
                    | ModulePrimitive::Icon { .. } => None,
                })
        })
}
