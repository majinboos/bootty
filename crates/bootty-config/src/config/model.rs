use super::keybind_presets::{
    owned_keybinds, preset_global_keybinds, preset_layout_keybinds, preset_tmux_backend_keybinds,
    resolve_macos_option_alt_keybinds, sidebar_keybinds,
};
use crate::color::Color;
use bootty_font::FontFeature;
pub use bootty_mux_model::{
    MuxBackendKind as MultiplexerBackendConfig, MuxBindingConfig as MultiplexerConfig,
    SshTarget as SshRemoteConfig,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};
#[derive(Clone, Debug, PartialEq)]
pub struct BoottyConfig {
    pub version: u32,
    pub appearance: AppearanceConfig,
    pub cursor: CursorConfig,
    pub font: FontConfig,
    pub chrome: ChromeConfig,
    pub sidebar: SidebarConfig,
    pub multiplexer: MultiplexerConfig,
    pub ssh_profiles: BTreeMap<String, SshProfileConfig>,
    /// Settings extensions declared for themselves, keyed by module stem then setting key. The
    /// loader accepts any of the three value shapes; what a key *means* is the declaring module's
    /// business, and a module may only ever read or write its own table.
    pub extensions: BTreeMap<String, BTreeMap<String, ExtensionSettingValue>>,
    pub input: InputConfig,
    pub session: SessionConfig,
    pub diagnostics: DiagnosticsConfig,
    pub window: WindowConfig,
    pub config_path: PathBuf,
    pub compatibility_warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeInfo {
    pub name: String,
    pub source: String,
    pub license: String,
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

/// A value an extension stores in its own settings table.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ExtensionSettingValue {
    Bool(bool),
    Number(f64),
    Text(String),
}

/// Where an extension's setting lives in the config: `extensions.<module>.<key>`. The module part
/// is supplied by the host from the module's own identity, never by the module itself.
#[must_use]
pub fn extension_setting_path(module: &str, key: &str) -> [String; 3] {
    ["extensions".to_owned(), module.to_owned(), key.to_owned()]
}

/// The TOML token for a config enum value, taken from its own `Serialize` derive so a writer can
/// never disagree with the parser about spelling. The loader normalizes `-` to `_` and lowercases
/// before matching, so a kebab token and the historic snake token both load to the same variant.
///
/// Only unit variants have a token; anything else returns `None`.
#[must_use]
pub fn config_token<T: Serialize>(value: &T) -> Option<String> {
    match value.serialize(toml_edit::ser::ValueSerializer::new()) {
        Ok(toml_edit::Value::String(token)) => Some(token.into_value()),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WindowFullscreen {
    #[default]
    Disabled,
    Native,
    NonNative,
    NonNativeVisibleMenu,
    NonNativePaddedNotch,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WindowDecoration {
    None,
    #[default]
    Auto,
    Client,
    Server,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
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
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
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

/// Sidebar placement and color overrides. Colors layer on top of the active theme; an unset slot
/// falls back to the theme-derived value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarConfig {
    pub position: SidebarPosition,
    pub background: Option<Color>,
    pub foreground: Option<Color>,
    pub selected: Option<Color>,
    pub hover: Option<Color>,
    pub border: Option<Color>,
    /// Ordered modules rendered for every session. User files use their stem under
    /// `<config>/session/`.
    pub session_modules: Vec<String>,
    /// Whether `session-modules` was explicitly present in loaded configuration.
    pub session_modules_configured: bool,
    /// Ordered modules that compose the overall sidebar. User files use their stem under
    /// `<config>/sidebar/`.
    pub modules: Vec<String>,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SidebarPosition {
    #[default]
    Left,
    Right,
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

#[derive(Clone, Copy, Debug, Default, Deserialize, Hash, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SegmentAlign {
    #[default]
    Left,
    Center,
    Right,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SshAuthenticationConfig {
    #[default]
    Auto,
    Agent,
    KeyFile,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SshHostKeyPolicyConfig {
    #[default]
    Strict,
    AcceptNew,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SshProfileConfig {
    pub name: String,
    pub host: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub authentication: SshAuthenticationConfig,
    #[serde(default)]
    pub host_key_policy: SshHostKeyPolicyConfig,
    #[serde(default)]
    pub identity_file: Option<PathBuf>,
    #[serde(default)]
    pub proxy_jump: Option<String>,
    #[serde(default = "default_ssh_program")]
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
}

fn default_ssh_program() -> String {
    "ssh".to_owned()
}

impl SshProfileConfig {
    pub fn to_remote(&self) -> SshRemoteConfig {
        let mut args = Vec::new();
        if let Some(proxy_jump) = nonempty_owned(self.proxy_jump.as_deref()) {
            args.extend(["-J".to_owned(), proxy_jump]);
        }
        match self.host_key_policy {
            SshHostKeyPolicyConfig::Strict => {
                args.extend(["-o".to_owned(), "StrictHostKeyChecking=yes".to_owned()]);
            }
            SshHostKeyPolicyConfig::AcceptNew => args.extend([
                "-o".to_owned(),
                "StrictHostKeyChecking=accept-new".to_owned(),
            ]),
        }
        if self.authentication == SshAuthenticationConfig::Agent {
            args.extend([
                "-o".to_owned(),
                "PreferredAuthentications=publickey".to_owned(),
                "-o".to_owned(),
                "PasswordAuthentication=no".to_owned(),
                "-o".to_owned(),
                "KbdInteractiveAuthentication=no".to_owned(),
            ]);
        }
        if self.authentication != SshAuthenticationConfig::Auto
            && let Some(identity_file) = &self.identity_file
        {
            args.extend([
                "-i".to_owned(),
                identity_file.display().to_string(),
                "-o".to_owned(),
                "IdentitiesOnly=yes".to_owned(),
            ]);
        }
        args.extend(self.args.iter().cloned());
        SshRemoteConfig {
            host: self.host.clone(),
            user: self.user.clone(),
            port: self.port,
            program: self.program.clone(),
            args,
        }
    }
}

fn nonempty_owned(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackendKeybindConfig {
    pub herdr: Vec<String>,
    pub native: Vec<String>,
    pub rmux: Vec<String>,
    pub tmux: Vec<String>,
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiagnosticsConfig {
    pub stability_trace: Option<PathBuf>,
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
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorConfig {
    pub style: Option<CursorStyleConfig>,
    pub blink: Option<bool>,
    pub dim_inactive_pane: bool,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            style: None,
            blink: None,
            dim_inactive_pane: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CursorStyleConfig {
    Bar,
    Block,
    Underline,
    HollowBlock,
}

impl FontConfig {
    pub fn ui_families(&self) -> &[String] {
        if self.ui_use_terminal_family {
            &self.family
        } else {
            &self.ui_family
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
impl InputConfig {
    pub fn keybinds_for_backend(&self, backend: MultiplexerBackendConfig) -> Vec<String> {
        let mut keybinds = self.keybind.clone();
        let backend_keybinds = match backend {
            MultiplexerBackendConfig::Herdr => &self.backend_keybinds.herdr,
            MultiplexerBackendConfig::Native => &self.backend_keybinds.native,
            MultiplexerBackendConfig::Rmux => &self.backend_keybinds.rmux,
            MultiplexerBackendConfig::Tmux => &self.backend_keybinds.tmux,
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

    pub(super) fn reset_default_keybinds(&mut self) {
        let prefix = self.effective_prefix();
        self.keybind = preset_global_keybinds(self.preset);
        self.sidebar_keybind = owned_keybinds(sidebar_keybinds());
        self.backend_keybinds = BackendKeybindConfig {
            herdr: preset_layout_keybinds(self.preset, prefix.as_deref()),
            native: preset_layout_keybinds(self.preset, prefix.as_deref()),
            rmux: preset_layout_keybinds(self.preset, prefix.as_deref()),
            tmux: preset_tmux_backend_keybinds(self.preset, prefix.as_deref()),
        };
    }
}

impl WindowConfig {
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
