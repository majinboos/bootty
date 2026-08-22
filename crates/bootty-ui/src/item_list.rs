//! Painting one row of an item list: a group header, a session row with its badge and primitives,
//! or a generic published item. Plus the tree guides, the hover row hit-test, and the drag preview
//! chip.
//!
//! The row carries only what painting needs. Whose row it is, and what clicking it means, belong
//! to the caller.

use eframe::egui::{self, Pos2, Rect, Stroke};

use bootty_item::ModulePrimitive;

use crate::item_paint::{PrimitivePaintStyle, paint_item_primitives};
use crate::{ThemePalette, icons::paint_icon_slug, mix, readable_color, truncate_label};

/// One row's visual content.
pub struct ListRow<'a> {
    pub text: &'a str,
    /// A small numbered badge before the label.
    pub number: Option<usize>,
    /// Nesting depth, in indent steps.
    pub indent: u16,
    /// Tree guide glyph for this row: `middle`, `last`, or none.
    pub tree: Option<&'a str>,
    /// Row kind, which decides which painter applies.
    pub kind: &'a str,
    pub color: egui::Color32,
    /// The color used when the row is not the active one.
    pub dim_color: egui::Color32,
    pub icon: Option<&'a str>,
    /// The selected row.
    pub current: bool,
    /// Whether the row is in the active binding.
    pub active: bool,
    pub primitives: &'a [ModulePrimitive],
}

pub const ROW_HEIGHT: f32 = 24.0;
pub const ROW_PAD_X: f32 = 14.0;
pub fn hovered_row(pos: Pos2, left: f32, top: f32, width: f32, max_rows: usize) -> Option<usize> {
    let list_rect = Rect::from_min_size(
        Pos2::new(left, top),
        egui::vec2(width, max_rows as f32 * ROW_HEIGHT),
    );
    if !list_rect.contains(pos) {
        return None;
    }
    let row = ((pos.y - top) / ROW_HEIGHT).floor() as usize;
    (row < max_rows).then_some(row)
}

pub fn paint_drag_preview(
    ui: &egui::Ui,
    pointer_pos: Option<Pos2>,
    preview: &str,
    palette: ThemePalette,
) {
    let Some(pointer_pos) = pointer_pos else {
        return;
    };
    let preview = truncate_label(preview, 24);
    let font = egui::FontId::monospace(13.0);
    let width = preview.chars().count() as f32 * 7.4 + 18.0;
    let rect = Rect::from_min_size(
        pointer_pos + egui::vec2(14.0, 14.0),
        egui::vec2(width.max(48.0), ROW_HEIGHT - 2.0),
    );
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("mux-sidebar-drag-preview"),
    ));
    painter.rect_filled(rect, 6.0, mix(palette.base, palette.text, 0.12));
    painter.rect_stroke(
        rect,
        6.0,
        Stroke::new(1.0, palette.primary),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.left_center() + egui::vec2(9.0, 0.0),
        egui::Align2::LEFT_CENTER,
        preview,
        font,
        palette.text,
    );
}

