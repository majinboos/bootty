use eframe::egui::{self, RichText};

use super::SettingsWindow;
use crate::{
    color::Color,
    config::{ChromeConfig, SegmentAlign, StatusSegment},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StatusBarPosition {
    Top,
    Bottom,
}

impl StatusBarPosition {
    const ALL: [Self; 2] = [Self::Top, Self::Bottom];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Top => "Top",
            Self::Bottom => "Bottom",
        }
    }

    pub(super) fn segment_key(self) -> &'static str {
        match self {
            Self::Top => "top-segment",
            Self::Bottom => "bottom-segment",
        }
    }

    fn list_id(self) -> &'static str {
        match self {
            Self::Top => "top_status_segments",
            Self::Bottom => "bottom_status_segments",
        }
    }

    fn selection_id(self) -> &'static str {
        match self {
            Self::Top => "settings_top_status_selected_segment",
            Self::Bottom => "settings_bottom_status_selected_segment",
        }
    }

    pub(super) fn segments(self, chrome: &ChromeConfig) -> &[StatusSegment] {
        match self {
            Self::Top => &chrome.top_segments,
            Self::Bottom => &chrome.bottom_segments,
        }
    }

    fn segments_mut(self, chrome: &mut ChromeConfig) -> &mut Vec<StatusSegment> {
        match self {
            Self::Top => &mut chrome.top_segments,
            Self::Bottom => &mut chrome.bottom_segments,
        }
    }
}

