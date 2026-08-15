use std::path::Path;

use crate::{Theme, ThemePalette, contrast_ratio, icons, readable_color};
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Vec2};

/// A combo box whose dropdown has a search filter at the top. Returns the chosen option index.
pub fn searchable_combo(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    id_salt: &str,
    selected_text: &str,
    width: f32,
    options: &[&str],
    current: Option<usize>,
) -> Option<usize> {
    let filter_id = ui.make_persistent_id((id_salt, "filter"));
    let mut chosen = None;
    egui::ComboBox::from_id_salt(id_salt)
        .selected_text(selected_text)
        .width(width)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show_ui(ui, |ui| {
            ui.set_min_width(width.max(260.0));
            let mut filter: String =
                ui.memory(|memory| memory.data.get_temp(filter_id).unwrap_or_default());
            let response = settings_text_edit(ui, palette, &mut filter, "Search");
            if !response.has_focus() {
                response.request_focus();
            }
            ui.memory_mut(|memory| memory.data.insert_temp(filter_id, filter.clone()));
            let needle = filter.to_ascii_lowercase();
            ui.separator();
            // Size the list from the full option count, not the filtered subset, so a query that
            // matches nothing can't collapse the popup and leave it stuck small afterward.
            let list_height = (options.len() as f32 * 24.0).clamp(0.0, 300.0);
            egui::ScrollArea::vertical()
                .max_height(list_height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_min_height(list_height);
                    for (index, option) in options.iter().enumerate() {
                        if !needle.is_empty() && !option.to_ascii_lowercase().contains(&needle) {
                            continue;
                        }
                        let is_current = current == Some(index);
                        let (rect, response) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 24.0),
                            egui::Sense::click(),
                        );
                        let fill = if is_current {
                            palette.accent
                        } else if response.hovered() {
                            palette.hover
                        } else {
                            palette.pane
                        };
                        ui.painter().rect_filled(
                            rect,
                            egui::CornerRadius::same(palette.radius),
                            fill,
                        );
                        ui.painter().text(
                            rect.left_center() + Vec2::new(8.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            *option,
                            egui::TextStyle::Button.resolve(ui.style()),
                            readable_color(fill, palette.text),
                        );
                        if response.clicked() {
                            chosen = Some(index);
                            ui.memory_mut(|memory| memory.data.remove_temp::<String>(filter_id));
                            ui.close();
                        }
                    }
                });
        });
    chosen
}

/// Presentation knobs for [`described_combo`].
pub struct ComboStyle {
    pub width: f32,
    /// Show a search field above the list (for long option sets).
    pub searchable: bool,
    /// Closed-combo text when `current` matches no option.
    pub placeholder: &'static str,
}

