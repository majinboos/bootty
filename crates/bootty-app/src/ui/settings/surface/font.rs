use eframe::egui;

use super::SettingsSurface;
use bootty_font::parse_font_features;

pub(super) fn ui(win: &mut SettingsSurface, ui: &mut egui::Ui) {
    let palette = win.palette;
    let installed = win.font_families.clone();
    let options: Vec<&str> = installed.iter().map(String::as_str).collect();

    super::section(ui, palette, "UI FONT");
    let mut use_terminal = win.config.font.ui_use_terminal_family;
    super::settings_row(
        ui,
        palette,
        "Use terminal font",
        "Share the terminal font stack for Bootty's UI chrome.",
        |ui| {
            if super::settings_toggle(ui, palette, &mut use_terminal) {
                win.config.font.ui_use_terminal_family = use_terminal;
                win.writeback
                    .set_bool(&["font", "ui-use-terminal-family"], use_terminal);
            }
        },
    );
    if !win.config.font.ui_use_terminal_family {
        let mut ui_family = win.config.font.ui_family.clone();
        let ui_changed = font_stack_editor(
            ui,
            palette,
            &options,
            &mut ui_family,
            FontStackSpec {
                id_prefix: "ui_font",
                primary_title: "UI font",
                primary_help: "Used for settings, sidebar, and status chrome.",
                fallback_prefix: "UI fallback font",
                fallback_help: "Used when earlier UI fonts are missing a glyph.",
                add_label: "+ Add UI fallback",
            },
        );
        if ui_changed {
            win.config.font.ui_family = ui_family.clone();
            win.writeback
                .set_strings(&["font", "ui-family"], &ui_family);
        }
    }

    super::section(ui, palette, "TERMINAL FONT");
    let mut family = win.config.font.family.clone();
    let changed = font_stack_editor(
        ui,
        palette,
        &options,
        &mut family,
        FontStackSpec {
            id_prefix: "term_font",
            primary_title: "Primary font",
            primary_help: "Bootty tries this font first for terminal cells.",
            fallback_prefix: "Fallback font",
            fallback_help: "Used when earlier terminal fonts are missing a glyph.",
            add_label: "+ Add terminal fallback",
        },
    );
    if changed {
        win.config.font.family = family.clone();
        win.writeback.set_strings(&["font", "family"], &family);
    }

    super::section(ui, palette, "TERMINAL METRICS");
    win.setting(ui, "font.size");
    optional_slider(
        ui,
        win,
        MetricOverrideRow {
            label: "Cell width",
            help: "Leave automatic unless glyphs look too tight or too loose.",
            path: &["font", "cell-width"],
            range: 1.0..=64.0,
            suffix: "px",
            default_value: bootty_render::geometry::DEFAULT_CELL_WIDTH,
            field: |font| &mut font.cell_width,
        },
    );
    optional_slider(
        ui,
        win,
        MetricOverrideRow {
            label: "Cell height",
            help: "Leave automatic unless lines look clipped or too airy.",
            path: &["font", "cell-height"],
            range: 1.0..=128.0,
            suffix: "px",
            default_value: bootty_render::geometry::DEFAULT_LINE_HEIGHT,
            field: |font| &mut font.cell_height,
        },
    );
    win.setting(ui, "font.fit-cell-height");
    win.setting(ui, "font.fit-cell-width");

    super::section(ui, palette, "GLYPH BEHAVIOR");
    win.setting(ui, "font.baseline-adjustment");
    win.setting(ui, "font.underline-position");
    win.setting(ui, "font.underline-thickness");
    font_feature_picker(win, ui);
}

struct FontStackSpec<'a> {
    /// Unique per-stack id root so the UI and terminal stacks never share combo ids (which would
    /// wire one stack's dropdown to the other's).
    id_prefix: &'a str,
    primary_title: &'a str,
    primary_help: &'a str,
    fallback_prefix: &'a str,
    fallback_help: &'a str,
    add_label: &'a str,
}

fn font_stack_editor(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    options: &[&str],
    family: &mut Vec<String>,
    spec: FontStackSpec<'_>,
) -> bool {
    let mut changed = false;
    let mut remove: Option<usize> = None;
    let reorder = super::reorderable_list(
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
    if super::settings_button(ui, palette, spec.add_label).clicked() {
        family.push(String::new());
        changed = true;
    }
    if let Some((from, slot)) = reorder {
        super::apply_reorder(family, from, slot);
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
    handle: &'a super::DragHandle,
}

/// One font-stack entry. Laid out naturally (no height measurement); the grip is overlaid into the
/// finished row rect afterwards so it stays vertically centered, and separators sit between entries
/// only — the last entry carries no trailing border.
fn font_stack_row(ui: &mut egui::Ui, palette: bootty_ui::ThemePalette, row: FontStackRow<'_>) {
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
                if super::settings_icon_button(ui, palette, "x", "Remove font").clicked() {
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
                if let Some(choice) = super::searchable_combo(
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

fn font_feature_picker(win: &mut SettingsSurface, ui: &mut egui::Ui) {
    let palette = win.palette;
    let mut features = win
        .config
        .font
        .features
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    super::settings_row(
        ui,
        palette,
        "OpenType features",
        "Comma-separated tags such as +liga, -kern, +zero, or +ss01.",
        |ui| {
            if super::settings_text_edit(ui, palette, &mut features, "+liga, -kern").changed() {
                write_features(win, &features);
            }
        },
    );
}

fn write_features(win: &mut SettingsSurface, features: &str) {
    let mut parsed = Vec::new();
    for feature in parse_font_features(features) {
        if !parsed.contains(&feature) {
            parsed.push(feature);
        }
    }
    win.config.font.features = parsed.clone();
    if parsed.is_empty() {
        win.writeback.remove(&["font", "features"]);
    } else {
        let values = parsed.iter().map(ToString::to_string).collect::<Vec<_>>();
        win.writeback.set_strings(&["font", "features"], &values);
    }
}

struct MetricOverrideRow<'a> {
    label: &'a str,
    help: &'a str,
    path: &'a [&'a str],
    range: std::ops::RangeInclusive<f32>,
    suffix: &'a str,
    /// Shown while the row is on "Auto": the value the renderer would pick anyway.
    default_value: f32,
    field: fn(&mut bootty_config::config::FontConfig) -> &mut Option<f32>,
}

fn optional_slider(ui: &mut egui::Ui, win: &mut SettingsSurface, row: MetricOverrideRow<'_>) {
    let value = (row.field)(&mut win.config.font);
    if super::optional_number_row(
        ui,
        win.palette,
        value,
        row.default_value,
        super::NumberRow {
            label: row.label,
            help: row.help,
            path: row.path,
            range: row.range,
            suffix: row.suffix,
            scale: 1.0,
        },
    ) {
        match *value {
            Some(value) => win.writeback.set_f32(row.path, value),
            None => win.writeback.remove(row.path),
        }
    }
}
