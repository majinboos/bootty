use eframe::egui;

use super::SettingsWindow;
use bootty_font::{FontFeature, parse_font_features};

pub(super) fn ui(win: &mut SettingsWindow, ui: &mut egui::Ui) {
    let palette = win.palette;
    let installed = win
        .font_families
        .get_or_insert_with(installed_font_families)
        .clone();
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
                win.set_bool(&["font", "ui-use-terminal-family"], use_terminal);
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
            win.set_strings(&["font", "ui-family"], &ui_family);
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
        win.set_strings(&["font", "family"], &family);
    }

    super::section(ui, palette, "TERMINAL METRICS");
    slider(
        ui,
        win,
        MetricSliderRow {
            label: "Font size",
            help: "Main terminal text size.",
            path: &["font", "size"],
            range: 6.0..=48.0,
            suffix: "pt",
            field: |font| &mut font.size,
        },
    );
    optional_slider(
        ui,
        win,
        MetricOverrideRow {
            label: "Cell width",
            help: "Leave automatic unless glyphs look too tight or too loose.",
            path: &["font", "cell-width"],
            range: 1.0..=64.0,
            suffix: "px",
            default_value: crate::geometry::DEFAULT_CELL_WIDTH,
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
            default_value: crate::geometry::DEFAULT_LINE_HEIGHT,
            field: |font| &mut font.cell_height,
        },
    );
    let mut fit_cell_height = win.config.font.fit_cell_height;
    super::settings_row(
        ui,
        palette,
        "Fit rows to window",
        "Stretch row spacing so terminal content fills available height.",
        |ui| {
            if super::settings_toggle(ui, palette, &mut fit_cell_height) {
                win.config.font.fit_cell_height = fit_cell_height;
                win.set_bool(&["font", "fit-cell-height"], fit_cell_height);
            }
        },
    );
    let mut fit_cell_width = win.config.font.fit_cell_width;
    super::settings_row(
        ui,
        palette,
        "Fit columns to window",
        "Stretch column spacing so terminal content fills available width (avoids a gap on the right, common with split panes).",
        |ui| {
            if super::settings_toggle(ui, palette, &mut fit_cell_width) {
                win.config.font.fit_cell_width = fit_cell_width;
                win.set_bool(&["font", "fit-cell-width"], fit_cell_width);
            }
        },
    );

    super::section(ui, palette, "GLYPH BEHAVIOR");
    slider(
        ui,
        win,
        MetricSliderRow {
            label: "Baseline adjustment",
            help: "Move glyphs up or down inside each cell.",
            path: &["font", "baseline-adjustment"],
            range: -12.0..=12.0,
            suffix: "px",
            field: |font| &mut font.baseline_adjustment,
        },
    );
    slider(
        ui,
        win,
        MetricSliderRow {
            label: "Underline position",
            help: "Tune where underline decoration is drawn.",
            path: &["font", "underline-position"],
            range: -12.0..=12.0,
            suffix: "px",
            field: |font| &mut font.underline_position,
        },
    );
    slider(
        ui,
        win,
        MetricSliderRow {
            label: "Underline thickness",
            help: "Tune underline stroke thickness.",
            path: &["font", "underline-thickness"],
            range: 0.0..=8.0,
            suffix: "px",
            field: |font| &mut font.underline_thickness,
        },
    );
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

fn font_feature_picker(win: &mut SettingsWindow, ui: &mut egui::Ui) {
    let palette = win.palette;
    let mut features = win
        .config
        .font
        .features
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    egui::Frame::NONE
        .fill(palette.pane)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new("Font features")
                            .color(palette.text)
                            .strong(),
                    );
                    ui.label(
                        egui::RichText::new("OpenType feature tags written to font.features.")
                            .color(palette.muted)
                            .size(11.0),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Clear").clicked() {
                        features.clear();
                        write_feature_values(win, &features);
                    }
                });
            });
            ui.add_space(8.0);
            let card_width = ((ui.available_width() - 14.0) * 0.5).clamp(220.0, 360.0);
            for chunk in FONT_FEATURES.chunks(2) {
                ui.horizontal(|ui| {
                    for feature in chunk {
                        feature_option(ui, win, &mut features, feature, card_width);
                    }
                });
            }
            ui.add_space(8.0);
            let mut raw = features.join(", ");
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Advanced").color(palette.muted));
                if super::settings_text_edit_width(ui, palette, &mut raw, "+liga, -kern", 300.0)
                    .changed()
                {
                    write_features(win, &raw);
                }
            });
        });
}

fn feature_option(
    ui: &mut egui::Ui,
    win: &mut SettingsWindow,
    features: &mut Vec<String>,
    feature: &FontFeatureOption,
    card_width: f32,
) {
    let palette = win.palette;
    let Some(token) = normalized_feature(feature.token) else {
        return;
    };
    let enabled = features
        .iter()
        .any(|value| normalized_feature(value).as_deref() == Some(token.as_str()));
    let (rect, response) =
        ui.allocate_exact_size(egui::Vec2::new(card_width, 58.0), egui::Sense::click());
    let fill = if enabled {
        palette.surface
    } else if response.hovered() {
        palette.hover
    } else {
        palette.pane
    };
    let stroke = egui::Stroke::new(
        if enabled { 2.0 } else { 1.0 },
        if enabled {
            palette.primary
        } else {
            palette.border
        },
    );
    ui.painter().rect_filled(rect, palette.radius, fill);
    ui.painter()
        .rect_stroke(rect, palette.radius, stroke, egui::StrokeKind::Inside);
    ui.painter().text(
        rect.left_top() + egui::vec2(10.0, 8.0),
        egui::Align2::LEFT_TOP,
        format!("{}  {}", feature.token, feature.label),
        egui::TextStyle::Button.resolve(ui.style()),
        palette.text,
    );
    ui.painter().text(
        rect.left_top() + egui::vec2(10.0, 31.0),
        egui::Align2::LEFT_TOP,
        feature.description,
        egui::TextStyle::Small.resolve(ui.style()),
        palette.muted,
    );
    if response.clicked() {
        if enabled {
            features.retain(|value| normalized_feature(value).as_deref() != Some(token.as_str()));
        } else {
            features.push(token);
        }
        write_feature_values(win, features);
    }
}