pub(super) fn ui(win: &mut SettingsWindow, ui: &mut egui::Ui) {
    let palette = win.palette;

    super::section(ui, palette, "BARS");
    super::settings_toggle_row(
        ui,
        palette,
        "Top bar",
        "Show the module bar above the terminal.",
        win.config.chrome.top_bar,
        |enabled| {
            win.config.chrome.top_bar = enabled;
            win.set_top_bar(enabled);
        },
    );
    super::settings_toggle_row(
        ui,
        palette,
        "Bottom bar",
        "Show the module bar below the terminal.",
        win.config.chrome.bottom_bar,
        |enabled| {
            win.config.chrome.bottom_bar = enabled;
            win.set_bool(&["chrome", "bottom-bar"], enabled);
        },
    );

    super::section(ui, palette, "STATUS BARS");
    super::settings_row(ui, palette, "Height", "Module strip height.", |ui| {
        let mut height = win.config.chrome.status_height;
        if super::settings_slider_with_edit(
            ui,
            palette,
            &mut height,
            super::NumberEditSpec {
                path: &["chrome", "status-height"],
                range: 20.0..=80.0,
                suffix: " px",
                precision: 1,
                display_scale: 1.0,
            },
        ) {
            win.config.chrome.status_height = height;
            win.set_f32(&["chrome", "status-height"], height);
        }
    });
    super::settings_toggle_row(
        ui,
        palette,
        "Hide tmux's own bar",
        "Avoid duplicate status bars when the tmux backend is active.",
        win.config.multiplexer.hide_tmux_status,
        |enabled| {
            win.config.multiplexer.hide_tmux_status = enabled;
            win.set_bool(&["multiplexer", "hide-tmux-status"], enabled);
        },
    );

    super::section(ui, palette, "MODULES");
    super::settings_notice(
        ui,
        palette.muted,
        "Arrange module instances, edit their sources, or create a new module here.",
    );
    ui.add_space(6.0);

    let selected_bar_id = ui.make_persistent_id("settings_status_selected_bar");
    let mut selected_bar = ui
        .memory(|memory| memory.data.get_temp(selected_bar_id).unwrap_or(0usize))
        .min(StatusBarPosition::ALL.len() - 1);
    let labels = StatusBarPosition::ALL.map(StatusBarPosition::label);
    if let Some(index) = super::settings_segmented(ui, palette, &labels, selected_bar) {
        selected_bar = index;
    }
    ui.memory_mut(|memory| memory.data.insert_temp(selected_bar_id, selected_bar));
    let position = StatusBarPosition::ALL[selected_bar];
    ui.add_space(8.0);

    let available = win
        .config_path
        .parent()
        .map(|parent| crate::extensions::available_module_names(&parent.join("status")))
        .unwrap_or_default();

    let mut changed = false;
    let mut remove_index: Option<usize> = None;
    let count = position.segments(&win.config.chrome).len();
    let selected_id = ui.make_persistent_id(position.selection_id());
    let mut selected: usize = ui
        .memory(|memory| memory.data.get_temp(selected_id).unwrap_or(0usize))
        .min(count.saturating_sub(1));

    let available_width = ui.available_width();
    let render_list = |ui: &mut egui::Ui,
                       win: &mut SettingsWindow,
                       selected: &mut usize,
                       remove_index: &mut Option<usize>| {
        super::reorderable_list(
            ui,
            palette,
            position.list_id(),
            count,
            |ui, index, handle| {
                segment_list_row(
                    win,
                    ui,
                    position,
                    SegmentListContext {
                        index,
                        selected,
                        remove_index,
                        handle,
                    },
                );
            },
        )
    };
    let reorder = if available_width >= 920.0 {
        let detail_width = 360.0;
        let list_width = (available_width - detail_width - 18.0).max(420.0);
        ui.horizontal_top(|ui| {
            let reorder = ui
                .allocate_ui_with_layout(
                    egui::Vec2::new(list_width, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| render_list(ui, win, &mut selected, &mut remove_index),
                )
                .inner;
            ui.add_space(18.0);
            if count > 0 {
                ui.allocate_ui_with_layout(
                    egui::Vec2::new(detail_width, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        segment_detail_panel(
                            win,
                            ui,
                            position,
                            SegmentDetailContext {
                                available: &available,
                                index: selected,
                                changed: &mut changed,
                            },
                        );
                    },
                );
            }
            reorder
        })
        .inner
    } else {
        let reorder = render_list(ui, win, &mut selected, &mut remove_index);
        if count > 0 {
            ui.add_space(12.0);
            segment_detail_panel(
                win,
                ui,
                position,
                SegmentDetailContext {
                    available: &available,
                    index: selected,
                    changed: &mut changed,
                },
            );
        }
        reorder
    };

    if let Some((from, slot)) = reorder {
        super::apply_reorder(position.segments_mut(&mut win.config.chrome), from, slot);
        changed = true;
    }
    if let Some(index) = remove_index {
        let segments = position.segments_mut(&mut win.config.chrome);
        segments.remove(index);
        if selected >= segments.len() {
            selected = selected.saturating_sub(1);
        }
        changed = true;
    }

    ui.add_space(8.0);
    if super::settings_button(ui, palette, "+ Add segment").clicked() {
        let module = available
            .first()
            .cloned()
            .unwrap_or_else(|| "clock".to_owned());
        position
            .segments_mut(&mut win.config.chrome)
            .push(StatusSegment {
                align: SegmentAlign::Left,
                module,
                ..StatusSegment::default()
            });
        changed = true;
    }
    if let Some(module) =
        super::modules::new_module_ui(win, ui, crate::extensions::ModuleKind::Status)
    {
        let segments = position.segments_mut(&mut win.config.chrome);
        segments.push(StatusSegment {
            align: SegmentAlign::Left,
            module,
            ..StatusSegment::default()
        });
        selected = segments.len() - 1;
        changed = true;
    }

    if changed {
        win.set_status_segments(position);
    }
    ui.memory_mut(|memory| memory.data.insert_temp(selected_id, selected));
    if let Some(module) = position
        .segments(&win.config.chrome)
        .get(selected)
        .map(|segment| segment.module.clone())
    {
        ui.add_space(16.0);
        super::section(ui, palette, "SOURCE");
        super::modules::source_editor(win, ui, crate::extensions::ModuleKind::Status, &module);
    }
}

fn segment_list_row(
    win: &mut SettingsWindow,
    ui: &mut egui::Ui,
    position: StatusBarPosition,
    ctx: SegmentListContext<'_>,
) {
    let palette = win.palette;
    let selected = *ctx.selected == ctx.index;
    // Allocate the row's clickable surface first so its interaction registers before the inner
    // controls; egui then lets the Remove button claim its own clicks while clicks anywhere else on
    // the row fall through to selection.
    let row_height = 54.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::Vec2::new(ui.available_width(), row_height),
        egui::Sense::click(),
    );
    let fill = if selected {
        palette.surface
    } else if response.hovered() {
        palette.hover
    } else {
        palette.pane
    };
    let radius = egui::CornerRadius::same(palette.radius);
    ui.painter().rect_filled(rect, radius, fill);
    ui.painter().rect_stroke(
        rect,
        radius,
        egui::Stroke::new(
            if selected { 2.0 } else { 1.0 },
            if selected {
                palette.primary
            } else {
                palette.border
            },
        ),
        egui::StrokeKind::Inside,
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let segment = &position.segments(&win.config.chrome)[ctx.index];
    let module_name = module_label(segment.module.as_str()).to_owned();
    let module_id = segment.module.clone();
    let icon_slug = segment.icon.clone();
    let align_text = align_label(segment.align);

    let content_rect = rect.shrink2(egui::Vec2::new(12.0, 8.0));
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(content_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    content.set_min_width(content_rect.width());
    content.spacing_mut().item_spacing.x = 8.0;
    content.add_space(22.0); // reserve the handle gutter; the grip is overlaid after
    segment_marker(&mut content, palette, icon_slug.as_deref());
    content.vertical(|ui| {
        ui.label(RichText::new(module_name).color(palette.text).strong());
        ui.label(
            RichText::new(module_id)
                .color(palette.muted)
                .size(11.0)
                .monospace(),
        );
    });
    content.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if super::settings_icon_button(ui, palette, "x", "Remove segment").clicked() {
            *ctx.remove_index = Some(ctx.index);
        }
        ui.add_space(6.0);
        super::settings_value_chip(ui, palette, align_text);
    });

    // Overlay the grip centered in the row's left gutter, registered last so it wins drags there
    // while the rest of the row stays click-to-select.
    let gutter = egui::Rect::from_min_max(
        content_rect.left_top(),
        egui::Pos2::new(content_rect.left() + 22.0, content_rect.bottom()),
    );
    ctx.handle.paint_in(ui, palette, gutter);

    if response.clicked() {
        *ctx.selected = ctx.index;
    }
    ui.add_space(8.0);
}

