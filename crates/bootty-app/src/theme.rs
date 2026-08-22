use bootty_config::{
    color::Color,
    config::{AppearanceVariant, BoottyConfig, ColorConfig},
};
use bootty_ui::{Theme, ThemePalette, UiColorConfig};
use eframe::egui::Color32;

pub fn theme_from_config(config: &BoottyConfig, variant: AppearanceVariant) -> Theme {
    Theme::new(theme_palette_from_config(config, variant))
}

pub fn theme_palette_from_config(
    config: &BoottyConfig,
    variant: AppearanceVariant,
) -> ThemePalette {
    theme_palette_from_colors(config.colors_for_appearance(variant))
}

pub fn theme_palette_from_colors(colors: &ColorConfig) -> ThemePalette {
    ThemePalette::from_config(ui_color_config_from_colors(colors))
}

fn ui_color_config_from_colors(colors: &ColorConfig) -> UiColorConfig {
    let mut palette = [None; 16];
    for (slot, color) in palette.iter_mut().zip(colors.palette.iter()) {
        *slot = Some(config_color32(*color));
    }
    UiColorConfig {
        background: colors.background.map(config_color32),
        foreground: colors.foreground.map(config_color32),
        palette,
    }
}

pub(crate) fn config_color32(color: Color) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r, color.g, color.b, color.a)
}

/// Named theme colors as `#rrggbb` strings, exposed to Lua extensions as `bootty.theme.*` so
/// extensions style themselves with palette tokens instead of hardcoded hex.
pub fn theme_tokens(config: &BoottyConfig, variant: AppearanceVariant) -> Vec<(String, String)> {
    let palette = theme_palette_from_config(config, variant);
    let hex = |color: Color32| format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b());
    [
        ("base", palette.base),
        ("mantle", palette.mantle),
        ("pane", palette.pane),
        ("surface", palette.surface),
        ("hover", palette.hover),
        ("border", palette.border),
        ("text", palette.text),
        ("subtext", palette.subtext),
        ("muted", palette.muted),
        ("primary", palette.primary),
        ("accent", palette.accent),
        ("warning", palette.warning),
        ("success", palette.success),
        ("destructive", palette.destructive),
    ]
    .into_iter()
    .map(|(name, color)| (name.to_owned(), hex(color)))
    .collect()
}