struct FontFeatureOption {
    token: &'static str,
    label: &'static str,
    description: &'static str,
}

const FONT_FEATURES: &[FontFeatureOption] = &[
    FontFeatureOption {
        token: "+liga",
        label: "Standard ligatures",
        description: "Combines common glyph sequences such as fi and fl.",
    },
    FontFeatureOption {
        token: "-liga",
        label: "Disable ligatures",
        description: "Keeps all characters separate when a font enables ligatures.",
    },
    FontFeatureOption {
        token: "+calt",
        label: "Contextual alternates",
        description: "Allows glyphs to adapt based on neighboring characters.",
    },
    FontFeatureOption {
        token: "+dlig",
        label: "Discretionary ligatures",
        description: "Enables optional decorative ligatures when the font has them.",
    },
    FontFeatureOption {
        token: "+kern",
        label: "Kerning",
        description: "Applies pair spacing supplied by the font.",
    },
    FontFeatureOption {
        token: "+zero",
        label: "Slashed zero",
        description: "Distinguishes zero from capital O when supported.",
    },
    FontFeatureOption {
        token: "+tnum",
        label: "Tabular numbers",
        description: "Uses equal-width digits for aligned columns.",
    },
    FontFeatureOption {
        token: "+onum",
        label: "Oldstyle numbers",
        description: "Uses text-style numerals when available.",
    },
    FontFeatureOption {
        token: "+ss01",
        label: "Stylistic set 1",
        description: "Enables the font's first stylistic alternate set.",
    },
    FontFeatureOption {
        token: "+ss02",
        label: "Stylistic set 2",
        description: "Enables the font's second stylistic alternate set.",
    },
];

fn write_feature_values(win: &mut SettingsWindow, features: &[String]) {
    let mut normalized = Vec::new();
    for feature in features {
        if let Some(value) = normalized_feature(feature)
            && !normalized.iter().any(|existing| existing == &value)
        {
            normalized.push(value);
        }
    }
    write_features(win, &normalized.join(", "));
}

fn write_features(win: &mut SettingsWindow, features: &str) {
    let mut parsed = Vec::new();
    for feature in parse_font_features(features) {
        if !parsed.contains(&feature) {
            parsed.push(feature);
        }
    }
    win.config.font.features = parsed.clone();
    if parsed.is_empty() {
        win.remove(&["font", "features"]);
    } else {
        let values = parsed.iter().map(ToString::to_string).collect::<Vec<_>>();
        win.set_strings(&["font", "features"], &values);
    }
}

fn normalized_feature(value: &str) -> Option<String> {
    FontFeature::parse(value).map(|feature| feature.to_string())
}

fn slider(ui: &mut egui::Ui, win: &mut SettingsWindow, row: MetricSliderRow<'_>) {
    super::settings_row(ui, win.palette, row.label, row.help, |ui| {
        let mut value = *(row.field)(&mut win.config.font);
        if super::settings_slider_with_edit(
            ui,
            win.palette,
            &mut value,
            super::NumberEditSpec {
                path: row.path,
                range: row.range,
                suffix: row.suffix,
                precision: 1,
                display_scale: 1.0,
            },
        ) {
            *(row.field)(&mut win.config.font) = value;
            win.set_f32(row.path, value);
        }
    });
}

struct MetricSliderRow<'a> {
    label: &'a str,
    help: &'a str,
    path: &'a [&'a str],
    range: std::ops::RangeInclusive<f32>,
    suffix: &'a str,
    field: fn(&mut crate::config::FontConfig) -> &mut f32,
}

struct MetricOverrideRow<'a> {
    label: &'a str,
    help: &'a str,
    path: &'a [&'a str],
    range: std::ops::RangeInclusive<f32>,
    suffix: &'a str,
    default_value: f32,
    field: fn(&mut crate::config::FontConfig) -> &mut Option<f32>,
}

fn optional_slider(ui: &mut egui::Ui, win: &mut SettingsWindow, row: MetricOverrideRow<'_>) {
    super::settings_row(ui, win.palette, row.label, row.help, |ui| {
        let current = *(row.field)(&mut win.config.font);
        let mut value = current.unwrap_or(row.default_value);
        if super::settings_slider_with_edit(
            ui,
            win.palette,
            &mut value,
            super::NumberEditSpec {
                path: row.path,
                range: row.range.clone(),
                suffix: row.suffix,
                precision: 1,
                display_scale: 1.0,
            },
        ) {
            *(row.field)(&mut win.config.font) = Some(value);
            win.set_f32(row.path, value);
        }
        let mut automatic = current.is_none();
        if super::settings_toggle(ui, win.palette, &mut automatic) {
            if automatic {
                *(row.field)(&mut win.config.font) = None;
                win.remove(row.path);
            } else {
                // Turning auto off must write a concrete value; otherwise
                // `automatic` re-derives to true next frame and the toggle snaps
                // back, leaving the user unable to switch to manual.
                *(row.field)(&mut win.config.font) = Some(value);
                win.set_f32(row.path, value);
            }
        }
        ui.label(egui::RichText::new("Auto").color(win.palette.muted));
    });
}

fn installed_font_families() -> Vec<String> {
    let database = bootty_render::font_database::system_font_database();
    let mut names: Vec<String> = database
        .faces()
        .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}