/// Where a sidebar drag would drop: the block to insert before (`None` = end) and the indicator
/// y, or `None` when the drop is a no-op. The decision itself is shared with the status bar.
pub const ROW_INDENT_PX: f32 = 7.0;
/// Fraction of a color kept when dimming an unfocused session row; the rest blends to the row
/// background, so each element fades in its own hue rather than washing toward white.
/// Fraction of a color kept when dimming an unfocused session row; the rest blends to the row
/// background, so each element fades in its own hue rather than washing toward white.
pub const UNFOCUSED_ROW_KEEP: f32 = 0.5;
pub fn item_text_x(rect: Rect, item: &ListRow<'_>) -> f32 {
    rect.min.x + 12.0 + f32::from(item.indent) * ROW_INDENT_PX
}
pub fn paint_tree_guide(painter: &egui::Painter, rect: Rect, item: &ListRow<'_>) {
    let x = rect.min.x + 15.5;
    let cy = rect.center().y;
    let stroke = Stroke::new(1.0, item.dim_color.gamma_multiply(0.8));
    match item.tree {
        Some(tree @ ("middle" | "last")) => {
            let bottom = if tree == "middle" { rect.max.y } else { cy };
            painter.line_segment([Pos2::new(x, rect.min.y), Pos2::new(x, bottom)], stroke);
            painter.line_segment([Pos2::new(x, cy), Pos2::new(x + 5.0, cy)], stroke);
        }
        Some("pipe") => {
            painter.line_segment([Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)], stroke);
        }
        _ => {}
    }
}
pub fn paint_group_item(
    painter: &egui::Painter,
    rect: Rect,
    item: &ListRow<'_>,
    background: egui::Color32,
) {
    let title_color = if item.current {
        item.color
    } else {
        item.dim_color
    };
    painter.text(
        Pos2::new(item_text_x(rect, item), rect.center().y),
        egui::Align2::LEFT_CENTER,
        truncate_label(item.text, 28),
        egui::FontId::monospace(12.0),
        title_color,
    );
    paint_item_primitives(
        painter,
        rect,
        item.primitives,
        PrimitivePaintStyle {
            default_color: item.color,
            background,
            respect_color: true,
            keep: 1.0,
            round_end: false,
            time: 0.0,
            hover: None,
        },
    );
}
pub fn paint_session_item(
    painter: &egui::Painter,
    rect: Rect,
    item: &ListRow<'_>,
    active: bool,
    palette: ThemePalette,
    background: egui::Color32,
) {
    // Render the session name in its own session color verbatim — vivid when active, dim when not —
    // rather than through readable_color, whose AAA contrast gate flattens both tints to flat white.
    let label_color = if active { item.color } else { item.dim_color };
    let x = item_text_x(rect, item);
    let cy = rect.center().y;
    let number = item.number;
    let mut text_x = x;
    if let Some(number) = number {
        let badge = Rect::from_center_size(Pos2::new(x + 7.0, cy), egui::vec2(14.0, 14.0));
        if active {
            painter.rect_filled(badge, 3.0, item.color);
        } else {
            painter.rect_stroke(
                badge,
                3.0,
                Stroke::new(1.0, item.dim_color),
                egui::StrokeKind::Inside,
            );
        }
        painter.text(
            badge.center(),
            egui::Align2::CENTER_CENTER,
            number % 100,
            egui::FontId::monospace(10.0),
            if active {
                readable_color(item.color, palette.base)
            } else {
                item.dim_color
            },
        );
        text_x = badge.max.x + 6.0;
    }
    painter.text(
        Pos2::new(text_x, cy),
        egui::Align2::LEFT_CENTER,
        truncate_label(item.text, 20),
        egui::FontId::monospace(13.0),
        label_color,
    );
    let keep = if active { 1.0 } else { UNFOCUSED_ROW_KEEP };
    paint_item_primitives(
        painter,
        rect,
        item.primitives,
        PrimitivePaintStyle {
            default_color: item.dim_color,
            background,
            respect_color: true,
            keep,
            round_end: false,
            time: 0.0,
            hover: None,
        },
    );
}
pub fn paint_generic_sidebar_item(
    painter: &egui::Painter,
    rect: Rect,
    item: &ListRow<'_>,
    palette: ThemePalette,
    background: egui::Color32,
) {
    let keep = if item.current {
        1.0
    } else {
        UNFOCUSED_ROW_KEEP
    };
    paint_item_primitives(
        painter,
        rect,
        item.primitives,
        PrimitivePaintStyle {
            default_color: item.dim_color,
            background,
            respect_color: true,
            keep,
            round_end: false,
            time: 0.0,
            hover: None,
        },
    );
    if !item.primitives.is_empty() {
        return;
    }
    let x = item_text_x(rect, item);
    let cy = rect.center().y;
    let mut text_x = x;
    if let Some(icon) = item.icon
        && paint_icon_slug(
            painter,
            icon,
            Pos2::new(x + 6.0, cy),
            12.0,
            readable_color(background, item.color),
        )
    {
        text_x += 16.0;
    }
    if !item.text.is_empty() {
        painter.text(
            Pos2::new(text_x, cy),
            egui::Align2::LEFT_CENTER,
            truncate_label(item.text, 28),
            egui::FontId::monospace(11.0),
            readable_color(background, palette.muted),
        );
    }
}
