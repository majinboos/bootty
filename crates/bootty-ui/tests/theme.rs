use bootty_ui::*;
use eframe::egui::Color32;

#[test]
fn palette_uses_configured_base_foreground_and_ansi_accents() {
    let palette = ThemePalette::from_config(UiColorConfig {
        background: Some(Color32::from_rgb(1, 2, 3)),
        foreground: Some(Color32::from_rgb(240, 241, 242)),
        palette: [
            None,
            Some(Color32::from_rgb(100, 0, 0)),
            Some(Color32::from_rgb(0, 100, 0)),
            Some(Color32::from_rgb(100, 80, 0)),
            Some(Color32::from_rgb(0, 0, 100)),
            Some(Color32::from_rgb(80, 0, 100)),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ],
    });

    assert_eq!(palette.base, Color32::from_rgb(1, 2, 3));
    assert_eq!(palette.text, Color32::from_rgb(240, 241, 242));
    assert_eq!(
        palette.hover,
        mix(
            Color32::from_rgb(1, 2, 3),
            Color32::from_rgb(240, 241, 242),
            0.20
        )
    );
    assert_eq!(palette.primary, Color32::from_rgb(80, 0, 100));
    assert_eq!(palette.accent, Color32::from_rgb(0, 0, 100));
    assert_eq!(palette.warning, Color32::from_rgb(100, 80, 0));
    assert_eq!(palette.success, Color32::from_rgb(0, 100, 0));
    assert_eq!(palette.destructive, Color32::from_rgb(100, 0, 0));
}
