//! Render settings from their specs.
//!
//! One function per value kind, driven by [`SettingSpec`], replaces the hand-written
//! read/widget/write block each row used to carry. The display value comes from the draft
//! document, falling back to the spec's default, so no page keeps a per-field copy of the accepted
//! config and nothing here is keyed on a revision — an in-progress draft survives an accepted
//! rebind for free.

use bootty_config::config::BoottyConfig;
use bootty_config::settings_schema::{
    NumberControl, SettingKind, SettingSpec, SettingValue, SettingsSchema,
};
use bootty_ui::ThemePalette;
use eframe::egui;

use super::writeback::SettingsWriteback;
use super::{NumberEditSpec, section, settings_number_edit, settings_row, settings_segmented};
use super::{searchable_combo, settings_slider_with_edit, settings_text_edit, settings_toggle};

/// Everything the schema renderer needs for one frame.
pub(super) struct SettingsRenderContext<'a> {
    pub palette: ThemePalette,
    /// The draft document: both the display source and the write target.
    pub draft: &'a mut SettingsWriteback,
    /// The accepted config, read only for a spec's fallback when the draft document says nothing
    /// about its key. It is the effective config, so a value coming from an `include`d file still
    /// displays as itself rather than as the built-in default.
    pub accepted: &'a BoottyConfig,
}

/// Render every spec on `page`, emitting a section header whenever the section changes.
///
/// `after_section` runs once at the end of each section, so a page that still needs a hand-written
/// control can keep it in its original position among the schema rows.
pub(super) fn render_page(
    ui: &mut egui::Ui,
    ctx: &mut SettingsRenderContext<'_>,
    schema: &SettingsSchema,
    page: &str,
    mut after_section: impl FnMut(&mut egui::Ui, &mut SettingsRenderContext<'_>, &str),
) {
    let mut current_section: Option<String> = None;
    for spec in schema.page(page) {
        if current_section.as_deref() != Some(spec.section.as_ref()) {
            if let Some(previous) = current_section.take() {
                after_section(ui, ctx, &previous);
            }
            section(ui, ctx.palette, &spec.section);
            current_section = Some(spec.section.to_string());
        }
        render_setting(ui, ctx, spec);
    }
    if let Some(last) = current_section {
        after_section(ui, ctx, &last);
    }
}

/// Render one spec. Public to the settings module so a hand-written page can place a single
/// schema-backed row among its own controls.
pub(super) fn render_setting(
    ui: &mut egui::Ui,
    ctx: &mut SettingsRenderContext<'_>,
    spec: &SettingSpec,
) {
    let path = spec.path_parts();
    let current = ctx
        .draft
        .value_of(spec)
        .unwrap_or_else(|| spec.default_value(ctx.accepted));
    let palette = ctx.palette;

    match &spec.kind {
        SettingKind::Bool => {
            let mut value = current.as_bool().unwrap_or_default();
            settings_row(ui, palette, &spec.label, &spec.help, |ui| {
                if settings_toggle(ui, palette, &mut value) {
                    ctx.draft.write(spec, &SettingValue::Bool(value));
                }
            });
        }
        SettingKind::Text {
            placeholder,
            optional,
        } => {
            let mut value = current.as_str().unwrap_or_default().to_owned();
            settings_row(ui, palette, &spec.label, &spec.help, |ui| {
                if settings_text_edit(ui, palette, &mut value, placeholder).changed() {
                    if *optional && value.trim().is_empty() {
                        // An optional value clears its key so the built-in default returns.
                        ctx.draft.remove(&path);
                    } else {
                        ctx.draft.write(spec, &SettingValue::Text(value.clone()));
                    }
                }
            });
        }
        SettingKind::Number {
            range,
            control,
            suffix,
            display_scale,
        } => {
            let mut value = current.as_number().unwrap_or_default();
            settings_row(ui, palette, &spec.label, &spec.help, |ui| {
                let edit = NumberEditSpec {
                    id_salt: &path,
                    range: range.clone(),
                    suffix,
                    precision: 1,
                    display_scale: *display_scale,
                };
                let changed = match control {
                    NumberControl::Edit => settings_number_edit(ui, palette, &mut value, edit),
                    NumberControl::Slider => {
                        settings_slider_with_edit(ui, palette, &mut value, edit)
                    }
                };
                if changed {
                    ctx.draft.write(spec, &SettingValue::Number(value));
                }
            });
        }
        SettingKind::Choice { options } => {
            let labels: Vec<&str> = options.iter().map(|option| option.label.as_ref()).collect();
            let token = current.as_str().unwrap_or_default().to_owned();
            let selected = options.iter().position(|option| option.token == token);
            settings_row(ui, palette, &spec.label, &spec.help, |ui| {
                let Some(selected) = selected else {
                    // A token the options do not cover: leave it alone rather than silently
                    // rewriting a value this build does not understand.
                    ui.label(egui::RichText::new(&token).color(palette.muted));
                    return;
                };
                // Short lists read better as segments; longer ones need the filterable combo.
                let next = if labels.len() <= 5 {
                    settings_segmented(ui, palette, &labels, selected)
                } else {
                    searchable_combo(
                        ui,
                        palette,
                        &spec.id(),
                        labels[selected],
                        220.0,
                        &labels,
                        Some(selected),
                    )
                };
                if let Some(index) = next {
                    let token = options[index].token.to_string();
                    ctx.draft.write(spec, &SettingValue::Token(token));
                }
            });
        }
    }
}
