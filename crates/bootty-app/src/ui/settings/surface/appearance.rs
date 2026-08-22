use bootty_config::{
    color::Color,
    config::{AppearanceBranchConfig, AppearanceMode, AppearanceVariant, ColorConfig},
};
use bootty_ui::readable_color;
use eframe::egui;
use libghostty_vt::style::RgbColor;

use super::SettingsSurface;

pub(super) fn ui(win: &mut SettingsSurface, ui: &mut egui::Ui) {
    let palette = win.palette;

    super::section(ui, palette, "APPEARANCE");
    mode_row(win, ui);
    branch_tabs(win, ui);

    let variant = win.appearance_variant;

    super::section(ui, palette, "THEME");
    theme_row(win, ui, variant);

    super::section(ui, palette, "TERMINAL COLORS");
    #[rustfmt::skip]
    let colors: &[TerminalColorRow] = &[
        ("Background", "Terminal background override.", "background", palette.base, |c| &mut c.background),
        ("Foreground", "Primary terminal text override.", "foreground", palette.text, |c| &mut c.foreground),
        ("Cursor", "Cursor fill color.", "cursor", palette.primary, |c| &mut c.cursor),
        ("Cursor text", "Text drawn under the cursor.", "cursor-text", palette.base, |c| &mut c.cursor_text),
        ("Selection", "Selection background and foreground.", "selection-background", palette.hover, |c| &mut c.selection_background),
        ("Selection text", "Selected text foreground.", "selection-foreground", palette.subtext, |c| &mut c.selection_foreground),
        ("Highlight", "Search or match highlight background.", "highlight-background", palette.hover, |c| &mut c.highlight_background),
        ("Highlight text", "Search or match highlight foreground.", "highlight-foreground", palette.text, |c| &mut c.highlight_foreground),
        ("Pointer foreground", "Pointer text/foreground override.", "pointer-foreground", palette.base, |c| &mut c.pointer_foreground),
        ("Pointer background", "Pointer background override.", "pointer-background", palette.text, |c| &mut c.pointer_background),
        ("Tektronix foreground", "Tektronix vector-display foreground override.", "tektronix-foreground", palette.text, |c| &mut c.tektronix_foreground),
        ("Tektronix background", "Tektronix vector-display background override.", "tektronix-background", palette.base, |c| &mut c.tektronix_background),
        ("Tektronix cursor", "Tektronix vector-display cursor override.", "tektronix-cursor", palette.primary, |c| &mut c.tektronix_cursor),
    ];
    for &(label, help, key, seed, field) in colors {
        terminal_color_row(win, ui, label, help, key, seed, field);
    }

    super::section(ui, palette, "SIDEBAR COLORS");
    super::settings_notice(
        ui,
        palette.muted,
        "Unset colors inherit from the active theme.",
    );
    #[rustfmt::skip]
    let sidebar_colors: &[SidebarColorRow] = &[
        ("Background", "Sidebar panel background.", "background", palette.mantle, |s| &mut s.background),
        ("Foreground", "Sidebar text and icons.", "foreground", palette.text, |s| &mut s.foreground),
        ("Selected row", "Selected session fill.", "selected", palette.surface, |s| &mut s.selected),
        ("Hover row", "Hovered session fill.", "hover", palette.hover, |s| &mut s.hover),
        ("Border", "Separator between sidebar and terminal content.", "border", palette.border, |s| &mut s.border),
    ];
    for &(label, help, key, seed, field) in &sidebar_colors[..1] {
        super::sidebar_color_row(win, ui, label, help, &["sidebar", key], seed, field);
    }
    // Sits beside the sidebar background it defaults to, not at the end of the list.
    super::chrome_color_row(
        win,
        ui,
        "Status bar background",
        "Status strip background; unset uses the sidebar background default.",
        &["chrome", "status-background"],
        palette.mantle,
        |chrome| &mut chrome.status_background,
    );
    for &(label, help, key, seed, field) in &sidebar_colors[1..] {
        super::sidebar_color_row(win, ui, label, help, &["sidebar", key], seed, field);
    }

    super::section(ui, palette, "FULLSCREEN NOTCH");
    super::settings_toggle_row(
        ui,
        palette,
        "Use black notch chrome",
        "In dark mode on notched fullscreen displays, paint sidebar, status bar, and split dividers solid black.",
        win.config.chrome.notched_fullscreen_black_chrome,
        |enabled| {
            win.config.chrome.notched_fullscreen_black_chrome = enabled;
            win.writeback
                .set_bool(&["chrome", "notched-fullscreen-black-chrome"], enabled);
        },
    );

    super::section(ui, palette, "SPLIT PANES");
    super::chrome_color_row(
        win,
        ui,
        "Divider",
        "Color of the gap between split panes; unset uses the window background.",
        &["chrome", "pane-divider-color"],
        palette.mantle,
        |chrome| &mut chrome.pane_divider_color,
    );
    super::chrome_color_row_with_alpha(
        win,
        ui,
        "Focus border",
        "Border around the focused split pane; unset uses the theme accent.",
        &["chrome", "pane-focus-border-color"],
        palette.accent,
        |chrome| &mut chrome.pane_focus_border_color,
    );

    palette_section(win, ui, variant);
}

