use bootty_ui::{Theme, ThemePalette, UiColorConfig};
use eframe::egui::Color32;

use crate::{
    color::Color,
    config::{AppearanceVariant, BoottyConfig, ColorConfig},
};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_theme_uses_configured_terminal_colors_and_palette_accents() {
        let mut config = BoottyConfig::default();
        config.colors.background = Some(Color {
            r: 1,
            g: 2,
            b: 3,
            a: 0xff,
        });
        config.colors.foreground = Some(Color {
            r: 240,
            g: 241,
            b: 242,
            a: 0xff,
        });
        config.colors.palette = vec![
            Color {
                r: 0,
                g: 0,
                b: 0,
                a: 0xff,
            },
            Color {
                r: 100,
                g: 0,
                b: 0,
                a: 0xff,
            },
            Color {
                r: 0,
                g: 100,
                b: 0,
                a: 0xff,
            },
            Color {
                r: 100,
                g: 80,
                b: 0,
                a: 0xff,
            },
            Color {
                r: 0,
                g: 0,
                b: 100,
                a: 0xff,
            },
            Color {
                r: 80,
                g: 0,
                b: 100,
                a: 0xff,
            },
        ];

        let palette = theme_palette_from_colors(&config.colors);

        assert_eq!(palette.base, Color32::from_rgb(1, 2, 3));
        assert_eq!(palette.text, Color32::from_rgb(240, 241, 242));
        assert_eq!(palette.primary, Color32::from_rgb(80, 0, 100));
        assert_eq!(palette.accent, Color32::from_rgb(0, 0, 100));
        assert_eq!(palette.warning, Color32::from_rgb(100, 80, 0));
        assert_eq!(palette.success, Color32::from_rgb(0, 100, 0));
    }
}
