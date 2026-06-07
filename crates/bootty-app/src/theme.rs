use bootty_ui::{Theme, ThemePalette, UiColorConfig};
use eframe::egui::Color32;

use crate::{
    color::Color,
    config::{BoottyConfig, ColorConfig},
};

pub fn theme_from_config(config: &BoottyConfig) -> Theme {
    Theme::new(theme_palette_from_config(config))
}

pub fn theme_palette_from_config(config: &BoottyConfig) -> ThemePalette {
    ThemePalette::from_config(ui_color_config_from_colors(&config.colors))
}

fn ui_color_config_from_colors(colors: &ColorConfig) -> UiColorConfig {
    let mut palette = [None; 16];
    for (slot, color) in palette.iter_mut().zip(colors.palette.iter()) {
        *slot = Some(config_color32(*color));
    }
    UiColorConfig {
        background: colors.background.map(config_color32),
        foreground: colors.foreground.map(config_color32),
        selection_background: colors.selection_background.map(config_color32),
        selection_foreground: colors.selection_foreground.map(config_color32),
        palette,
    }
}

fn config_color32(color: Color) -> Color32 {
    Color32::from_rgb(color.r, color.g, color.b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_theme_uses_configured_terminal_colors_and_palette_accents() {
        let mut config = BoottyConfig::default();
        config.colors.background = Some(Color { r: 1, g: 2, b: 3 });
        config.colors.foreground = Some(Color {
            r: 240,
            g: 241,
            b: 242,
        });
        config.colors.palette = vec![
            Color { r: 0, g: 0, b: 0 },
            Color { r: 100, g: 0, b: 0 },
            Color { r: 0, g: 100, b: 0 },
            Color {
                r: 100,
                g: 80,
                b: 0,
            },
            Color { r: 0, g: 0, b: 100 },
            Color {
                r: 80,
                g: 0,
                b: 100,
            },
        ];

        let palette = theme_palette_from_config(&config);

        assert_eq!(palette.base, Color32::from_rgb(1, 2, 3));
        assert_eq!(palette.text, Color32::from_rgb(240, 241, 242));
        assert_eq!(palette.primary, Color32::from_rgb(80, 0, 100));
        assert_eq!(palette.accent, Color32::from_rgb(0, 0, 100));
        assert_eq!(palette.warning, Color32::from_rgb(100, 80, 0));
        assert_eq!(palette.success, Color32::from_rgb(0, 100, 0));
    }
}
