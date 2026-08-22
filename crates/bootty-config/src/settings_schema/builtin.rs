//! The built-in settings, as data.
//!
//! Adding a setting here is one entry. Its TOML path appears once, its fallback is read off the
//! default config rather than copied, and its choice tokens come from the config enum's own
//! `Serialize` impl, so a spec cannot drift from the parser.

use crate::config::{MacosTitlebarStyle, SidebarPosition, WindowDecoration, WindowFullscreen};

use super::{NumberControl, SettingDefault, SettingKind, SettingOption, SettingSpec, SettingValue};

/// Build a spec with the fields every entry sets.
fn spec(
    path: &[&'static str],
    label: &'static str,
    help: &'static str,
    page: &'static str,
    section: &'static str,
    kind: SettingKind,
    default: SettingDefault,
) -> SettingSpec {
    SettingSpec {
        path: path.iter().map(|part| (*part).into()).collect(),
        label: label.into(),
        help: help.into(),
        page: page.into(),
        section: section.into(),
        kind,
        supersedes: Vec::new(),
        default,
    }
}

fn number(
    range: std::ops::RangeInclusive<f32>,
    control: NumberControl,
    suffix: &'static str,
) -> SettingKind {
    SettingKind::Number {
        range,
        control,
        suffix: suffix.into(),
        display_scale: 1.0,
    }
}

fn text(placeholder: &'static str, optional: bool) -> SettingKind {
    SettingKind::Text {
        placeholder: placeholder.into(),
        optional,
    }
}

/// A 0.0-1.0 fraction the user edits as a percentage.
fn fraction(control: NumberControl) -> SettingKind {
    SettingKind::Number {
        range: 0.0..=1.0,
        control,
        suffix: "%".into(),
        display_scale: 100.0,
    }
}

pub(super) fn specs() -> Vec<SettingSpec> {
    vec![
        spec(
            &["window", "title"],
            "Title",
            "Shown in native window chrome.",
            "window",
            "WINDOW",
            text("Bootty", false),
            SettingDefault::Field(|config| SettingValue::Text(config.window.title.clone())),
        ),
        spec(
            &["window", "macos-titlebar-style"],
            "Titlebar style",
            "macOS window chrome treatment.",
            "window",
            "WINDOW",
            SettingKind::Choice {
                options: vec![
                    SettingOption::of(&MacosTitlebarStyle::Native, "System titlebar"),
                    SettingOption::of(&MacosTitlebarStyle::Transparent, "Transparent"),
                    SettingOption::of(&MacosTitlebarStyle::Hidden, "Hidden"),
                ],
            },
            SettingDefault::Field(|config| {
                SettingValue::Token(token(&config.window.macos_titlebar_style))
            }),
        ),
        spec(
            &["window", "window-decoration"],
            "Decoration",
            "Choose who draws the outer window border.",
            "window",
            "WINDOW",
            SettingKind::Choice {
                options: vec![
                    SettingOption::of(&WindowDecoration::Auto, "Automatic"),
                    SettingOption::of(&WindowDecoration::None, "Borderless"),
                    SettingOption::of(&WindowDecoration::Client, "Drawn by Bootty"),
                    SettingOption::of(&WindowDecoration::Server, "Drawn by system"),
                ],
            },
            SettingDefault::Field(|config| {
                SettingValue::Token(token(&config.window.window_decoration))
            }),
        ),
        spec(
            &["window", "fullscreen"],
            "Fullscreen mode",
            "Controls native fullscreen and notch-aware non-native modes.",
            "window",
            "WINDOW",
            SettingKind::Choice {
                options: vec![
                    SettingOption::of(&WindowFullscreen::Disabled, "Disabled"),
                    SettingOption::of(&WindowFullscreen::Native, "Native"),
                    SettingOption::of(&WindowFullscreen::NonNative, "Borderless"),
                    SettingOption::of(
                        &WindowFullscreen::NonNativeVisibleMenu,
                        "Borderless + menu bar",
                    ),
                    SettingOption::of(
                        &WindowFullscreen::NonNativePaddedNotch,
                        "Borderless + notch padding",
                    ),
                ],
            },
            SettingDefault::Field(|config| SettingValue::Token(token(&config.window.fullscreen))),
        ),
        spec(
            &["window", "width"],
            "Width",
            "Applies to newly created windows.",
            "window",
            "DEFAULT SIZE",
            number(400.0..=6000.0, NumberControl::Edit, " px"),
            SettingDefault::Field(|config| SettingValue::Number(config.window.width)),
        ),
        spec(
            &["window", "height"],
            "Height",
            "Applies to newly created windows.",
            "window",
            "DEFAULT SIZE",
            number(300.0..=4000.0, NumberControl::Edit, " px"),
            SettingDefault::Field(|config| SettingValue::Number(config.window.height)),
        ),
        spec(
            &["window", "fullscreen-tabs-in-notch"],
            "Tabs in notch band",
            "Allow terminal chrome to occupy the notch/menu-bar band.",
            "window",
            "FULLSCREEN NOTCH",
            SettingKind::Bool,
            SettingDefault::Field(|config| {
                SettingValue::Bool(config.window.fullscreen_tabs_in_notch)
            }),
        ),
        spec(
            &["chrome", "gap"],
            "Chrome gap",
            "Spacing between sidebar, status, and terminal content.",
            "window",
            "CHROME",
            number(0.0..=24.0, NumberControl::Slider, " px"),
            SettingDefault::Field(|config| SettingValue::Number(config.chrome.gap)),
        ),
        spec(
            &["chrome", "unfocused-sidebar-dim"],
            "Inactive sidebar dim",
            "Opacity reduction when the window is not focused.",
            "window",
            "CHROME",
            fraction(NumberControl::Slider),
            SettingDefault::Field(|config| {
                SettingValue::Number(config.chrome.unfocused_sidebar_dim)
            }),
        ),
        spec(
            &["chrome", "unfocused-terminal-dim"],
            "Inactive terminal dim",
            "Opacity reduction when the window is not focused.",
            "window",
            "CHROME",
            fraction(NumberControl::Slider),
            SettingDefault::Field(|config| {
                SettingValue::Number(config.chrome.unfocused_terminal_dim)
            }),
        ),
        spec(
            &["chrome", "pane-divider-width"],
            "Divider width",
            "Thickness of the divider between split panes.",
            "window",
            "SPLIT PANES",
            number(0.0..=16.0, NumberControl::Slider, " px"),
            SettingDefault::Field(|config| SettingValue::Number(config.chrome.pane_divider_width)),
        ),
        spec(
            &["chrome", "pane-focus-border-width"],
            "Focus border width",
            "Border drawn around the focused split pane (0 hides it).",
            "window",
            "SPLIT PANES",
            number(0.0..=8.0, NumberControl::Slider, " px"),
            SettingDefault::Field(|config| {
                SettingValue::Number(config.chrome.pane_focus_border_width)
            }),
        ),
        spec(
            &["chrome", "pane-corner-radius"],
            "Corner radius",
            "Rounding of split pane corners.",
            "window",
            "SPLIT PANES",
            number(0.0..=40.0, NumberControl::Slider, " px"),
            SettingDefault::Field(|config| SettingValue::Number(config.chrome.pane_corner_radius)),
        ),
        spec(
            &["font", "size"],
            "Font size",
            "Main terminal text size.",
            "text",
            "TERMINAL METRICS",
            number(6.0..=48.0, NumberControl::Slider, "pt"),
            SettingDefault::Field(|config| SettingValue::Number(config.font.size)),
        ),
        spec(
            &["font", "fit-cell-height"],
            "Fit rows to window",
            "Stretch row spacing so terminal content fills available height.",
            "text",
            "TERMINAL METRICS",
            SettingKind::Bool,
            SettingDefault::Field(|config| SettingValue::Bool(config.font.fit_cell_height)),
        ),
        spec(
            &["font", "fit-cell-width"],
            "Fit columns to window",
            "Stretch column spacing so terminal content fills available width (avoids a gap on the right, common with split panes).",
            "text",
            "TERMINAL METRICS",
            SettingKind::Bool,
            SettingDefault::Field(|config| SettingValue::Bool(config.font.fit_cell_width)),
        ),
        spec(
            &["font", "baseline-adjustment"],
            "Baseline adjustment",
            "Move glyphs up or down inside each cell.",
            "text",
            "GLYPH BEHAVIOR",
            number(-12.0..=12.0, NumberControl::Slider, "px"),
            SettingDefault::Field(|config| SettingValue::Number(config.font.baseline_adjustment)),
        ),
        spec(
            &["font", "underline-position"],
            "Underline position",
            "Tune where underline decoration is drawn.",
            "text",
            "GLYPH BEHAVIOR",
            number(-12.0..=12.0, NumberControl::Slider, "px"),
            SettingDefault::Field(|config| SettingValue::Number(config.font.underline_position)),
        ),
        spec(
            &["font", "underline-thickness"],
            "Underline thickness",
            "Tune underline stroke thickness.",
            "text",
            "GLYPH BEHAVIOR",
            number(0.0..=8.0, NumberControl::Slider, "px"),
            SettingDefault::Field(|config| SettingValue::Number(config.font.underline_thickness)),
        ),
        spec(
            &["session", "shell"],
            "Shell",
            "Empty uses the macOS account login shell. Applies to new sessions.",
            "shell",
            "SHELL",
            text("default login shell", true),
            SettingDefault::Field(|config| {
                SettingValue::Text(config.session.shell.clone().unwrap_or_default())
            }),
        ),
        spec(
            &["session", "working-directory"],
            "Working directory",
            "Empty starts new sessions in your home directory.",
            "shell",
            "SHELL",
            text("inherit from launcher", true),
            SettingDefault::Field(|config| {
                SettingValue::Text(
                    config
                        .session
                        .working_directory
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_default(),
                )
            }),
        ),
        spec(
            &["session", "term"],
            "TERM",
            "Advertised terminal type for new shells.",
            "shell",
            "TERMINAL IDENTITY",
            text("xterm-256color", true),
            SettingDefault::Field(|config| SettingValue::Text(config.session.term.clone())),
        ),
        spec(
            &["session", "colorterm"],
            "COLORTERM",
            "Advertised color capability for new shells.",
            "shell",
            "TERMINAL IDENTITY",
            text("truecolor", true),
            SettingDefault::Field(|config| SettingValue::Text(config.session.colorterm.clone())),
        ),
        spec(
            &["session", "glyph-protocol"],
            "Glyph protocol",
            "Expose terminal image/glyph protocol support to new sessions.",
            "shell",
            "TERMINAL IDENTITY",
            SettingKind::Bool,
            SettingDefault::Field(|config| SettingValue::Bool(config.session.glyph_protocol)),
        ),
        spec(
            &["sidebar", "position"],
            "Position",
            "Dock the sidebar on the left or right edge.",
            "sidebar",
            "NAVIGATION",
            SettingKind::Choice {
                options: vec![
                    SettingOption::of(&SidebarPosition::Left, "left"),
                    SettingOption::of(&SidebarPosition::Right, "right"),
                ],
            },
            SettingDefault::Field(|config| SettingValue::Token(token(&config.sidebar.position))),
        ),
        spec(
            &["chrome", "sidebar-width"],
            "Width",
            "Width of the session sidebar.",
            "sidebar",
            "NAVIGATION",
            number(120.0..=600.0, NumberControl::Slider, " px"),
            SettingDefault::Field(|config| SettingValue::Number(config.chrome.sidebar_width)),
        ),
        spec(
            &["chrome", "bottom-bar"],
            "Bottom bar",
            "Show the module bar below the terminal.",
            "status",
            "BARS",
            SettingKind::Bool,
            SettingDefault::Field(|config| SettingValue::Bool(config.chrome.bottom_bar)),
        ),
        spec(
            &["chrome", "status-height"],
            "Height",
            "Module strip height.",
            "status",
            "STATUS BARS",
            number(20.0..=80.0, NumberControl::Slider, " px"),
            SettingDefault::Field(|config| SettingValue::Number(config.chrome.status_height)),
        ),
        spec(
            &["multiplexer", "hide-tmux-status"],
            "Hide tmux's own bar",
            "Avoid duplicate status bars when the tmux backend is active.",
            "status",
            "STATUS BARS",
            SettingKind::Bool,
            SettingDefault::Field(|config| SettingValue::Bool(config.multiplexer.hide_tmux_status)),
        ),
        spec(
            &["diagnostics", "stability-trace"],
            "Stability trace",
            "Writes frame-timing diagnostics to this file. Leave empty to disable.",
            "diagnostics",
            "TRACE",
            text("path to trace log", true),
            SettingDefault::Field(|config| {
                SettingValue::Text(
                    config
                        .diagnostics
                        .stability_trace
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_default(),
                )
            }),
        ),
    ]
}

fn token<T: serde::Serialize>(value: &T) -> String {
    crate::config::config_token(value).expect("config enum token")
}
