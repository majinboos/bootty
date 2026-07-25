use eframe::egui::{self, Color32, CornerRadius, Stroke, Ui, Widget};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiColorConfig {
    pub background: Option<Color32>,
    pub foreground: Option<Color32>,
    pub palette: [Option<Color32>; 16],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemePalette {
    pub mantle: Color32,
    pub base: Color32,
    pub pane: Color32,
    pub surface: Color32,
    pub hover: Color32,
    pub border: Color32,
    pub text: Color32,
    pub subtext: Color32,
    pub muted: Color32,
    pub primary: Color32,
    pub accent: Color32,
    pub warning: Color32,
    pub success: Color32,
    pub destructive: Color32,
    pub radius: u8,
}

impl Default for ThemePalette {
    fn default() -> Self {
        Self {
            mantle: Color32::from_rgb(0x11, 0x11, 0x1b),
            base: Color32::from_rgb(0x15, 0x15, 0x20),
            pane: Color32::from_rgb(0x1a, 0x1b, 0x26),
            surface: Color32::from_rgb(0x1c, 0x1c, 0x29),
            hover: Color32::from_rgb(0x28, 0x29, 0x3a),
            border: Color32::from_rgb(0x31, 0x32, 0x44),
            text: Color32::from_rgb(0xcd, 0xd6, 0xf4),
            subtext: Color32::from_rgb(0xba, 0xc2, 0xde),
            muted: Color32::from_rgb(0x6c, 0x70, 0x86),
            primary: Color32::from_rgb(0xcb, 0xa6, 0xf7),
            accent: Color32::from_rgb(0x89, 0xb4, 0xfa),
            warning: Color32::from_rgb(0xfa, 0xb3, 0x87),
            success: Color32::from_rgb(0xa6, 0xe3, 0xa1),
            destructive: Color32::from_rgb(0xf3, 0x8b, 0xa8),
            radius: 8,
        }
    }
}

impl ThemePalette {
    #[must_use]
    pub fn from_config(config: UiColorConfig) -> Self {
        let mut palette = Self::default();
        let custom_background = config.background.is_some();
        let custom_foreground = config.foreground.is_some();
        if let Some(background) = config.background {
            palette.base = background;
        }
        if let Some(foreground) = config.foreground {
            palette.text = foreground;
        }
        if custom_background || custom_foreground {
            let surface_source = palette.text;
            palette.pane = mix(palette.base, surface_source, 0.07);
            palette.surface = mix(palette.base, surface_source, 0.13);
            palette.hover = mix(palette.base, surface_source, 0.20);
            palette.border = mix(palette.base, surface_source, 0.28);
            palette.mantle = if is_dark_palette(palette) {
                mix(palette.base, Color32::BLACK, 0.28)
            } else {
                mix(palette.base, surface_source, 0.18)
            };
            palette.subtext = mix(palette.text, palette.base, 0.12);
            palette.muted = mix(palette.text, palette.base, 0.34);
        }
        // Terminal selection colors can be much more saturated than UI hover/label surfaces.
        // Keep chrome hover/subtext derived from the background and foreground so widgets remain
        // readable across imported terminal themes.
        if let Some(primary) = config.palette.get(5).copied().flatten() {
            palette.primary = primary;
        }
        if let Some(accent) = config.palette.get(4).copied().flatten() {
            palette.accent = accent;
        }
        if let Some(warning) = config.palette.get(3).copied().flatten() {
            palette.warning = warning;
        }
        if let Some(success) = config.palette.get(2).copied().flatten() {
            palette.success = success;
        }
        if let Some(destructive) = config.palette.get(1).copied().flatten() {
            palette.destructive = destructive;
        }
        palette
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub palette: ThemePalette,
}

impl Theme {
    #[must_use]
    pub const fn new(palette: ThemePalette) -> Self {
        Self { palette }
    }
}

fn is_dark_palette(palette: ThemePalette) -> bool {
    let luma = u32::from(palette.base.r()) * 299
        + u32::from(palette.base.g()) * 587
        + u32::from(palette.base.b()) * 114;
    luma < 128_000
}

fn linear_channel(value: u8) -> f32 {
    let value = f32::from(value) / 255.0;
    if value <= 0.03928 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn relative_luminance(color: Color32) -> f32 {
    0.2126 * linear_channel(color.r())
        + 0.7152 * linear_channel(color.g())
        + 0.0722 * linear_channel(color.b())
}

pub fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let a = relative_luminance(a);
    let b = relative_luminance(b);
    let (lighter, darker) = if a >= b { (a, b) } else { (b, a) };
    (lighter + 0.05) / (darker + 0.05)
}

#[must_use]
pub fn readable_color(background: Color32, preferred: Color32) -> Color32 {
    const MIN_TEXT_CONTRAST: f32 = 7.0;
    if contrast_ratio(background, preferred) >= MIN_TEXT_CONTRAST {
        return preferred;
    }
    let black = Color32::BLACK;
    let white = Color32::WHITE;
    if contrast_ratio(background, black) >= contrast_ratio(background, white) {
        black
    } else {
        white
    }
}

pub fn configure_style(style: &mut egui::Style, theme: Theme) {
    let palette = theme.palette;
    // Start from a dark base so widgets (buttons, combos, checkboxes) don't inherit the OS
    // light-mode visuals, then layer the palette on top.
    style.visuals = egui::Visuals::dark();
    style.visuals.dark_mode = is_dark_palette(palette);
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    // A taller interact size keeps buttons, combo boxes, and text fields the same height so rows of
    // mixed widgets line up.
    style.spacing.interact_size.y = 26.0;
    style.visuals.override_text_color = None;
    style.visuals.window_fill = palette.pane;
    style.visuals.window_stroke = Stroke::new(1.0, palette.border);
    style.visuals.window_corner_radius = CornerRadius::same(palette.radius);
    style.visuals.menu_corner_radius = CornerRadius::same(palette.radius);
    style.visuals.popup_shadow = egui::epaint::Shadow::NONE;
    style.visuals.panel_fill = palette.base;
    style.visuals.extreme_bg_color = palette.mantle;
    style.visuals.faint_bg_color = palette.surface;
    style.visuals.hyperlink_color = readable_color(palette.base, palette.accent);
    style.visuals.selection.bg_fill = palette.accent;
    style.visuals.selection.stroke = Stroke::new(1.0, readable_color(palette.accent, palette.text));
    style.visuals.text_cursor.stroke =
        Stroke::new(2.0, readable_color(palette.surface, palette.text));

    for widget in [
        &mut style.visuals.widgets.noninteractive,
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.open,
    ] {
        // egui fills buttons/combos/checkboxes with `weak_bg_fill`; `bg_fill` covers sliders and
        // a few others. Set both so every interactive surface picks up the theme.
        widget.bg_fill = palette.surface;
        widget.weak_bg_fill = palette.surface;
        widget.bg_stroke = Stroke::new(1.0, palette.border);
        widget.fg_stroke = Stroke::new(1.0, readable_color(palette.surface, palette.text));
        widget.corner_radius = CornerRadius::same(palette.radius);
    }
    let hovered = &mut style.visuals.widgets.hovered;
    hovered.bg_fill = palette.hover;
    hovered.weak_bg_fill = palette.hover;
    hovered.bg_stroke = Stroke::new(1.0, palette.accent);
    hovered.fg_stroke = Stroke::new(1.0, readable_color(palette.hover, palette.text));
    hovered.corner_radius = CornerRadius::same(palette.radius);
    let active = &mut style.visuals.widgets.active;
    active.bg_fill = palette.accent;
    active.weak_bg_fill = palette.accent;
    active.bg_stroke = Stroke::new(1.0, palette.accent);
    active.fg_stroke = Stroke::new(1.0, readable_color(palette.accent, palette.text));
    active.corner_radius = CornerRadius::same(palette.radius);
}

pub fn themed_text_edit_singleline(
    ui: &mut Ui,
    buf: &mut String,
    theme: Theme,
    configure: impl for<'a> FnOnce(egui::TextEdit<'a>) -> egui::TextEdit<'a>,
) -> egui::Response {
    let mut style = (**ui.style()).clone();
    configure_style(&mut style, theme);
    style.visuals.extreme_bg_color = theme.palette.mantle;
    style.visuals.widgets.inactive.bg_fill = theme.palette.mantle;
    style.visuals.widgets.hovered.bg_fill = theme.palette.mantle;
    style.visuals.widgets.active.bg_fill = theme.palette.mantle;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, theme.palette.border);
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, theme.palette.border);
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0, theme.palette.accent);

    ui.scope_builder(egui::UiBuilder::new().style(style), |ui| {
        let edit = egui::TextEdit::singleline(buf)
            .font(egui::TextStyle::Monospace)
            .margin(egui::vec2(9.0, 5.0))
            .min_size(egui::vec2(0.0, 34.0));
        configure(edit).ui(ui)
    })
    .inner
}