/// A combo whose options each render a bold label over a muted one-line description (the look in
/// the fullscreen/decoration pickers). Shared across settings: set [`ComboStyle::searchable`] for
/// long lists (e.g. the keybind action picker). Returns whether the selection changed.
pub fn described_combo<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    id: &str,
    current: &mut T,
    options: &[(T, &str, &str)],
    style: ComboStyle,
) -> bool {
    let ComboStyle {
        width,
        searchable,
        placeholder,
    } = style;
    let selected = options.iter().position(|(value, _, _)| *value == *current);
    let selected_text = selected.map_or(placeholder, |index| options[index].1);
    let filter_id = ui.make_persistent_id((id, "filter"));
    let mut changed = false;
    egui::ComboBox::from_id_salt(id)
        .selected_text(selected_text)
        .width(width)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show_ui(ui, |ui| {
            ui.set_min_width(width.max(300.0));
            let needle = if searchable {
                let mut filter: String =
                    ui.memory(|memory| memory.data.get_temp(filter_id).unwrap_or_default());
                let response = settings_text_edit(ui, palette, &mut filter, "Search");
                if !response.has_focus() {
                    response.request_focus();
                }
                ui.memory_mut(|memory| memory.data.insert_temp(filter_id, filter.clone()));
                ui.separator();
                filter.to_ascii_lowercase()
            } else {
                String::new()
            };
            // Size from the full option count so a query matching nothing can't collapse the popup.
            let list_height = (options.len() as f32 * 54.0).clamp(54.0, 320.0);
            egui::ScrollArea::vertical()
                .max_height(list_height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_min_height(list_height);
                    for (value, label, description) in options {
                        if !needle.is_empty()
                            && !label.to_ascii_lowercase().contains(&needle)
                            && !description.to_ascii_lowercase().contains(&needle)
                        {
                            continue;
                        }
                        let is_current = *value == *current;
                        let (rect, response) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 52.0),
                            egui::Sense::click(),
                        );
                        let fill = if is_current {
                            palette.surface
                        } else if response.hovered() {
                            palette.hover
                        } else {
                            palette.pane
                        };
                        ui.painter().rect_filled(rect, palette.radius, fill);
                        ui.painter().text(
                            rect.left_top() + Vec2::new(12.0, 8.0),
                            egui::Align2::LEFT_TOP,
                            *label,
                            egui::TextStyle::Button.resolve(ui.style()),
                            readable_color(fill, palette.text),
                        );
                        if !description.is_empty() {
                            ui.painter().text(
                                rect.left_top() + Vec2::new(12.0, 29.0),
                                egui::Align2::LEFT_TOP,
                                *description,
                                egui::TextStyle::Small.resolve(ui.style()),
                                readable_color(fill, palette.muted),
                            );
                        }
                        if response.clicked() {
                            *current = *value;
                            changed = true;
                            if searchable {
                                ui.memory_mut(|memory| {
                                    memory.data.remove_temp::<String>(filter_id);
                                });
                            }
                            ui.close();
                        }
                        ui.add_space(2.0);
                    }
                });
        });
    changed
}