/// The segment's leading marker: a resolved iconflow icon when its slug is a known id, the literal
/// text when the user typed a glyph, or a small painted dot when no icon is set. The empty case is a
/// drawn shape rather than a font bullet so it always renders regardless of the UI font.
fn segment_marker(ui: &mut egui::Ui, palette: bootty_ui::ThemePalette, icon: Option<&str>) {
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(18.0), egui::Sense::hover());
    match icon {
        Some(slug) if crate::ui::icons::has_slug(slug) => {
            crate::ui::icons::paint_icon_slug(
                ui.painter(),
                slug,
                rect.center(),
                15.0,
                palette.primary,
            );
        }
        Some(literal) if !literal.is_empty() => {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                literal,
                egui::FontId::proportional(15.0),
                palette.primary,
            );
        }
        _ => {
            ui.painter()
                .circle_filled(rect.center(), 4.0, palette.primary);
        }
    }
}

fn segment_detail_panel(
    win: &mut SettingsWindow,
    ui: &mut egui::Ui,
    position: StatusBarPosition,
    ctx: SegmentDetailContext<'_>,
) {
    let palette = win.palette;
    // Fill whatever column we were allocated; control widths derive from this so nothing spills past
    // the frame regardless of which layout branch placed us.
    let panel_width = ui.available_width();
    let control_width = (panel_width - 24.0).max(120.0);
    egui::Frame::NONE
        .fill(palette.pane)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .inner_margin(egui::Margin::symmetric(12, 12))
        .show(ui, |ui| {
            ui.set_min_width(control_width);
            let segment = &mut position.segments_mut(&mut win.config.chrome)[ctx.index];
            ui.label(
                RichText::new("Module details")
                    .color(palette.subtext)
                    .strong()
                    .size(12.0),
            );
            ui.add_space(8.0);
            ui.label(
                RichText::new(module_label(segment.module.as_str()))
                    .color(palette.text)
                    .strong(),
            );
            ui.add_space(12.0);
            ui.label(RichText::new("Module").color(palette.muted).size(11.0));
            if ctx.available.is_empty() {
                let mut module = segment.module.clone();
                if super::settings_text_edit_width(
                    ui,
                    palette,
                    &mut module,
                    "module",
                    control_width,
                )
                .changed()
                {
                    segment.module = module;
                    *ctx.changed = true;
                }
            } else {
                let options: Vec<&str> = ctx.available.iter().map(String::as_str).collect();
                let selected = if segment.module.is_empty() {
                    "module"
                } else {
                    segment.module.as_str()
                };
                let current = options.iter().position(|option| *option == segment.module);
                if let Some(choice) = super::searchable_combo(
                    ui,
                    palette,
                    &format!("{}_module_{}", position.segment_key(), ctx.index),
                    selected,
                    control_width,
                    &options,
                    current,
                ) {
                    segment.module = options[choice].to_owned();
                    *ctx.changed = true;
                }
            }

            ui.add_space(12.0);
            ui.label(RichText::new("Alignment").color(palette.muted).size(11.0));
            let aligns = [
                SegmentAlign::Left,
                SegmentAlign::Center,
                SegmentAlign::Right,
            ];
            let labels = ["Left", "Center", "Right"];
            let current = aligns.iter().position(|a| *a == segment.align).unwrap_or(0);
            if let Some(selected) = super::settings_segmented_ltr(ui, palette, &labels, current)
                && aligns[selected] != segment.align
            {
                segment.align = aligns[selected];
                *ctx.changed = true;
            }

            ui.add_space(12.0);
            ui.label(RichText::new("Icon").color(palette.muted).size(11.0));
            let mut icon = segment.icon.clone().unwrap_or_default();
            if super::settings_text_edit_width(ui, palette, &mut icon, "icon", control_width)
                .changed()
            {
                segment.icon = (!icon.is_empty()).then_some(icon);
                *ctx.changed = true;
            }

            ui.add_space(12.0);
            *ctx.changed |=
                optional_color(ui, palette, "Foreground", &mut segment.fg, palette.subtext);
            ui.add_space(8.0);
            *ctx.changed |=
                optional_color(ui, palette, "Background", &mut segment.bg, palette.surface);
        });
}

