use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use bootty_config::{
    color::Color,
    config::{
        BoottyConfig, CursorStyleConfig, MacosOptionAsAltConfig, MacosTitlebarStyle,
        MultiplexerBackendConfig, SidebarPosition, SshRemoteConfig, WindowDecoration,
        WindowFullscreen,
    },
};
use bootty_font::FontFeature;
use bootty_terminal::terminal_engine::NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE;
use clap::{Args, ValueEnum};

#[derive(Clone, Debug, Default, Args)]
pub(super) struct ConfigOverrides {
    /// Force the multiplexer backend.
    #[arg(long, value_enum, value_name = "BACKEND")]
    backend: Option<CliBackend>,

    /// Select the named Herdr server session.
    #[arg(long, value_name = "NAME")]
    herdr_session: Option<String>,

    /// Attach the multiplexer running on this SSH host instead of the local one.
    #[arg(long, value_name = "HOST")]
    ssh_remote: Option<String>,

    /// Force tmux status hiding on.
    #[arg(long, conflicts_with = "show_tmux_status")]
    hide_tmux_status: bool,

    /// Force tmux status hiding off.
    #[arg(long)]
    show_tmux_status: bool,

    /// Force fullscreen mode. Omitting a value is the same as --fullscreen native.
    #[arg(
        long,
        value_enum,
        value_name = "MODE",
        num_args = 0..=1,
        default_missing_value = "native",
        conflicts_with = "no_fullscreen"
    )]
    fullscreen: Option<CliFullscreen>,

    /// Force fullscreen off, regardless of config.
    #[arg(long)]
    no_fullscreen: bool,

    /// Force non-native fullscreen top offset.
    #[arg(long, value_name = "PX")]
    fullscreen_top_offset: Option<f32>,

    /// Let fullscreen tabs occupy the notch band.
    #[arg(long, conflicts_with = "no_fullscreen_tabs_in_notch")]
    fullscreen_tabs_in_notch: bool,

    /// Keep fullscreen tabs below the notch band.
    #[arg(long)]
    no_fullscreen_tabs_in_notch: bool,

    /// Force native window decoration mode.
    #[arg(long, value_enum, value_name = "MODE")]
    window_decoration: Option<CliWindowDecoration>,

    /// Force macOS titlebar style.
    #[arg(
        long = "titlebar",
        alias = "macos-titlebar-style",
        value_enum,
        value_name = "STYLE"
    )]
    titlebar: Option<CliTitlebarStyle>,

    /// Force the window title.
    #[arg(long, value_name = "TITLE")]
    title: Option<String>,

    /// Force the initial window width.
    #[arg(long, value_name = "PX")]
    width: Option<f32>,

    /// Force the initial window height.
    #[arg(long, value_name = "PX")]
    height: Option<f32>,

    /// Force the active theme name.
    #[arg(long, value_name = "NAME")]
    theme: Option<String>,

    /// Force terminal background color.
    #[arg(long, value_name = "#RRGGBB", value_parser = Color::from_hex)]
    background: Option<Color>,

    /// Force terminal foreground color.
    #[arg(long, value_name = "#RRGGBB", value_parser = Color::from_hex)]
    foreground: Option<Color>,

    /// Force terminal cursor color.
    #[arg(long, value_name = "#RRGGBB", value_parser = Color::from_hex)]
    cursor_color: Option<Color>,

    /// Force text color under the cursor.
    #[arg(long, value_name = "#RRGGBB", value_parser = Color::from_hex)]
    cursor_text: Option<Color>,

    /// Force selection background color.
    #[arg(long, value_name = "#RRGGBB", value_parser = Color::from_hex)]
    selection_background: Option<Color>,

    /// Force selection foreground color.
    #[arg(long, value_name = "#RRGGBB", value_parser = Color::from_hex)]
    selection_foreground: Option<Color>,

    /// Force the ANSI palette. Repeat the flag or pass a comma-separated list.
    #[arg(long, value_name = "#RRGGBB", value_parser = Color::from_hex, value_delimiter = ',', num_args = 1..)]
    palette: Vec<Color>,

    /// Enable generated 256-color palette entries.
    #[arg(long, conflicts_with = "no_palette_generate")]
    palette_generate: bool,

    /// Disable generated 256-color palette entries.
    #[arg(long)]
    no_palette_generate: bool,

    /// Enable harmonious generated palette entries.
    #[arg(long, conflicts_with = "no_palette_harmonious")]
    palette_harmonious: bool,

    /// Disable harmonious generated palette entries.
    #[arg(long)]
    no_palette_harmonious: bool,

    /// Force the font size.
    #[arg(long, value_name = "PT")]
    font_size: Option<f32>,

    /// Force font families. Repeat the flag or pass a comma-separated list.
    #[arg(long, value_name = "FAMILY", value_delimiter = ',', num_args = 1..)]
    font_family: Vec<String>,

    /// Add font feature settings such as +liga or ss01.
    #[arg(long, value_name = "FEATURE", value_delimiter = ',', num_args = 1..)]
    font_feature: Vec<String>,

    /// Force fixed terminal cell width.
    #[arg(long, value_name = "PX")]
    font_cell_width: Option<f32>,

    /// Force fixed terminal cell height.
    #[arg(long, value_name = "PX")]
    font_cell_height: Option<f32>,

    /// Stretch row spacing to fit the available terminal height.
    #[arg(long, conflicts_with = "no_fit_cell_height")]
    fit_cell_height: bool,

    /// Disable row spacing stretch-to-fit.
    #[arg(long)]
    no_fit_cell_height: bool,

    /// Stretch column spacing to fit the available terminal width.
    #[arg(long, conflicts_with = "no_fit_cell_width")]
    fit_cell_width: bool,

    /// Disable column spacing stretch-to-fit.
    #[arg(long)]
    no_fit_cell_width: bool,

    /// Force font baseline adjustment.
    #[arg(long, value_name = "PX")]
    font_baseline_adjustment: Option<f32>,

    /// Force underline position adjustment.
    #[arg(long, value_name = "PX")]
    font_underline_position: Option<f32>,

    /// Force underline thickness adjustment.
    #[arg(long, value_name = "PX")]
    font_underline_thickness: Option<f32>,

    /// Force cursor style.
    #[arg(long, value_enum, value_name = "STYLE")]
    cursor_style: Option<CliCursorStyle>,

    /// Force cursor blinking on.
    #[arg(long, conflicts_with = "no_cursor_blink")]
    cursor_blink: bool,

    /// Force cursor blinking off.
    #[arg(long)]
    no_cursor_blink: bool,

    /// Force the shell used for new sessions.
    #[arg(long, value_name = "PATH")]
    shell: Option<String>,

    /// Force the working directory used for new sessions.
    #[arg(long, value_name = "DIR")]
    working_directory: Option<PathBuf>,

    /// Replace session environment with NAME=VALUE entries.
    #[arg(long = "env", value_name = "NAME=VALUE", value_parser = parse_env, num_args = 1..)]
    env: Vec<(String, String)>,

    /// Force TERM for new sessions.
    #[arg(long, value_name = "TERM")]
    term: Option<String>,

    /// Force COLORTERM for new sessions.
    #[arg(long, value_name = "COLORTERM")]
    colorterm: Option<String>,

    /// Force max scrollback rows.
    #[arg(long, value_name = "ROWS")]
    max_scrollback: Option<usize>,

    /// Enable the terminal glyph protocol.
    #[arg(long, conflicts_with = "no_glyph_protocol")]
    glyph_protocol: bool,

    /// Disable the terminal glyph protocol.
    #[arg(long)]
    no_glyph_protocol: bool,

    /// Force macOS Option-as-Alt mode.
    #[arg(long, value_enum, value_name = "MODE")]
    macos_option_as_alt: Option<CliMacosOptionAsAlt>,

    /// Replace modifier remaps. Repeat the flag or pass a comma-separated list.
    #[arg(long, value_name = "REMAP", value_delimiter = ',', num_args = 1..)]
    modifier_remap: Vec<String>,

    /// Force the sidebar on.
    #[arg(long, conflicts_with = "no_sidebar")]
    sidebar: bool,

    /// Force the sidebar off.
    #[arg(long)]
    no_sidebar: bool,

    /// Force the sidebar position.
    #[arg(long, value_enum, value_name = "POSITION")]
    sidebar_position: Option<CliSidebarPosition>,

    /// Force sidebar width.
    #[arg(long, value_name = "PX")]
    sidebar_width: Option<f32>,

    /// Force sidebar background color.
    #[arg(long, value_name = "#RRGGBB", value_parser = Color::from_hex)]
    sidebar_background: Option<Color>,

    /// Force sidebar foreground color.
    #[arg(long, value_name = "#RRGGBB", value_parser = Color::from_hex)]
    sidebar_foreground: Option<Color>,

    /// Force selected sidebar row color.
    #[arg(long, value_name = "#RRGGBB", value_parser = Color::from_hex)]
    sidebar_selected: Option<Color>,

    /// Force hovered sidebar row color.
    #[arg(long, value_name = "#RRGGBB", value_parser = Color::from_hex)]
    sidebar_hover: Option<Color>,

    /// Force sidebar border color.
    #[arg(long, value_name = "#RRGGBB", value_parser = Color::from_hex)]
    sidebar_border: Option<Color>,

    /// Force the top bar on. `--status-bar` remains a compatibility alias.
    #[arg(long, alias = "status-bar", conflicts_with = "no_top_bar")]
    top_bar: bool,

    /// Force the top bar off. `--no-status-bar` remains a compatibility alias.
    #[arg(long, alias = "no-status-bar")]
    no_top_bar: bool,

    /// Force the bottom bar on.
    #[arg(long, conflicts_with = "no_bottom_bar")]
    bottom_bar: bool,

    /// Force the bottom bar off.
    #[arg(long)]
    no_bottom_bar: bool,

    /// Force status bar height.
    #[arg(long, value_name = "PX")]
    status_height: Option<f32>,

    /// Force chrome gap size.
    #[arg(long = "chrome-gap", alias = "gap", value_name = "PX")]
    chrome_gap: Option<f32>,

    /// Force unfocused sidebar dim amount.
    #[arg(long, value_name = "0..1")]
    unfocused_sidebar_dim: Option<f32>,

    /// Force unfocused terminal dim amount.
    #[arg(long, value_name = "0..1")]
    unfocused_terminal_dim: Option<f32>,

    /// Write stability trace CSV to this path.
    #[arg(long, value_name = "PATH")]
    stability_trace: Option<PathBuf>,
}

