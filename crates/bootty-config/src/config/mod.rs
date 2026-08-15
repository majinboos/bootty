mod defaults;
mod keybind_presets;
mod load;
mod model;
mod raw;
mod resolve;
mod theme_catalog;
mod writeback;

pub use defaults::{config_path_from_env, default_config_path, default_working_directory};
pub use keybind_presets::split_keybind_entry;
pub use load::{
    ConfigDocument, ConfigFileSnapshot, ConfigLoadError, ConfigResult, config_file_snapshot,
    load_config_document, load_config_from_path, load_or_create_config_document,
};
pub use model::{
    AppearanceBranchConfig, AppearanceConfig, AppearanceMode, AppearanceVariant,
    BackendKeybindConfig, BoottyConfig, ChromeConfig, ColorConfig, CursorConfig, CursorStyleConfig,
    DiagnosticsConfig, FontConfig, InputConfig, KeybindPreset, MacosOptionAsAltConfig,
    MacosTitlebarStyle, MultiplexerBackendConfig, MultiplexerConfig, ResolvedTheme, SegmentAlign,
    SessionConfig, SidebarConfig, SidebarPosition, SshAuthenticationConfig, SshHostKeyPolicyConfig,
    SshProfileConfig, SshRemoteConfig, StatusSegment, ThemeInfo, WindowConfig, WindowDecoration,
    WindowFullscreen,
};
pub use resolve::resolve_theme;
pub use theme_catalog::{DEFAULT_DARK_THEME, DEFAULT_LIGHT_THEME, builtin_theme_names};
pub use writeback::{ConfigWriteOutcome, update_config_document, write_font_size_preference};

pub(crate) use load::{config_dependency_snapshot, load_config_attempt};
