use std::{
    collections::HashSet,
    env,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use serde::{Deserialize, Deserializer};
use toml_edit::{DocumentMut, Item, Table, TableLike};

use bootty_render::terminal_text::{FontFeature, TerminalTextConfig};
use bootty_runtime::{SessionLaunchConfig, TerminalSessionConfig};
use bootty_terminal::{
    terminal_engine::{
        NATIVE_MAX_SCROLLBACK, TERMINAL_TERM, TerminalColorConfig, TerminalCursorConfig,
        TerminalCursorStyle, TerminalFeatureConfig,
    },
    terminal_input_model::MacosOptionAsAlt,
};
use bootty_winit::modifier_remap::ModifierRemapSet;

use crate::color::Color;

mod keybind_presets;
mod theme_catalog;

pub use theme_catalog::{DEFAULT_DARK_THEME, DEFAULT_LIGHT_THEME, builtin_theme_names};

use keybind_presets::{
    owned_keybinds, preset_global_keybinds, preset_layout_keybinds, preset_tmux_backend_keybinds,
    resolve_macos_option_alt_keybinds, sidebar_keybinds,
};
use theme_catalog::{load_builtin_theme, parse_theme_source};

#[derive(Clone, Debug, PartialEq)]
pub struct BoottyConfig {
    pub version: u32,
    pub theme: Option<String>,
    pub colors: ColorConfig,
    pub appearance: AppearanceConfig,
    pub cursor: CursorConfig,
    pub font: FontConfig,
    pub chrome: ChromeConfig,
    pub sidebar: SidebarConfig,
    pub multiplexer: MultiplexerConfig,
    pub input: InputConfig,
    pub session: SessionConfig,
    pub diagnostics: DiagnosticsConfig,
    pub window: WindowConfig,
    pub config_path: PathBuf,
    pub compatibility_warnings: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    theme: Option<String>,
    #[serde(default)]
    include: Vec<String>,
    #[serde(default)]
    colors: ColorPatch,
    #[serde(default)]
    appearance: AppearancePatch,
    #[serde(default)]
    cursor: CursorPatch,
    #[serde(default)]
    font: FontPatch,
    #[serde(default)]
    font_feature: Vec<String>,
    #[serde(default)]
    chrome: ChromePatch,
    #[serde(default)]
    sidebar: SidebarPatch,
    #[serde(default)]
    multiplexer: MultiplexerPatch,
    #[serde(default)]
    input: InputPatch,
    #[serde(default)]
    session: SessionPatch,
    #[serde(default)]
    diagnostics: DiagnosticsPatch,
    #[serde(default)]
    window: WindowPatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeInfo {
    pub name: String,
    pub source: String,
    pub license: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct WindowPatch {
    title: Option<String>,
    width: Option<f32>,
    height: Option<f32>,
    fullscreen: Option<WindowFullscreen>,
    fullscreen_top_offset: Option<f32>,
    fullscreen_tabs_in_notch: Option<bool>,
    window_decoration: Option<WindowDecoration>,
    macos_titlebar_style: Option<MacosTitlebarStyle>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WindowConfig {
    pub title: String,
    pub width: f32,
    pub height: f32,
    pub fullscreen: WindowFullscreen,
    /// Top offset reserved when the window covers a notched screen in fullscreen. `None` uses the
    /// calibrated auto-detected notch offset; `Some` overrides it exactly.
    pub fullscreen_top_offset: Option<f32>,
    /// When fullscreen on a notched screen, let the terminal/tab bar sit inside the notch band
    /// instead of being pushed entirely below it.
    pub fullscreen_tabs_in_notch: bool,
    pub window_decoration: WindowDecoration,
    pub macos_titlebar_style: MacosTitlebarStyle,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowFullscreen {
    #[default]
    Disabled,
    Native,
    NonNative,
    NonNativeVisibleMenu,
    NonNativePaddedNotch,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowDecoration {
    None,
    #[default]
    Auto,
    Client,
    Server,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MacosTitlebarStyle {
    Native,
    #[default]
    Transparent,
    Hidden,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FontConfig {
    pub family: Vec<String>,
    pub ui_family: Vec<String>,
    pub ui_use_terminal_family: bool,
    pub features: Vec<FontFeature>,
    pub size: f32,
    pub cell_width: Option<f32>,
    pub cell_height: Option<f32>,
    pub fit_cell_height: bool,
    pub fit_cell_width: bool,
    pub baseline_adjustment: f32,
    pub underline_position: f32,
    pub underline_thickness: f32,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct FontPatch {
    family: Option<Vec<String>>,
    ui_family: Option<Vec<String>>,
    ui_use_terminal_family: Option<bool>,
    features: Option<Vec<String>>,
    size: Option<f32>,
    cell_width: Option<f32>,
    cell_height: Option<f32>,
    fit_cell_height: Option<bool>,
    fit_cell_width: Option<bool>,
    baseline_adjustment: Option<f32>,
    underline_position: Option<f32>,
    underline_thickness: Option<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChromeConfig {
    pub sidebar: bool,
    /// Whether to show the module bar above the terminal.
    pub top_bar: bool,
    /// Whether to show the module bar below the terminal.
    pub bottom_bar: bool,
    pub status_background: Option<Color>,
    pub sidebar_width: f32,
    pub status_height: f32,
    pub gap: f32,
    /// Visual width (px) of the gap/divider between native split panes. The grab area is widened
    /// past this so thin dividers stay draggable.
    pub pane_divider_width: f32,
    /// Divider color; falls back to the window background (the sidebar's default background) so the
    /// gap reads as a cohesive backdrop behind the rounded panes.
    pub pane_divider_color: Option<Color>,
    /// In dark appearance on a notched fullscreen display, paint the notch-integrated chrome
    /// (sidebar, status bar, and pane dividers) solid black.
    pub notched_fullscreen_black_chrome: bool,
    /// Border (px) drawn around the focused native split pane. 0 hides it.
    pub pane_focus_border_width: f32,
    /// Color of the focused-pane border; falls back to the theme accent when unset.
    pub pane_focus_border_color: Option<Color>,
    /// Corner radius (px) of split panes, clamped to the pane's shorter half-extent.
    pub pane_corner_radius: f32,
    pub unfocused_sidebar_dim: f32,
    pub unfocused_terminal_dim: f32,
    /// Ordered top-bar segments. Composed left/center/right; builtins plus Lua modules.
    pub top_segments: Vec<StatusSegment>,
    /// Ordered bottom-bar segments. Composed left/center/right; builtins plus Lua modules.
    pub bottom_segments: Vec<StatusSegment>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct ChromePatch {
    sidebar: Option<bool>,
    #[serde(alias = "status-bar")]
    top_bar: Option<bool>,
    bottom_bar: Option<bool>,
    #[serde(rename = "window-tabs")]
    _window_tabs: Option<bool>,
    sidebar_width: Option<f32>,
    status_height: Option<f32>,
    status_background: Option<Color>,
    gap: Option<f32>,
    pane_divider_width: Option<f32>,
    pane_divider_color: Option<Color>,
    notched_fullscreen_black_chrome: Option<bool>,
    pane_focus_border_width: Option<f32>,
    pane_focus_border_color: Option<Color>,
    pane_corner_radius: Option<f32>,
    unfocused_sidebar_dim: Option<f32>,
    unfocused_terminal_dim: Option<f32>,
    #[serde(alias = "status-segment")]
    top_segment: Option<Vec<StatusSegment>>,
    bottom_segment: Option<Vec<StatusSegment>>,
}

/// Sidebar placement and color overrides. Colors layer on top of the active theme; an unset slot
/// falls back to the theme-derived value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SidebarConfig {
    pub position: SidebarPosition,
    pub background: Option<Color>,
    pub foreground: Option<Color>,
    pub selected: Option<Color>,
    pub hover: Option<Color>,
    pub border: Option<Color>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SidebarPosition {
    #[default]
    Left,
    Right,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct SidebarPatch {
    position: Option<SidebarPosition>,
    background: Option<Color>,
    #[serde(rename = "fullscreen-background")]
    _fullscreen_background: Option<Color>,
    foreground: Option<Color>,
    selected: Option<Color>,
    hover: Option<Color>,
    #[serde(rename = "fullscreen-hover")]
    _fullscreen_hover: Option<Color>,
    border: Option<Color>,
}

/// One status-bar segment: a Luau module (builtin default or user file) plus optional style. The
/// module's own per-item style overrides these; these fill in where it leaves a field unset.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct StatusSegment {
    #[serde(default)]
    pub align: SegmentAlign,
    /// Module name: an embedded default (`windows`, `clock`, `session`, ...) or a `*.luau` file
    /// stem under `<config>/status/`.
    pub module: String,
    #[serde(default)]
    pub fg: Option<Color>,
    #[serde(default)]
    pub bg: Option<Color>,
    #[serde(default)]
    pub icon: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SegmentAlign {
    #[default]
    Left,
    Center,
    Right,
}

fn default_status_segments() -> Vec<StatusSegment> {
    vec![
        StatusSegment {
            align: SegmentAlign::Left,
            module: "session".to_owned(),
            ..StatusSegment::default()
        },
        StatusSegment {
            align: SegmentAlign::Left,
            module: "windows".to_owned(),
            ..StatusSegment::default()
        },
        StatusSegment {
            align: SegmentAlign::Right,
            module: "sysinfo".to_owned(),
            ..StatusSegment::default()
        },
        StatusSegment {
            align: SegmentAlign::Right,
            module: "clock".to_owned(),
            ..StatusSegment::default()
        },
    ]
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultiplexerConfig {
    pub backend: MultiplexerBackendConfig,
    /// Hide tmux's own status bar in bootty's client by toggling the attached
    /// session's `status` option off (and restoring it on detach). tmux-only.
    pub hide_tmux_status: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct MultiplexerPatch {
    backend: Option<MultiplexerBackendConfig>,
    hide_tmux_status: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum MultiplexerBackendConfig {
    Rmux,
    #[default]
    Native,
    Tmux,
    Zellij,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputConfig {
    pub modifier_remap: Vec<String>,
    pub macos_option_as_alt: MacosOptionAsAltConfig,
    pub hide_mouse_pointer_while_typing: bool,
    pub copy_on_select: bool,
    pub preset: KeybindPreset,
    /// Leader trigger for the active preset's prefixed chords. `None` uses the preset's own
    /// default; ignored by presets without a prefix concept (Ghostty).
    pub prefix: Option<String>,
    pub keybind: Vec<String>,
    pub sidebar_keybind: Vec<String>,
    pub backend_keybinds: BackendKeybindConfig,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum KeybindPreset {
    // Ghostty is the default: direct combos with no leader concept, the friendliest starting
    // point for new users.
    #[default]
    Ghostty,
    Bootty,
    Tmux,
}

impl KeybindPreset {
    pub const ALL: [Self; 3] = [Self::Ghostty, Self::Bootty, Self::Tmux];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bootty => "bootty",
            Self::Ghostty => "ghostty",
            Self::Tmux => "tmux",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Bootty => "Bootty",
            Self::Ghostty => "Ghostty",
            Self::Tmux => "Tmux",
        }
    }

    pub fn default_prefix(self) -> Option<&'static str> {
        match self {
            Self::Bootty => Some("ctrl+space"),
            Self::Ghostty => None,
            Self::Tmux => Some("ctrl+b"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum MacosOptionAsAltConfig {
    #[serde(alias = "false")]
    None,
    Left,
    Right,
    #[default]
    #[serde(alias = "true")]
    Both,
}

impl From<MacosOptionAsAltConfig> for MacosOptionAsAlt {
    fn from(value: MacosOptionAsAltConfig) -> Self {
        match value {
            MacosOptionAsAltConfig::None => Self::None,
            MacosOptionAsAltConfig::Left => Self::Left,
            MacosOptionAsAltConfig::Right => Self::Right,
            MacosOptionAsAltConfig::Both => Self::Both,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackendKeybindConfig {
    pub native: Vec<String>,
    pub rmux: Vec<String>,
    pub tmux: Vec<String>,
    pub zellij: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct InputPatch {
    modifier_remap: Option<Vec<String>>,
    macos_option_as_alt: Option<MacosOptionAsAltConfig>,
    hide_mouse_pointer_while_typing: Option<bool>,
    copy_on_select: Option<bool>,
    preset: Option<KeybindPreset>,
    prefix: Option<String>,
    keybind: Option<Vec<String>>,
    sidebar_keybind: Option<Vec<String>>,
    backend_keybind: Option<BackendKeybindPatch>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct BackendKeybindPatch {
    native: Option<Vec<String>>,
    rmux: Option<Vec<String>>,
    tmux: Option<Vec<String>>,
    zellij: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionConfig {
    pub shell: Option<String>,
    pub working_directory: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub term: String,
    pub colorterm: String,
    pub max_scrollback: usize,
    pub glyph_protocol: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct SessionPatch {
    shell: Option<String>,
    working_directory: Option<PathBuf>,
    env: Option<Vec<EnvConfigEntry>>,
    term: Option<String>,
    colorterm: Option<String>,
    max_scrollback: Option<usize>,
    glyph_protocol: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct EnvConfigEntry {
    name: String,
    value: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiagnosticsConfig {
    pub stability_trace: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct DiagnosticsPatch {
    stability_trace: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColorConfig {
    pub background: Option<Color>,
    pub foreground: Option<Color>,
    pub cursor: Option<Color>,
    pub cursor_text: Option<Color>,
    pub pointer_foreground: Option<Color>,
    pub pointer_background: Option<Color>,
    pub tektronix_foreground: Option<Color>,
    pub tektronix_background: Option<Color>,
    pub highlight_background: Option<Color>,
    pub tektronix_cursor: Option<Color>,
    pub highlight_foreground: Option<Color>,
    pub selection_background: Option<Color>,
    pub selection_foreground: Option<Color>,
    pub palette: Vec<Color>,
    pub palette_generate: bool,
    pub palette_harmonious: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedTheme {
    pub info: ThemeInfo,
    pub colors: ColorConfig,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct ColorPatch {
    background: Option<Color>,
    foreground: Option<Color>,
    cursor: Option<Color>,
    cursor_text: Option<Color>,
    pointer_foreground: Option<Color>,
    pointer_background: Option<Color>,
    tektronix_foreground: Option<Color>,
    tektronix_background: Option<Color>,
    highlight_background: Option<Color>,
    tektronix_cursor: Option<Color>,
    highlight_foreground: Option<Color>,
    selection_background: Option<Color>,
    selection_foreground: Option<Color>,
    palette: Option<Vec<Color>>,
    palette_generate: Option<bool>,
    palette_harmonious: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AppearanceMode {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AppearanceVariant {
    Light,
    #[default]
    Dark,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppearanceConfig {
    pub mode: AppearanceMode,
    pub light: AppearanceBranchConfig,
    pub dark: AppearanceBranchConfig,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppearanceBranchConfig {
    pub theme: Option<String>,
    pub colors: ColorConfig,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct AppearancePatch {
    mode: Option<AppearanceMode>,
    #[serde(default)]
    light: AppearanceBranchPatch,
    #[serde(default)]
    dark: AppearanceBranchPatch,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct AppearanceBranchPatch {
    theme: Option<String>,
    #[serde(default)]
    colors: ColorPatch,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CursorConfig {
    pub style: Option<CursorStyleConfig>,
    pub blink: Option<bool>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CursorStyleConfig {
    Bar,
    Block,
    Underline,
    HollowBlock,
}

impl From<CursorStyleConfig> for TerminalCursorStyle {
    fn from(value: CursorStyleConfig) -> Self {
        match value {
            CursorStyleConfig::Bar => Self::Bar,
            CursorStyleConfig::Block => Self::Block,
            CursorStyleConfig::Underline => Self::Underline,
            CursorStyleConfig::HollowBlock => Self::HollowBlock,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct CursorPatch {
    style: Option<CursorStyleConfig>,
    blink: Option<bool>,
}

#[derive(Debug)]
pub struct ConfigLoadError {
    message: String,
}

impl ConfigLoadError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ConfigLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConfigLoadError {}

pub type ConfigResult<T> = Result<T, ConfigLoadError>;

impl Default for FontConfig {
    fn default() -> Self {
        let text = TerminalTextConfig::default();
        Self {
            family: text.families,
            ui_family: Vec::new(),
            ui_use_terminal_family: false,
            features: text.font_features,
            size: text.font_size,
            cell_width: text.cell_width,
            cell_height: text.cell_height,
            fit_cell_height: text.fit_cell_height,
            fit_cell_width: text.fit_cell_width,
            baseline_adjustment: text.baseline_adjustment,
            underline_position: text.underline_position,
            underline_thickness: text.underline_thickness,
        }
    }
}

impl FontConfig {
    pub fn ui_families(&self) -> &[String] {
        if self.ui_use_terminal_family {
            &self.family
        } else {
            &self.ui_family
        }
    }

    pub fn terminal_text_config(&self) -> TerminalTextConfig {
        TerminalTextConfig {
            families: self.family.clone(),
            font_size: self.size,
            font_features: self.features.clone(),
            cell_width: self.cell_width,
            cell_height: self.cell_height,
            fit_cell_height: self.fit_cell_height,
            fit_cell_width: self.fit_cell_width,
            baseline_adjustment: self.baseline_adjustment,
            underline_position: self.underline_position,
            underline_thickness: self.underline_thickness,
            ..TerminalTextConfig::default()
        }
    }
}

impl SessionConfig {
    pub fn terminal_feature_config(&self) -> TerminalFeatureConfig {
        TerminalFeatureConfig {
            glyph_protocol: self.glyph_protocol,
        }
    }
}

impl Default for ChromeConfig {
    fn default() -> Self {
        Self {
            sidebar: true,
            top_bar: true,
            bottom_bar: false,
            status_background: None,
            sidebar_width: 286.0,
            status_height: 30.0,
            gap: 1.0,
            pane_divider_width: 3.0,
            pane_divider_color: None,
            notched_fullscreen_black_chrome: true,
            pane_focus_border_width: 1.0,
            pane_focus_border_color: None,
            pane_corner_radius: 0.0,
            unfocused_sidebar_dim: 0.16,
            unfocused_terminal_dim: 0.08,
            top_segments: default_status_segments(),
            bottom_segments: Vec::new(),
        }
    }
}

impl Default for MultiplexerConfig {
    fn default() -> Self {
        Self {
            backend: MultiplexerBackendConfig::Native,
            hide_tmux_status: false,
        }
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            shell: None,
            working_directory: None,
            env: Vec::new(),
            term: TERMINAL_TERM.to_owned(),
            colorterm: "truecolor".to_owned(),
            max_scrollback: NATIVE_MAX_SCROLLBACK,
            glyph_protocol: true,
        }
    }
}

impl SessionConfig {
    pub fn launch_config(&self) -> SessionLaunchConfig {
        SessionLaunchConfig {
            shell: self.shell.clone(),
            args: Vec::new(),
            working_directory: self
                .working_directory
                .clone()
                .or_else(default_working_directory),
            env: self.env.clone(),
            env_remove: Vec::new(),
            term: self.term.clone(),
            colorterm: self.colorterm.clone(),
        }
    }
}

impl BoottyConfig {
    pub fn terminal_session_config(&self) -> TerminalSessionConfig {
        TerminalSessionConfig {
            launch: self.session.launch_config(),
            colors: self.colors.terminal_color_config(),
            cursor: self.cursor.terminal_cursor_config(),
            features: self.session.terminal_feature_config(),
            max_scrollback: self.session.max_scrollback,
            macos_option_as_alt: self.input.macos_option_as_alt.into(),
            side_effect_tx: None,
            side_effect_pane_id: None,
            benchmark_trace: None,
        }
    }
}

pub fn default_working_directory() -> Option<PathBuf> {
    default_working_directory_from(|name| env::var_os(name))
}

fn default_working_directory_from(
    mut var: impl FnMut(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(user_profile) = non_empty_env_path(var("USERPROFILE")) {
            return Some(user_profile);
        }
        let home_drive = non_empty_env_path(var("HOMEDRIVE"))?;
        let home_path = non_empty_env_path(var("HOMEPATH"))?;
        Some(home_drive.join(home_path))
    }

    #[cfg(not(windows))]
    {
        non_empty_env_path(var("HOME"))
    }
}

fn non_empty_env_path(value: Option<OsString>) -> Option<PathBuf> {
    value.filter(|value| !value.is_empty()).map(PathBuf::from)
}

impl ColorConfig {
    pub fn terminal_color_config(&self) -> TerminalColorConfig {
        let mut terminal = TerminalColorConfig::default();
        if let Some(background) = self.background {
            terminal.background = background.into();
        }
        if let Some(foreground) = self.foreground {
            terminal.foreground = foreground.into();
        }
        if let Some(cursor) = self.cursor {
            terminal.cursor = Some(cursor.into());
        }
        terminal.cursor_text = self.cursor_text.map(Into::into);
        terminal.pointer_foreground = self.pointer_foreground.map(Into::into);
        terminal.pointer_background = self.pointer_background.map(Into::into);
        terminal.tektronix_foreground = self.tektronix_foreground.map(Into::into);
        terminal.tektronix_background = self.tektronix_background.map(Into::into);
        terminal.highlight_background = self.highlight_background.map(Into::into);
        terminal.tektronix_cursor = self.tektronix_cursor.map(Into::into);
        terminal.highlight_foreground = self.highlight_foreground.map(Into::into);
        terminal.selection_background = self.selection_background.map(Into::into);
        terminal.selection_foreground = self.selection_foreground.map(Into::into);
        if !self.palette.is_empty() {
            terminal.palette = self
                .palette
                .iter()
                .take(256)
                .copied()
                .map(Into::into)
                .collect();
        }
        terminal.palette_generate = self.palette_generate;
        terminal.palette_harmonious = self.palette_harmonious;
        terminal
    }
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            mode: AppearanceMode::System,
            light: AppearanceBranchConfig {
                theme: Some(DEFAULT_LIGHT_THEME.to_owned()),
                colors: load_builtin_theme(DEFAULT_LIGHT_THEME)
                    .expect("default light theme must be built in")
                    .colors,
            },
            dark: AppearanceBranchConfig {
                theme: Some(DEFAULT_DARK_THEME.to_owned()),
                colors: load_builtin_theme(DEFAULT_DARK_THEME)
                    .expect("default dark theme must be built in")
                    .colors,
            },
        }
    }
}

impl AppearanceMode {
    pub fn variant(self, system: AppearanceVariant) -> AppearanceVariant {
        match self {
            Self::System => system,
            Self::Light => AppearanceVariant::Light,
            Self::Dark => AppearanceVariant::Dark,
        }
    }
}

impl BoottyConfig {
    pub fn colors_for_appearance(&self, variant: AppearanceVariant) -> &ColorConfig {
        match variant {
            AppearanceVariant::Light => &self.appearance.light.colors,
            AppearanceVariant::Dark => &self.appearance.dark.colors,
        }
    }

    pub fn theme_for_appearance(&self, variant: AppearanceVariant) -> Option<&str> {
        match variant {
            AppearanceVariant::Light => self.appearance.light.theme.as_deref(),
            AppearanceVariant::Dark => self.appearance.dark.theme.as_deref(),
        }
    }
}

impl CursorConfig {
    pub fn terminal_cursor_config(&self) -> TerminalCursorConfig {
        TerminalCursorConfig {
            style: self.style.map(Into::into),
            blink: self.blink,
        }
    }
}

impl InputConfig {
    pub fn modifier_remaps(&self) -> ConfigResult<ModifierRemapSet> {
        let mut set = ModifierRemapSet::default();
        for remap in &self.modifier_remap {
            set.parse(remap).map_err(|error| {
                ConfigLoadError::new(format!("invalid modifier-remap {remap:?}: {error}"))
            })?;
        }
        set.finalize();
        Ok(set)
    }

    pub fn keybinds_for_backend(&self, backend: MultiplexerBackendConfig) -> Vec<String> {
        let mut keybinds = self.keybind.clone();
        let backend_keybinds = match backend {
            MultiplexerBackendConfig::Native => &self.backend_keybinds.native,
            MultiplexerBackendConfig::Rmux => &self.backend_keybinds.rmux,
            MultiplexerBackendConfig::Tmux => &self.backend_keybinds.tmux,
            MultiplexerBackendConfig::Zellij => &self.backend_keybinds.zellij,
        };
        keybinds.extend(backend_keybinds.iter().cloned());
        resolve_macos_option_alt_keybinds(keybinds, self.macos_option_as_alt)
    }

    /// The leader trigger prefixed chords are recorded and built with; `None` when the active
    /// preset has no prefix concept.
    pub fn effective_prefix(&self) -> Option<String> {
        let default = self.preset.default_prefix()?;
        Some(
            self.prefix
                .as_deref()
                .filter(|prefix| !prefix.is_empty())
                .unwrap_or(default)
                .to_owned(),
        )
    }

    fn reset_default_keybinds(&mut self) {
        let prefix = self.effective_prefix();
        self.keybind = preset_global_keybinds(self.preset);
        self.sidebar_keybind = owned_keybinds(sidebar_keybinds());
        self.backend_keybinds = BackendKeybindConfig {
            native: preset_layout_keybinds(self.preset, prefix.as_deref()),
            rmux: preset_layout_keybinds(self.preset, prefix.as_deref()),
            tmux: preset_tmux_backend_keybinds(self.preset, prefix.as_deref()),
            zellij: Vec::new(),
        };
    }
}

impl Default for InputConfig {
    fn default() -> Self {
        let mut input = Self {
            modifier_remap: Vec::new(),
            macos_option_as_alt: MacosOptionAsAltConfig::default(),
            hide_mouse_pointer_while_typing: true,
            copy_on_select: false,
            preset: KeybindPreset::default(),
            prefix: None,
            keybind: Vec::new(),
            sidebar_keybind: Vec::new(),
            backend_keybinds: BackendKeybindConfig::default(),
        };
        input.reset_default_keybinds();
        input
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum BoolOrString {
    Bool(bool),
    String(String),
}

impl<'de> Deserialize<'de> for WindowFullscreen {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = BoolOrString::deserialize(deserializer)?;
        match value {
            BoolOrString::Bool(false) => Ok(Self::Disabled),
            BoolOrString::Bool(true) => Ok(Self::Native),
            BoolOrString::String(value) => parse_window_fullscreen(&value)
                .ok_or_else(|| serde::de::Error::custom(format!("invalid fullscreen: {value}"))),
        }
    }
}

impl<'de> Deserialize<'de> for WindowDecoration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = BoolOrString::deserialize(deserializer)?;
        match value {
            BoolOrString::Bool(false) => Ok(Self::None),
            BoolOrString::Bool(true) => Ok(Self::Auto),
            BoolOrString::String(value) => parse_window_decoration(&value).ok_or_else(|| {
                serde::de::Error::custom(format!("invalid window-decoration: {value}"))
            }),
        }
    }
}

impl<'de> Deserialize<'de> for MacosTitlebarStyle {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        parse_macos_titlebar_style(&value).ok_or_else(|| {
            serde::de::Error::custom(format!("invalid macos-titlebar-style: {value}"))
        })
    }
}

fn parse_window_fullscreen(input: &str) -> Option<WindowFullscreen> {
    match normalize_config_value(input).as_str() {
        "false" | "off" | "disabled" | "none" | "no" => Some(WindowFullscreen::Disabled),
        "true" | "native" | "yes" => Some(WindowFullscreen::Native),
        "non_native" => Some(WindowFullscreen::NonNative),
        "non_native_visible_menu" => Some(WindowFullscreen::NonNativeVisibleMenu),
        "non_native_padded_notch" => Some(WindowFullscreen::NonNativePaddedNotch),
        _ => None,
    }
}

fn parse_window_decoration(input: &str) -> Option<WindowDecoration> {
    match normalize_config_value(input).as_str() {
        "false" | "none" | "off" | "disabled" | "no" => Some(WindowDecoration::None),
        "true" | "auto" | "on" | "yes" => Some(WindowDecoration::Auto),
        "client" => Some(WindowDecoration::Client),
        "server" => Some(WindowDecoration::Server),
        _ => None,
    }
}

fn parse_macos_titlebar_style(input: &str) -> Option<MacosTitlebarStyle> {
    match normalize_config_value(input).as_str() {
        "native" => Some(MacosTitlebarStyle::Native),
        "transparent" => Some(MacosTitlebarStyle::Transparent),
        "hidden" => Some(MacosTitlebarStyle::Hidden),
        _ => None,
    }
}

fn normalize_config_value(input: &str) -> String {
    input.trim().to_ascii_lowercase().replace('-', "_")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigFileSnapshot {
    files: Vec<ConfigFileStamp>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConfigFileStamp {
    path: PathBuf,
    modified: Option<SystemTime>,
    len: Option<u64>,
}

impl ConfigFileSnapshot {
    pub fn from_paths(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut files = paths
            .into_iter()
            .map(|path| ConfigFileStamp::from_path(config_file_id(&path)))
            .collect::<Vec<_>>();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files.dedup_by(|a, b| a.path == b.path);
        Self { files }
    }

    pub fn refresh_known_paths(&self) -> Self {
        Self::from_paths(self.files.iter().map(|file| file.path.clone()))
    }
}

impl ConfigFileStamp {
    fn from_path(path: PathBuf) -> Self {
        let metadata = fs::metadata(&path).ok();
        Self {
            path,
            modified: metadata
                .as_ref()
                .and_then(|metadata| metadata.modified().ok()),
            len: metadata.map(|metadata| metadata.len()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConfigDocument {
    path: PathBuf,
    document: DocumentMut,
}

impl ConfigDocument {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn document(&self) -> &DocumentMut {
        &self.document
    }

    pub fn to_toml_string(&self) -> String {
        self.document.to_string()
    }

    pub fn set_item(&mut self, path: &[&str], item: Item) -> ConfigResult<()> {
        let Some((leaf, parents)) = path.split_last() else {
            return Err(ConfigLoadError::new(
                "config writeback path cannot be empty",
            ));
        };
        let mut table = self.document.as_table_mut();
        for key in parents {
            let entry = &mut table[*key];
            if entry.is_none() {
                *entry = Item::Table(Table::new());
            }
            table = entry.as_table_mut().ok_or_else(|| {
                ConfigLoadError::new(format!(
                    "config writeback path {} is not a table",
                    parents.join(".")
                ))
            })?;
        }
        table[*leaf] = item;
        Ok(())
    }

    /// Remove a key, restoring its built-in default on the next load. Missing keys are a no-op.
    pub fn remove_item(&mut self, path: &[&str]) -> ConfigResult<()> {
        let Some((leaf, parents)) = path.split_last() else {
            return Err(ConfigLoadError::new(
                "config writeback path cannot be empty",
            ));
        };
        let mut table = self.document.as_table_mut();
        for key in parents {
            match table.get_mut(key).and_then(Item::as_table_mut) {
                Some(child) => table = child,
                None => return Ok(()),
            }
        }
        table.remove(leaf);
        Ok(())
    }

    pub fn write_to_disk(&self) -> ConfigResult<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                ConfigLoadError::new(format!(
                    "failed to create config directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        fs::write(&self.path, self.to_toml_string()).map_err(|error| {
            ConfigLoadError::new(format!(
                "failed to write config file {}: {error}",
                self.path.display()
            ))
        })
    }
}

impl Default for BoottyConfig {
    fn default() -> Self {
        let appearance = AppearanceConfig::default();
        let theme = appearance.dark.theme.clone();
        let colors = appearance.dark.colors.clone();
        Self {
            version: 1,
            appearance,
            theme,
            colors,
            cursor: CursorConfig::default(),
            font: FontConfig::default(),
            chrome: ChromeConfig::default(),
            sidebar: SidebarConfig::default(),
            multiplexer: MultiplexerConfig::default(),
            input: InputConfig::default(),
            session: SessionConfig::default(),
            diagnostics: DiagnosticsConfig::default(),
            window: WindowConfig {
                title: "Bootty".to_owned(),
                width: 1220.0,
                height: 760.0,
                fullscreen: WindowFullscreen::default(),
                fullscreen_top_offset: None,
                fullscreen_tabs_in_notch: true,
                window_decoration: WindowDecoration::default(),
                macos_titlebar_style: MacosTitlebarStyle::default(),
            },
            config_path: default_config_path(),
            compatibility_warnings: Vec::new(),
        }
    }
}

impl WindowConfig {
    pub fn fullscreen_enabled(&self) -> bool {
        self.fullscreen != WindowFullscreen::Disabled
    }

    pub fn native_fullscreen_enabled(&self) -> bool {
        self.fullscreen == WindowFullscreen::Native
    }

    pub fn non_native_fullscreen_enabled(&self) -> bool {
        matches!(
            self.fullscreen,
            WindowFullscreen::NonNative
                | WindowFullscreen::NonNativeVisibleMenu
                | WindowFullscreen::NonNativePaddedNotch
        )
    }

    pub fn hides_macos_menu_bar_in_non_native_fullscreen(&self) -> bool {
        matches!(
            self.fullscreen,
            WindowFullscreen::NonNative | WindowFullscreen::NonNativePaddedNotch
        )
    }

    pub fn decorations_enabled(&self) -> bool {
        self.window_decoration != WindowDecoration::None
            && self.macos_titlebar_style != MacosTitlebarStyle::Hidden
            && !self.non_native_fullscreen_enabled()
    }

    pub fn custom_chrome_title_visible(&self) -> bool {
        self.macos_titlebar_style != MacosTitlebarStyle::Hidden
    }

    pub fn reserves_macos_titlebar_button_area(&self) -> bool {
        cfg!(target_os = "macos")
            && self.decorations_enabled()
            && self.macos_titlebar_style == MacosTitlebarStyle::Transparent
    }
}

pub fn default_config_path() -> PathBuf {
    config_path_from_env(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

pub fn config_path_from_env(
    xdg_config_home: Option<impl AsRef<Path>>,
    home: Option<impl AsRef<Path>>,
) -> PathBuf {
    if let Some(xdg) = xdg_config_home {
        return xdg.as_ref().join("bootty/config.toml");
    }
    if let Some(home) = home {
        return home.as_ref().join(".config/bootty/config.toml");
    }
    PathBuf::from("bootty/config.toml")
}

pub fn load_config_from_path(path: impl AsRef<Path>) -> ConfigResult<BoottyConfig> {
    let path = path.as_ref();
    if !path.exists() {
        let config = BoottyConfig {
            config_path: path.to_path_buf(),
            ..Default::default()
        };
        return Ok(config);
    }

    let mut stack = Vec::new();
    let mut loaded = HashSet::new();
    let mut document = load_merged_config_document(path, &mut stack, &mut loaded)?;
    let compatibility_warnings = take_ghostty_compatibility_warnings(&mut document);
    let raw = parse_raw_config_document(document, path)?;
    let config_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut config = ConfigResolver {
        path: path.to_path_buf(),
        config_dir,
    }
    .resolve(raw)?;
    config.compatibility_warnings = compatibility_warnings;
    Ok(config)
}

pub fn config_file_snapshot(path: impl AsRef<Path>) -> ConfigResult<ConfigFileSnapshot> {
    let mut stack = Vec::new();
    let mut loaded = HashSet::new();
    let mut paths = Vec::new();
    collect_config_paths(path.as_ref(), &mut stack, &mut loaded, &mut paths)?;
    Ok(ConfigFileSnapshot::from_paths(paths))
}

pub fn load_config_document(path: impl AsRef<Path>) -> ConfigResult<Option<ConfigDocument>> {
    let path = path.as_ref();
    match fs::read_to_string(path) {
        Ok(source) => {
            let document = source.parse::<DocumentMut>().map_err(|error| {
                ConfigLoadError::new(format!(
                    "failed to parse config file {}: {error}",
                    path.display()
                ))
            })?;
            Ok(Some(ConfigDocument {
                path: path.to_path_buf(),
                document,
            }))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ConfigLoadError::new(format!(
            "failed to read config file {}: {error}",
            path.display()
        ))),
    }
}

pub fn load_or_create_config_document(path: impl AsRef<Path>) -> ConfigResult<ConfigDocument> {
    let path = path.as_ref();
    load_config_document(path).map(|document| {
        document.unwrap_or_else(|| ConfigDocument {
            path: path.to_path_buf(),
            document: DocumentMut::new(),
        })
    })
}

pub fn write_font_size_preference(path: impl AsRef<Path>, size: f32) -> ConfigResult<()> {
    let mut document = load_or_create_config_document(path)?;
    document.set_item(&["font", "size"], toml_edit::value(f64::from(size)))?;
    document.write_to_disk()
}

fn load_merged_config_document(
    path: &Path,
    stack: &mut Vec<PathBuf>,
    loaded: &mut HashSet<PathBuf>,
) -> ConfigResult<DocumentMut> {
    let id = config_file_id(path);
    if stack.contains(&id) {
        return Err(ConfigLoadError::new(format!(
            "config include cycle detected at {}",
            path.display()
        )));
    }
    if loaded.contains(&id) {
        return Ok(DocumentMut::new());
    }

    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ConfigLoadError::new(format!(
                "config file not found: {}",
                path.display()
            )));
        }
        Err(error) => {
            return Err(ConfigLoadError::new(format!(
                "failed to read config file {}: {error}",
                path.display()
            )));
        }
    };
    let mut document = source.parse::<DocumentMut>().map_err(|error| {
        ConfigLoadError::new(format!(
            "failed to parse config file {}: {error}",
            path.display()
        ))
    })?;
    let includes = config_document_includes(&document, path)?;

    stack.push(id.clone());
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    for include in includes {
        let include = IncludePath::parse(&include);
        let include_path = include.resolve(base_dir);
        if !include_path.exists() && include.optional {
            continue;
        }
        let child = load_merged_config_document(&include_path, stack, loaded)?;
        merge_toml_tables(document.as_table_mut(), child.into_table());
    }
    stack.pop();
    loaded.insert(id);
    Ok(document)
}

fn merge_toml_tables(target: &mut Table, overlay: Table) {
    merge_toml_table_like(target, &overlay);
}

fn merge_toml_table_like(target: &mut dyn TableLike, overlay: &dyn TableLike) {
    for (key, value) in overlay.iter() {
        if let Some(target_table) = target.get_mut(key).and_then(Item::as_table_like_mut)
            && let Some(overlay_table) = value.as_table_like()
        {
            merge_toml_table_like(target_table, overlay_table);
            continue;
        }
        target.insert(key, value.clone());
    }
}

const GHOSTTY_COMPATIBILITY_KEYS: &[&str] = &[
    "background-opacity",
    "background-blur-radius",
    "window-padding-x",
    "window-padding-y",
    "window-padding-balance",
    "window-save-state",
    "shell-integration",
    "shell-integration-features",
    "copy-on-select",
    "confirm-close-surface",
    "quit-after-last-window-closed",
];

fn take_ghostty_compatibility_warnings(document: &mut DocumentMut) -> Vec<String> {
    GHOSTTY_COMPATIBILITY_KEYS
        .iter()
        .filter_map(|key| {
            document
                .as_table_mut()
                .remove(key)
                .map(|_| format!("unsupported Ghostty compatibility key ignored: {key}"))
        })
        .collect()
}
fn parse_raw_config_document(document: DocumentMut, path: &Path) -> ConfigResult<RawConfig> {
    toml_edit::de::from_document(document).map_err(|error| {
        ConfigLoadError::new(format!(
            "failed to parse config file {}: {error}",
            path.display()
        ))
    })
}

fn config_document_includes(document: &DocumentMut, path: &Path) -> ConfigResult<Vec<String>> {
    let Some(item) = document.get("include") else {
        return Ok(Vec::new());
    };
    let Some(array) = item.as_array() else {
        return Err(ConfigLoadError::new(format!(
            "failed to parse config file {}: include must be an array of strings",
            path.display()
        )));
    };
    array
        .iter()
        .map(|value| {
            value.as_str().map(str::to_owned).ok_or_else(|| {
                ConfigLoadError::new(format!(
                    "failed to parse config file {}: include must contain only strings",
                    path.display()
                ))
            })
        })
        .collect()
}

fn parse_raw_config_source(source: &str, path: &Path) -> ConfigResult<RawConfig> {
    let mut document = source.parse::<DocumentMut>().map_err(|error| {
        ConfigLoadError::new(format!(
            "failed to parse config file {}: {error}",
            path.display()
        ))
    })?;
    take_ghostty_compatibility_warnings(&mut document);
    parse_raw_config_document(document, path)
}

fn collect_config_paths(
    path: &Path,
    stack: &mut Vec<PathBuf>,
    loaded: &mut HashSet<PathBuf>,
    paths: &mut Vec<PathBuf>,
) -> ConfigResult<()> {
    let id = config_file_id(path);
    paths.push(id.clone());
    if stack.contains(&id) {
        return Err(ConfigLoadError::new(format!(
            "config include cycle detected at {}",
            path.display()
        )));
    }
    if loaded.contains(&id) {
        return Ok(());
    }

    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            loaded.insert(id);
            return Ok(());
        }
        Err(error) => {
            return Err(ConfigLoadError::new(format!(
                "failed to read config file {}: {error}",
                path.display()
            )));
        }
    };
    let raw = parse_raw_config_source(&source, path)?;

    stack.push(id.clone());
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    for include in raw.include {
        let include = IncludePath::parse(&include);
        let include_path = include.resolve(base_dir);
        collect_config_paths(&include_path, stack, loaded, paths)?;
    }
    stack.pop();
    loaded.insert(id);
    Ok(())
}

fn config_file_id(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

struct IncludePath<'a> {
    path: &'a str,
    optional: bool,
}

impl<'a> IncludePath<'a> {
    fn parse(input: &'a str) -> Self {
        input.strip_prefix('?').map_or(
            Self {
                path: input,
                optional: false,
            },
            |path| Self {
                path,
                optional: true,
            },
        )
    }

    fn resolve(&self, base_dir: &Path) -> PathBuf {
        let path = Path::new(self.path);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            base_dir.join(path)
        }
    }
}

fn apply_value<T>(target: &mut T, value: Option<T>) {
    if let Some(value) = value {
        *target = value;
    }
}

fn apply_present<T>(target: &mut Option<T>, value: Option<T>) {
    if let Some(value) = value {
        *target = Some(value);
    }
}

struct ConfigResolver<'a> {
    path: PathBuf,
    config_dir: &'a Path,
}

impl ConfigResolver<'_> {
    fn resolve(&self, raw: RawConfig) -> ConfigResult<BoottyConfig> {
        let mut config = BoottyConfig {
            config_path: self.path.clone(),
            ..BoottyConfig::default()
        };
        apply_value(&mut config.version, raw.version);
        // Legacy top-level `theme`/`[colors]` seed both appearance branches, but only when the
        // config actually sets them — `config.theme` defaults to the dark theme, and treating
        // that default as legacy would overwrite the light branch's own default.
        let has_legacy_appearance = raw.theme.is_some() || raw.colors != ColorPatch::default();
        if let Some(theme) = &raw.theme {
            config.theme = Some(theme.clone());
            config.colors = resolve_theme_colors(theme, self.config_dir)?;
        }
        apply_partial_colors(&mut config.colors, raw.colors);
        let legacy_branch = has_legacy_appearance.then(|| AppearanceBranchConfig {
            theme: config.theme.clone(),
            colors: config.colors.clone(),
        });
        config.appearance = resolve_appearance(raw.appearance, legacy_branch, self.config_dir)?;
        config.theme = config.appearance.dark.theme.clone();
        config.colors = config.appearance.dark.colors.clone();
        apply_partial_cursor(&mut config.cursor, raw.cursor);
        apply_partial_font(&mut config.font, raw.font)?;
        apply_font_features(&mut config.font, raw.font_feature)?;
        apply_partial_chrome(&mut config.chrome, raw.chrome);
        apply_partial_sidebar(&mut config.sidebar, raw.sidebar);
        apply_partial_multiplexer(&mut config.multiplexer, raw.multiplexer);
        apply_partial_input(&mut config.input, raw.input);
        apply_partial_session(&mut config.session, raw.session);
        apply_partial_diagnostics(&mut config.diagnostics, raw.diagnostics);
        apply_partial_window(&mut config.window, raw.window);
        Ok(config)
    }
}

fn apply_partial_window(window: &mut WindowConfig, partial: WindowPatch) {
    apply_value(&mut window.title, partial.title);
    apply_value(&mut window.width, partial.width);
    apply_value(&mut window.height, partial.height);
    apply_value(&mut window.fullscreen, partial.fullscreen);
    apply_present(
        &mut window.fullscreen_top_offset,
        partial.fullscreen_top_offset,
    );
    apply_value(
        &mut window.fullscreen_tabs_in_notch,
        partial.fullscreen_tabs_in_notch,
    );
    apply_value(&mut window.window_decoration, partial.window_decoration);
    apply_value(
        &mut window.macos_titlebar_style,
        partial.macos_titlebar_style,
    );
}

fn apply_partial_font(font: &mut FontConfig, partial: FontPatch) -> ConfigResult<()> {
    apply_value(&mut font.family, partial.family);
    apply_value(&mut font.ui_family, partial.ui_family);
    apply_value(
        &mut font.ui_use_terminal_family,
        partial.ui_use_terminal_family,
    );
    apply_value(&mut font.size, partial.size);
    apply_present(&mut font.cell_width, partial.cell_width);
    apply_present(&mut font.cell_height, partial.cell_height);
    apply_value(&mut font.fit_cell_height, partial.fit_cell_height);
    apply_value(&mut font.fit_cell_width, partial.fit_cell_width);
    apply_value(&mut font.baseline_adjustment, partial.baseline_adjustment);
    apply_value(&mut font.underline_position, partial.underline_position);
    apply_value(&mut font.underline_thickness, partial.underline_thickness);
    if let Some(features) = partial.features {
        apply_font_features(font, features)?;
    }
    Ok(())
}

fn apply_font_features(font: &mut FontConfig, features: Vec<String>) -> ConfigResult<()> {
    for feature in features {
        let parsed = FontFeature::parse(&feature)
            .ok_or_else(|| ConfigLoadError::new(format!("invalid font feature: {feature}")))?;
        font.features.push(parsed);
    }
    Ok(())
}

fn apply_partial_chrome(chrome: &mut ChromeConfig, partial: ChromePatch) {
    apply_value(&mut chrome.sidebar, partial.sidebar);
    apply_value(&mut chrome.top_bar, partial.top_bar);
    apply_value(&mut chrome.bottom_bar, partial.bottom_bar);
    apply_value(&mut chrome.sidebar_width, partial.sidebar_width);
    apply_value(&mut chrome.status_height, partial.status_height);
    apply_present(&mut chrome.status_background, partial.status_background);
    apply_value(&mut chrome.gap, partial.gap);
    apply_value(&mut chrome.pane_divider_width, partial.pane_divider_width);
    apply_present(&mut chrome.pane_divider_color, partial.pane_divider_color);
    apply_value(
        &mut chrome.notched_fullscreen_black_chrome,
        partial.notched_fullscreen_black_chrome,
    );
    apply_value(
        &mut chrome.pane_focus_border_width,
        partial.pane_focus_border_width,
    );
    apply_present(
        &mut chrome.pane_focus_border_color,
        partial.pane_focus_border_color,
    );
    apply_value(&mut chrome.pane_corner_radius, partial.pane_corner_radius);
    apply_value(
        &mut chrome.unfocused_sidebar_dim,
        partial.unfocused_sidebar_dim,
    );
    apply_value(
        &mut chrome.unfocused_terminal_dim,
        partial.unfocused_terminal_dim,
    );
    if let Some(segments) = partial.top_segment {
        chrome.top_segments = segments;
    }
    if let Some(segments) = partial.bottom_segment {
        chrome.bottom_segments = segments;
    }
}

fn apply_partial_sidebar(sidebar: &mut SidebarConfig, partial: SidebarPatch) {
    apply_value(&mut sidebar.position, partial.position);
    apply_present(&mut sidebar.background, partial.background);
    apply_present(&mut sidebar.foreground, partial.foreground);
    apply_present(&mut sidebar.selected, partial.selected);
    apply_present(&mut sidebar.hover, partial.hover);
    apply_present(&mut sidebar.border, partial.border);
}

fn apply_partial_multiplexer(multiplexer: &mut MultiplexerConfig, partial: MultiplexerPatch) {
    apply_value(&mut multiplexer.backend, partial.backend);
    apply_value(&mut multiplexer.hide_tmux_status, partial.hide_tmux_status);
}

fn apply_partial_input(input: &mut InputConfig, partial: InputPatch) {
    apply_value(&mut input.modifier_remap, partial.modifier_remap);
    apply_value(&mut input.macos_option_as_alt, partial.macos_option_as_alt);
    apply_value(
        &mut input.hide_mouse_pointer_while_typing,
        partial.hide_mouse_pointer_while_typing,
    );
    apply_value(&mut input.copy_on_select, partial.copy_on_select);
    apply_value(&mut input.preset, partial.preset);
    apply_present(&mut input.prefix, partial.prefix);
    // Preset and prefix select which built-in default arrays the user's keybind rows layer
    // onto, so the defaults must be rebuilt before the merges below.
    input.reset_default_keybinds();
    if let Some(value) = partial.keybind {
        input.keybind = merge_keybind_entries(&input.keybind, value);
    }
    if let Some(value) = partial.sidebar_keybind {
        input.sidebar_keybind = merge_keybind_entries(&input.sidebar_keybind, value);
    }
    if let Some(value) = partial.backend_keybind {
        apply_partial_backend_keybind(&mut input.backend_keybinds, value);
    }
}

fn apply_partial_backend_keybind(
    keybinds: &mut BackendKeybindConfig,
    partial: BackendKeybindPatch,
) {
    if let Some(value) = partial.native {
        keybinds.native = merge_keybind_entries(&keybinds.native, value);
    }
    if let Some(value) = partial.rmux {
        keybinds.rmux = merge_keybind_entries(&keybinds.rmux, value);
    }
    if let Some(value) = partial.tmux {
        keybinds.tmux = merge_keybind_entries(&keybinds.tmux, value);
    }
    if let Some(value) = partial.zellij {
        keybinds.zellij = merge_keybind_entries(&keybinds.zellij, value);
    }
}

// User keybinds layer on top of the defaults so new default bindings reach existing configs;
// later entries override earlier ones for the same trigger. A "clear" entry opts out of the
// defaults entirely, keeping only the user's bindings (and individual defaults can be dropped with
// an `=unbind` action).
fn merge_keybind_entries(defaults: &[String], entries: Vec<String>) -> Vec<String> {
    if entries.iter().any(|entry| entry == "clear") {
        return entries
            .into_iter()
            .filter(|entry| entry != "clear")
            .collect();
    }
    let mut merged = defaults.to_vec();
    merged.extend(entries);
    merged
}

fn apply_partial_session(session: &mut SessionConfig, partial: SessionPatch) {
    apply_present(&mut session.shell, partial.shell);
    apply_present(&mut session.working_directory, partial.working_directory);
    if let Some(value) = partial.env {
        session.env = value
            .into_iter()
            .map(|entry| (entry.name, entry.value))
            .collect();
    }
    apply_value(&mut session.term, partial.term);
    apply_value(&mut session.colorterm, partial.colorterm);
    apply_value(&mut session.max_scrollback, partial.max_scrollback);
    apply_value(&mut session.glyph_protocol, partial.glyph_protocol);
}

fn apply_partial_diagnostics(diagnostics: &mut DiagnosticsConfig, partial: DiagnosticsPatch) {
    apply_present(&mut diagnostics.stability_trace, partial.stability_trace);
}

fn apply_partial_colors(colors: &mut ColorConfig, partial: ColorPatch) {
    apply_present(&mut colors.background, partial.background);
    apply_present(&mut colors.foreground, partial.foreground);
    apply_present(&mut colors.cursor, partial.cursor);
    apply_present(&mut colors.cursor_text, partial.cursor_text);
    apply_present(&mut colors.pointer_foreground, partial.pointer_foreground);
    apply_present(&mut colors.pointer_background, partial.pointer_background);
    apply_present(
        &mut colors.tektronix_foreground,
        partial.tektronix_foreground,
    );
    apply_present(
        &mut colors.tektronix_background,
        partial.tektronix_background,
    );
    apply_present(
        &mut colors.highlight_background,
        partial.highlight_background,
    );
    apply_present(&mut colors.tektronix_cursor, partial.tektronix_cursor);
    apply_present(
        &mut colors.highlight_foreground,
        partial.highlight_foreground,
    );
    apply_present(
        &mut colors.selection_background,
        partial.selection_background,
    );
    apply_present(
        &mut colors.selection_foreground,
        partial.selection_foreground,
    );
    apply_value(&mut colors.palette, partial.palette);
    apply_value(&mut colors.palette_generate, partial.palette_generate);
    apply_value(&mut colors.palette_harmonious, partial.palette_harmonious);
}

fn apply_partial_cursor(cursor: &mut CursorConfig, partial: CursorPatch) {
    apply_present(&mut cursor.style, partial.style);
    apply_present(&mut cursor.blink, partial.blink);
}

fn resolve_appearance(
    partial: AppearancePatch,
    legacy_branch: Option<AppearanceBranchConfig>,
    config_dir: &Path,
) -> ConfigResult<AppearanceConfig> {
    let default_appearance = AppearanceConfig::default();
    let mut appearance = AppearanceConfig {
        mode: AppearanceMode::System,
        light: legacy_branch.clone().unwrap_or(default_appearance.light),
        dark: legacy_branch.unwrap_or(default_appearance.dark),
    };
    apply_value(&mut appearance.mode, partial.mode);
    apply_appearance_branch(&mut appearance.light, partial.light, config_dir)?;
    apply_appearance_branch(&mut appearance.dark, partial.dark, config_dir)?;
    Ok(appearance)
}

fn apply_appearance_branch(
    branch: &mut AppearanceBranchConfig,
    partial: AppearanceBranchPatch,
    config_dir: &Path,
) -> ConfigResult<()> {
    if let Some(theme) = partial.theme {
        branch.colors = resolve_theme_colors(&theme, config_dir)?;
        branch.theme = Some(theme);
    }
    apply_partial_colors(&mut branch.colors, partial.colors);
    Ok(())
}

fn resolve_theme_colors(theme: &str, config_dir: &Path) -> ConfigResult<ColorConfig> {
    resolve_theme(theme, config_dir).map(|theme| theme.colors)
}

pub fn resolve_theme(theme: &str, config_dir: &Path) -> ConfigResult<ResolvedTheme> {
    if let Some(theme) = load_user_theme(theme, config_dir)? {
        return Ok(theme);
    }
    load_builtin_theme(theme).ok_or_else(|| {
        ConfigLoadError::new(format!(
            "theme {theme:?} not found in {} or built-in catalog",
            config_dir.join("themes").display()
        ))
    })
}

fn load_user_theme(theme: &str, config_dir: &Path) -> ConfigResult<Option<ResolvedTheme>> {
    for path in user_theme_candidates(theme, config_dir) {
        if !path.exists() {
            continue;
        }
        let source = fs::read_to_string(&path).map_err(|error| {
            ConfigLoadError::new(format!(
                "failed to read theme file {}: {error}",
                path.display()
            ))
        })?;
        return parse_theme_source(&source, &path.display().to_string()).map(Some);
    }
    Ok(None)
}

fn user_theme_candidates(theme: &str, config_dir: &Path) -> [PathBuf; 2] {
    let theme_dir = config_dir.join("themes");
    [
        theme_dir.join(theme),
        theme_dir.join(format!("{theme}.toml")),
    ]
}

#[derive(Clone, Debug)]
pub struct ConfigState {
    current: BoottyConfig,
    last_error: Option<String>,
}

impl ConfigState {
    pub fn new(current: BoottyConfig) -> Self {
        Self {
            current,
            last_error: None,
        }
    }

    pub fn current(&self) -> &BoottyConfig {
        &self.current
    }

    pub fn current_mut(&mut self) -> &mut BoottyConfig {
        &mut self.current
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn accept(&mut self, config: BoottyConfig) {
        self.current = config;
        self.last_error = None;
    }

    pub fn reject(&mut self, error: impl Into<String>) {
        self.last_error = Some(error.into());
    }

    pub fn reload_from_path(&mut self, path: impl AsRef<Path>) -> ConfigResult<()> {
        match load_config_from_path(path) {
            Ok(config) => {
                self.accept(config);
                Ok(())
            }
            Err(error) => {
                self.reject(error.to_string());
                Err(error)
            }
        }
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
