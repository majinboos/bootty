use super::model::{
    AppearanceBranchConfig, AppearanceConfig, AppearanceMode, BackendKeybindConfig, BoottyConfig,
    ChromeConfig, CursorConfig, DiagnosticsConfig, FontConfig, InputConfig, KeybindPreset,
    MacosOptionAsAltConfig, MacosTitlebarStyle, MultiplexerConfig, SegmentAlign, SessionConfig,
    SidebarConfig, SidebarPosition, StatusSegment, WindowConfig, WindowDecoration,
    WindowFullscreen,
};
use super::theme_catalog::{DEFAULT_DARK_THEME, DEFAULT_LIGHT_THEME, load_builtin_theme};
use bootty_font::FontFeature;
use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    path::{Path, PathBuf},
};
const DEFAULT_MAX_SCROLLBACK: usize = 320_000_000;
const DEFAULT_TERM: &str = "xterm-bootty";
const DEFAULT_FONT_FAMILY: &str = "monospace";
const DEFAULT_FONT_FEATURE: FontFeature = FontFeature::new(*b"liga", 1);
const DEFAULT_FONT_SIZE: f32 = 11.75 * 96.0 / 72.0;
const DEFAULT_FONT_FIT_CELL_HEIGHT: bool = true;
const DEFAULT_FONT_FIT_CELL_WIDTH: bool = false;
const DEFAULT_FONT_BASELINE_ADJUSTMENT: f32 = 3.0;
const DEFAULT_FONT_UNDERLINE_POSITION: f32 = 2.0;
const DEFAULT_FONT_UNDERLINE_THICKNESS: f32 = 1.0;
impl Default for SidebarConfig {
    fn default() -> Self {
        Self {
            position: SidebarPosition::Left,
            background: None,
            foreground: None,
            selected: None,
            hover: None,
            border: None,
            session_modules: vec![
                "diffs".to_owned(),
                "process".to_owned(),
                "agent".to_owned(),
                "directory".to_owned(),
                "branch".to_owned(),
                "ports".to_owned(),
                "progress".to_owned(),
            ],
            session_modules_configured: false,
            modules: vec!["sessions".to_owned(), "codexbar".to_owned()],
        }
    }
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
impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: vec![DEFAULT_FONT_FAMILY.to_owned()],
            ui_family: Vec::new(),
            ui_use_terminal_family: false,
            features: vec![DEFAULT_FONT_FEATURE],
            size: DEFAULT_FONT_SIZE,
            cell_width: None,
            cell_height: None,
            fit_cell_height: DEFAULT_FONT_FIT_CELL_HEIGHT,
            fit_cell_width: DEFAULT_FONT_FIT_CELL_WIDTH,
            baseline_adjustment: DEFAULT_FONT_BASELINE_ADJUSTMENT,
            underline_position: DEFAULT_FONT_UNDERLINE_POSITION,
            underline_thickness: DEFAULT_FONT_UNDERLINE_THICKNESS,
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

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            shell: None,
            working_directory: None,
            env: Vec::new(),
            term: DEFAULT_TERM.to_owned(),
            colorterm: "truecolor".to_owned(),
            max_scrollback: DEFAULT_MAX_SCROLLBACK,
            glyph_protocol: true,
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

impl Default for BoottyConfig {
    fn default() -> Self {
        Self {
            version: 1,
            appearance: AppearanceConfig::default(),
            cursor: CursorConfig::default(),
            font: FontConfig::default(),
            chrome: ChromeConfig::default(),
            sidebar: SidebarConfig::default(),
            multiplexer: MultiplexerConfig::default(),
            ssh_profiles: BTreeMap::new(),
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
    bootty_identity::config_path_from_env(
        bootty_identity::ApplicationIdentity::Production,
        xdg_config_home,
        home,
    )
}