impl ConfigOverrides {
    pub(super) fn apply(&self, config: &mut BoottyConfig) -> Result<()> {
        self.apply_multiplexer(config)?;
        self.apply_window(config);
        self.apply_theme_and_colors(config)?;
        self.apply_font(config)?;
        self.apply_cursor(config);
        self.apply_session(config);
        self.apply_input(config);
        self.apply_chrome(config);
        self.apply_sidebar(config);
        self.apply_diagnostics(config);
        Ok(())
    }

    fn apply_multiplexer(&self, config: &mut BoottyConfig) -> Result<()> {
        if let Some(backend) = self.backend {
            config.multiplexer.backend = backend.into();
        }
        if let Some(session) = &self.herdr_session {
            config.multiplexer.herdr_session.clone_from(session);
        }
        if let Some(hide_tmux_status) = bool_override(self.hide_tmux_status, self.show_tmux_status)
        {
            config.multiplexer.hide_tmux_status = hide_tmux_status;
        }
        if let Some(host) = &self.ssh_remote {
            // The flag names a host and nothing else, so whatever `[multiplexer.remote]` says about
            // reaching it — port, identity, jump host — stays in place.
            config
                .multiplexer
                .remote
                .get_or_insert_with(|| SshRemoteConfig::for_host(host.clone()))
                .host = host.clone();
        }
        config.multiplexer.validate_remote()?;
        Ok(())
    }