fn mode_row(win: &mut SettingsSurface, ui: &mut egui::Ui) {
    let selected = match win.config.appearance.mode {
        AppearanceMode::System => 0,
        AppearanceMode::Light => 1,
        AppearanceMode::Dark => 2,
    };
    super::settings_row(
        ui,
        win.palette,
        "Mode",
        "Follow the system appearance or force a light/dark branch.",
        |ui| {
            if let Some(index) = super::settings_segmented_ltr(
                ui,
                win.palette,
                &["System", "Light", "Dark"],
                selected,
            ) {
                let mode = match index {
                    0 => AppearanceMode::System,
                    1 => AppearanceMode::Light,
                    _ => AppearanceMode::Dark,
                };
                win.config.appearance.mode = mode;
                let token = match mode {
                    AppearanceMode::System => "system",
                    AppearanceMode::Light => "light",
                    AppearanceMode::Dark => "dark",
                };
                win.writeback.set_str(&["appearance", "mode"], token);
            }
        },
    );
}

fn branch_tabs(win: &mut SettingsSurface, ui: &mut egui::Ui) {
    let selected = match win.appearance_variant {
        AppearanceVariant::Light => 0,
        AppearanceVariant::Dark => 1,
    };
    super::settings_row(
        ui,
        win.palette,
        "Edit branch",
        "Theme and color choices are stored separately for light and dark.",
        |ui| {
            if let Some(index) =
                super::settings_segmented_ltr(ui, win.palette, &["Light", "Dark"], selected)
            {
                win.appearance_variant = if index == 0 {
                    AppearanceVariant::Light
                } else {
                    AppearanceVariant::Dark
                };
            }
        },
    );
}

fn branch_key(variant: AppearanceVariant) -> &'static str {
    match variant {
        AppearanceVariant::Light => "light",
        AppearanceVariant::Dark => "dark",
    }
}

fn branch(
    config: &bootty_config::config::BoottyConfig,
    variant: AppearanceVariant,
) -> &AppearanceBranchConfig {
    match variant {
        AppearanceVariant::Light => &config.appearance.light,
        AppearanceVariant::Dark => &config.appearance.dark,
    }
}

fn branch_mut(
    config: &mut bootty_config::config::BoottyConfig,
    variant: AppearanceVariant,
) -> &mut AppearanceBranchConfig {
    match variant {
        AppearanceVariant::Light => &mut config.appearance.light,
        AppearanceVariant::Dark => &mut config.appearance.dark,
    }
}