/// A text button with a constant 1px border in every state, so only its fill changes on hover.
/// egui's default button reads its border in from `hovered`/`active` visuals, which makes the
/// frame appear to grow under the pointer; this keeps the footprint fixed.
pub fn settings_button(ui: &mut egui::Ui, palette: ThemePalette, label: &str) -> egui::Response {
    let font = egui::FontId::proportional(13.0);
    let text_color = readable_color(palette.surface, palette.text);
    let galley = settings_button_galley(ui, label, font, text_color);
    let padding = Vec2::new(14.0, 8.0);
    let size = Vec2::new(galley.size().x + padding.x * 2.0, 30.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let fill = if response.hovered() {
        palette.hover
    } else {
        palette.surface
    };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(palette.radius), fill);
    ui.painter().rect_stroke(
        rect,
        egui::CornerRadius::same(palette.radius),
        egui::Stroke::new(1.0, palette.border),
        egui::StrokeKind::Inside,
    );
    let text_pos = Pos2::new(rect.center().x - galley.size().x * 0.5, rect.center().y);
    ui.painter().galley(
        Pos2::new(text_pos.x, text_pos.y - galley.size().y * 0.5),
        galley,
        readable_color(fill, text_color),
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

fn settings_button_galley(
    ui: &egui::Ui,
    label: &str,
    font: egui::FontId,
    color: Color32,
) -> std::sync::Arc<egui::Galley> {
    let Some(label) = label.strip_prefix("+ ") else {
        return ui.painter().layout_no_wrap(label.to_owned(), font, color);
    };

    let mut job = egui::text::LayoutJob::default();
    if let Some((glyph, family)) = icons::icon_glyph("plus") {
        job.append(
            &glyph.to_string(),
            0.0,
            egui::text::TextFormat {
                font_id: egui::FontId::new(14.0, egui::FontFamily::Name(family.into())),
                color,
                ..Default::default()
            },
        );
    }
    job.append(
        label,
        4.0,
        egui::text::TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    ui.painter().layout_job(job)
}

/// A drag-and-drop payload identifying which list and row is being dragged. Namespaced by the
/// list id so two reorderable lists on the same page never pick up each other's drags.
#[derive(Clone, Copy)]
struct ReorderPayload {
    list: egui::Id,
    index: usize,
}

/// The grip a reorderable row hands to its renderer; calling `ui` paints the drag handle and makes
/// it the row's only drag source.
pub struct DragHandle {
    list: egui::Id,
    index: usize,
}

impl DragHandle {
    /// Paint the grip centered in `rect` and make that rect the row's sole drag source. Drawing into
    /// a caller-supplied rect (an overlay) rather than allocating in the layout flow keeps the grip
    /// vertically centered on multi-line rows: egui grows a horizontal layout's cross-axis as items
    /// are added, so an in-flow handle placed first anchors to the first line.
    pub fn paint_in(&self, ui: &mut egui::Ui, palette: ThemePalette, rect: Rect) {
        let id = self.list.with(("handle", self.index));
        let response = ui.interact(rect, id, egui::Sense::click_and_drag());
        if response.drag_started() || response.dragged() {
            egui::DragAndDrop::set_payload(
                ui.ctx(),
                ReorderPayload {
                    list: self.list,
                    index: self.index,
                },
            );
        }
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        icons::paint_icon_slug(
            ui.painter(),
            "grip-vertical",
            rect.center(),
            16.0,
            palette.muted,
        );
    }
}

/// Render a vertical, drag-reorderable list. `render_row(ui, index, handle)` draws one row and must
/// call `handle.paint_in(rect)` over the gutter where the grip belongs. Returns `Some((from, slot))` on the frame an item
/// is dropped into a new position; pass it to [`apply_reorder`]. Uses a grip handle as the sole
/// drag source — no up/down arrows.
pub fn reorderable_list(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    id_salt: &str,
    len: usize,
    mut render_row: impl FnMut(&mut egui::Ui, usize, &DragHandle),
) -> Option<(usize, usize)> {
    let list = ui.make_persistent_id(("reorderable_list", id_salt));
    let mut rects = Vec::with_capacity(len);
    for index in 0..len {
        let handle = DragHandle { list, index };
        let inner = ui.scope(|ui| render_row(ui, index, &handle));
        rects.push(inner.response.rect);
    }

    if len == 0 {
        return None;
    }

    // The payload survives until end-of-frame even on release, so reading it here covers both the
    // live drag (draw the insertion line) and the drop frame (commit the move).
    let from = egui::DragAndDrop::payload::<ReorderPayload>(ui.ctx())
        .filter(|payload| payload.list == list)
        .map(|payload| payload.index)?;
    let pointer = ui.input(|input| input.pointer.interact_pos())?;

    let mut slot = len;
    for (index, rect) in rects.iter().enumerate() {
        if pointer.y < rect.center().y {
            slot = index;
            break;
        }
    }

    let left = rects.iter().map(|r| r.left()).fold(f32::INFINITY, f32::min);
    let right = rects
        .iter()
        .map(|r| r.right())
        .fold(f32::NEG_INFINITY, f32::max);
    let line_y = if slot < len {
        rects[slot].top() - 3.0
    } else {
        rects[len - 1].bottom() + 3.0
    };
    ui.painter().line_segment(
        [Pos2::new(left, line_y), Pos2::new(right, line_y)],
        egui::Stroke::new(2.0, palette.accent),
    );

    if ui.input(|input| input.pointer.any_released()) {
        egui::DragAndDrop::clear_payload(ui.ctx());
        // Dropping onto your own edges is a no-op (slot == from keeps position, slot == from+1 lands
        // back in place after removal).
        if slot != from && slot != from + 1 {
            return Some((from, slot));
        }
    }
    None
}

/// Apply a [`reorderable_list`] result: lift item `from` and reinsert it at the `slot` boundary.
pub fn apply_reorder<T>(items: &mut Vec<T>, from: usize, slot: usize) {
    if from >= items.len() {
        return;
    }
    let item = items.remove(from);
    let to = if slot > from { slot - 1 } else { slot };
    items.insert(to.min(items.len()), item);
}

pub fn settings_icon_button(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    slug: &str,
    tooltip: &str,
) -> egui::Response {
    let size = Vec2::splat(30.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let fill = if response.hovered() {
        palette.hover
    } else {
        palette.surface
    };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(palette.radius), fill);
    ui.painter().rect_stroke(
        rect,
        egui::CornerRadius::same(palette.radius),
        egui::Stroke::new(1.0, palette.border),
        egui::StrokeKind::Inside,
    );
    icons::paint_icon_slug(
        ui.painter(),
        slug,
        rect.center(),
        15.0,
        readable_color(fill, palette.text),
    );
    response.on_hover_text(tooltip)
}

pub fn settings_page_header(ui: &mut egui::Ui, palette: ThemePalette, eyebrow: &str, title: &str) {
    ui.label(
        RichText::new(eyebrow)
            .color(readable_color(palette.base, palette.muted))
            .size(12.0),
    );
    ui.add_space(6.0);
    ui.label(
        RichText::new(title)
            .color(readable_color(palette.base, palette.text))
            .strong()
            .size(24.0),
    );
    ui.add_space(18.0);
}

/// Memory flag: the next `settings_row` is the first in its section, so it draws no separator above
/// it. This makes row dividers act as separators *between* rows — the last row keeps no trailing
/// border, which is what bleeds into a following framed block.
fn section_first_row_id() -> egui::Id {
    egui::Id::new("bootty::settings::section_first_row")
}

/// Section heading inside a page.
pub fn section(ui: &mut egui::Ui, palette: ThemePalette, title: &str) {
    ui.add_space(12.0);
    ui.label(
        RichText::new(title)
            .color(readable_color(palette.base, palette.subtext))
            .strong()
            .size(12.0),
    );
    ui.add_space(6.0);
    ui.memory_mut(|memory| memory.data.insert_temp(section_first_row_id(), true));
}

pub fn settings_row(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    label: &str,
    help: &str,
    add_control: impl FnOnce(&mut egui::Ui),
) {
    let first_in_section = ui.memory(|memory| {
        memory
            .data
            .get_temp::<bool>(section_first_row_id())
            .unwrap_or(false)
    });
    ui.memory_mut(|memory| memory.data.insert_temp(section_first_row_id(), false));

    let top = ui.cursor().top();
    ui.add_space(7.0);
    ui.horizontal(|ui| {
        ui.set_min_width(ui.available_width());
        let full_width = ui.available_width();
        let label_width = full_width.min(300.0);
        ui.allocate_ui_with_layout(
            Vec2::new(label_width, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(label)
                            .color(readable_color(palette.base, palette.text))
                            .strong(),
                    )
                    .wrap(),
                );
                ui.add(
                    egui::Label::new(
                        RichText::new(help)
                            .color(readable_color(palette.base, palette.muted))
                            .size(11.0),
                    )
                    .wrap(),
                );
            },
        );
        ui.add_space(16.0);
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), 34.0),
            egui::Layout::right_to_left(egui::Align::Center),
            add_control,
        );
    });
    ui.add_space(7.0);
    let bottom = ui.cursor().top();
    // Separator above the row (skipped for the first in a section), so no border trails the last row.
    if !first_in_section {
        let rect = Rect::from_min_max(
            Pos2::new(ui.min_rect().left(), top),
            Pos2::new(ui.min_rect().right(), top + 1.0),
        );
        ui.painter().rect_filled(rect, 0.0, palette.border);
    }
    ui.set_min_height((bottom - top).max(54.0));
}