    fn apply_window(&self, config: &mut BoottyConfig) {
        if let Some(fullscreen) = self.fullscreen {
            config.window.fullscreen = fullscreen.into();
        }
        if self.no_fullscreen {
            config.window.fullscreen = WindowFullscreen::Disabled;
        }
        if let Some(offset) = self.fullscreen_top_offset {
            config.window.fullscreen_top_offset = Some(offset);
        }
        if let Some(tabs_in_notch) = bool_override(
            self.fullscreen_tabs_in_notch,
            self.no_fullscreen_tabs_in_notch,
        ) {
            config.window.fullscreen_tabs_in_notch = tabs_in_notch;
        }
        if let Some(decoration) = self.window_decoration {
            config.window.window_decoration = decoration.into();
        }
        if let Some(titlebar) = self.titlebar {
            config.window.macos_titlebar_style = titlebar.into();
        }
        if let Some(title) = &self.title {
            config.window.title.clone_from(title);
        }
        apply_value(&mut config.window.width, self.width);
        apply_value(&mut config.window.height, self.height);
    }

    fn apply_theme_and_colors(&self, config: &mut BoottyConfig) -> Result<()> {
        if self.theme.is_none()
            && [
                self.background,
                self.foreground,
                self.cursor_color,
                self.cursor_text,
                self.selection_background,
                self.selection_foreground,
            ]
            .iter()
            .all(Option::is_none)
            && self.palette.is_empty()
            && !self.palette_generate
            && !self.no_palette_generate
            && !self.palette_harmonious
            && !self.no_palette_harmonious
        {
            return Ok(());
        }
        let config_dir = config
            .config_path
            .parent()
            .unwrap_or_else(|| Path::new("."));
        Ok(config.appearance.apply_global_override(
            self.theme.as_deref(),
            config_dir,
            |colors| {
                apply_present(&mut colors.background, self.background);
                apply_present(&mut colors.foreground, self.foreground);
                apply_present(&mut colors.cursor, self.cursor_color);
                apply_present(&mut colors.cursor_text, self.cursor_text);
                apply_present(&mut colors.selection_background, self.selection_background);
                apply_present(&mut colors.selection_foreground, self.selection_foreground);
                if !self.palette.is_empty() {
                    colors.palette.clone_from(&self.palette);
                }
                if let Some(value) = bool_override(self.palette_generate, self.no_palette_generate)
                {
                    colors.palette_generate = value;
                }
                if let Some(value) =
                    bool_override(self.palette_harmonious, self.no_palette_harmonious)
                {
                    colors.palette_harmonious = value;
                }
            },
        )?)
    }

