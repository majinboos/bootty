use eframe::egui;

use bootty_ui::font_stack::{FontStackSpec, font_stack_editor};

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