pub fn settings_toggle_row(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    label: &str,
    help: &str,
    mut value: bool,
    on_change: impl FnOnce(bool),
) {
    let mut changed = false;
    settings_row(ui, palette, label, help, |ui| {
        changed = settings_toggle(ui, palette, &mut value);
    });
    if changed {
        on_change(value);
    }
}

pub fn settings_toggle(ui: &mut egui::Ui, palette: ThemePalette, value: &mut bool) -> bool {
    let size = Vec2::new(46.0, 26.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let changed = response.clicked();
    if changed {
        *value = !*value;
    }
    let fill = if *value {
        palette.accent
    } else if response.hovered() {
        palette.hover
    } else {
        palette.surface
    };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(13), fill);
    ui.painter().rect_stroke(
        rect,
        egui::CornerRadius::same(13),
        egui::Stroke::new(
            1.0,
            if *value {
                palette.accent
            } else {
                palette.border
            },
        ),
        egui::StrokeKind::Inside,
    );
    let knob_x = if *value {
        rect.right() - 13.0
    } else {
        rect.left() + 13.0
    };
    ui.painter().circle_filled(
        Pos2::new(knob_x, rect.center().y),
        9.0,
        readable_color(
            fill,
            if *value {
                palette.base
            } else {
                palette.subtext
            },
        ),
    );
    changed
}

