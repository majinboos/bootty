//! The sidebar's footer row: extension-published footer items, or the fallback identity row.

use bootty_extension::{ExtensionUiAction, ModuleItem, PublishedSurfaceItem};
use bootty_ui::{ThemePalette, icons::paint_icon_slug, readable_color};
use eframe::egui::{self, Pos2, Rect, Stroke};

use super::sidebar_panel::SIDEBAR_FOOTER_ITEM_HEIGHT;
use bootty_ui::item_paint::module_color32;

use bootty_ui::item_paint::{PrimitivePaintStyle, paint_item_primitives};
use bootty_ui::truncate_label;

pub(super) fn paint_sidebar_footer(
    ui: &mut egui::Ui,
    rect: Rect,
    footer_h: f32,
    footer_items: &[PublishedSurfaceItem],
    separator_visible: bool,
    palette: ThemePalette,
    border_color: egui::Color32,
) -> Option<ExtensionUiAction> {
    let painter = ui.painter_at(rect);
    let y = rect.max.y - footer_h;
    if separator_visible {
        painter.line_segment(
            [Pos2::new(rect.min.x, y), Pos2::new(rect.max.x, y)],
            Stroke::new(1.0, border_color),
        );
    }

    let mut action = None;
    for (index, published) in footer_items.iter().enumerate() {
        let item = &published.item;
        let row_y = y + 18.0 + index as f32 * SIDEBAR_FOOTER_ITEM_HEIGHT;
        let item_rect = Rect::from_min_size(
            Pos2::new(rect.min.x + 14.0, row_y - 10.0),
            egui::vec2(rect.width() - 28.0, 26.0),
        );
        let response = ui.interact(
            item_rect,
            ui.make_persistent_id((
                "extension-sidebar-footer",
                published.module.as_str(),
                published.generation,
                published.surface.as_str(),
                index,
            )),
            if item.action.is_some() {
                egui::Sense::click()
            } else {
                egui::Sense::hover()
            },
        );
        if response.hovered() && item.action.is_some() {
            ui.set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if action.is_none() && response.clicked_by(egui::PointerButton::Primary) {
            action = published.action();
        }
        let color = readable_color(
            palette.base,
            item.fg.map(module_color32).unwrap_or(palette.subtext),
        );
        paint_item_primitives(
            &painter,
            item_rect,
            &item.primitives,
            PrimitivePaintStyle {
                default_color: color,
                background: palette.base,
                respect_color: false,
                keep: 1.0,
                round_end: false,
                hover: None,
            },
        );
        if item.primitives.is_empty() {
            paint_footer_fallback(&painter, item_rect, item, color, palette);
        }
    }
    action
}

fn paint_footer_fallback(
    painter: &egui::Painter,
    rect: Rect,
    item: &ModuleItem,
    color: egui::Color32,
    palette: ThemePalette,
) {
    let mut text_x = rect.min.x;
    if let Some(icon) = item.icon.as_deref()
        && paint_icon_slug(
            painter,
            icon,
            Pos2::new(rect.min.x + 6.0, rect.min.y + 6.0),
            12.0,
            readable_color(palette.base, color),
        )
    {
        text_x += 16.0;
    }
    if !item.text.is_empty() {
        painter.text(
            Pos2::new(text_x, rect.min.y + 6.0),
            egui::Align2::LEFT_CENTER,
            truncate_label(&item.text, 28),
            egui::FontId::monospace(11.0),
            readable_color(palette.base, palette.subtext),
        );
    }
}
