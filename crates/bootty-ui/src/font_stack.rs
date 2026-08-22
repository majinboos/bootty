//! The font-stack editor: an ordered list of family names with a primary and its fallbacks.
//!
//! Reorderable, removable, and each row picks from an installed-family list. It knows nothing about
//! whose stack it is editing — the caller owns the `Vec<String>` and decides what a change means.

use eframe::egui;

use crate::ThemePalette;
use crate::settings::{
    DragHandle, apply_reorder, reorderable_list, searchable_combo, settings_button,
    settings_icon_button,
};

pub struct FontStackSpec<'a> {
    /// Unique per-stack id root so the UI and terminal stacks never share combo ids (which would
    /// wire one stack's dropdown to the other's).
    pub id_prefix: &'a str,
    pub primary_title: &'a str,
    pub primary_help: &'a str,
    pub fallback_prefix: &'a str,
    pub fallback_help: &'a str,
    pub add_label: &'a str,
}

pub fn font_stack_editor(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    options: &[&str],
    family: &mut Vec<String>,
    spec: FontStackSpec<'_>,
) -> bool {
    let mut changed = false;
    let mut remove: Option<usize> = None;
    let reorder = reorderable_list(
        ui,
        palette,
        spec.id_prefix,
        family.len(),
        |ui, index, handle| {
            let title = if index == 0 {
                spec.primary_title.to_owned()
            } else {
                format!("{} {index}", spec.fallback_prefix)
            };
            let help = if index == 0 {
                spec.primary_help
            } else {
                spec.fallback_help
            };
            font_stack_row(
                ui,
                palette,
                FontStackRow {
                    id_prefix: spec.id_prefix,
                    index,
                    title: &title,
                    help,
                    entry: &mut family[index],
                    options,
                    changed: &mut changed,
                    remove: &mut remove,
                    handle,
                },
            );
        },
    );
    ui.add_space(10.0);
    if settings_button(ui, palette, spec.add_label).clicked() {
        family.push(String::new());
        changed = true;
    }
    if let Some((from, slot)) = reorder {
        apply_reorder(family, from, slot);
        changed = true;
    }
    if let Some(index) = remove {
        family.remove(index);
        changed = true;
    }
    changed
}

struct FontStackRow<'a> {
    id_prefix: &'a str,
    index: usize,
    title: &'a str,
    help: &'a str,
    entry: &'a mut String,
    options: &'a [&'a str],
    changed: &'a mut bool,
    remove: &'a mut Option<usize>,
    handle: &'a DragHandle,
}

/// One font-stack entry. Laid out naturally (no height measurement); the grip is overlaid into the
/// finished row rect afterwards so it stays vertically centered, and separators sit between entries
/// only — the last entry carries no trailing border.
fn font_stack_row(ui: &mut egui::Ui, palette: ThemePalette, row: FontStackRow<'_>) {
    const GUTTER: f32 = 28.0;

    // A gap above every entry but the first; the separator line is drawn into it after layout.
    if row.index > 0 {
        ui.add_space(9.0);
    }

    let response = ui
        .horizontal(|ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.add_space(GUTTER); // reserve the handle gutter; the grip is overlaid after

            let label_width = (ui.available_width() - 330.0).clamp(150.0, 360.0);
            ui.allocate_ui_with_layout(
                egui::Vec2::new(label_width, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(row.title).color(palette.text).strong(),
                        )
                        .wrap(),
                    );
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(row.help)
                                .color(palette.muted)
                                .size(11.0),
                        )
                        .wrap(),
                    );
                },
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if settings_icon_button(ui, palette, "x", "Remove font").clicked() {
                    *row.remove = Some(row.index);
                }
                ui.add_space(6.0);
                let selected_text = if row.entry.is_empty() {
                    "Choose a font".to_owned()
                } else {
                    row.entry.clone()
                };
                let current_index = row
                    .options
                    .iter()
                    .position(|name| *name == row.entry.as_str());
                let combo_width = (ui.available_width() - 6.0).clamp(180.0, 300.0);
                if let Some(choice) = searchable_combo(
                    ui,
                    palette,
                    &format!("{}_combo_{}", row.id_prefix, row.index),
                    &selected_text,
                    combo_width,
                    row.options,
                    current_index,
                ) {
                    *row.entry = row.options[choice].to_owned();
                    *row.changed = true;
                }
            });
        })
        .response;

    // Overlay the grip centered in the finished row rect, no measurement required.
    let gutter = egui::Rect::from_min_max(
        response.rect.left_top(),
        egui::Pos2::new(response.rect.left() + GUTTER, response.rect.bottom()),
    );
    row.handle.paint_in(ui, palette, gutter);

    if row.index > 0 {
        let y = response.rect.top() - 5.0;
        let line = egui::Rect::from_min_max(
            egui::Pos2::new(response.rect.left(), y),
            egui::Pos2::new(response.rect.right(), y + 1.0),
        );
        ui.painter().rect_filled(line, 0.0, palette.border);
    }
    ui.add_space(8.0);
}