pub fn settings_notice(ui: &mut egui::Ui, color: Color32, text: &str) {
    ui.label(
        RichText::new(text)
            .color(readable_color(ui.visuals().panel_fill, color))
            .size(12.0),
    );
}

pub fn settings_text_edit(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    value: &mut String,
    hint: &str,
) -> egui::Response {
    let inner_width = (ui.available_width().min(360.0) - 22.0).max(80.0);
    settings_text_edit_width(ui, palette, value, hint, inner_width)
}

pub fn settings_text_edit_width(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    value: &mut String,
    hint: &str,
    width: f32,
) -> egui::Response {
    crate::themed_text_edit_singleline(ui, value, Theme::new(palette), |edit| {
        edit.hint_text(hint).desired_width(width)
    })
}

pub fn settings_segmented(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    labels: &[&str],
    selected: usize,
) -> Option<usize> {
    settings_segmented_unit(ui, palette, labels, selected, 82.0)
}

pub fn settings_segmented_ltr(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    labels: &[&str],
    selected: usize,
) -> Option<usize> {
    settings_segmented_unit(ui, palette, labels, selected, 68.0)
}

fn settings_segmented_unit(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    labels: &[&str],
    selected: usize,
    min_item_width: f32,
) -> Option<usize> {
    if labels.is_empty() {
        return None;
    }
    let mut changed = None;
    let natural_item_width = labels
        .iter()
        .map(|label| (label.len() as f32 * 8.5 + 24.0).max(min_item_width))
        .fold(min_item_width, f32::max);
    // Never exceed the column we were handed: a control wider than the available width spills
    // leftward (the control column lays out right-to-left) and paints over the row's label/help.
    let max_item_width = (ui.available_width() / labels.len() as f32).max(1.0);
    let item_width = natural_item_width.min(max_item_width);
    let size = Vec2::new(item_width * labels.len() as f32, 34.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let radius = egui::CornerRadius::same(palette.radius);
    ui.painter().rect_filled(rect, radius, palette.surface);
    ui.painter().rect_stroke(
        rect,
        radius,
        egui::Stroke::new(1.0, palette.border),
        egui::StrokeKind::Inside,
    );
    for (index, label) in labels.iter().enumerate() {
        let min = Pos2::new(rect.left() + item_width * index as f32, rect.top());
        let item = Rect::from_min_size(min, Vec2::new(item_width, rect.height()));
        if index > 0 {
            ui.painter().line_segment(
                [item.left_top(), item.left_bottom()],
                egui::Stroke::new(1.0, palette.border),
            );
        }
        let pointer_hovered = response.hover_pos().is_some_and(|pos| item.contains(pos));
        if pointer_hovered && index != selected {
            ui.painter()
                .rect_filled(item.shrink(3.0), egui::CornerRadius::same(5), palette.hover);
        }
        if index == selected {
            let selected_rect = item.shrink(3.0);
            ui.painter()
                .rect_filled(selected_rect, egui::CornerRadius::same(5), palette.accent);
        }
        let fill = if index == selected {
            palette.accent
        } else if pointer_hovered {
            palette.hover
        } else {
            palette.surface
        };
        let color = if index == selected {
            readable_color(fill, palette.text)
        } else {
            readable_color(fill, palette.subtext)
        };
        // Shrink the label to match a clamped item so long labels (e.g. "Drawn by system") stay
        // inside their cell instead of bleeding into the neighbour.
        let font_size = (12.5 * (item_width / natural_item_width)).clamp(9.5, 12.5);
        ui.painter().text(
            item.center(),
            egui::Align2::CENTER_CENTER,
            *label,
            egui::FontId::proportional(font_size),
            color,
        );
    }
    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let index = ((pos.x - rect.left()) / item_width).floor() as usize;
        if index < labels.len() && index != selected {
            changed = Some(index);
        }
    }
    changed
}

