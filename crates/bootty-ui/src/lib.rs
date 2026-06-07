use eframe::egui::{self, Color32, CornerRadius, Stroke, Ui, Widget, WidgetText};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiColorConfig {
    pub background: Option<Color32>,
    pub foreground: Option<Color32>,
    pub selection_background: Option<Color32>,
    pub selection_foreground: Option<Color32>,
    pub palette: [Option<Color32>; 16],
}

impl UiColorConfig {
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            background: None,
            foreground: None,
            selection_background: None,
            selection_foreground: None,
            palette: [None; 16],
        }
    }
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
        if let Some(background) = config.background {
            palette.base = background;
            palette.pane = mix(background, palette.text, 0.04);
            palette.mantle = mix(background, Color32::BLACK, 0.28);
            palette.surface = mix(background, palette.text, 0.07);
            palette.hover = mix(background, palette.text, 0.12);
            palette.border = mix(background, palette.text, 0.17);
        }
        if let Some(foreground) = config.foreground {
            palette.text = foreground;
            palette.subtext = mix(foreground, palette.base, 0.18);
            palette.muted = mix(foreground, palette.base, 0.48);
        }
        if let Some(selection) = config.selection_background {
            palette.hover = selection;
        }
        if let Some(selection_foreground) = config.selection_foreground {
            palette.subtext = selection_foreground;
        }
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

pub struct ThemedUi<'a> {
    ui: &'a mut Ui,
    theme: Theme,
}

impl<'a> ThemedUi<'a> {
    #[must_use]
    pub fn new(ui: &'a mut Ui, theme: Theme) -> Self {
        Self { ui, theme }
    }

    pub fn raw(&mut self) -> &mut Ui {
        self.ui
    }

    #[must_use]
    pub const fn theme(&self) -> Theme {
        self.theme
    }

    #[must_use]
    pub const fn palette(&self) -> ThemePalette {
        self.theme.palette
    }

    pub fn label(&mut self, text: impl Into<WidgetText>) -> egui::Response {
        self.ui.label(text)
    }

    pub fn button(&mut self, label: &str, selected: bool) -> bool {
        themed_button(self.ui, label, self.theme, selected).clicked()
    }

    pub fn text_edit_singleline_with(
        &mut self,
        buf: &mut String,
        configure: impl for<'b> FnOnce(egui::TextEdit<'b>) -> egui::TextEdit<'b>,
    ) -> egui::Response {
        themed_text_edit_singleline(self.ui, buf, self.theme, configure)
    }
}

pub fn configure_style(style: &mut egui::Style, theme: Theme) {
    let palette = theme.palette;
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    style.visuals.override_text_color = Some(palette.text);
    style.visuals.window_fill = palette.pane;
    style.visuals.panel_fill = palette.base;
    style.visuals.extreme_bg_color = palette.mantle;
    style.visuals.faint_bg_color = palette.surface;
    style.visuals.selection.bg_fill = palette.primary;
    style.visuals.selection.stroke = Stroke::new(1.0, palette.text);

    for widget in [
        &mut style.visuals.widgets.noninteractive,
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.open,
    ] {
        widget.bg_fill = palette.surface;
        widget.bg_stroke = Stroke::new(1.0, palette.border);
        widget.fg_stroke = Stroke::new(1.0, palette.text);
        widget.corner_radius = CornerRadius::same(palette.radius);
    }
    style.visuals.widgets.hovered.bg_fill = palette.hover;
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, palette.primary);
    style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, palette.text);
    style.visuals.widgets.hovered.corner_radius = CornerRadius::same(palette.radius);
    style.visuals.widgets.active.bg_fill = palette.primary;
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0, palette.primary);
    style.visuals.widgets.active.fg_stroke = Stroke::new(1.0, palette.base);
    style.visuals.widgets.active.corner_radius = CornerRadius::same(palette.radius);
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
            selection_background: Some(Color32::from_rgb(20, 21, 22)),
            selection_foreground: None,
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
        assert_eq!(palette.hover, Color32::from_rgb(20, 21, 22));
        assert_eq!(palette.primary, Color32::from_rgb(80, 0, 100));
        assert_eq!(palette.accent, Color32::from_rgb(0, 0, 100));
        assert_eq!(palette.warning, Color32::from_rgb(100, 80, 0));
        assert_eq!(palette.success, Color32::from_rgb(0, 100, 0));
        assert_eq!(palette.destructive, Color32::from_rgb(100, 0, 0));
    }
}