struct SegmentListContext<'a> {
    index: usize,
    selected: &'a mut usize,
    remove_index: &'a mut Option<usize>,
    handle: &'a super::DragHandle,
}

struct SegmentDetailContext<'a> {
    available: &'a [String],
    index: usize,
    changed: &'a mut bool,
}

fn module_label(module: &str) -> &str {
    match module {
        "session" => "Session",
        "windows" => "Windows",
        "sysinfo" => "System info",
        "clock" => "Clock",
        other => other,
    }
}

fn align_label(align: SegmentAlign) -> &'static str {
    match align {
        SegmentAlign::Left => "Left",
        SegmentAlign::Center => "Center",
        SegmentAlign::Right => "Right",
    }
}

fn optional_color(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    label: &str,
    slot: &mut Option<Color>,
    seed: egui::Color32,
) -> bool {
    ui.label(RichText::new(label).size(11.0));
    let mut rgb = slot.map_or([seed.r(), seed.g(), seed.b()], |color| {
        [color.r, color.g, color.b]
    });
    let mut changed = false;
    if super::settings_color_picker(ui, palette, &mut rgb).changed() {
        *slot = Some(Color {
            r: rgb[0],
            g: rgb[1],
            b: rgb[2],
            a: 0xff,
        });
        changed = true;
    }
    if slot.is_some() && super::settings_icon_button(ui, palette, "x", "Clear color").clicked() {
        *slot = None;
        changed = true;
    }
    changed
}