pub fn settings_color_picker(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    rgb: &mut [u8; 3],
) -> egui::Response {
    let mut style = (*ui.ctx().global_style()).clone();
    style.spacing.interact_size = Vec2::splat(30.0);
    for widget in [
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        widget.bg_fill = palette.border;
        widget.weak_bg_fill = palette.border;
        widget.expansion = 0.0;
    }
    ui.scope(|ui| {
        ui.set_style(style);
        let response = egui::color_picker::color_edit_button_srgb(ui, rgb);
        let swatch = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
        let border = swatch_border_color(palette, swatch);
        let hover = response.hovered();
        let stroke = if hover {
            egui::Stroke::new(2.0, readable_color(swatch, palette.accent))
        } else {
            egui::Stroke::new(1.5, border)
        };
        ui.painter().rect_stroke(
            response.rect,
            egui::CornerRadius::same(4),
            stroke,
            egui::StrokeKind::Inside,
        );
        if hover {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        response
    })
    .inner
}

fn swatch_border_color(palette: ThemePalette, swatch: Color32) -> Color32 {
    if contrast_ratio(swatch, palette.border) >= 3.0 {
        return palette.border;
    }
    [palette.text, palette.muted, Color32::BLACK, Color32::WHITE]
        .into_iter()
        .max_by(|a, b| {
            contrast_ratio(swatch, *a)
                .partial_cmp(&contrast_ratio(swatch, *b))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(Color32::BLACK)
}

pub struct NumberEditSpec<'a> {
    pub id_salt: &'a [&'a str],
    pub range: std::ops::RangeInclusive<f32>,
    pub suffix: &'a str,
    pub precision: usize,
    pub display_scale: f32,
}

pub fn settings_slider_with_edit(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    value: &mut f32,
    spec: NumberEditSpec<'_>,
) -> bool {
    let edit_id = number_edit_id(ui, spec.id_salt);
    let group_width = 190.0 + 8.0 + number_edit_outer_width(&spec);
    ui.allocate_ui_with_layout(
        Vec2::new(group_width, 34.0),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            let mut changed = settings_number_edit_with_id(ui, palette, value, &spec, edit_id);
            ui.add_space(8.0);
            if settings_slider(ui, palette, value, spec.range.clone()) {
                ui.memory_mut(|memory| {
                    memory
                        .data
                        .insert_temp(edit_id, format_number_value(*value, &spec));
                });
                changed = true;
            }
            changed
        },
    )
    .inner
}

pub fn settings_number_edit(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    value: &mut f32,
    spec: NumberEditSpec<'_>,
) -> bool {
    let edit_id = number_edit_id(ui, spec.id_salt);
    settings_number_edit_with_id(ui, palette, value, &spec, edit_id)
}

fn number_edit_id(ui: &mut egui::Ui, id_salt: &[&str]) -> egui::Id {
    ui.make_persistent_id(("settings-number-edit", id_salt.join(".")))
}