    fn apply_font(&self, config: &mut BoottyConfig) -> Result<()> {
        apply_value(&mut config.font.size, self.font_size);
        if !self.font_family.is_empty() {
            config.font.family.clone_from(&self.font_family);
        }
        for feature in &self.font_feature {
            let parsed = FontFeature::parse(feature)
                .ok_or_else(|| anyhow!("invalid font feature: {feature}"))?;
            config.font.features.push(parsed);
        }
        apply_present(&mut config.font.cell_width, self.font_cell_width);
        apply_present(&mut config.font.cell_height, self.font_cell_height);
        if let Some(fit_cell_height) = bool_override(self.fit_cell_height, self.no_fit_cell_height)
        {
            config.font.fit_cell_height = fit_cell_height;
        }
        if let Some(fit_cell_width) = bool_override(self.fit_cell_width, self.no_fit_cell_width) {
            config.font.fit_cell_width = fit_cell_width;
        }
        if let Some(adjustment) = self.font_baseline_adjustment {
            config.font.baseline_adjustment = adjustment;
        }
        if let Some(position) = self.font_underline_position {
            config.font.underline_position = position;
        }
        if let Some(thickness) = self.font_underline_thickness {
            config.font.underline_thickness = thickness;
        }
        Ok(())
    }

    fn apply_cursor(&self, config: &mut BoottyConfig) {
        apply_present(&mut config.cursor.style, self.cursor_style.map(Into::into));
        if let Some(blink) = bool_override(self.cursor_blink, self.no_cursor_blink) {
            config.cursor.blink = Some(blink);
        }
    }

    fn apply_session(&self, config: &mut BoottyConfig) {
        apply_present(&mut config.session.shell, self.shell.clone());
        if let Some(working_directory) = &self.working_directory {
            config.session.working_directory = Some(working_directory.clone());
        }
        if !self.env.is_empty() {
            config.session.env.clone_from(&self.env);
        }
        if let Some(term) = &self.term {
            config.session.term.clone_from(term);
        }
        if let Some(colorterm) = &self.colorterm {
            config.session.colorterm.clone_from(colorterm);
        }
        if let Some(max_scrollback) = self.max_scrollback {
            config.session.max_scrollback =
                max_scrollback.saturating_mul(NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE);
        }
        if let Some(glyph_protocol) = bool_override(self.glyph_protocol, self.no_glyph_protocol) {
            config.session.glyph_protocol = glyph_protocol;
        }
    }

