use bootty_config::{
    color::Color,
    config::{ChromeConfig, SegmentAlign, StatusSegment},
};
use eframe::egui::{self, RichText};

use bootty_extension::{ModuleSources, SurfacePlacement};

use super::SettingsSurface;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StatusBarPosition {
    Top,
    Bottom,
}

impl StatusBarPosition {
    const ALL: [Self; 2] = [Self::Top, Self::Bottom];

    fn metadata(self) -> (&'static str, &'static str, &'static str, &'static str) {
        match self {
            Self::Top => (
                "Top",
                "top-segment",
                "top_status_segments",
                "settings_top_status_selected_segment",
            ),
            Self::Bottom => (
                "Bottom",
                "bottom-segment",
                "bottom_status_segments",
                "settings_bottom_status_selected_segment",
            ),
        }
    }

    pub(super) fn label(self) -> &'static str {
        self.metadata().0
    }

    pub(super) fn segment_key(self) -> &'static str {
        self.metadata().1
    }

    fn list_id(self) -> &'static str {
        self.metadata().2
    }

    fn selection_id(self) -> &'static str {
        self.metadata().3
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

pub(super) fn ui(win: &mut SettingsSurface, ui: &mut egui::Ui, sources: ModuleSources<'_>) {
    let palette = win.palette;

    super::modules::scan_error_notice(ui, palette, &sources);
    status_preview(ui, palette, &win.config.chrome);
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
    win.setting(ui, "chrome.bottom-bar");

    super::section(ui, palette, "STATUS BARS");
    win.setting(ui, "chrome.status-height");
    win.setting(ui, "multiplexer.hide-tmux-status");

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

    // Only surfaces a module declared for the status placement can render in a bar. Before any
    // module has published one, fall back to file stems so the picker is not empty on a cold start.
    let mut available = sources.declared_for(SurfacePlacement::Status);
    if available.is_empty() {
        available = sources
            .identities
            .iter()
            .filter_map(|identity| {
                identity
                    .as_ref()
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(str::to_owned)
            })
            .collect();
        available.sort();
        available.dedup();
    }
    let selected_id = ui.make_persistent_id(position.selection_id());
    let count = position.segments(&win.config.chrome).len();
    let mut selected: usize = ui
        .memory(|memory| memory.data.get_temp(selected_id).unwrap_or(0usize))
        .min(count.saturating_sub(1));

    super::modules::settings_pane(
        win,
        ui,
        |win, ui| {
            let mut changed = false;
            let mut remove_index = None;
            let reorder = super::reorderable_list(
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
                            selected: &mut selected,
                            remove_index: &mut remove_index,
                            handle,
                        },
                    );
                },
            );
            if let Some((from, slot)) = reorder {
                super::apply_reorder(position.segments_mut(&mut win.config.chrome), from, slot);
                changed = true;
            }
            if let Some(index) = remove_index {
                let segments = position.segments_mut(&mut win.config.chrome);
                segments.remove(index);
                selected = selected.min(segments.len().saturating_sub(1));
                changed = true;
            }

            ui.add_space(4.0);
            if super::settings_button(ui, palette, "+ Add segment").clicked() {
                let module = available
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "clock".to_owned());
                let segments = position.segments_mut(&mut win.config.chrome);
                segments.push(StatusSegment {
                    align: SegmentAlign::Left,
                    module,
                    ..StatusSegment::default()
                });
                selected = segments.len() - 1;
                changed = true;
            }
            if let Some(identity) = super::modules::new_module_ui(win, ui) {
                let module = identity
                    .as_ref()
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("extension")
                    .to_owned();
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
            selected
        },
        |win, ui, selected| {
            if position
                .segments(&win.config.chrome)
                .get(selected)
                .is_none()
            {
                super::settings_notice(ui, palette.muted, "No status modules configured.");
                return;
            }

            let mut changed = false;
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
            if changed {
                win.set_status_segments(position);
            }
            let module = position.segments(&win.config.chrome)[selected]
                .module
                .clone();
            ui.add_space(12.0);
            super::modules::source_editor_for_surface(win, ui, &module, &sources);
        },
    );
}

fn segment_list_row(
    win: &mut SettingsSurface,
    ui: &mut egui::Ui,
    position: StatusBarPosition,
    ctx: SegmentListContext<'_>,
) {
    let palette = win.palette;
    let segment = &position.segments(&win.config.chrome)[ctx.index];
    let label = format!(
        "{} · {}",
        super::module_display_name(&segment.module),
        align_label(segment.align)
    );
    let response = super::modules::module_selector_row(
        ui,
        palette,
        &label,
        *ctx.selected == ctx.index,
        Some(ctx.handle),
        true,
        |ui| {
            if super::settings_icon_button(ui, palette, "x", "Remove segment").clicked() {
                *ctx.remove_index = Some(ctx.index);
            }
        },
    );
    if response.clicked() {
        *ctx.selected = ctx.index;
    }
}