fn appearance_config_path<'a>(variant: AppearanceVariant, path: &'a [&'a str]) -> Vec<&'a str> {
    let mut full = vec!["appearance", branch_key(variant)];
    full.extend_from_slice(path);
    full
}

fn remove_branch_config_value(
    win: &mut SettingsSurface,
    variant: AppearanceVariant,
    path: &[&str],
) {
    let full_path = appearance_config_path(variant, path);
    win.writeback.remove(&full_path);
    if variant == AppearanceVariant::Dark {
        win.writeback.remove(path);
    }
}

fn theme_row(win: &mut SettingsSurface, ui: &mut egui::Ui, variant: AppearanceVariant) {
    let themes = win.theme_names.clone();
    super::settings_row(
        ui,
        win.palette,
        "Theme",
        "Built-in or config-directory theme.",
        |ui| {
            let fallback = match variant {
                AppearanceVariant::Light => bootty_config::config::DEFAULT_LIGHT_THEME,
                AppearanceVariant::Dark => bootty_config::config::DEFAULT_DARK_THEME,
            };
            let current = branch(&win.config, variant)
                .theme
                .clone()
                .unwrap_or_else(|| fallback.to_owned());
            let label = current.clone();
            let options: Vec<&str> = themes.iter().map(String::as_str).collect();
            let current_index = themes.iter().position(|theme| *theme == current);
            let combo_id = format!("settings_theme_{}", branch_key(variant));
            if let Some(index) = super::searchable_combo(
                ui,
                win.palette,
                &combo_id,
                &label,
                300.0,
                &options,
                current_index,
            ) {
                let chosen = themes[index].clone();
                branch_mut(&mut win.config, variant).theme = Some(chosen.clone());
                win.writeback
                    .set_str(&["appearance", branch_key(variant), "theme"], &chosen);
            }
        },
    );
}

fn terminal_color_row(
    win: &mut SettingsSurface,
    ui: &mut egui::Ui,
    label: &str,
    help: &str,
    key: &str,
    seed: egui::Color32,
    field: fn(&mut ColorConfig) -> &mut Option<Color>,
) {
    let variant = win.appearance_variant;
    super::settings_row(ui, win.palette, label, help, |ui| {
        let current = *field(&mut branch_mut(&mut win.config, variant).colors);
        let mut rgb = current.map_or([seed.r(), seed.g(), seed.b()], |color| {
            [color.r, color.g, color.b]
        });
        if super::settings_color_picker(ui, win.palette, &mut rgb).changed() {
            *field(&mut branch_mut(&mut win.config, variant).colors) = Some(Color {
                r: rgb[0],
                g: rgb[1],
                b: rgb[2],
                a: 0xff,
            });
            win.writeback
                .set_color(&["appearance", branch_key(variant), "colors", key], rgb);
        }
        let override_path = ["appearance", branch_key(variant), "colors", key];
        let legacy_path = ["colors", key];
        let has_override = win.writeback.contains(&override_path)
            || (variant == AppearanceVariant::Dark && win.writeback.contains(&legacy_path));
        if has_override && super::settings_button(ui, win.palette, "Reset").clicked() {
            remove_branch_config_value(win, variant, &["colors", key]);
        }
    });
}

type TerminalColorRow = (
    &'static str,
    &'static str,
    &'static str,
    egui::Color32,
    fn(&mut ColorConfig) -> &mut Option<Color>,
);

type SidebarColorRow = (
    &'static str,
    &'static str,
    &'static str,
    egui::Color32,
    fn(&mut bootty_config::config::SidebarConfig) -> &mut Option<Color>,
);

fn palette_section(win: &mut SettingsSurface, ui: &mut egui::Ui, variant: AppearanceVariant) {
    let palette = win.palette;
    super::section(ui, palette, "ANSI PALETTE");
    super::settings_toggle_row(
        ui,
        palette,
        "Generate 256-color cube",
        "Generate the extended color cube from the ANSI base palette.",
        branch(&win.config, variant).colors.palette_generate,
        |enabled| {
            branch_mut(&mut win.config, variant).colors.palette_generate = enabled;
            win.writeback.set_bool(
                &[
                    "appearance",
                    branch_key(variant),
                    "colors",
                    "palette-generate",
                ],
                enabled,
            );
        },
    );
    super::settings_toggle_row(
        ui,
        palette,
        "Harmonious blend",
        "Blend generated colors toward the active theme.",
        branch(&win.config, variant).colors.palette_harmonious,
        |enabled| {
            branch_mut(&mut win.config, variant)
                .colors
                .palette_harmonious = enabled;
            win.writeback.set_bool(
                &[
                    "appearance",
                    branch_key(variant),
                    "colors",
                    "palette-harmonious",
                ],
                enabled,
            );
        },
    );

    let branch_colors = &branch(&win.config, variant).colors;
    let palette_override_path = ["appearance", branch_key(variant), "colors", "palette"];
    let legacy_palette_path = ["colors", "palette"];
    let has_palette_overrides = win.writeback.contains(&palette_override_path)
        || (variant == AppearanceVariant::Dark && win.writeback.contains(&legacy_palette_path));
    let mut colors = if has_palette_overrides {
        branch_colors.palette.clone()
    } else {
        Vec::new()
    };
    let mut changed = false;
    // Seed sources for the "active" palette buttons: explicit overrides win and theme defaults fill
    // the rest. The editable grid itself only shows values explicitly stored in config.
    let base_overrides = colors.clone();
    let theme_palette = branch_colors.palette.clone();
    let harmonious = branch_colors.palette_harmonious;
    let bg_override = branch_colors.background;
    let fg_override = branch_colors.foreground;
    egui::Frame::NONE
        .fill(palette.pane)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new("ANSI colors")
                    .color(readable_color(palette.pane, palette.text))
                    .strong(),
            );
            ui.label(
                egui::RichText::new(
                    "Override indexed terminal colors. Empty inherits the active theme.",
                )
                .color(readable_color(palette.pane, palette.muted))
                .size(11.0),
            );
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if super::settings_button(ui, palette, "Use standard 16").clicked() {
                    colors = active_base16(&base_overrides, &theme_palette);
                    changed = true;
                }
                if super::settings_button(ui, palette, "Use xterm 256").clicked() {
                    colors = active_xterm256(
                        &base_overrides,
                        &theme_palette,
                        bg_override,
                        fg_override,
                        harmonious,
                    );
                    changed = true;
                }
                if super::settings_button(ui, palette, "Add empty slot").clicked() {
                    colors.push(Color {
                        r: 0,
                        g: 0,
                        b: 0,
                        a: 0xff,
                    });
                    changed = true;
                }
                if !colors.is_empty()
                    && super::settings_button(ui, palette, "Remove last slot").clicked()
                {
                    colors.pop();
                    changed = true;
                }
                if !colors.is_empty()
                    && super::settings_button(ui, palette, "Clear overrides").clicked()
                {
                    colors.clear();
                    changed = true;
                }
            });
            ui.add_space(10.0);
            if colors.is_empty() {
                ui.label(
                    egui::RichText::new("No ANSI overrides set.")
                        .color(readable_color(palette.pane, palette.muted)),
                );
            } else {
                egui::Grid::new("settings_ansi_palette_grid")
                    .num_columns(8)
                    .spacing([18.0, 10.0])
                    .show(ui, |ui| {
                        for (index, color) in colors.iter_mut().enumerate() {
                            ui.vertical(|ui| {
                                ui.label(
                                    egui::RichText::new(format!("{index:02}"))
                                        .color(readable_color(palette.pane, palette.muted))
                                        .size(11.0),
                                );
                                let mut rgb = [color.r, color.g, color.b];
                                if super::settings_color_picker(ui, palette, &mut rgb).changed() {
                                    *color = Color {
                                        r: rgb[0],
                                        g: rgb[1],
                                        b: rgb[2],
                                        a: 0xff,
                                    };
                                    changed = true;
                                }
                            });
                            if (index + 1) % 8 == 0 {
                                ui.end_row();
                            }
                        }
                    });
            }
        });
    ui.add_space(8.0);

    if changed {
        branch_mut(&mut win.config, variant).colors.palette = colors.clone();
        if colors.is_empty() {
            remove_branch_config_value(win, variant, &["colors", "palette"]);
        } else {
            let hex: Vec<String> = colors
                .iter()
                .map(|color| format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b))
                .collect();
            win.writeback.set_strings(
                &["appearance", branch_key(variant), "colors", "palette"],
                &hex,
            );
        }
    }
}