    fn apply_input(&self, config: &mut BoottyConfig) {
        if let Some(mode) = self.macos_option_as_alt {
            config.input.macos_option_as_alt = mode.into();
        }
        if !self.modifier_remap.is_empty() {
            config.input.modifier_remap.clone_from(&self.modifier_remap);
        }
    }

    fn apply_chrome(&self, config: &mut BoottyConfig) {
        if let Some(sidebar) = bool_override(self.sidebar, self.no_sidebar) {
            config.chrome.sidebar = sidebar;
        }
        if let Some(top_bar) = bool_override(self.top_bar, self.no_top_bar) {
            config.chrome.top_bar = top_bar;
        }
        if let Some(bottom_bar) = bool_override(self.bottom_bar, self.no_bottom_bar) {
            config.chrome.bottom_bar = bottom_bar;
        }
        apply_value(&mut config.chrome.sidebar_width, self.sidebar_width);
        apply_value(&mut config.chrome.status_height, self.status_height);
        apply_value(&mut config.chrome.gap, self.chrome_gap);
        if let Some(dim) = self.unfocused_sidebar_dim {
            config.chrome.unfocused_sidebar_dim = dim;
        }
        if let Some(dim) = self.unfocused_terminal_dim {
            config.chrome.unfocused_terminal_dim = dim;
        }
    }

    fn apply_sidebar(&self, config: &mut BoottyConfig) {
        if let Some(position) = self.sidebar_position {
            config.sidebar.position = position.into();
        }
        apply_present(&mut config.sidebar.background, self.sidebar_background);
        apply_present(&mut config.sidebar.foreground, self.sidebar_foreground);
        apply_present(&mut config.sidebar.selected, self.sidebar_selected);
        apply_present(&mut config.sidebar.hover, self.sidebar_hover);
        apply_present(&mut config.sidebar.border, self.sidebar_border);
    }

    fn apply_diagnostics(&self, config: &mut BoottyConfig) {
        if let Some(path) = &self.stability_trace {
            config.diagnostics.stability_trace = Some(path.clone());
        }
    }
}

macro_rules! cli_value_enum {
    (
        $(
            $cli:ident => $config:ident {
                $( $variant:ident ),+ $(,)?
            }
        )+
    ) => {
        $(
            #[derive(Clone, Copy, Debug, ValueEnum)]
            #[value(rename_all = "kebab-case")]
            enum $cli {
                $( $variant, )+
            }

            impl From<$cli> for $config {
                fn from(value: $cli) -> Self {
                    match value {
                        $( $cli::$variant => Self::$variant, )+
                    }
                }
            }
        )+
    };
}

cli_value_enum! {
    CliBackend => MultiplexerBackendConfig {
        Herdr,
        Native,
        Rmux,
        Tmux,
    }
    CliFullscreen => WindowFullscreen {
        Disabled,
        Native,
        NonNative,
        NonNativeVisibleMenu,
        NonNativePaddedNotch,
    }
    CliWindowDecoration => WindowDecoration {
        None,
        Auto,
        Client,
        Server,
    }
    CliTitlebarStyle => MacosTitlebarStyle {
        Native,
        Transparent,
        Hidden,
    }
    CliCursorStyle => CursorStyleConfig {
        Bar,
        Block,
        Underline,
        HollowBlock,
    }
    CliMacosOptionAsAlt => MacosOptionAsAltConfig {
        None,
        Left,
        Right,
        Both,
    }
    CliSidebarPosition => SidebarPosition {
        Left,
        Right,
    }
}

fn parse_env(input: &str) -> Result<(String, String), String> {
    let (name, value) = input
        .split_once('=')
        .ok_or_else(|| format!("expected NAME=VALUE, got {input:?}"))?;
    if name.is_empty() {
        return Err(format!(
            "environment variable name cannot be empty in {input:?}"
        ));
    }
    Ok((name.to_owned(), value.to_owned()))
}

fn bool_override(enable: bool, disable: bool) -> Option<bool> {
    if enable {
        Some(true)
    } else if disable {
        Some(false)
    } else {
        None
    }
}

fn apply_present<T>(target: &mut Option<T>, value: Option<T>) {
    if let Some(value) = value {
        *target = Some(value);
    }
}

fn apply_value<T>(target: &mut T, value: Option<T>) {
    if let Some(value) = value {
        *target = value;
    }
}