fn settings_number_edit_with_id(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    value: &mut f32,
    spec: &NumberEditSpec<'_>,
    edit_id: egui::Id,
) -> bool {
    let focused = ui.memory(|memory| memory.has_focus(edit_id));
    let mut text = ui
        .memory(|memory| memory.data.get_temp::<String>(edit_id))
        .unwrap_or_else(|| format_number_value(*value, spec));
    if !focused {
        text = format_number_value(*value, spec);
    }

    let fill = palette.surface;
    let response = egui::Frame::NONE
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .inner_margin(egui::Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.add_sized(
                [number_edit_inner_width(spec), 22.0],
                egui::TextEdit::singleline(&mut text)
                    .id(edit_id)
                    .text_color(readable_color(fill, palette.text))
                    .horizontal_align(egui::Align::RIGHT)
                    .vertical_align(egui::Align::Center)
                    .background_color(fill)
                    .frame(egui::Frame::NONE),
            )
        })
        .inner;

    ui.memory_mut(|memory| memory.data.insert_temp(edit_id, text.clone()));
    if response.changed()
        && let Some(parsed) = parse_number_value(&text, spec)
    {
        *value = parsed;
        return true;
    }
    if response.lost_focus() {
        ui.memory_mut(|memory| {
            memory
                .data
                .insert_temp(edit_id, format_number_value(*value, spec));
        });
    }
    false
}

fn number_edit_outer_width(spec: &NumberEditSpec<'_>) -> f32 {
    number_edit_inner_width(spec) + 16.0
}

fn number_edit_inner_width(spec: &NumberEditSpec<'_>) -> f32 {
    let start = format_number_value(*spec.range.start(), spec);
    let end = format_number_value(*spec.range.end(), spec);
    let widest = start.len().max(end.len()).max(6) as f32;
    (widest * 8.0 + 8.0).clamp(74.0, 112.0)
}

fn parse_number_value(text: &str, spec: &NumberEditSpec<'_>) -> Option<f32> {
    let trimmed = text.trim();
    let without_suffix = if spec.suffix.trim().is_empty() {
        trimmed
    } else {
        trimmed
            .strip_suffix(spec.suffix.trim())
            .unwrap_or(trimmed)
            .trim()
    };
    let number = without_suffix.parse::<f32>().ok()? / spec.display_scale;
    Some(number.clamp(*spec.range.start(), *spec.range.end()))
}

fn format_number_value(value: f32, spec: &NumberEditSpec<'_>) -> String {
    let displayed = value * spec.display_scale;
    format!("{:.*}{}", spec.precision, displayed, spec.suffix)
}

fn settings_slider(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
) -> bool {
    let size = Vec2::new(190.0, 26.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let start = *range.start();
    let end = *range.end();
    let mut normalized = ((*value - start) / (end - start)).clamp(0.0, 1.0);
    if (response.dragged() || response.clicked())
        && let Some(pos) = response.interact_pointer_pos()
    {
        normalized = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        *value = start + (end - start) * normalized;
    }
    let rail = Rect::from_center_size(rect.center(), Vec2::new(rect.width(), 9.0));
    ui.painter()
        .rect_filled(rail, egui::CornerRadius::same(5), palette.surface);
    ui.painter().rect_stroke(
        rail,
        egui::CornerRadius::same(5),
        egui::Stroke::new(1.0, palette.border),
        egui::StrokeKind::Inside,
    );
    let active = Rect::from_min_max(
        rail.min,
        Pos2::new(rail.left() + rail.width() * normalized, rail.bottom()),
    );
    ui.painter()
        .rect_filled(active, egui::CornerRadius::same(4), palette.accent);
    let thumb = Pos2::new(rail.left() + rail.width() * normalized, rail.center().y);
    ui.painter().circle_filled(
        thumb,
        10.0,
        readable_color(palette.surface, palette.subtext),
    );
    ui.painter()
        .circle_stroke(thumb, 10.0, egui::Stroke::new(2.0, palette.accent));
    response.changed() || response.dragged() || response.clicked()
}

pub fn path_row(ui: &mut egui::Ui, palette: ThemePalette, label: &str, path: &Path) {
    settings_row(ui, palette, label, "Read-only location.", |ui| {
        settings_value_chip(ui, palette, &path.display().to_string());
    });
}

fn settings_value_chip(ui: &mut egui::Ui, palette: ThemePalette, text: &str) {
    egui::Frame::NONE
        .fill(palette.surface)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .inner_margin(egui::Margin::symmetric(10, 5))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(palette.text).monospace());
        });
}
