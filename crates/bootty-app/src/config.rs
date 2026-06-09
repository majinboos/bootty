use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use serde::{Deserialize, Deserializer};
use toml_edit::{DocumentMut, Item, Table, TableLike};

use crate::{
    color::Color,
    modifier_remap::ModifierRemapSet,
    terminal::{SessionLaunchConfig, TerminalSessionConfig},
    terminal_engine::TerminalColorConfig,
    terminal_text::TerminalTextConfig,
};

#[derive(Clone, Debug, PartialEq)]
pub struct BoottyConfig {
    pub version: u32,
    pub theme: Option<String>,
    pub colors: ColorConfig,
    pub font: FontConfig,
    pub chrome: ChromeConfig,
    pub multiplexer: MultiplexerConfig,
    pub input: InputConfig,
    pub session: SessionConfig,
    pub diagnostics: DiagnosticsConfig,
    pub window: WindowConfig,
    pub config_path: PathBuf,
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
    font: FontPatch,
    #[serde(default)]
    chrome: ChromePatch,
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

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawTheme {
    #[serde(default)]
    metadata: ThemeMetadata,
    #[serde(default)]
    colors: ColorPatch,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct ThemeMetadata {
    name: Option<String>,
    source: Option<String>,
    license: Option<String>,
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
    window_decoration: Option<WindowDecoration>,
    macos_titlebar_style: Option<MacosTitlebarStyle>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WindowConfig {
    pub title: String,
    pub width: f32,
    pub height: f32,
    pub fullscreen: WindowFullscreen,
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
    Tabs,
    Hidden,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FontConfig {
    pub family: Vec<String>,
    pub size: f32,
    pub cell_width: f32,
    pub cell_height: f32,
    pub baseline_adjustment: f32,
    pub underline_position: f32,
    pub underline_thickness: f32,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct FontPatch {
    family: Option<Vec<String>>,
    size: Option<f32>,
    cell_width: Option<f32>,
    cell_height: Option<f32>,
    baseline_adjustment: Option<f32>,
    underline_position: Option<f32>,
    underline_thickness: Option<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChromeConfig {
    pub sidebar: bool,
    pub status_bar: bool,
    pub sidebar_width: f32,
    pub status_height: f32,
    pub gap: f32,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct ChromePatch {
    sidebar: Option<bool>,
    status_bar: Option<bool>,
    sidebar_width: Option<f32>,
    status_height: Option<f32>,
    gap: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultiplexerConfig {
    pub backend: MultiplexerBackendConfig,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct MultiplexerPatch {
    backend: Option<MultiplexerBackendConfig>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum MultiplexerBackendConfig {
    #[default]
    Rmux,
    Native,
    Tmux,
    Zellij,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputConfig {
    pub modifier_remap: Vec<String>,
    pub keybind: Vec<String>,
    pub backend_keybinds: BackendKeybindConfig,
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
    keybind: Option<Vec<String>>,
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
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct SessionPatch {
    shell: Option<String>,
    working_directory: Option<PathBuf>,
    env: Option<Vec<EnvConfigEntry>>,
    term: Option<String>,
    colorterm: Option<String>,
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
    pub selection_background: Option<Color>,
    pub selection_foreground: Option<Color>,
    pub palette: Vec<Color>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedTheme {
    pub info: ThemeInfo,
    pub colors: ColorConfig,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct ColorPatch {
    background: Option<Color>,
    foreground: Option<Color>,
    cursor: Option<Color>,
    cursor_text: Option<Color>,
    selection_background: Option<Color>,
    selection_foreground: Option<Color>,
    palette: Option<Vec<Color>>,
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
            size: text.font_size,
            cell_width: text.cell_width,
            cell_height: text.cell_height,
            baseline_adjustment: text.baseline_adjustment,
            underline_position: text.underline_position,
            underline_thickness: text.underline_thickness,
        }
    }
}

impl FontConfig {
    pub fn terminal_text_config(&self) -> TerminalTextConfig {
        TerminalTextConfig {
            families: self.family.clone(),
            font_size: self.size,
            cell_width: self.cell_width,
            cell_height: self.cell_height,
            baseline_adjustment: self.baseline_adjustment,
            underline_position: self.underline_position,
            underline_thickness: self.underline_thickness,
            ..TerminalTextConfig::default()
        }
    }
}

impl Default for ChromeConfig {
    fn default() -> Self {
        Self {
            sidebar: true,
            status_bar: true,
            sidebar_width: 286.0,
            status_height: 30.0,
            gap: 1.0,
        }
    }
}

impl Default for MultiplexerConfig {
    fn default() -> Self {
        Self {
            backend: MultiplexerBackendConfig::Rmux,
        }
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            shell: None,
            working_directory: None,
            env: Vec::new(),
            term: "xterm-ghostty".to_owned(),
            colorterm: "truecolor".to_owned(),
        }
    }
}

impl SessionConfig {
    pub fn launch_config(&self) -> SessionLaunchConfig {
        SessionLaunchConfig {
            shell: self.shell.clone(),
            args: Vec::new(),
            working_directory: self.working_directory.clone(),
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
            max_scrollback: 0,
        }
    }
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
        terminal
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
        keybinds
    }
}

fn common_keybinds() -> &'static [&'static str] {
    &[
        "cmd+shift+r=reload_config",
        "cmd+-=decrease_font_size:1",
        "cmd+==increase_font_size:1",
        "cmd++=increase_font_size:1",
        "cmd+0=reset_font_size",
        "performable:cmd+v=paste_from_clipboard",
        "shift+Enter=text:\\n",
        "cmd+alt+n=new_window",
        "cmd+shift+w=close_window",
        "cmd+w=close_surface",
        "cmd+q=quit",
        "cmd+alt+ctrl+f=toggle_fullscreen",
        "cmd+o=toggle_sidebar_focus",
        "cmd+shift+e=toggle_sidebar_visibility",
    ]
}

fn native_keybinds() -> &'static [&'static str] {
    &[
        "cmd+n=new_mux_session",
        "ctrl+space>c=new_tab",
        "ctrl+space>v=split_right",
        "ctrl+space>-=split_down",
        "ctrl+space>s=new_mux_session",
        "ctrl+space>x=ditch_session",
        "ctrl+space>shift+x=ditch_session",
        "ctrl+space>1=select_session:1",
        "ctrl+space>2=select_session:2",
        "ctrl+space>3=select_session:3",
        "ctrl+space>4=select_session:4",
        "ctrl+space>5=select_session:5",
        "ctrl+space>6=select_session:6",
        "ctrl+space>7=select_session:7",
        "ctrl+space>8=select_session:8",
        "ctrl+space>9=select_session:9",
        "ctrl+space>shift+,=move_tab:-1",
        "ctrl+space>shift+.=move_tab:1",
        "alt+n=next_tab",
        "alt+shift+n=next_tab",
        "alt+p=previous_tab",
        "alt+shift+p=previous_tab",
        "alt+shift+]=next_tab",
        "alt+shift+[=previous_tab",
        "alt+Tab=last_tab",
        "alt+1=select_tab:1",
        "alt+2=select_tab:2",
        "alt+3=select_tab:3",
        "alt+4=select_tab:4",
        "alt+5=select_tab:5",
        "alt+6=select_tab:6",
        "alt+7=select_tab:7",
        "alt+8=select_tab:8",
        "alt+9=select_tab:9",
        "alt+shift+,=move_tab:-1",
        "alt+shift+.=move_tab:1",
        "alt+h=select_pane:left",
        "alt+j=select_pane:down",
        "alt+k=select_pane:up",
        "alt+l=select_pane:right",
        "alt+o=next_pane",
        "alt+x=kill_pane",
        "alt+z=toggle_pane_zoom",
        "ctrl+Tab=last_session",
        "ctrl+shift+Tab=last_session",
        "cmd+shift+n=next_session",
        "cmd+shift+]=next_session",
        "cmd+shift+p=previous_session",
        "cmd+shift+[=previous_session",
        "cmd+shift+,=move_session:-1",
        "cmd+shift+.=move_session:1",
        "cmd+1=select_session:1",
        "cmd+2=select_session:2",
        "cmd+3=select_session:3",
        "cmd+4=select_session:4",
        "cmd+5=select_session:5",
        "cmd+6=select_session:6",
        "cmd+7=select_session:7",
        "cmd+8=select_session:8",
        "cmd+9=select_session:9",
        "cmd+alt+x=ditch_session",
        "cmd+y=scroll_page_up",
        "cmd+shift+y=scroll_page_down",
        "cmd+ArrowUp=scroll_page_lines:-1",
        "cmd+ArrowDown=scroll_page_lines:1",
    ]
}

fn tmux_keybinds() -> &'static [&'static str] {
    &[
        "ctrl+Tab=csi:60~",
        "cmd+;=csi:61~",
        "cmd+n=csi:62~",
        "cmd+p=csi:63~",
        "cmd+shift+n=csi:64~",
        "cmd+shift+p=csi:65~",
        "cmd+shift+,=csi:66~",
        "cmd+shift+.=csi:67~",
        "cmd+ctrl+n=csi:68~",
        "ctrl+alt+[=csi:69~",
        "cmd+shift+]=csi:70~",
        "cmd+y=csi:71~",
        "cmd+c=csi:72~",
        "cmd+shift+[=csi:73~",
        "ctrl+shift+Tab=csi:76~",
        "cmd+j=csi:90;1~",
        "cmd+s=csi:90;2~",
        "cmd+shift+c=csi:90;3~",
        "cmd+alt+shift+c=csi:90;4~",
        "cmd+.=csi:90;6~",
        "cmd+e=csi:90;7~",
        "cmd+b=csi:90;8~",
        "cmd+i=csi:90;9~",
        "cmd+l=csi:90;10~",
        "cmd+shift+i=csi:90;11~",
        "cmd+k=csi:90;12~",
        "cmd+alt+v=csi:90;13~",
        "cmd+d=csi:90;14~",
        "cmd+shift+d=csi:90;15~",
        "cmd+u=csi:90;16~",
        "cmd+shift+u=csi:90;17~",
        "cmd+alt+k=csi:90;18~",
        "cmd+alt+j=csi:90;19~",
        "cmd+alt+shift+k=csi:90;20~",
        "cmd+alt+shift+j=csi:90;21~",
        "alt+\\=esc:\\",
        "alt+shift+c=esc:C",
        "alt+h=esc:H",
        "alt+j=esc:J",
        "alt+k=esc:K",
        "alt+l=esc:L",
        "alt+o=esc:O",
        "alt+n=esc:N",
        "alt+p=esc:P",
        "alt+shift+]=esc:}",
        "alt+shift+[=esc:{",
        "ctrl+alt+]=text:\\x1b\\x1d",
        "alt+x=esc:X",
        "alt+z=esc:Z",
        "alt+Tab=text:\\x1b\\t",
        "alt+r=esc:R",
        "alt+1=esc:1",
        "alt+2=esc:2",
        "alt+3=esc:3",
        "alt+4=esc:4",
        "alt+5=esc:5",
        "alt+6=esc:6",
        "alt+7=esc:7",
        "alt+8=esc:8",
        "alt+9=esc:9",
        "alt+shift+,=esc:<",
        "alt+shift+.=esc:>",
        "ctrl+space=text:\\x00",
    ]
}

fn owned_keybinds(entries: &[&str]) -> Vec<String> {
    entries.iter().map(|entry| (*entry).to_owned()).collect()
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            modifier_remap: Vec::new(),
            keybind: owned_keybinds(common_keybinds()),
            backend_keybinds: BackendKeybindConfig {
                native: owned_keybinds(native_keybinds()),
                rmux: Vec::new(),
                tmux: owned_keybinds(tmux_keybinds()),
                zellij: Vec::new(),
            },
        }
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
        "tabs" => Some(MacosTitlebarStyle::Tabs),
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
        Self {
            version: 1,
            theme: None,
            colors: ColorConfig::default(),
            font: FontConfig::default(),
            chrome: ChromeConfig::default(),
            multiplexer: MultiplexerConfig::default(),
            input: InputConfig::default(),
            session: SessionConfig::default(),
            diagnostics: DiagnosticsConfig::default(),
            window: WindowConfig {
                title: "Bootty".to_owned(),
                width: 1220.0,
                height: 760.0,
                fullscreen: WindowFullscreen::default(),
                window_decoration: WindowDecoration::default(),
                macos_titlebar_style: MacosTitlebarStyle::default(),
            },
            config_path: default_config_path(),
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
            && matches!(
                self.macos_titlebar_style,
                MacosTitlebarStyle::Transparent | MacosTitlebarStyle::Tabs
            )
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
    let document = load_merged_config_document(path, &mut stack, &mut loaded)?;
    let raw = parse_raw_config_source(&document.to_string(), path)?;
    let config_dir = path.parent().unwrap_or_else(|| Path::new("."));
    ConfigResolver {
        path: path.to_path_buf(),
        config_dir,
    }
    .resolve(raw)
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
    let raw = parse_raw_config_source(&source, path)?;

    stack.push(id.clone());
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    for include in raw.include {
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

fn parse_raw_config_source(source: &str, path: &Path) -> ConfigResult<RawConfig> {
    toml_edit::de::from_str(source).map_err(|error| {
        ConfigLoadError::new(format!(
            "failed to parse config file {}: {error}",
            path.display()
        ))
    })
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
        if let Some(theme) = &raw.theme {
            config.theme = Some(theme.clone());
            config.colors = resolve_theme_colors(theme, self.config_dir)?;
        }
        apply_partial_colors(&mut config.colors, raw.colors);
        apply_partial_font(&mut config.font, raw.font);
        apply_partial_chrome(&mut config.chrome, raw.chrome);
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
    apply_value(&mut window.window_decoration, partial.window_decoration);
    apply_value(
        &mut window.macos_titlebar_style,
        partial.macos_titlebar_style,
    );
}

fn apply_partial_font(font: &mut FontConfig, partial: FontPatch) {
    apply_value(&mut font.family, partial.family);
    apply_value(&mut font.size, partial.size);
    apply_value(&mut font.cell_width, partial.cell_width);
    apply_value(&mut font.cell_height, partial.cell_height);
    apply_value(&mut font.baseline_adjustment, partial.baseline_adjustment);
    apply_value(&mut font.underline_position, partial.underline_position);
    apply_value(&mut font.underline_thickness, partial.underline_thickness);
}

fn apply_partial_chrome(chrome: &mut ChromeConfig, partial: ChromePatch) {
    apply_value(&mut chrome.sidebar, partial.sidebar);
    apply_value(&mut chrome.status_bar, partial.status_bar);
    apply_value(&mut chrome.sidebar_width, partial.sidebar_width);
    apply_value(&mut chrome.status_height, partial.status_height);
    apply_value(&mut chrome.gap, partial.gap);
}

fn apply_partial_multiplexer(multiplexer: &mut MultiplexerConfig, partial: MultiplexerPatch) {
    apply_value(&mut multiplexer.backend, partial.backend);
}

fn apply_partial_input(input: &mut InputConfig, partial: InputPatch) {
    apply_value(&mut input.modifier_remap, partial.modifier_remap);
    if let Some(value) = partial.keybind {
        input.keybind = apply_keybind_entries(value);
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
        keybinds.native = apply_keybind_entries(value);
    }
    if let Some(value) = partial.rmux {
        keybinds.rmux = apply_keybind_entries(value);
    }
    if let Some(value) = partial.tmux {
        keybinds.tmux = apply_keybind_entries(value);
    }
    if let Some(value) = partial.zellij {
        keybinds.zellij = apply_keybind_entries(value);
    }
}

fn apply_keybind_entries(entries: Vec<String>) -> Vec<String> {
    entries
        .into_iter()
        .filter(|entry| entry != "clear")
        .collect()
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
}

fn apply_partial_diagnostics(diagnostics: &mut DiagnosticsConfig, partial: DiagnosticsPatch) {
    apply_present(&mut diagnostics.stability_trace, partial.stability_trace);
}

fn apply_partial_colors(colors: &mut ColorConfig, partial: ColorPatch) {
    apply_present(&mut colors.background, partial.background);
    apply_present(&mut colors.foreground, partial.foreground);
    apply_present(&mut colors.cursor, partial.cursor);
    apply_present(&mut colors.cursor_text, partial.cursor_text);
    apply_present(
        &mut colors.selection_background,
        partial.selection_background,
    );
    apply_present(
        &mut colors.selection_foreground,
        partial.selection_foreground,
    );
    apply_value(&mut colors.palette, partial.palette);
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

fn load_builtin_theme(theme: &str) -> Option<ResolvedTheme> {
    BUILTIN_THEMES
        .iter()
        .find(|builtin| builtin.name.eq_ignore_ascii_case(theme))
        .map(|builtin| {
            parse_theme_source(builtin.source, &format!("built-in theme {}", builtin.name))
                .expect("built-in themes must parse")
        })
}

pub fn builtin_theme_names() -> impl Iterator<Item = &'static str> {
    BUILTIN_THEMES.iter().map(|theme| theme.name)
}

fn parse_theme_source(source: &str, label: &str) -> ConfigResult<ResolvedTheme> {
    let raw: RawTheme = toml_edit::de::from_str(source)
        .map_err(|error| ConfigLoadError::new(format!("failed to parse theme {label}: {error}")))?;
    let mut colors = ColorConfig::default();
    apply_partial_colors(&mut colors, raw.colors);
    Ok(ResolvedTheme {
        info: ThemeInfo {
            name: raw.metadata.name.unwrap_or_else(|| label.to_owned()),
            source: raw.metadata.source.unwrap_or_default(),
            license: raw.metadata.license.unwrap_or_default(),
        },
        colors,
    })
}

struct BuiltinTheme {
    name: &'static str,
    source: &'static str,
}

const BUILTIN_THEMES: &[BuiltinTheme] = &[
    BuiltinTheme {
        name: "Catppuccin Mocha",
        source: CATPPUCCIN_MOCHA_THEME,
    },
    BuiltinTheme {
        name: "Catppuccin Latte",
        source: CATPPUCCIN_LATTE_THEME,
    },
    BuiltinTheme {
        name: "TokyoNight Night",
        source: TOKYONIGHT_NIGHT_THEME,
    },
    BuiltinTheme {
        name: "Gruvbox Dark",
        source: GRUVBOX_DARK_THEME,
    },
];

const CATPPUCCIN_MOCHA_THEME: &str = r##"
[metadata]
name = "Catppuccin Mocha"
source = "catppuccin/ghostty and mbadolato/iTerm2-Color-Schemes ghostty/Catppuccin Mocha"
license = "MIT"

[colors]
palette = ["#45475a", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5", "#a6adc8", "#585b70", "#f37799", "#89d88b", "#ebd391", "#74a8fc", "#f2aede", "#6bd7ca", "#bac2de"]
background = "#1e1e2e"
foreground = "#cdd6f4"
cursor = "#f5e0dc"
cursor-text = "#1e1e2e"
selection-background = "#585b70"
selection-foreground = "#cdd6f4"
"##;

const CATPPUCCIN_LATTE_THEME: &str = r##"
[metadata]
name = "Catppuccin Latte"
source = "catppuccin/ghostty and mbadolato/iTerm2-Color-Schemes ghostty/Catppuccin Latte"
license = "MIT"

[colors]
palette = ["#5c5f77", "#d20f39", "#40a02b", "#df8e1d", "#1e66f5", "#ea76cb", "#179299", "#acb0be", "#6c6f85", "#d20f39", "#40a02b", "#df8e1d", "#1e66f5", "#ea76cb", "#179299", "#bcc0cc"]
background = "#eff1f5"
foreground = "#4c4f69"
cursor = "#dc8a78"
cursor-text = "#eff1f5"
selection-background = "#acb0be"
selection-foreground = "#4c4f69"
"##;

const TOKYONIGHT_NIGHT_THEME: &str = r##"
[metadata]
name = "TokyoNight Night"
source = "mbadolato/iTerm2-Color-Schemes ghostty/TokyoNight Night"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#15161e", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6", "#414868", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#c0caf5"]
background = "#1a1b26"
foreground = "#c0caf5"
cursor = "#c0caf5"
selection-background = "#33467c"
selection-foreground = "#c0caf5"
"##;

const GRUVBOX_DARK_THEME: &str = r##"
[metadata]
name = "Gruvbox Dark"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Gruvbox Dark"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#282828", "#cc241d", "#98971a", "#d79921", "#458588", "#b16286", "#689d6a", "#a89984", "#928374", "#fb4934", "#b8bb26", "#fabd2f", "#83a598", "#d3869b", "#8ec07c", "#ebdbb2"]
background = "#282828"
foreground = "#ebdbb2"
cursor = "#ebdbb2"
selection-background = "#504945"
selection-foreground = "#ebdbb2"
"##;

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