fn segment_detail_panel(
    win: &mut SettingsSurface,
    ui: &mut egui::Ui,
    position: StatusBarPosition,
    ctx: SegmentDetailContext<'_>,
) {
    let palette = win.palette;
    egui::Frame::NONE
        .fill(palette.pane)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            let segment = &mut position.segments_mut(&mut win.config.chrome)[ctx.index];
            ui.columns(2, |columns| {
                columns[0].label(RichText::new("Module").color(palette.muted).size(11.0));
                let width = columns[0].available_width();
                if ctx.available.is_empty() {
                    let mut module = segment.module.clone();
                    if super::settings_text_edit_width(
                        &mut columns[0],
                        palette,
                        &mut module,
                        "module",
                        width,
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
                        &mut columns[0],
                        palette,
                        &format!("{}_module_{}", position.segment_key(), ctx.index),
                        selected,
                        width,
                        &options,
                        current,
                    ) {
                        segment.module = options[choice].to_owned();
                        *ctx.changed = true;
                    }
                }

                columns[1].label(RichText::new("Alignment").color(palette.muted).size(11.0));
                let aligns = [
                    SegmentAlign::Left,
                    SegmentAlign::Center,
                    SegmentAlign::Right,
                ];
                let labels = ["Left", "Center", "Right"];
                let current = aligns.iter().position(|a| *a == segment.align).unwrap_or(0);
                if let Some(selected) =
                    super::settings_segmented_ltr(&mut columns[1], palette, &labels, current)
                    && aligns[selected] != segment.align
                {
                    segment.align = aligns[selected];
                    *ctx.changed = true;
                }
            });

            ui.add_space(6.0);
            ui.columns(3, |columns| {
                columns[0].label(
                    RichText::new("Icon (optional)")
                        .color(palette.muted)
                        .size(11.0),
                );
                let mut icon = segment.icon.clone().unwrap_or_default();
                let width = columns[0].available_width();
                if super::settings_text_edit_width(
                    &mut columns[0],
                    palette,
                    &mut icon,
                    "lucide slug or glyph",
                    width,
                )
                .changed()
                {
                    let icon = icon.trim();
                    segment.icon = (!icon.is_empty()).then(|| icon.to_owned());
                    *ctx.changed = true;
                }

                *ctx.changed |= optional_color(
                    &mut columns[1],
                    palette,
                    "Foreground",
                    &mut segment.fg,
                    palette.subtext,
                );
                *ctx.changed |= optional_color(
                    &mut columns[2],
                    palette,
                    "Background",
                    &mut segment.bg,
                    palette.surface,
                );
            });
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

fn optional_color(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    label: &str,
    slot: &mut Option<Color>,
    seed: egui::Color32,
) -> bool {
    ui.label(RichText::new(label).size(11.0));
    ui.horizontal(|ui| super::optional_color_edit(ui, palette, slot, seed, false, &[label]))
        .inner
}

fn align_label(align: SegmentAlign) -> &'static str {
    match align {
        SegmentAlign::Left => "Left",
        SegmentAlign::Center => "Center",
        SegmentAlign::Right => "Right",
    }
}

/// A mock of the configured bars, one chip per segment at its true alignment, in the segment's own
/// colours. Previews the *configuration* — height, alignment, colours, icons — which is what this
/// page edits; live module content is what the module editor's own preview shows.
fn status_preview(ui: &mut egui::Ui, palette: bootty_ui::ThemePalette, chrome: &ChromeConfig) {
    egui::Frame::NONE
        .fill(palette.mantle)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            let height = chrome.status_height.clamp(24.0, 40.0);
            let background = chrome
                .status_background
                .map_or(palette.mantle, crate::theme::config_color32);
            let bars = [
                (chrome.top_bar, &chrome.top_segments),
                (chrome.bottom_bar, &chrome.bottom_segments),
            ];
            let mut drawn = false;
            for (visible, segments) in bars {
                if !visible {
                    continue;
                }
                if drawn {
                    ui.add_space(6.0);
                }
                status_preview_bar(ui, palette, height, background, segments);
                drawn = true;
            }
            if !drawn {
                ui.label(RichText::new("Both module bars are hidden.").color(palette.muted));
            }
        });
    ui.add_space(10.0);
}

fn status_preview_bar(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    height: f32,
    background: egui::Color32,
    segments: &[StatusSegment],
) {
    const CHIP: egui::Vec2 = egui::vec2(84.0, 24.0);
    const STRIDE: f32 = 92.0;
    let (bar, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    ui.painter()
        .rect_filled(bar, egui::CornerRadius::same(palette.radius), background);

    for (align, anchor) in [
        (SegmentAlign::Left, bar.left() + 10.0),
        (SegmentAlign::Center, bar.center().x),
        (SegmentAlign::Right, bar.right() - 10.0),
    ] {
        let group = segments
            .iter()
            .filter(|segment| segment.align == align)
            .collect::<Vec<_>>();
        let width = group.len() as f32 * STRIDE;
        let mut x = match align {
            SegmentAlign::Left => anchor,
            SegmentAlign::Center => anchor - width * 0.5,
            SegmentAlign::Right => anchor - width,
        };
        for segment in group {
            let fill = segment
                .bg
                .map_or(palette.hover, crate::theme::config_color32);
            let chip =
                egui::Rect::from_min_size(egui::pos2(x, bar.center().y - CHIP.y * 0.5), CHIP);
            ui.painter()
                .rect_filled(chip, egui::CornerRadius::same(5), fill);
            ui.painter().rect_stroke(
                chip,
                egui::CornerRadius::same(5),
                egui::Stroke::new(1.0, palette.border),
                egui::StrokeKind::Inside,
            );
            ui.painter().text(
                chip.center(),
                egui::Align2::CENTER_CENTER,
                segment.icon.as_deref().unwrap_or(&segment.module),
                egui::FontId::monospace(12.0),
                bootty_ui::readable_color(
                    fill,
                    segment
                        .fg
                        .map_or(palette.text, crate::theme::config_color32),
                ),
            );
            x += STRIDE;
        }
    }
}