/// The active base 16 ANSI colors: existing overrides take priority, theme defaults fill the rest.
fn active_base16(overrides: &[Color], theme_palette: &[Color]) -> Vec<Color> {
    let defaults = bootty_terminal::terminal_palette::default_base16();
    (0..16)
        .map(|index| {
            overrides.get(index).copied().unwrap_or_else(|| {
                theme_palette
                    .get(index)
                    .copied()
                    .unwrap_or_else(|| rgb_to_color(defaults[index]))
            })
        })
        .collect()
}

/// The full active 256-color palette, generated from the active base 16 the same way the terminal
/// does (Lab-space cube + grayscale ramp), honoring the harmonious-blend toggle.
fn active_xterm256(
    overrides: &[Color],
    theme_palette: &[Color],
    bg: Option<Color>,
    fg: Option<Color>,
    harmonious: bool,
) -> Vec<Color> {
    let base16 = active_base16(overrides, theme_palette);
    let base: [RgbColor; 256] = std::array::from_fn(|index| {
        base16
            .get(index)
            .map_or(RgbColor { r: 0, g: 0, b: 0 }, |color| color_to_rgb(*color))
    });
    // Keep the base 16 verbatim; generate everything above them.
    let skip: [bool; 256] = std::array::from_fn(|index| index < 16);
    let defaults = bootty_terminal::terminal_palette::default_base16();
    let bg_rgb = bg.map_or(defaults[0], color_to_rgb);
    let fg_rgb = fg.map_or(defaults[15], color_to_rgb);
    bootty_terminal::terminal_palette::generate_256_palette(
        &base, &skip, bg_rgb, fg_rgb, harmonious,
    )
    .iter()
    .map(|color| rgb_to_color(*color))
    .collect()
}

fn color_to_rgb(color: Color) -> RgbColor {
    RgbColor {
        r: color.r,
        g: color.g,
        b: color.b,
    }
}

fn rgb_to_color(color: RgbColor) -> Color {
    Color {
        r: color.r,
        g: color.g,
        b: color.b,
        a: 0xff,
    }
}
