use super::load::{ConfigLoadError, ConfigResult};
use super::model::{
    AppearanceBranchConfig, AppearanceConfig, BackendKeybindConfig, BoottyConfig, ChromeConfig,
    ColorConfig, CursorConfig, DiagnosticsConfig, FontConfig, InputConfig, MultiplexerConfig,
    ResolvedTheme, SessionConfig, SidebarConfig, SshAuthenticationConfig, SshProfileConfig,
    WindowConfig,
};
use super::raw::{
    AppearanceBranchPatch, AppearancePatch, BackendKeybindPatch, ChromePatch, ColorPatch,
    CursorPatch, DiagnosticsPatch, FontPatch, InputPatch, MultiplexerPatch, RawConfig,
    SessionPatch, SidebarPatch, WindowPatch,
};
use super::theme_catalog::{load_builtin_theme, parse_theme_source};
use bootty_font::FontFeature;
use std::{
    fs,
    path::{Path, PathBuf},
};

impl SshProfileConfig {
    fn validate(&self, id: &str) -> ConfigResult<()> {
        if id.trim().is_empty() || self.name.trim().is_empty() || self.host.trim().is_empty() {
            return Err(ConfigLoadError::new(
                "SSH profiles need a stable id, display name, and host",
            ));
        }
        if self.authentication != SshAuthenticationConfig::Auto && self.identity_file.is_none() {
            let mode = match self.authentication {
                SshAuthenticationConfig::Agent => "agent",
                SshAuthenticationConfig::KeyFile => "key-file",
                SshAuthenticationConfig::Auto => unreachable!(),
            };
            return Err(ConfigLoadError::new(format!(
                "ssh-profiles.{id}.identity-file is required for {mode} authentication"
            )));
        }
        Ok(())
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

pub(super) struct ConfigResolver<'a> {
    pub(super) path: PathBuf,
    pub(super) config_dir: &'a Path,
}

impl ConfigResolver<'_> {
    pub(super) fn resolve(&self, raw: RawConfig) -> ConfigResult<BoottyConfig> {
        let mut config = BoottyConfig {
            config_path: self.path.clone(),
            ..BoottyConfig::default()
        };
        apply_value(&mut config.version, raw.version);
        config.appearance = resolve_appearance(
            raw.appearance,
            raw.theme.as_deref(),
            raw.colors,
            self.config_dir,
        )?;
        apply_partial_cursor(&mut config.cursor, raw.cursor);
        apply_partial_font(&mut config.font, raw.font)?;
        apply_font_features(&mut config.font, raw.font_feature)?;
        apply_partial_chrome(&mut config.chrome, raw.chrome);
        apply_partial_sidebar(&mut config.sidebar, raw.sidebar);
        apply_partial_multiplexer(&mut config.multiplexer, raw.multiplexer)?;
        config.ssh_profiles = raw.ssh_profiles;
        config.extensions = raw.extensions;
        for (id, profile) in &config.ssh_profiles {
            profile.validate(id)?;
        }
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
    if let Some(modules) = partial.session_modules {
        sidebar.session_modules = modules;
        sidebar.session_modules_configured = true;
    }
    apply_value(&mut sidebar.modules, partial.modules);
}

fn apply_partial_multiplexer(
    multiplexer: &mut MultiplexerConfig,
    partial: MultiplexerPatch,
) -> ConfigResult<()> {
    apply_value(&mut multiplexer.backend, partial.backend);
    apply_value(&mut multiplexer.hide_tmux_status, partial.hide_tmux_status);
    apply_present(&mut multiplexer.remote, partial.remote);
    multiplexer
        .validate_remote()
        .map_err(|error| ConfigLoadError::new(error.to_string()))
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

pub(super) fn apply_partial_colors(colors: &mut ColorConfig, partial: ColorPatch) {
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
    legacy_theme: Option<&str>,
    legacy_colors: ColorPatch,
    config_dir: &Path,
) -> ConfigResult<AppearanceConfig> {
    let mut appearance = AppearanceConfig::default();
    if legacy_theme.is_some() || legacy_colors != ColorPatch::default() {
        appearance.apply_global_override(legacy_theme, config_dir, |colors| {
            apply_partial_colors(colors, legacy_colors);
        })?;
    }
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

impl AppearanceConfig {
    /// Apply one process-wide override to both appearance branches. The theme resolves first so
    /// explicit color overrides take precedence, matching legacy top-level config semantics.
    pub fn apply_global_override(
        &mut self,
        theme: Option<&str>,
        config_dir: &Path,
        override_colors: impl FnOnce(&mut ColorConfig),
    ) -> ConfigResult<()> {
        let mut branch = self.dark.clone();
        if let Some(theme) = theme {
            branch.theme = Some(theme.to_owned());
            branch.colors = resolve_theme_colors(theme, config_dir)?;
        }
        override_colors(&mut branch.colors);
        self.light = branch.clone();
        self.dark = branch;
        Ok(())
    }
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

/// Every theme a user can select: the built-in catalog plus `themes/*.toml` beside the config
/// file. Ordered case-insensitively with case-duplicates collapsed, so a user copy of a
/// built-in theme replaces it in the list instead of appearing twice.
pub fn available_theme_names(config_path: &Path) -> Vec<String> {
    let mut names: Vec<String> = super::theme_catalog::builtin_theme_names()
        .map(str::to_owned)
        .collect();
    if let Some(config_dir) = config_path.parent()
        && let Ok(entries) = fs::read_dir(config_dir.join("themes"))
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "toml")
                && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
            {
                names.push(stem.to_owned());
            }
        }
    }
    names.sort_unstable_by_key(|name| name.to_ascii_lowercase());
    names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    names
}

fn user_theme_candidates(theme: &str, config_dir: &Path) -> [PathBuf; 2] {
    let theme_dir = config_dir.join("themes");
    [
        theme_dir.join(theme),
        theme_dir.join(format!("{theme}.toml")),
    ]
}