pub fn flat_text_edit_singleline(
    ui: &mut Ui,
    buf: &mut String,
    theme: Theme,
    configure: impl for<'a> FnOnce(egui::TextEdit<'a>) -> egui::TextEdit<'a>,
) -> egui::Response {
    let mut style = (**ui.style()).clone();
    configure_style(&mut style, theme);
    let palette = theme.palette;
    style.visuals.extreme_bg_color = palette.mantle;
    style.visuals.selection.stroke = Stroke::new(1.0, palette.accent);

    for widget in [
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
    ] {
        widget.bg_fill = palette.mantle;
        widget.bg_stroke = Stroke::NONE;
        widget.fg_stroke = Stroke::new(1.0, palette.text);
        widget.corner_radius = CornerRadius::ZERO;
    }

    ui.scope_builder(egui::UiBuilder::new().style(style), |ui| {
        let edit = egui::TextEdit::singleline(buf)
            .font(egui::TextStyle::Monospace)
            .frame(egui::Frame::NONE)
            .margin(egui::vec2(0.0, 2.0))
            .min_size(egui::vec2(0.0, 24.0));
        configure(edit).ui(ui)
    })
    .inner
}

pub fn themed_button(ui: &mut Ui, label: &str, theme: Theme, selected: bool) -> egui::Response {
    let palette = theme.palette;
    let fill = if selected {
        palette.primary
    } else {
        palette.surface
    };
    let text = if selected { palette.base } else { palette.text };
    ui.add(
        egui::Button::new(egui::RichText::new(label).color(text).monospace())
            .fill(fill)
            .stroke(Stroke::new(
                1.0,
                if selected {
                    palette.primary
                } else {
                    palette.border
                },
            ))
            .corner_radius(CornerRadius::same(palette.radius))
            .min_size(egui::vec2(76.0, 32.0)),
    )
}

#[must_use]
pub fn mix(a: Color32, b: Color32, b_weight: f32) -> Color32 {
    let weight = b_weight.clamp(0.0, 1.0);
    let inv = 1.0 - weight;
    Color32::from_rgb(
        ((f32::from(a.r()) * inv) + (f32::from(b.r()) * weight)).round() as u8,
        ((f32::from(a.g()) * inv) + (f32::from(b.g()) * weight)).round() as u8,
        ((f32::from(a.b()) * inv) + (f32::from(b.b()) * weight)).round() as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
