use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use anyhow::Result;
use bootty_config::config::MultiplexerBackendConfig;
use eframe::egui::{self, Pos2, Rect};

mod recorded_chord;

use super::binding_session_names::RenameSessionOutcome;
use super::binding_terminal_facts::TerminalProgress;
use super::command_runtime::CommandRuntime;
use super::config_runtime::AppConfigRuntime;
use super::dialog_runtime::{DialogRuntime, ModalDialog};
use super::terminal_config::{
    terminal_live_config, terminal_session_config_with_side_effects, terminal_text_config,
};
use super::terminal_interaction::{
    TerminalFocusIntent, TerminalInteractionInput, TerminalInteractionRuntime,
};
use super::workspace_runtime::{BindingStateCandidate, WorkspaceRuntime};
use recorded_chord::normalize_recorded_chord;

use crate::{
    app_actions::{
        AppAction, FontSizeAction, KeybindAction, MuxKeyAction, SidebarAction, TerminalFindAction,
        TerminalScrollAction, builtin_app_invocation_for_direct_key,
    },
    commands::{Caller, CommandInvocation, CommandTarget, ResourceKind},
    config::{
        AppearanceMode, AppearanceVariant, BoottyConfig, ConfigWriteOutcome, WindowConfig,
        update_config_document,
    },
    config_reload::CONFIG_HOT_RELOAD_INTERVAL,
    diagnostics::StabilityTraceSample,
    direct_input::{DirectKeyInput, ModifierSideState},
    geometry::{TerminalSurface, ViewTransform},
    input::{
        InputSnapshot, TerminalInputCommand, WheelScrollState,
        focus::InputFocus,
        router::{RoutedInput, route_events},
    },
    input_binding::CopyToClipboard,
    layout::{Divider, SplitDirection},
    mux::{
        RepaintHandle,
        capability::BindingOperation,
        command::MuxCommand,
        controller::{MuxController, MuxScope, SpaceId},
        provider::{MuxAppBackendRegistry, SelectionPublicationPolicy, selected_backend},
        snapshot::{MuxSession, MuxWindow},
        terminal::{ActiveTerminal, TerminalRuntime, decode_scoped_pane_id},
    },
    platform::{
        apply_macos_non_native_fullscreen_presentation, macos_handles_non_native_fullscreen_frame,
        read_clipboard_text, restore_macos_presentation, show_desktop_notification,
        write_clipboard_text,
    },
    renderer::RendererMetrics,
    scheduler::{RepaintScheduler, RepaintSignal},
    terminal::{DrainStats, MouseButton},
    terminal_text::TerminalTextConfig,
    theme::theme_from_config,
    ui::{
        command_palette::{CommandPaletteDialog, CommandPaletteEvent},
        ditch::{DitchAction, DitchSessionDialog, DitchSessionEvent},
        keybind_help::{KeybindHelpDialog, KeybindHelpEvent},
        new_session_picker::{NewMuxSessionDialog, NewSessionPickerEvent},
        rename::{RenameSessionDialog, RenameSessionEvent, RenameTabDialog, RenameTabEvent},
        session_navigation::{BindingSessionGroup, ScopedSessionTarget},
        session_picker::{SessionPickerDialog, SessionPickerEvent},
        space::{SpaceEditorDialog, SpaceEditorEvent, default_space_icon},
        terminal_find::{TerminalFindDialog, TerminalFindEvent},
        theme_picker::{ThemePickerDialog, ThemePickerEvent},
    },
    workspace::SpaceMuxOverride,
};
use bootty_terminal::terminal_engine::{
    TerminalLiveConfig, TerminalSideEffect, TerminalSideEffectEvent,
    encode_iterm2_report_cell_size, encode_iterm2_report_variable, encode_osc52_response,
};

const PRIMARY_WINDOW_STATE_KEY: &str = "main";

/// Per-frame snapshot of everything the state machine needs from the host.
/// Captured once at frame start; `egui::Context` never enters this module.
#[derive(Clone, Debug)]
pub struct FrameInputs {
    pub now: Instant,
    pub events: Vec<egui::Event>,
    pub dropped_file_paths: Vec<PathBuf>,
    pub modifiers: egui::Modifiers,
    pub hover_pos: Option<Pos2>,
    pub pressed_mouse_button: Option<MouseButton>,
    pub viewport: ViewportSnapshot,
    /// Whether the window has focus. Background work that only someone watching would notice —
    /// polling the backend for sessions, animating chrome — backs off when it is false.
    pub window_focused: bool,
    pub renderer_metrics: RendererMetrics,
    pub terminal_cell_width: f32,
    pub terminal_cell_height: f32,
    pub terminal_scale_factor: f32,
    pub terminal_view_transform: ViewTransform,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LocalFileHandoff {
    Ready(String),
    Rejected(&'static str),
}

fn local_file_handoff(paths: &[PathBuf]) -> LocalFileHandoff {
    if paths.is_empty() {
        return LocalFileHandoff::Rejected("file handoff ignored: no local files");
    }
    if paths.iter().any(|path| !path.exists()) {
        return LocalFileHandoff::Rejected("file handoff rejected: local path is unavailable");
    }
    bootty_winit::file_paths::format_file_paths_for_paste(paths.iter().map(PathBuf::as_path))
        .map(LocalFileHandoff::Ready)
        .unwrap_or(LocalFileHandoff::Rejected(
            "file handoff rejected: unsupported local path",
        ))
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ViewportSnapshot {
    pub fullscreen: bool,
    pub maximized: bool,
    pub content_height: f32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceSummary {
    pub id: SpaceId,
    pub name: String,
    pub icon: String,
    pub color: [u8; 3],
    pub tint_sidebar: bool,
    pub active: bool,
    pub error: Option<String>,
}

/// Host actions requested by a frame update, applied by the eframe adapter.
#[derive(Clone, Debug, PartialEq)]
pub enum AppEffect {
    CloseWindow,
    QuitApplication,
    SetWindowTitle(String),
    SetFullscreen(bool),
    SetMaximized(bool),
    SetDecorations(bool),
    RequestCopy,
    RequestRepaint,
    Bell,
    RepaintAfter(Duration),
    SetTerminalTextConfig(TerminalTextConfig),
    SetTerminalCursorIcon(egui::CursorIcon),
    /// Reinstall egui's UI-chrome fonts (settings/sidebar/status) so a `font.ui-family` edit applies
    /// live, mirroring how `SetTerminalTextConfig` re-fonts the terminal.
    SetUiFonts(Vec<String>),
    SetWindowFocus,
    OpenUrl(String),
    OpenSettings,
    /// Open settings to the keybindings page focused on the given action name,
    /// adding an editable row for it if none exists yet.
    ConfigureKeybind(String),
}

pub struct AppState {
    pub(super) window_state_key: String,
    pub(super) commands: CommandRuntime,
    pub(super) workspace: WorkspaceRuntime,
    repaint_scheduler: RepaintScheduler,
    pub(super) last_error: Option<String>,
    last_drain: DrainStats,
    terminal_surface: Option<TerminalSurface>,
    /// The full terminal area the panes were last laid out within, for geometric neighbor lookup.
    last_pane_area: Option<Rect>,
    terminal_view_transform: ViewTransform,
    config_runtime: AppConfigRuntime,
    active_appearance_variant: AppearanceVariant,
    input_focus: InputFocus,
    pub(super) repaint: RepaintHandle,
    direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
    modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
    modifier_sides: ModifierSideState,
    pending_direct_input: Vec<DirectKeyInput>,
    /// While the settings overlay is open the terminal behind it must receive no input, so the
    /// direct (winit) input path is gated on this just like it is on the modal mux dialogs.
    settings_open: bool,
    /// Mirrors whether a Luau-opened floating window is showing. That window lives on `BoottyApp`
    /// rather than here, so input gating reads this mirror to stop feeding the terminal behind it.
    extension_overlay_open: bool,
    terminal_interaction: TerminalInteractionRuntime,
    /// Screen rects of chrome resize handles (sidebar edge, pane dividers) registered during the
    /// previous frame's UI build. A primary press inside one of these must not begin a terminal
    /// text selection — the handle owns that drag. Populated each frame in `show_fixed_layout`.
    chrome_handle_rects: Vec<egui::Rect>,
    wheel_scroll_state: WheelScrollState,
    terminal_cursor_icon: egui::CursorIcon,
    mouse_pointer_hidden_while_typing: bool,
    last_mouse_hover_pos: Option<Pos2>,
    dialogs: DialogRuntime,
    sidebar_hovered_session: Option<ScopedSessionTarget>,
    theme_picker_restore_config: Option<BoottyConfig>,
    macos_non_native_fullscreen_active: bool,
    macos_non_native_fullscreen_pending_apply: bool,
}

fn route_find_modeless_events(
    focus: InputFocus,
    events: Vec<egui::Event>,
    find_rect: Option<egui::Rect>,
    hover_pos: Option<Pos2>,
) -> RoutedInput {
    let Some(find_rect) = find_rect else {
        return route_events(focus, events);
    };

    let mut routed = RoutedInput::default();
    for event in events {
        let inside_find = event_pointer_pos(&event)
            .or(hover_pos.filter(|_| matches!(event, egui::Event::MouseWheel { .. })))
            .is_some_and(|pos| find_rect.contains(pos));
        if inside_find {
            routed.ui_events.push(event);
        } else if focus.terminal_owns_input() || event_is_terminal_pointer(&event) {
            routed.terminal_events.push(event);
        } else {
            routed.ui_events.push(event);
        }
    }
    routed
}

fn event_pointer_pos(event: &egui::Event) -> Option<Pos2> {
    match event {
        egui::Event::PointerMoved(pos) => Some(*pos),
        egui::Event::PointerButton { pos, .. } => Some(*pos),
        _ => None,
    }
}

fn event_is_terminal_pointer(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::PointerMoved(_)
            | egui::Event::PointerButton { .. }
            | egui::Event::MouseWheel { .. }
    )
}

fn scoped_terminal_transition_key(
    scope: MuxScope,
    backend: MultiplexerBackendConfig,
    session_id: &str,
    pane_id: Option<&str>,
) -> String {
    format!(
        "{}:{}:{backend:?}:{session_id}:{}",
        scope.space_id().persistence_value(),
        scope.binding_id().persistence_value(),
        pane_id.unwrap_or_default(),
    )
}

fn terminal_cursor_icon_for_mouse_shape(shape: &str) -> Option<egui::CursorIcon> {
    let normalized = shape.to_ascii_lowercase().replace('_', "-");
    for token in normalized
        .split([';', ',', ':', '=', ' '])
        .filter(|token| !token.is_empty())
    {
        let icon = match token {
            "default" | "reset" | "arrow" => egui::CursorIcon::Default,
            "none" | "hidden" => egui::CursorIcon::None,
            "pointer" | "hand" | "pointing-hand" => egui::CursorIcon::PointingHand,
            "text" | "ibeam" | "i-beam" => egui::CursorIcon::Text,
            "vertical-text" => egui::CursorIcon::VerticalText,
            "crosshair" => egui::CursorIcon::Crosshair,
            "help" => egui::CursorIcon::Help,
            "wait" => egui::CursorIcon::Wait,
            "progress" => egui::CursorIcon::Progress,
            "cell" => egui::CursorIcon::Cell,
            "copy" => egui::CursorIcon::Copy,
            "alias" => egui::CursorIcon::Alias,
            "move" => egui::CursorIcon::Move,
            "no-drop" => egui::CursorIcon::NoDrop,
            "not-allowed" | "forbidden" => egui::CursorIcon::NotAllowed,
            "grab" => egui::CursorIcon::Grab,
            "grabbing" => egui::CursorIcon::Grabbing,
            "all-scroll" => egui::CursorIcon::AllScroll,
            "ew-resize" | "col-resize" | "resize-horizontal" => egui::CursorIcon::ResizeHorizontal,
            "ns-resize" | "row-resize" | "resize-vertical" => egui::CursorIcon::ResizeVertical,
            "nesw-resize" | "resize-nesw" => egui::CursorIcon::ResizeNeSw,
            "nwse-resize" | "resize-nwse" => egui::CursorIcon::ResizeNwSe,
            "e-resize" | "resize-east" => egui::CursorIcon::ResizeEast,
            "s-resize" | "resize-south" => egui::CursorIcon::ResizeSouth,
            "w-resize" | "resize-west" => egui::CursorIcon::ResizeWest,
            "n-resize" | "resize-north" => egui::CursorIcon::ResizeNorth,
            "ne-resize" | "resize-north-east" => egui::CursorIcon::ResizeNorthEast,
            "nw-resize" | "resize-north-west" => egui::CursorIcon::ResizeNorthWest,
            "se-resize" | "resize-south-east" => egui::CursorIcon::ResizeSouthEast,
            "sw-resize" | "resize-south-west" => egui::CursorIcon::ResizeSouthWest,
            "zoom-in" => egui::CursorIcon::ZoomIn,
            "zoom-out" => egui::CursorIcon::ZoomOut,
            _ => continue,
        };
        return Some(icon);
    }
    None
}
fn terminal_report_variable_response(name: &str, session_name: Option<&str>) -> Option<Vec<u8>> {
    match name {
        "session.name" => session_name.map(encode_iterm2_report_variable),
        _ => None,
    }
}

pub(super) fn new_mux_session_request_with_name(
    config: &BoottyConfig,
    name: impl Into<String>,
) -> crate::ui::new_session_picker::NewMuxSessionRequest {
    let cwd = config
        .session
        .working_directory
        .clone()
        .or_else(crate::config::default_working_directory)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| {
            config
                .config_path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_owned()
        });
    crate::ui::new_session_picker::NewMuxSessionRequest {
        session_id: name.into(),
        cwd: cwd.to_string_lossy().into_owned(),
    }
}

pub(super) fn terminal_cwd_for_mux_command(
    live_terminal_cwd: Option<String>,
    anchor_cwd: Option<String>,
) -> Option<String> {
    live_terminal_cwd
        .and_then(|cwd| normalize_terminal_cwd(&cwd))
        .or(anchor_cwd)
}

fn normalize_terminal_cwd(cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    if let Some(path) = cwd.strip_prefix("file://") {
        let path_start = path.find('/')?;
        let path = &path[path_start..];
        return percent_decode(path);
    }
    Some(cwd.to_owned())
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hi = hex_value(*bytes.get(index + 1)?)?;
            let lo = hex_value(*bytes.get(index + 2)?)?;
            decoded.push((hi << 4) | lo);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

impl AppState {
    pub fn new(
        config: BoottyConfig,
        backends: Arc<MuxAppBackendRegistry>,
        repaint: RepaintHandle,
        direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
        modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
    ) -> Result<Self> {
        Self::new_for_window(
            config,
            PRIMARY_WINDOW_STATE_KEY.to_owned(),
            backends,
            repaint,
            direct_input_rx,
            modifier_side_rx,
        )
    }

    pub fn new_for_window(
        config: BoottyConfig,
        window_state_key: String,
        backends: Arc<MuxAppBackendRegistry>,
        repaint: RepaintHandle,
        direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
        modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
    ) -> Result<Self> {
        let mut config_runtime = AppConfigRuntime::new(config)?;
        let config = config_runtime.current();
        let active_appearance_variant = config.appearance.mode.variant(AppearanceVariant::Dark);
        let workspace = WorkspaceRuntime::open(
            config,
            &window_state_key,
            backends,
            active_appearance_variant,
            repaint.clone(),
        )?;
        let app_key_bindings =
            config_runtime.prepare_backend_keybindings(workspace.multiplexer_backend());
        let macos_non_native_fullscreen_active = config.window.non_native_fullscreen_enabled();
        let macos_non_native_fullscreen_applied =
            apply_macos_non_native_fullscreen_presentation(&config.window);
        let macos_non_native_fullscreen_pending_apply =
            macos_non_native_fullscreen_active && !macos_non_native_fullscreen_applied;
        config_runtime.publish_backend_keybindings(app_key_bindings);
        let commands = CommandRuntime::new(repaint.clone());

        Ok(Self {
            window_state_key,
            commands,
            workspace,
            repaint_scheduler: RepaintScheduler::default(),
            last_error: None,
            last_drain: DrainStats::default(),
            terminal_surface: None,
            last_pane_area: None,
            chrome_handle_rects: Vec::new(),
            terminal_view_transform: ViewTransform::IDENTITY,
            config_runtime,
            active_appearance_variant,
            input_focus: InputFocus::Terminal,
            repaint,
            direct_input_rx,
            modifier_side_rx,
            modifier_sides: ModifierSideState::default(),
            pending_direct_input: Vec::new(),
            settings_open: false,
            extension_overlay_open: false,
            terminal_interaction: TerminalInteractionRuntime::default(),
            wheel_scroll_state: WheelScrollState::default(),
            terminal_cursor_icon: egui::CursorIcon::Text,
            mouse_pointer_hidden_while_typing: false,
            last_mouse_hover_pos: None,
            dialogs: DialogRuntime::default(),
            sidebar_hovered_session: None,
            theme_picker_restore_config: None,
            macos_non_native_fullscreen_active,
            macos_non_native_fullscreen_pending_apply,
        })
    }

    pub fn config(&self) -> &BoottyConfig {
        self.config_runtime.current()
    }

    /// Apply a dragged sidebar width to the live config without touching disk, so the layout
    /// tracks the pointer each frame. [`Self::persist_sidebar_width`] writes the final value.
    pub fn set_sidebar_width_live(&mut self, width: f32) {
        self.config_runtime.set_sidebar_width(width);
    }

    /// Persist the sidebar width to `config.toml` on drag release. The live value already matches,
    /// so the hot-reload baseline is refreshed to skip the redundant reload the write would trigger.
    pub fn persist_sidebar_width(&mut self, width: f32) {
        let path = self.config().config_path.clone();
        let result = update_config_document(&path, |document| {
            document.set_item(
                &["chrome", "sidebar-width"],
                bootty_config::toml_edit::value(f64::from(width)),
            )
        });
        match result {
            Ok(outcome) => {
                self.config_runtime.refresh_dependency_graph();
                self.record_config_write_warning(&outcome);
            }
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn persist_appearance_mode(&mut self, mode: AppearanceMode, effects: &mut Vec<AppEffect>) {
        let path = self.config().config_path.clone();
        let token = match mode {
            AppearanceMode::System => "system",
            AppearanceMode::Light => "light",
            AppearanceMode::Dark => "dark",
        };
        let result = update_config_document(&path, |document| {
            document.set_item(
                &["appearance", "mode"],
                bootty_config::toml_edit::value(token),
            )
        });
        match result {
            Ok(outcome) => {
                self.reload_config(effects);
                self.record_config_write_warning(&outcome);
            }
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn persist_active_theme(&mut self, theme: &str, effects: &mut Vec<AppEffect>) {
        let path = self.config().config_path.clone();
        let branch = match self.active_appearance_variant {
            AppearanceVariant::Light => "light",
            AppearanceVariant::Dark => "dark",
        };
        let result = update_config_document(&path, |document| {
            document.set_item(
                &["appearance", branch, "theme"],
                bootty_config::toml_edit::value(theme),
            )
        });
        match result {
            Ok(outcome) => {
                self.reload_config(effects);
                self.record_config_write_warning(&outcome);
            }
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn record_config_write_warning(&mut self, outcome: &ConfigWriteOutcome) {
        let Some(warning) = outcome.durability_warning() else {
            return;
        };
        self.last_error = Some(match self.last_error.take() {
            Some(existing) => format!("{existing}; {warning}"),
            None => warning.to_owned(),
        });
    }

    fn preview_active_theme(&mut self, theme: &str, effects: &mut Vec<AppEffect>) {
        let path = self.config().config_path.clone();
        let Some(config_dir) = path.parent() else {
            return;
        };
        let resolved = match bootty_config::config::resolve_theme(theme, config_dir) {
            Ok(theme) => theme,
            Err(error) => {
                self.last_error = Some(error.to_string());
                return;
            }
        };
        let variant = self.active_appearance_variant;
        let mut config = self.config().clone();
        let branch = match variant {
            AppearanceVariant::Light => &mut config.appearance.light,
            AppearanceVariant::Dark => &mut config.appearance.dark,
        };
        branch.theme = Some(theme.to_owned());
        branch.colors = resolved.colors;
        self.config_runtime.replace_preview_config(config);
        self.publish_live_terminal_config(variant);
        effects.push(AppEffect::RequestRepaint);
    }

    fn restore_theme_picker_preview(&mut self) -> bool {
        let Some(config) = self.theme_picker_restore_config.clone() else {
            return false;
        };
        self.config_runtime.replace_preview_config(config);
        self.publish_live_terminal_config(self.active_appearance_variant);
        true
    }

    fn publish_live_terminal_config(&mut self, variant: AppearanceVariant) {
        let config = self.config().clone();
        let live_config = terminal_live_config(&config, variant);
        let warnings = self.publish_terminal_config(&config, variant, Some(&live_config));
        if !warnings.is_empty() {
            self.last_error = Some(warnings.join("; "));
        }
    }

    pub fn theme_picker_preview_active(&self) -> bool {
        self.theme_picker_restore_config.is_some() && self.dialogs.is_theme_picker()
    }

    pub fn set_appearance_variant(&mut self, variant: AppearanceVariant) {
        if self.active_appearance_variant == variant {
            return;
        }
        self.active_appearance_variant = variant;
        self.publish_live_terminal_config(variant);
    }

    pub fn active_appearance_variant(&self) -> AppearanceVariant {
        self.active_appearance_variant
    }

    pub fn ui_theme(&self) -> bootty_ui::Theme {
        theme_from_config(self.config(), self.active_appearance_variant)
    }

    pub fn mux(&self) -> &MuxController {
        &self.workspace.active.binding.mux
    }

    pub fn mux_scope(&self) -> MuxScope {
        self.workspace.active.binding.scope
    }

    pub fn binding_count(&self) -> usize {
        self.workspace.binding_count()
    }

    pub fn active_space_id(&self) -> SpaceId {
        self.workspace.active_space_id()
    }

    pub fn space_summaries(&self) -> Vec<SpaceSummary> {
        self.workspace.space_summaries()
    }

    pub fn space_transition(&self, now: Instant) -> Option<(SpaceId, SpaceId, f32)> {
        self.workspace.transition(now)
    }

    fn select_space(&mut self, index: u32) -> bool {
        let Some(index) = usize::try_from(index)
            .ok()
            .and_then(|index| index.checked_sub(1))
        else {
            return false;
        };
        self.space_summaries()
            .get(index)
            .is_some_and(|space| self.activate_space_from_ui(space.id))
    }
    fn create_space_with_backend_from_ui(
        &mut self,
        name: &str,
        icon: &str,
        color: [u8; 3],
        tint_sidebar: bool,
        mux: SpaceMuxOverride,
    ) -> bool {
        let config = self.config().clone();
        let space = match self
            .workspace
            .create_space(name, icon, color, tint_sidebar, mux, &config)
        {
            Ok(Some(space)) => space,
            Ok(None) => return false,
            Err(error) => {
                self.last_error = Some(error.to_string());
                return false;
            }
        };
        let space_id = self.workspace.insert_space(
            &space,
            &config,
            self.active_appearance_variant,
            self.repaint.clone(),
        );
        self.activate_space_from_ui(space_id)
    }

    pub fn close_space_from_ui(&mut self, space_id: SpaceId) -> bool {
        let spaces = self.space_summaries();
        if spaces.len() <= 1 {
            return false;
        }
        let Some(index) = spaces.iter().position(|space| space.id == space_id) else {
            return false;
        };
        if space_id == self.workspace.active.id {
            let neighbor = spaces
                .get(index + 1)
                .or_else(|| index.checked_sub(1).and_then(|index| spaces.get(index)));
            if !neighbor.is_some_and(|space| self.activate_space_from_ui(space.id)) {
                return false;
            }
        }
        match self.workspace.delete_space(space_id) {
            Ok(true) => {
                self.workspace
                    .inactive_spaces
                    .retain(|space| space.id != space_id);
                true
            }
            Ok(false) => false,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    pub fn update_space_from_ui(
        &mut self,
        space_id: SpaceId,
        name: &str,
        icon: &str,
        color: [u8; 3],
        tint_sidebar: bool,
        mux: SpaceMuxOverride,
    ) -> bool {
        let Some(binding_scope) = self.workspace.selected_binding_scope(space_id) else {
            return false;
        };
        let Some(previous_placement) = self.workspace.space_placement(space_id) else {
            return false;
        };
        let resolved_backend = mux.backend.unwrap_or(self.config().multiplexer.backend);
        let app_key_bindings = if space_id == self.workspace.active.id {
            Some(
                self.config_runtime
                    .prepare_backend_keybindings(resolved_backend),
            )
        } else {
            None
        };
        // The remote decides which machine the binding's sessions live on, so a change to it needs
        // the same rebuild a backend change does.
        let backend_changed = previous_placement != mux;
        let runtime_config = self.config().clone();
        let active_appearance_variant = self.active_appearance_variant;
        let repaint = self.repaint.clone();
        match self
            .workspace
            .update_space(binding_scope, name, icon, color, tint_sidebar, mux)
        {
            Ok(true) => {
                if backend_changed {
                    self.workspace.rebuild_binding(
                        binding_scope,
                        &runtime_config,
                        active_appearance_variant,
                        repaint,
                    );
                    if space_id == self.workspace.active.id {
                        self.config_runtime.publish_backend_keybindings(
                            app_key_bindings.expect("active backend bindings were validated"),
                        );
                        self.terminal_surface = None;
                        self.last_pane_area = None;
                        if let Err(error) = self.sync_terminal_panes() {
                            self.last_error = Some(error.to_string());
                        }
                    }
                }
                true
            }
            Ok(false) => false,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    fn activate_relative_space(&mut self, delta: isize) -> bool {
        let spaces = self.space_summaries();
        let Some(active) = spaces.iter().position(|space| space.active) else {
            return false;
        };
        let Some(target) = active
            .checked_add_signed(delta)
            .and_then(|index| spaces.get(index))
        else {
            return false;
        };
        self.activate_space_from_ui(target.id)
    }

    fn persist_selection_before_publish(
        &mut self,
        session_id: &str,
        window_id: Option<&str>,
    ) -> bool {
        if self
            .workspace
            .active
            .binding
            .backend_policy
            .selection_publication
            != SelectionPublicationPolicy::PersistBeforePublish
        {
            return true;
        }
        match self.workspace.persist_binding_restore_selection(
            self.workspace.active.binding.scope,
            session_id,
            window_id,
        ) {
            Ok(()) => true,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    pub fn activate_space_from_ui(&mut self, space_id: SpaceId) -> bool {
        if space_id == self.workspace.active.id {
            return false;
        }
        let Some(backend) = self.workspace.space_backend(space_id) else {
            return false;
        };
        let app_key_bindings = self.config_runtime.prepare_backend_keybindings(backend);
        let switch_started = crate::diagnostics::latency_start();
        let config = self.config().clone();
        if let Err(error) = self.workspace.activate_space(
            space_id,
            &self.window_state_key,
            &config,
            self.active_appearance_variant,
            &self.repaint,
            Instant::now(),
        ) {
            self.last_error = Some(error.to_string());
            return false;
        }
        self.config_runtime
            .publish_backend_keybindings(app_key_bindings);
        self.terminal_surface = None;
        self.last_pane_area = None;
        self.clear_space_context_dialogs();
        self.input_focus = InputFocus::Terminal;
        let phase = crate::diagnostics::latency_start();
        if let Err(error) = self.sync_terminal_panes() {
            self.last_error = Some(error.to_string());
        }
        crate::diagnostics::trace_phase("space.sync_terminal_panes", phase);
        crate::diagnostics::trace_phase("space.TOTAL", switch_started);
        (self.repaint)();
        true
    }

    fn clear_space_context_dialogs(&mut self) {
        self.dialogs.clear_space_context();
        self.sidebar_hovered_session = None;
    }

    pub fn binding_session_groups(&self) -> Vec<BindingSessionGroup> {
        self.workspace.active_binding_session_groups()
    }

    /// Every session the workspace can reach, grouped by the Space that owns it, with a trailing
    /// group for the sessions no Space claims. The finder needs the owner to know whether selecting a
    /// session means switching Spaces or adopting the session into the current one; the sidebar stays
    /// on `binding_session_groups`, which is this Space only.
    pub fn session_finder_groups(&self) -> Vec<BindingSessionGroup> {
        self.workspace.session_finder_groups()
    }

    fn publish_terminal_config(
        &mut self,
        config: &BoottyConfig,
        variant: AppearanceVariant,
        live_config: Option<&TerminalLiveConfig>,
    ) -> Vec<String> {
        let mut warnings = Vec::new();

        if let Some(owner) = &mut self.workspace.parked_native_terminal {
            let mut owner_config = config.clone();
            owner_config.multiplexer.backend = MultiplexerBackendConfig::Native;
            let session_config = terminal_session_config_with_side_effects(
                &owner_config,
                variant,
                &owner.terminal_side_effect_tx,
            );
            owner.terminal.set_terminal_config(session_config);
            if let Some(live_config) = live_config
                && let Err(error) = owner.terminal.apply_live_config(live_config.clone())
            {
                warnings.push(format!(
                    "terminal config publication failed for parked native terminal: {error}"
                ));
            }
        }

        for binding in self.workspace.bindings_mut() {
            let mut binding_config = config.clone();
            binding_config.multiplexer = binding.multiplexer.clone();
            let session_config = terminal_session_config_with_side_effects(
                &binding_config,
                variant,
                &binding.terminal_side_effect_tx,
            );
            binding.terminal.set_terminal_config(session_config);
            if let Some(live_config) = live_config
                && let Err(error) = binding.terminal.apply_live_config(live_config.clone())
            {
                warnings.push(format!(
                    "terminal config publication failed for {:?}: {error}",
                    binding.scope
                ));
            }
        }

        warnings
    }

    pub(super) fn active_multiplexer(&self) -> &crate::config::MultiplexerConfig {
        &self.workspace.active.binding.multiplexer
    }

    pub fn multiplexer_backend(&self) -> crate::config::MultiplexerBackendConfig {
        self.workspace.active.binding.multiplexer.backend
    }

    pub fn terminal_transition_key(&self) -> Option<String> {
        self.workspace
            .active
            .binding
            .mux
            .selected_session_anchor()
            .map(|anchor| {
                scoped_terminal_transition_key(
                    self.workspace.active.binding.scope,
                    selected_backend(self.active_multiplexer()),
                    &anchor.session_id,
                    anchor.pane_id.as_deref(),
                )
            })
    }

    pub fn last_error(&self) -> Option<&str> {
        self.workspace
            .active
            .binding
            .mux
            .last_error()
            .or(self.last_error.as_deref())
    }

    pub fn clear_last_error(&mut self) {
        self.workspace.active.binding.mux.set_error(None);
        self.last_error = None;
    }

    pub fn sidebar_focused(&self) -> bool {
        self.input_focus == InputFocus::Sidebar
    }

    pub fn terminal_focused(&self) -> bool {
        self.direct_terminal_input_enabled()
    }

    pub fn sidebar_hovered_session(&self) -> Option<&ScopedSessionTarget> {
        self.sidebar_hovered_session.as_ref()
    }
    pub fn direct_input_suppresses_egui_events(&self) -> bool {
        self.direct_terminal_input_enabled()
    }

    /// Mirror the settings overlay's open/closed state so the direct input path stops feeding the
    /// terminal behind it (otherwise shortcuts like ⌘V paste into the hidden terminal).
    pub fn set_settings_open(&mut self, open: bool) {
        self.settings_open = open;
    }

    /// Mirror whether a Luau floating window is showing so the direct input path stops feeding the
    /// terminal behind it, matching how the native overlays gate input.
    pub fn set_extension_overlay_open(&mut self, open: bool) {
        self.extension_overlay_open = open;
    }

    pub fn macos_non_native_fullscreen_active(&self) -> bool {
        self.macos_non_native_fullscreen_active
    }

    fn sync_macos_non_native_fullscreen_presentation(&mut self) {
        if !self.macos_non_native_fullscreen_pending_apply {
            return;
        }
        if apply_macos_non_native_fullscreen_presentation(&self.config().window) {
            self.macos_non_native_fullscreen_pending_apply = false;
        }
    }

    pub fn terminal_mut(&mut self) -> &mut ActiveTerminal {
        &mut self.workspace.active.binding.terminal
    }

    pub fn record_surface(&mut self, surface: TerminalSurface) {
        self.terminal_surface = Some(surface);
    }

    pub fn record_render_error(&mut self, error: impl ToString) {
        self.last_error = Some(error.to_string());
    }

    /// Reset the registered chrome-handle rects at the start of a UI build; handles re-register
    /// themselves via `register_chrome_handle` as they are drawn.
    pub fn reset_chrome_handles(&mut self) {
        self.chrome_handle_rects.clear();
    }

    pub fn register_chrome_handle(&mut self, rect: egui::Rect) {
        self.chrome_handle_rects.push(rect);
    }

    pub(super) fn uses_native_terminal_layout(&self) -> bool {
        self.workspace.active.binding.uses_native_terminal_layout()
    }

    pub fn pane_widget_key(&self, pane_id: &str) -> String {
        self.workspace.active.binding.pane_widget_key(pane_id)
    }

    fn sync_terminal_panes(&mut self) -> Result<()> {
        self.workspace.sync_active_terminal_panes()
    }

    pub fn native_multi_pane(&self) -> bool {
        self.workspace.active.binding.native_multi_pane()
    }

    pub fn focused_pane(&self) -> Option<String> {
        self.workspace.active.binding.focused_pane()
    }

    pub(crate) fn current_terminal_progress(&self) -> Option<TerminalProgress> {
        self.workspace.active.binding.current_terminal_progress()
    }

    pub(crate) fn pane_progress(&self, pane_id: &str) -> Option<TerminalProgress> {
        self.workspace.active.binding.pane_progress(pane_id)
    }

    pub(crate) fn session_ports(&self, session: &MuxSession) -> Vec<u16> {
        self.workspace.active.binding.session_ports(session)
    }

    pub(crate) fn has_indeterminate_terminal_progress(&self) -> bool {
        self.workspace
            .active
            .binding
            .has_indeterminate_terminal_progress()
    }

    /// The names the active binding shows for `sessions`, in the same order.
    pub(crate) fn session_display_names(&self, sessions: &[MuxSession]) -> Vec<String> {
        self.workspace
            .active
            .binding
            .session_display_names(sessions)
    }

    pub(crate) fn window_has_indeterminate_progress(&self, window: &MuxWindow) -> bool {
        self.workspace
            .active
            .binding
            .window_has_indeterminate_progress(window)
    }

    pub(crate) fn window_progress(&self, window: &MuxWindow) -> Option<u8> {
        self.workspace.active.binding.window_progress(window)
    }

    pub fn pane_rects(&self, area: Rect, gap: f32) -> Vec<(String, Rect)> {
        self.workspace.active.binding.pane_rects(area, gap)
    }

    pub fn pane_dividers(&self, area: Rect, gap: f32) -> Vec<Divider> {
        self.workspace.active.binding.pane_dividers(area, gap)
    }

    pub fn focus_pane(&mut self, pane_id: &str) {
        self.workspace.active.binding.focus_pane(pane_id);
    }

    pub fn set_pane_ratio(&mut self, path: &[u8], ratio: f32, min_fraction: f32) {
        self.workspace
            .active
            .binding
            .set_pane_ratio(path, ratio, min_fraction);
    }

    pub fn terminal_runtime_for_pane(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut (dyn TerminalRuntime + '_)> {
        self.workspace
            .active
            .binding
            .terminal_runtime_for_pane(pane_id)
    }

    pub fn pane_terminal_window_size<F>(&self, leaf_size: F) -> Option<(u16, u16)>
    where
        F: FnMut(&str) -> Option<(u16, u16)>,
    {
        self.workspace
            .active
            .binding
            .pane_terminal_window_size(leaf_size)
    }

    pub fn resize_native_layout_window(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.workspace
            .active
            .binding
            .resize_native_layout_window(cols, rows)
    }

    pub(super) fn sync_native_layout_terminal_now(&mut self) {
        if !self.uses_native_terminal_layout() {
            return;
        }
        if let Err(error) = self.sync_terminal_panes() {
            self.last_error = Some(error.to_string());
        }
    }

    fn split_focused_pane(&mut self, direction: SplitDirection, target_pane_id: Option<&str>) {
        self.workspace
            .active
            .binding
            .split_focused_pane(&self.repaint, direction, target_pane_id);
    }

    pub fn record_pane_area(&mut self, area: Rect) {
        self.last_pane_area = Some(area);
    }

    fn focus_pane_neighbor(&mut self, direction: crate::mux::command::MuxDirection) {
        let Some(area) = self.last_pane_area else {
            return;
        };
        let gap = self.config().chrome.pane_divider_width;
        self.workspace
            .active
            .binding
            .focus_pane_neighbor(direction, area, gap);
    }

    fn focus_pane_relative(&mut self, delta: isize) {
        self.workspace.active.binding.focus_pane_relative(delta);
    }

    pub fn activate_scoped_session_from_ui(&mut self, target: &ScopedSessionTarget) -> bool {
        // A session that belongs to another Space is switched to there, not dragged over here: its
        // binding, terminal, and pane layout all live in that Space.
        if target.scope.space_id() != self.workspace.active.id
            && !self.activate_space_from_ui(target.scope.space_id())
        {
            return false;
        }
        if target.scope != self.workspace.active.binding.scope {
            let Some(backend) = self.workspace.binding_backend(target.scope) else {
                return false;
            };
            let app_key_bindings = self.config_runtime.prepare_backend_keybindings(backend);
            let config = self.config().clone();
            if !self.workspace.activate_binding(
                target.scope,
                &config,
                self.active_appearance_variant,
                &self.repaint,
            ) {
                return false;
            }
            self.config_runtime
                .publish_backend_keybindings(app_key_bindings);
            self.terminal_surface = None;
            self.last_pane_area = None;
        }
        if !self.persist_selection_before_publish(&target.session_id, None) {
            return false;
        }
        self.workspace
            .active
            .binding
            .mux
            .activate_session(&target.session_id);
        self.sync_native_layout_terminal_now();
        self.sidebar_hovered_session = Some(target.clone());
        (self.repaint)();
        true
    }

    pub fn activate_session_from_ui(&mut self, session_id: &str) {
        let target = ScopedSessionTarget::new(self.workspace.active.binding.scope, session_id);
        self.activate_scoped_session_from_ui(&target);
    }

    pub fn activate_relative_session_from_ui(&mut self, session_id: &str, delta: isize) -> bool {
        let Some(session_id) = self
            .workspace
            .active
            .binding
            .relative_session_id(session_id, delta)
        else {
            return false;
        };
        self.activate_session_from_ui(&session_id);
        true
    }

    pub fn activate_relative_scoped_session_from_ui(
        &mut self,
        target: &ScopedSessionTarget,
        delta: isize,
    ) -> bool {
        if !self.activate_scoped_session_from_ui(target) {
            return false;
        }
        self.activate_relative_session_from_ui(&target.session_id, delta)
    }

    pub fn activate_last_session_from_ui(&mut self) -> bool {
        let Some(session_id) = self.workspace.active.binding.previous_session_id() else {
            return false;
        };
        self.activate_session_from_ui(&session_id);
        true
    }

    pub fn activate_window_from_ui(&mut self, session_id: &str, window_id: &str) {
        if !self.persist_selection_before_publish(session_id, Some(window_id)) {
            return;
        }
        let mux_config = self.active_multiplexer().clone();
        self.workspace.active.binding.mux.activate_window(
            session_id,
            window_id,
            &self.repaint,
            &mux_config,
        );
        self.sync_native_layout_terminal_now();
    }

    pub fn activate_relative_window_from_ui(
        &mut self,
        session_id: &str,
        window_id: &str,
        delta: isize,
    ) -> bool {
        let Some((session_id, window_id)) = self
            .workspace
            .active
            .binding
            .relative_window_target(session_id, window_id, delta)
        else {
            return false;
        };
        self.activate_window_from_ui(&session_id, &window_id);
        true
    }

    pub fn activate_last_window_from_ui(&mut self, session_id: &str) -> bool {
        let changed = self
            .workspace
            .active
            .binding
            .activate_last_window(&self.repaint, session_id);
        if changed {
            self.sync_native_layout_terminal_now();
        }
        changed
    }

    pub fn new_tab_for_window_from_ui(&mut self, session_id: &str, window_id: &str) -> bool {
        let changed =
            self.workspace
                .active
                .binding
                .new_tab_for_window(&self.repaint, session_id, window_id);
        if changed {
            self.sync_native_layout_terminal_now();
        }
        changed
    }

    pub fn reorder_window_before_from_ui(&mut self, source: &str, before: Option<&str>) -> bool {
        let changed =
            self.workspace
                .active
                .binding
                .reorder_window_before(&self.repaint, source, before);
        if changed {
            self.sync_native_layout_terminal_now();
        }
        changed
    }

    pub fn move_window_from_ui(&mut self, session_id: &str, window_id: &str, delta: i32) -> bool {
        let changed =
            self.workspace
                .active
                .binding
                .move_window(&self.repaint, session_id, window_id, delta);
        if changed {
            self.sync_native_layout_terminal_now();
        }
        changed
    }

    pub fn close_pane_for_window_from_ui(&mut self, session_id: &str, window_id: &str) -> bool {
        self.workspace
            .active
            .binding
            .close_pane_for_window(&self.repaint, session_id, window_id)
    }

    pub(super) fn commit_binding_state_candidate(
        &mut self,
        candidate: BindingStateCandidate,
    ) -> bool {
        match self.workspace.commit_binding_state_candidate(candidate) {
            Ok(()) => true,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    fn begin_active_binding_membership_mutation(&mut self, command: &MuxCommand) -> bool {
        match self
            .workspace
            .begin_active_binding_membership_mutation(command, None)
        {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }
    /// Every session name the backend already answers to, plus the names bootty has asked it for and
    /// is still waiting on. `keep` is the name of the session being renamed, which must not count as
    /// taken against itself.
    pub(super) fn taken_session_names(&self, keep: Option<&str>) -> Vec<String> {
        self.workspace.taken_session_names(keep)
    }

    fn create_project_session_for_cwd(&mut self, cwd: String) {
        match self.workspace.create_project_session(cwd, &self.repaint) {
            Ok(true) => self.input_focus = InputFocus::Terminal,
            Ok(false) => {}
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn move_selected_session(&mut self, delta: i32) -> bool {
        let Some(selected) = self
            .workspace
            .active
            .binding
            .mux
            .selected_session()
            .map(str::to_owned)
        else {
            return false;
        };
        self.move_session_from_ui(&selected, delta)
    }

    pub fn move_session_from_ui(&mut self, session_id: &str, delta: i32) -> bool {
        let Some(session_name) = self
            .workspace
            .active
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .map(|session| session.name.clone())
        else {
            return false;
        };
        let sessions = self
            .workspace
            .active
            .binding
            .mux
            .sessions()
            .iter()
            .map(|session| session.name.clone())
            .collect::<Vec<_>>();
        let mut candidate = self.workspace.active_binding_state_candidate();
        if !candidate.session_order.move_session(
            &session_name,
            delta,
            sessions.iter().map(String::as_str),
        ) {
            return false;
        }
        self.commit_binding_state_candidate(candidate)
    }

    pub fn reorder_session_before(&mut self, source: &str, target: Option<&str>) -> bool {
        // Per-session anchors: a drag reorders within a group when source and target share one,
        // and moves the whole group across groups.
        let sessions = self
            .workspace
            .active
            .binding
            .mux
            .sessions()
            .iter()
            .map(|session| session.name.clone())
            .collect::<Vec<_>>();
        let mut candidate = self.workspace.active_binding_state_candidate();
        if !candidate.session_order.move_session_before(
            source,
            target,
            sessions.iter().map(String::as_str),
        ) {
            return false;
        }
        self.commit_binding_state_candidate(candidate)
    }

    pub fn take_modal_dialog(&mut self) -> Option<ModalDialog> {
        self.dialogs.take()
    }

    pub fn apply_space_editor_event(&mut self, dialog: SpaceEditorDialog, event: SpaceEditorEvent) {
        match event {
            SpaceEditorEvent::None => self.dialogs.open(ModalDialog::SpaceEditor(dialog)),
            SpaceEditorEvent::Close => self.input_focus = InputFocus::Terminal,
            SpaceEditorEvent::Save {
                space_id,
                name,
                icon,
                color,
                tint_sidebar,
                mux,
            } => {
                let saved = match space_id {
                    Some(space_id) => self.update_space_from_ui(
                        space_id,
                        &name,
                        &icon,
                        color,
                        tint_sidebar,
                        mux.clone(),
                    ),
                    None => self.create_space_with_backend_from_ui(
                        &name,
                        &icon,
                        color,
                        tint_sidebar,
                        mux,
                    ),
                };
                if !saved {
                    self.dialogs.open(ModalDialog::SpaceEditor(dialog));
                }
            }
        }
    }

    pub fn detach_scoped_session_from_space(&mut self, target: &ScopedSessionTarget) -> bool {
        let Some(binding) = self.workspace.binding(target.scope) else {
            return false;
        };
        let Some(name) = binding
            .mux
            .all_sessions()
            .iter()
            .find(|session| session.id == target.session_id || session.name == target.session_id)
            .map(|session| session.name.clone())
        else {
            return false;
        };
        let mut candidate = self
            .workspace
            .binding_state_candidate(target.scope)
            .expect("a live binding has committed workspace state");
        if !candidate.session_order.remove_session(&name) {
            return false;
        }
        if !self.commit_binding_state_candidate(candidate) {
            return false;
        }
        (self.repaint)();
        true
    }

    pub fn apply_session_picker_event(
        &mut self,
        dialog: SessionPickerDialog,
        event: SessionPickerEvent,
    ) {
        match event {
            SessionPickerEvent::None => {
                self.dialogs.open(ModalDialog::SessionPicker(dialog));
            }
            SessionPickerEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            SessionPickerEvent::ActivateSession(target) => {
                self.input_focus = InputFocus::Terminal;
                if let Some(binding) = self.workspace.binding(target.scope)
                    && let Some(name) = binding
                        .mux
                        .all_sessions()
                        .iter()
                        .find(|session| {
                            session.id == target.session_id || session.name == target.session_id
                        })
                        .map(|session| session.name.clone())
                {
                    let mut candidate = self
                        .workspace
                        .binding_state_candidate(target.scope)
                        .expect("a live binding has committed workspace state");
                    candidate.session_order.add_session(&name);
                    if !self.commit_binding_state_candidate(candidate) {
                        return;
                    }
                }
                self.activate_scoped_session_from_ui(&target);
            }
        }
    }

    pub fn apply_rename_session_event(
        &mut self,
        dialog: RenameSessionDialog,
        event: RenameSessionEvent,
    ) {
        match event {
            RenameSessionEvent::None => {
                self.dialogs.open(ModalDialog::RenameSession(dialog));
            }
            RenameSessionEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            RenameSessionEvent::Rename { session_id, name } => {
                let name = name.trim().to_owned();
                if name.is_empty() {
                    self.last_error = Some("session name cannot be empty".to_owned());
                    self.dialogs.open(ModalDialog::RenameSession(dialog));
                    return;
                }
                match self
                    .workspace
                    .rename_active_session(&session_id, &name, &self.repaint)
                {
                    Ok(RenameSessionOutcome::Missing | RenameSessionOutcome::Started) => {}
                    Ok(RenameSessionOutcome::Pending) => {
                        self.dialogs.open(ModalDialog::RenameSession(dialog));
                        return;
                    }
                    Err(error) => {
                        self.last_error = Some(error.to_string());
                        self.dialogs.open(ModalDialog::RenameSession(dialog));
                        return;
                    }
                }
                self.input_focus = InputFocus::Terminal;
            }
        }
    }

    pub fn apply_rename_tab_event(&mut self, dialog: RenameTabDialog, event: RenameTabEvent) {
        match event {
            RenameTabEvent::None => {
                self.dialogs.open(ModalDialog::RenameTab(dialog));
            }
            RenameTabEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            RenameTabEvent::Rename {
                session_id,
                window_id,
                name,
            } => {
                let name = name.trim();
                self.workspace.active.binding.set_custom_window_name(
                    &session_id,
                    &window_id,
                    name,
                    &self.repaint,
                );
                self.input_focus = InputFocus::Terminal;
            }
        }
    }

    pub fn take_terminal_find_dialog(&mut self) -> Option<TerminalFindDialog> {
        self.terminal_interaction.take_find_dialog()
    }

    pub fn apply_terminal_find_event(
        &mut self,
        dialog: TerminalFindDialog,
        event: TerminalFindEvent,
    ) {
        let focused_pane_id = self.focused_pane();
        let outcome = self.terminal_interaction.apply_find_event(
            &mut self.workspace.active.binding.terminal,
            dialog,
            event,
            focused_pane_id.as_deref(),
        );
        self.apply_terminal_focus_intent(outcome.focus_intent);
        if let Some(error) = outcome.last_error {
            self.last_error = Some(error);
        }
    }

    fn apply_terminal_focus_intent(&mut self, intent: TerminalFocusIntent) {
        match intent {
            TerminalFocusIntent::None => {}
            TerminalFocusIntent::Terminal => self.input_focus = InputFocus::Terminal,
            TerminalFocusIntent::Find => self.input_focus = InputFocus::Picker,
        }
    }

    pub fn apply_ditch_session_event(
        &mut self,
        dialog: DitchSessionDialog,
        event: DitchSessionEvent,
    ) {
        match event {
            DitchSessionEvent::None => {
                self.dialogs.open(ModalDialog::DitchSession(dialog));
            }
            DitchSessionEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            DitchSessionEvent::Ditch {
                session_id,
                cwd,
                action,
            } => {
                if let Err(error) = run_ditch_cleanup(cwd.as_deref(), &action) {
                    // The git cleanup failed; keep the session alive and re-show the
                    // dialog so the user sees the error instead of losing the session
                    // on top of an orphaned worktree.
                    self.last_error = Some(format!("ditch: {error}"));
                    self.dialogs.open(ModalDialog::DitchSession(dialog));
                    return;
                }
                let mux_config = self.active_multiplexer().clone();
                let command = MuxCommand::DitchSession {
                    session_id: session_id.clone(),
                };
                if !self.begin_active_binding_membership_mutation(&command) {
                    self.dialogs.open(ModalDialog::DitchSession(dialog));
                    return;
                }
                self.workspace.active.binding.mux.ditch_session(
                    &session_id,
                    &self.repaint,
                    &mux_config,
                );
                if self
                    .workspace
                    .active
                    .binding
                    .membership_completion_is_immediate()
                {
                    self.workspace
                        .active
                        .binding
                        .membership_reconciliation_ready = true;
                }
                self.input_focus = InputFocus::Terminal;
            }
        }
    }

    pub fn apply_keybind_help_event(&mut self, dialog: KeybindHelpDialog, event: KeybindHelpEvent) {
        match event {
            KeybindHelpEvent::None => {
                self.dialogs.open(ModalDialog::KeybindHelp(dialog));
            }
            KeybindHelpEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
        }
    }

    pub fn apply_command_palette_event(
        &mut self,
        dialog: CommandPaletteDialog,
        event: CommandPaletteEvent,
    ) {
        match event {
            CommandPaletteEvent::None => {
                self.dialogs.open(ModalDialog::CommandPalette(dialog));
            }
            CommandPaletteEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            CommandPaletteEvent::Run(command) => {
                // Resolve the user's current context before another queued caller can change it.
                self.input_focus = InputFocus::Terminal;
                let Some(mut invocation) =
                    CommandInvocation::from_catalog(command, Caller::CommandPalette)
                else {
                    return;
                };
                if let Some(kind) = self
                    .commands
                    .catalog()
                    .describe(&invocation.command)
                    .and_then(|descriptor| descriptor.target)
                {
                    let Some(target) = self.current_command_target_for(&invocation.command, kind)
                    else {
                        self.commands.clear_queue();
                        self.last_error = Some(format!("no current {kind:?} target is available"));
                        return;
                    };
                    invocation.target = Some(target);
                }
                self.commands.queue(invocation);
            }
        }
    }

    pub fn apply_theme_picker_event(
        &mut self,
        dialog: ThemePickerDialog,
        event: ThemePickerEvent,
        effects: &mut Vec<AppEffect>,
    ) {
        match event {
            ThemePickerEvent::None => {
                self.dialogs.open(ModalDialog::ThemePicker(dialog));
            }
            ThemePickerEvent::Close => {
                self.input_focus = InputFocus::Terminal;
                if self.restore_theme_picker_preview() {
                    effects.push(AppEffect::RequestRepaint);
                }
                self.theme_picker_restore_config = None;
            }
            ThemePickerEvent::RestorePreview => {
                if self.restore_theme_picker_preview() {
                    effects.push(AppEffect::RequestRepaint);
                }
                self.dialogs.open(ModalDialog::ThemePicker(dialog));
            }
            ThemePickerEvent::Preview(theme) => {
                self.preview_active_theme(&theme, effects);
                self.dialogs.open(ModalDialog::ThemePicker(dialog));
            }
            ThemePickerEvent::Select(theme) => {
                self.input_focus = InputFocus::Terminal;
                self.theme_picker_restore_config = None;
                self.persist_active_theme(&theme, effects);
            }
        }
    }

    pub fn apply_picker_event(
        &mut self,
        dialog: NewMuxSessionDialog,
        event: NewSessionPickerEvent,
    ) {
        match event {
            NewSessionPickerEvent::None => {
                self.dialogs.open(ModalDialog::NewSession(dialog));
            }
            NewSessionPickerEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            NewSessionPickerEvent::Error(error) => {
                self.last_error = Some(error);
                self.dialogs.open(ModalDialog::NewSession(dialog));
            }
            NewSessionPickerEvent::CreateWorktree { repo, branch } => {
                match crate::git::add_worktree(&repo, &branch) {
                    Ok(path) => {
                        self.create_project_session_for_cwd(path);
                        self.input_focus = InputFocus::Terminal;
                    }
                    Err(error) => {
                        self.last_error = Some(format!("worktree: {error}"));
                        self.dialogs.open(ModalDialog::NewSession(dialog));
                    }
                }
            }
            NewSessionPickerEvent::CreateSession { cwd } => {
                self.create_project_session_for_cwd(cwd);
                self.input_focus = InputFocus::Terminal;
            }
        }
    }

    pub fn drain_direct_input(&mut self) {
        if let Some(rx) = &self.modifier_side_rx
            && let Some(latest) = rx.try_iter().last()
        {
            self.modifier_sides = latest;
        }
        let Some(rx) = &self.direct_input_rx else {
            return;
        };
        self.pending_direct_input.extend(rx.try_iter());
    }

    fn drain_terminal_side_effects(
        &mut self,
        side_effects: Vec<TerminalSideEffectEvent>,
        effects: &mut Vec<AppEffect>,
        terminal_cell_width: f32,
        terminal_cell_height: f32,
        terminal_scale_factor: f32,
    ) {
        for side_effect in side_effects {
            self.apply_terminal_side_effect_event(
                side_effect,
                effects,
                terminal_cell_width,
                terminal_cell_height,
                terminal_scale_factor,
            );
        }
    }

    fn apply_terminal_side_effect_event(
        &mut self,
        event: TerminalSideEffectEvent,
        effects: &mut Vec<AppEffect>,
        terminal_cell_width: f32,
        terminal_cell_height: f32,
        terminal_scale_factor: f32,
    ) {
        let TerminalSideEffectEvent {
            source_pane_id,
            effect,
        } = event;
        let source_pane_id = match source_pane_id {
            Some(source_pane_id) => {
                if let Some((scope, pane_id)) = decode_scoped_pane_id(&source_pane_id) {
                    if scope != self.workspace.active.binding.scope {
                        return;
                    }
                    Some(pane_id)
                } else {
                    Some(source_pane_id)
                }
            }
            None => None,
        };
        match effect {
            TerminalSideEffect::Bell => effects.push(AppEffect::Bell),
            TerminalSideEffect::ClipboardWrite(text) => {
                if let Err(error) = write_clipboard_text(&text) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalSideEffect::ClipboardQuery { selection } => match read_clipboard_text() {
                Ok(Some(text)) => {
                    if let Err(error) = self
                        .workspace
                        .active
                        .binding
                        .terminal
                        .write_input(&encode_osc52_response(&selection, &text))
                    {
                        self.last_error = Some(error.to_string());
                    }
                }
                Ok(None) => {}
                Err(error) => self.last_error = Some(error.to_string()),
            },
            TerminalSideEffect::WindowTitle(title) => {
                self.apply_terminal_window_title(source_pane_id.as_deref(), title, effects);
            }
            TerminalSideEffect::WindowIcon(_) => {}
            TerminalSideEffect::DesktopNotification { title, body } => {
                if let Err(error) = show_desktop_notification(&title, &body) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalSideEffect::MouseShape(shape) => {
                if let Some(icon) = terminal_cursor_icon_for_mouse_shape(&shape) {
                    self.terminal_cursor_icon = icon;
                    effects.push(AppEffect::SetTerminalCursorIcon(
                        self.effective_terminal_cursor_icon(),
                    ));
                }
            }
            TerminalSideEffect::OpenUrl(url) => effects.push(AppEffect::OpenUrl(url)),
            TerminalSideEffect::FocusWindow => effects.push(AppEffect::SetWindowFocus),
            TerminalSideEffect::ReportCellSize => {
                let response = encode_iterm2_report_cell_size(
                    terminal_cell_width,
                    terminal_cell_height,
                    terminal_scale_factor,
                );
                if let Err(error) = self
                    .workspace
                    .active
                    .binding
                    .terminal
                    .write_input(&response)
                {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalSideEffect::ReportVariable(name) => {
                if let Some(response) = terminal_report_variable_response(
                    &name,
                    self.workspace.active.binding.mux.selected_session(),
                ) && let Err(error) = self
                    .workspace
                    .active
                    .binding
                    .terminal
                    .write_input(&response)
                {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalSideEffect::ConEmuProgress { state, value } => {
                self.workspace.active.binding.record_terminal_progress(
                    source_pane_id.as_deref(),
                    &state,
                    value,
                );
                effects.push(AppEffect::RequestRepaint);
            }
            TerminalSideEffect::Iterm2UserVarPorts(ports) => {
                self.workspace
                    .active
                    .binding
                    .record_terminal_ports(source_pane_id.as_deref(), ports);
                effects.push(AppEffect::RequestRepaint);
            }
            TerminalSideEffect::SemanticPrompt(_)
            | TerminalSideEffect::KittyTextSizing(_)
            | TerminalSideEffect::ConEmuControl(_)
            | TerminalSideEffect::Iterm2Control(_)
            | TerminalSideEffect::Iterm2File(_)
            | TerminalSideEffect::UnsupportedHostCommand { .. } => {}
        }
    }

    fn apply_terminal_window_title(
        &mut self,
        source_pane_id: Option<&str>,
        title: String,
        effects: &mut Vec<AppEffect>,
    ) {
        self.workspace.active.binding.apply_window_title(
            source_pane_id,
            title.clone(),
            &self.repaint,
        );
        if source_pane_id.is_none()
            || self.workspace.active.binding.terminal.focused_pane_id() == source_pane_id
        {
            effects.push(AppEffect::SetWindowTitle(title));
        }
    }

    fn effective_terminal_cursor_icon(&self) -> egui::CursorIcon {
        if self.mouse_pointer_hidden_while_typing {
            egui::CursorIcon::None
        } else {
            self.terminal_cursor_icon
        }
    }

    fn set_mouse_pointer_hidden_while_typing(
        &mut self,
        hidden: bool,
        effects: &mut Vec<AppEffect>,
    ) {
        let hidden = hidden && self.config().input.hide_mouse_pointer_while_typing;
        if self.mouse_pointer_hidden_while_typing == hidden {
            return;
        }
        self.mouse_pointer_hidden_while_typing = hidden;
        effects.push(AppEffect::SetTerminalCursorIcon(
            self.effective_terminal_cursor_icon(),
        ));
    }

    fn hide_mouse_pointer_for_terminal_typing(&mut self, effects: &mut Vec<AppEffect>) {
        self.set_mouse_pointer_hidden_while_typing(true, effects);
    }

    fn restore_mouse_pointer_after_pointer_moved(
        &mut self,
        events: &[egui::Event],
        hover_pos: Option<Pos2>,
        effects: &mut Vec<AppEffect>,
    ) {
        let moved_by_event = events
            .iter()
            .any(|event| matches!(event, egui::Event::PointerMoved(_)));
        let moved_by_hover_pos = hover_pos.is_some() && hover_pos != self.last_mouse_hover_pos;
        self.last_mouse_hover_pos = hover_pos;

        if moved_by_event || moved_by_hover_pos {
            self.set_mouse_pointer_hidden_while_typing(false, effects);
        }
    }

    pub fn pending_direct_input(&self) -> &[DirectKeyInput] {
        &self.pending_direct_input
    }

    /// The modifier keys held right now, with their left/right sides, as tracked by the direct
    /// winit input path. The settings recorder needs this for wheel steps, which arrive as egui
    /// events with side-less modifiers.
    pub fn modifier_sides(&self) -> ModifierSideState {
        self.modifier_sides
    }

    /// Drain the pending direct-input chords as binding-trigger strings for the settings keybind
    /// recorder. This is how the recorder captures cmd-modified chords like ⌘V and ⌘⌥X: egui
    /// collapses those into copy/cut/paste events with no key event, but bootty's direct winit path
    /// keeps the full key + modifiers. Only meaningful while settings is open (the terminal is not
    /// consuming this input).
    pub fn take_settings_capture_chords(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_direct_input)
            .into_iter()
            .map(|direct| {
                let chord =
                    crate::input_binding::BindingTrigger::from_key_input_with_modifier_sides(
                        direct.input(),
                    )
                    .format_entry();
                normalize_recorded_chord(chord)
            })
            .collect()
    }

    pub fn update_frame(&mut self, inputs: FrameInputs) -> Vec<AppEffect> {
        let frame_started = crate::diagnostics::latency_start();
        let FrameInputs {
            now,
            events,
            dropped_file_paths,
            modifiers,
            hover_pos,
            pressed_mouse_button,
            viewport,
            window_focused,
            renderer_metrics,
            terminal_cell_width,
            terminal_cell_height,
            terminal_scale_factor,
            terminal_view_transform,
        } = inputs;
        let mut effects = Vec::new();

        self.drain_app_commands(viewport, &mut effects);

        // A command-palette choice from the previous frame runs as soon as viewport/effects are
        // available, before mux refresh can retarget selected-window actions back to backend-active.
        if let Some(invocation) = self.commands.take_queued() {
            let _ = self.dispatch_command(invocation, viewport, &mut effects);
        }

        self.sync_macos_non_native_fullscreen_presentation();
        let drain = self.workspace.drain();
        self.last_drain = drain.active_drain;
        self.drain_terminal_side_effects(
            drain.active_terminal_side_effects,
            &mut effects,
            terminal_cell_width,
            terminal_cell_height,
            terminal_scale_factor,
        );
        let frame_config = self.config().clone();
        let workspace_frame = self.workspace.advance_frame(
            &frame_config,
            self.active_appearance_variant,
            &self.repaint,
            now,
            window_focused,
        );
        if let Some(after) = workspace_frame.next_wake {
            effects.push(AppEffect::RepaintAfter(after));
        }
        self.hot_reload_config_if_changed(&mut effects, now);
        for error in workspace_frame.errors {
            self.last_error = Some(error);
        }
        self.terminal_view_transform = terminal_view_transform;
        self.restore_mouse_pointer_after_pointer_moved(&events, hover_pos, &mut effects);
        let input_commands = self.handle_direct_input(viewport, &mut effects)
            + self.handle_egui_input(
                events,
                modifiers,
                hover_pos,
                pressed_mouse_button,
                viewport,
                &mut effects,
            )
            + self.handle_dropped_file_paths(dropped_file_paths);
        let pending_pty_bytes = self.workspace.active.binding.terminal.pending_pty_len();
        let (cols, rows) = self.workspace.active.binding.terminal.grid_size();
        self.config_runtime.record_stability(StabilityTraceSample {
            selected_session: self.workspace.active.binding.mux.selected_session(),
            cols,
            rows,
            pending_pty_bytes,
            drain_bytes: self.last_drain.bytes,
            drain_elapsed_us: self.last_drain.elapsed_us,
            text_runs: renderer_metrics.text_runs,
            last_error: self.last_error.as_deref(),
        });
        let repaint = self.repaint_scheduler.recommend(RepaintSignal {
            drained_bytes: self.last_drain.bytes,
            drain_elapsed_us: self.last_drain.elapsed_us,
            pending_bytes: pending_pty_bytes,
            dirty_rows: renderer_metrics.dirty_rows,
            cursor_blinking: renderer_metrics.cursor_blinking,
            input_commands,
        });
        let repaint_after = repaint.min(CONFIG_HOT_RELOAD_INTERVAL);
        if repaint_after.is_zero() {
            if !effects
                .iter()
                .any(|effect| matches!(effect, AppEffect::RequestRepaint))
            {
                effects.push(AppEffect::RequestRepaint);
            }
        } else {
            effects.push(AppEffect::RepaintAfter(repaint_after));
        }
        crate::diagnostics::trace_slow("frame.update_frame", frame_started, 8.0);
        effects
    }

    /// Only one floating dialog is shown at a time; opening one closes the rest.
    fn close_overlay_dialogs(&mut self) -> bool {
        let restored_preview = self.restore_theme_picker_preview();
        self.theme_picker_restore_config = None;
        self.dialogs.clear();
        let outcome = self
            .terminal_interaction
            .close_overlay_dialogs(&mut self.workspace.active.binding.terminal);
        if let Some(error) = outcome.last_error {
            self.last_error = Some(error);
        }
        self.apply_terminal_focus_intent(outcome.focus_intent);
        restored_preview
    }

    fn open_new_mux_session_dialog(&mut self) {
        self.close_overlay_dialogs();
        self.dialogs.open(ModalDialog::NewSession(
            self.active_multiplexer()
                .remote
                .clone()
                .map(|remote| NewMuxSessionDialog::open_remote(remote, self.repaint.clone()))
                .unwrap_or_else(NewMuxSessionDialog::open),
        ));
        self.input_focus = InputFocus::Picker;
    }
    pub fn open_create_space_dialog_from_ui(&mut self) -> bool {
        self.close_overlay_dialogs();
        let existing_icons = self
            .space_summaries()
            .into_iter()
            .map(|space| space.icon)
            .collect::<Vec<_>>();
        let profiles = self
            .config()
            .ssh_profiles
            .iter()
            .map(|(id, profile)| (id.clone(), profile.clone()))
            .collect::<Vec<_>>();
        self.dialogs.open(ModalDialog::SpaceEditor(
            SpaceEditorDialog::new_space(
                default_space_icon(&existing_icons),
                SpaceMuxOverride::default(),
            )
            .with_profiles(profiles.into_iter()),
        ));
        self.input_focus = InputFocus::Picker;
        true
    }

    pub fn open_edit_space_dialog_from_ui(&mut self, space_id: SpaceId) -> bool {
        let placement = self.workspace.space_placement(space_id);
        let Some((space, placement)) = self
            .space_summaries()
            .into_iter()
            .find(|space| space.id == space_id)
            .zip(placement)
        else {
            return false;
        };
        self.close_overlay_dialogs();
        let profiles = self
            .config()
            .ssh_profiles
            .iter()
            .map(|(id, profile)| (id.clone(), profile.clone()))
            .collect::<Vec<_>>();
        self.dialogs.open(ModalDialog::SpaceEditor(
            SpaceEditorDialog::edit_space(
                space.id,
                space.name,
                space.icon,
                space.color,
                space.tint_sidebar,
                placement,
            )
            .with_profiles(profiles.into_iter()),
        ));
        self.input_focus = InputFocus::Picker;
        true
    }

    pub fn open_new_session_dialog_from_ui(&mut self) -> bool {
        self.open_new_mux_session_dialog();
        true
    }

    fn open_session_picker_dialog(&mut self) {
        self.close_overlay_dialogs();
        self.dialogs
            .open(ModalDialog::SessionPicker(SessionPickerDialog::open()));
        self.input_focus = InputFocus::Picker;
    }

    pub fn open_session_picker_dialog_from_ui(&mut self) -> bool {
        self.open_session_picker_dialog();
        true
    }

    fn toggle_session_picker_dialog(&mut self) {
        if self.dialogs.is_session_picker() {
            self.dialogs.clear();
            self.input_focus = InputFocus::Terminal;
        } else {
            self.open_session_picker_dialog();
        }
    }

    fn open_rename_session_dialog(&mut self) {
        let Some(selected) = self
            .workspace
            .active
            .binding
            .mux
            .selected_session()
            .map(str::to_owned)
        else {
            return;
        };
        self.open_rename_session_dialog_for(&selected);
    }

    pub fn open_rename_session_dialog_for(&mut self, session_id: &str) -> bool {
        let Some((session_id, name)) = self
            .workspace
            .active
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .map(|session| {
                // Prefill what bootty shows, so a backend-only uniqueness suffix is not something
                // the user has to delete out of the field.
                let name = self
                    .workspace
                    .active
                    .binding
                    .session_names
                    .display_name(&session.id)
                    .unwrap_or(session.name.as_str())
                    .to_owned();
                (session.id.clone(), name)
            })
        else {
            return false;
        };
        self.close_overlay_dialogs();
        self.dialogs
            .open(ModalDialog::RenameSession(RenameSessionDialog::open(
                session_id, name,
            )));
        self.input_focus = InputFocus::Picker;
        true
    }

    fn open_rename_tab_dialog(&mut self) {
        let Some((session_id, window_id, _)) = self.selected_window_for_rename() else {
            return;
        };
        self.open_rename_tab_dialog_for(&session_id, &window_id);
    }

    pub fn open_rename_tab_dialog_for(&mut self, session_id: &str, window_id: &str) -> bool {
        let Some((session_id, window_id, name)) = self
            .workspace
            .active
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .and_then(|session| {
                session
                    .windows
                    .iter()
                    .find(|window| window.id == window_id)
                    .map(|window| (session.id.clone(), window.id.clone(), window.name.clone()))
            })
        else {
            return false;
        };
        self.close_overlay_dialogs();
        self.dialogs
            .open(ModalDialog::RenameTab(RenameTabDialog::open(
                session_id, window_id, name,
            )));
        self.input_focus = InputFocus::Picker;
        true
    }

    fn selected_window_for_rename(&self) -> Option<(String, String, String)> {
        let selected = self.workspace.active.binding.mux.selected_session()?;
        let session = self
            .workspace
            .active
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == selected || session.name == selected)?;
        let window_id = self
            .workspace
            .active
            .binding
            .mux
            .selected_window()
            .or(session.active_window_id.as_deref());
        let window = window_id
            .and_then(|id| session.windows.iter().find(|window| window.id == id))
            .or_else(|| session.windows.first())?;
        Some((session.id.clone(), window.id.clone(), window.name.clone()))
    }

    fn open_ditch_session_dialog(&mut self) {
        let Some(selected) = self
            .workspace
            .active
            .binding
            .mux
            .selected_session()
            .map(str::to_owned)
        else {
            return;
        };
        self.open_ditch_session_dialog_for(&selected);
    }

    pub fn open_ditch_session_dialog_for(&mut self, session_id: &str) -> bool {
        let Some((session_id, cwd)) = self
            .workspace
            .active
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .map(|session| (session.id.clone(), session.anchor.cwd.clone()))
        else {
            return false;
        };
        self.close_overlay_dialogs();
        self.dialogs
            .open(ModalDialog::DitchSession(DitchSessionDialog::open(
                session_id, cwd,
            )));
        self.input_focus = InputFocus::Picker;
        true
    }

    fn open_keybind_help_dialog(&mut self) {
        let bindings = self
            .config()
            .input
            .keybinds_for_backend(self.workspace.active.binding.multiplexer.backend);
        self.close_overlay_dialogs();
        self.dialogs
            .open(ModalDialog::KeybindHelp(KeybindHelpDialog::open(&bindings)));
        self.input_focus = InputFocus::Picker;
    }

    fn open_command_palette_dialog(&mut self) {
        let bindings = self
            .config()
            .input
            .keybinds_for_backend(self.workspace.active.binding.multiplexer.backend);
        self.close_overlay_dialogs();
        self.dialogs
            .open(ModalDialog::CommandPalette(CommandPaletteDialog::open(
                &bindings,
            )));
        self.input_focus = InputFocus::Picker;
    }

    fn open_theme_picker_dialog(&mut self) {
        let config = self.config();
        let branch = match self.active_appearance_variant {
            AppearanceVariant::Light => "Light appearance",
            AppearanceVariant::Dark => "Dark appearance",
        };
        let current = config
            .theme_for_appearance(self.active_appearance_variant)
            .map(str::to_owned);
        let config_path = config.config_path.clone();
        let restore_config = config.clone();
        self.close_overlay_dialogs();
        self.theme_picker_restore_config = Some(restore_config);
        self.dialogs
            .open(ModalDialog::ThemePicker(ThemePickerDialog::open(
                &config_path,
                current.as_deref(),
                branch,
            )));
        self.input_focus = InputFocus::Picker;
    }

    fn direct_terminal_input_enabled(&self) -> bool {
        self.input_focus.terminal_owns_input()
            && !self.dialogs.has_modal()
            && !self.extension_overlay_open
            && !self.settings_open
    }

    pub fn reload_config(&mut self, effects: &mut Vec<AppEffect>) -> bool {
        let change = match self.config_runtime.reload(
            self.workspace.active.binding.multiplexer.backend,
            self.active_appearance_variant,
        ) {
            Ok(change) => change,
            Err(error) => {
                self.last_error = Some(error.to_string());
                return false;
            }
        };
        if let Some(text_config) = change.text_config {
            effects.push(AppEffect::SetTerminalTextConfig(text_config));
        }
        if let Some(ui_fonts) = change.ui_fonts {
            effects.push(AppEffect::SetUiFonts(ui_fonts));
        }
        if let Some(window_title) = change.window_title {
            effects.push(AppEffect::SetWindowTitle(window_title));
        }

        let mut warnings = Vec::new();
        let profile_reload_error = if change.ssh_profiles_changed {
            self.workspace
                .rebuild_profile_bindings(
                    &change.config,
                    None,
                    self.active_appearance_variant,
                    self.repaint.clone(),
                )
                .err()
                .map(|error| error.to_string())
        } else {
            None
        };
        if let Some(error) = profile_reload_error {
            warnings.push(error);
        }
        warnings.extend(self.publish_terminal_config(
            &change.config,
            self.active_appearance_variant,
            change.live_config.as_ref(),
        ));
        self.set_mouse_pointer_hidden_while_typing(self.mouse_pointer_hidden_while_typing, effects);
        self.workspace
            .active
            .binding
            .clear_pending_generated_names();
        if let Err(error) = self.workspace.reconcile_binding_states() {
            warnings.push(error.to_string());
        }
        if self.config_runtime.has_new_session_config_changes() {
            warnings.push(
                "config reloaded; session/window settings require a new window or restart"
                    .to_owned(),
            );
        }
        if let Some(warning) = change.compatibility_warning {
            warnings.push(warning);
        }
        self.last_error = (!warnings.is_empty()).then(|| warnings.join("; "));
        effects.push(AppEffect::RequestRepaint);
        true
    }

    fn hot_reload_config_if_changed(&mut self, effects: &mut Vec<AppEffect>, now: Instant) {
        if !self.config_runtime.reload_due(now) {
            return;
        }
        self.reload_config(effects);
    }

    fn split_app_actions(
        &mut self,
        events: Vec<egui::Event>,
    ) -> (Vec<egui::Event>, Vec<CommandInvocation>) {
        self.config_runtime
            .split_app_actions(events, self.modifier_sides)
    }

    /// While the command palette is open, find and remove the configure-keybinding
    /// chord (`cmd+shift+,` on macOS, `ctrl+shift+,` elsewhere) from `events` so it
    /// doesn't also trigger whatever global binding shares that chord. Returns
    /// whether one was consumed.
    fn take_configure_keybind_chord(&self, events: &mut Vec<egui::Event>) -> bool {
        if !self.dialogs.is_command_palette() {
            return false;
        }
        let macos = cfg!(target_os = "macos");
        let Some(index) = events.iter().position(|event| {
            matches!(
                event,
                egui::Event::Key {
                    key: egui::Key::Comma,
                    pressed: true,
                    modifiers,
                    ..
                } if if macos {
                    modifiers.shift && (modifiers.command || modifiers.mac_cmd)
                        && !modifiers.alt && !modifiers.ctrl
                } else {
                    modifiers.shift && modifiers.ctrl && !modifiers.alt
                }
            )
        }) else {
            return false;
        };
        events.remove(index);
        true
    }

    fn handle_egui_input(
        &mut self,
        events: Vec<egui::Event>,
        modifiers: egui::Modifiers,
        hover_pos: Option<Pos2>,
        pressed_mouse_button: Option<MouseButton>,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) -> usize {
        let terminal_input_enabled = self.direct_terminal_input_enabled();
        let copy_on_select = self.config().input.copy_on_select;
        let surface = self.terminal_surface;
        let view = self.terminal_view_transform;
        let input_focus = self.input_focus;
        let chrome_handle_rects = self.chrome_handle_rects.clone();
        let outcome = self.terminal_interaction.handle_egui_input(
            &mut self.workspace.active.binding.terminal,
            TerminalInteractionInput {
                events,
                modifiers,
                pressed_mouse_button,
                input_focus,
                terminal_input_enabled,
                surface,
                view,
                chrome_handle_rects: &chrome_handle_rects,
                copy_on_select,
            },
        );
        let count = outcome.handled_count;
        if let Some(error) = outcome.last_error {
            self.last_error = Some(error);
        }
        effects.extend(outcome.effects);
        self.apply_terminal_focus_intent(outcome.focus_intent);

        let mut events = outcome.events;
        // `cmd+shift+,` over a palette row jumps to that command's keybinding editor.
        // Consume it here so it does not also fire its own global binding.
        if self.take_configure_keybind_chord(&mut events) {
            let action = self
                .dialogs
                .command_palette()
                .and_then(CommandPaletteDialog::current_action)
                .map(str::to_owned);
            self.close_overlay_dialogs();
            self.input_focus = InputFocus::Terminal;
            if let Some(action) = action {
                effects.push(AppEffect::ConfigureKeybind(action));
            }
        }
        let (events, actions) = self.split_app_actions(events);
        let routed = if let Some(find_rect) = self
            .terminal_interaction
            .find_dialog()
            .and_then(TerminalFindDialog::last_rect)
        {
            route_find_modeless_events(self.input_focus, events, Some(find_rect), hover_pos)
        } else {
            route_events(self.input_focus, events)
        };
        let sidebar_count = self.handle_sidebar_input(routed.ui_events, viewport, effects);
        let terminal_events =
            if terminal_input_enabled || self.terminal_interaction.find_dialog().is_some() {
                routed.terminal_events
            } else {
                Vec::new()
            };
        let snapshot = InputSnapshot {
            events: terminal_events,
            modifiers,
            modifier_sides: self.modifier_sides,
            hover_pos,
            pressed_mouse_button,
            surface: self.terminal_surface,
            mouse_exclusion: self
                .terminal_surface
                .map(crate::renderer::scrollbar_hit_rect),
            view: self.terminal_view_transform,
        };
        let commands = self
            .config_runtime
            .terminal_input_commands(snapshot, &mut self.wheel_scroll_state);
        let count = count + commands.len() + actions.len() + sidebar_count;
        for invocation in actions {
            let _ = self.dispatch_command(invocation, viewport, effects);
        }
        for command in commands {
            self.apply_terminal_input(command, effects);
        }
        count
    }

    fn handle_dropped_file_paths(&mut self, paths: Vec<PathBuf>) -> usize {
        if !self.direct_terminal_input_enabled() {
            return 0;
        }
        if paths.is_empty() {
            return 0;
        }
        if self.workspace.active.binding.multiplexer.remote.is_some() {
            self.last_error = Some("File handoff to remote Spaces is not supported.".to_owned());
            return 0;
        }
        let text = match local_file_handoff(&paths) {
            LocalFileHandoff::Ready(text) => text,
            LocalFileHandoff::Rejected(message) => {
                self.last_error = Some(message.to_owned());
                return 0;
            }
        };
        if let Err(error) = self.workspace.active.binding.terminal.write_paste(&text) {
            self.last_error = Some(error.to_string());
            return 0;
        }
        1
    }

    fn handle_direct_input(
        &mut self,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) -> usize {
        // While settings is open, leave the pending direct input untouched so the keybind recorder
        // can read it in the UI pass; the terminal behind settings must not consume it.
        if self.settings_open {
            return self.pending_direct_input.len();
        }
        let inputs = std::mem::take(&mut self.pending_direct_input);
        let count = inputs.len();
        if count == 0 {
            return 0;
        }
        if !self.direct_terminal_input_enabled() {
            return count;
        }

        let mut copy_mode_active = match self
            .terminal_interaction
            .copy_mode_active(&mut self.workspace.active.binding.terminal)
        {
            Ok(active) => active,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        };
        for input in inputs {
            let mut input = input.input();
            input.mods = self.config_runtime.remap_mods(input.mods);
            let interaction = self.terminal_interaction.handle_direct_input(
                &mut self.workspace.active.binding.terminal,
                input,
                copy_mode_active,
            );
            copy_mode_active = interaction.copy_mode_active;
            effects.extend(interaction.effects);
            if let Some(error) = interaction.last_error {
                self.last_error = Some(error);
            }
            self.apply_terminal_focus_intent(interaction.focus_intent);
            if interaction.consumed {
                continue;
            }
            if let Some(invocation) = self.config_runtime.invocation_for_input(input) {
                if invocation.command == "paste_from_clipboard" {
                    self.terminal_interaction.mark_paste_suppression();
                }
                let _ = self.dispatch_command(invocation, viewport, effects);
                continue;
            }
            if let Some(invocation) = builtin_app_invocation_for_direct_key(input) {
                self.dispatch_command(invocation, viewport, effects);
                continue;
            }
            if copy_mode_active {
                continue;
            }
            if input.mods.command {
                continue;
            }
            self.apply_terminal_input(TerminalInputCommand::Key(input), effects);
        }
        count
    }

    fn handle_sidebar_input(
        &mut self,
        events: Vec<egui::Event>,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) -> usize {
        if self.input_focus != InputFocus::Sidebar {
            return 0;
        }
        self.ensure_sidebar_hovered_session();
        let mut count = 0;
        for event in events {
            count += 1;
            let egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            let Some(invocation) = self.config_runtime.sidebar_invocation(key, modifiers) else {
                continue;
            };
            self.dispatch_command(invocation, viewport, effects);
        }
        count
    }

    pub(super) fn apply_resolved_keybind_action(
        &mut self,
        action: KeybindAction,
        target: Option<&CommandTarget>,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) {
        if let KeybindAction::Mux(action) = action {
            let target_id = |target: &CommandTarget| {
                serde_json::from_str::<Vec<String>>(&target.handle)
                    .expect("validated mux target")
                    .pop()
                    .expect("mux target identity")
            };
            let window_id = target
                .filter(|target| target.kind == ResourceKind::MuxWindow)
                .map(target_id);
            let pane_id = target
                .filter(|target| target.kind == ResourceKind::Pane)
                .map(target_id);
            self.apply_mux_key_action_to_target(action, window_id, pane_id);
            effects.push(AppEffect::RequestRepaint);
        } else {
            self.apply_keybind_action(action, viewport, effects);
        }
    }

    pub(super) fn apply_sidebar_action(&mut self, action: SidebarAction) -> bool {
        match action {
            SidebarAction::Ignore => {}
            SidebarAction::PreviousSession => self.move_sidebar_hover(-1),
            SidebarAction::NextSession => self.move_sidebar_hover(1),
            SidebarAction::ActivateSession => return self.activate_sidebar_hovered_session(),
            SidebarAction::FocusTerminal => self.input_focus = InputFocus::Terminal,
        }
        true
    }

    fn apply_keybind_action(
        &mut self,
        action: KeybindAction,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) {
        match action {
            KeybindAction::App(AppAction::ReloadConfig) => {
                self.reload_config(effects);
            }
            KeybindAction::App(AppAction::Ignore) => {}
            KeybindAction::App(AppAction::NewWindow | AppAction::NewMuxSession) => {
                self.open_new_mux_session_dialog();
            }

            KeybindAction::App(AppAction::SessionPicker) => {
                self.toggle_session_picker_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::CommandPalette) => {
                self.open_command_palette_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::ChangeAppearance(mode)) => {
                self.persist_appearance_mode(mode, effects);
            }
            KeybindAction::App(AppAction::SwitchTheme) => {
                self.open_theme_picker_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::RenameSession) => {
                self.open_rename_session_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::RenameTab) => {
                self.open_rename_tab_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::DitchSession) => {
                self.open_ditch_session_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::EditSpace) => {
                self.open_edit_space_dialog_from_ui(self.workspace.active.id);
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::Quit) => {
                effects.push(AppEffect::QuitApplication);
            }
            KeybindAction::App(AppAction::CreateSpace) => {
                self.open_create_space_dialog_from_ui();
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::CloseSpace) => {
                if !self.close_space_from_ui(self.workspace.active.id) {
                    self.last_error = Some("the last space cannot be closed".to_owned());
                }
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::NextSpace) => {
                if self.activate_relative_space(1) {
                    effects.push(AppEffect::RequestRepaint);
                } else {
                    self.last_error = Some("no other space is available".to_owned());
                }
            }
            KeybindAction::App(AppAction::PreviousSpace) => {
                if self.activate_relative_space(-1) {
                    effects.push(AppEffect::RequestRepaint);
                } else {
                    self.last_error = Some("no other space is available".to_owned());
                }
            }
            KeybindAction::App(AppAction::SelectSpace(index)) => {
                if self.select_space(index) {
                    effects.push(AppEffect::RequestRepaint);
                } else {
                    self.last_error = Some(format!("space {index} is unavailable"));
                }
            }
            KeybindAction::App(AppAction::ShowKeybinds) => {
                self.open_keybind_help_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::Close) => {
                effects.push(AppEffect::CloseWindow);
            }
            KeybindAction::App(AppAction::OpenSettings) => {
                effects.push(AppEffect::OpenSettings);
            }
            KeybindAction::App(AppAction::ToggleFullscreen) => {
                if should_toggle_native_fullscreen(&self.config().window) {
                    effects.push(AppEffect::SetFullscreen(!viewport.fullscreen));
                } else {
                    let next_maximized = next_non_native_fullscreen_state(
                        macos_handles_non_native_fullscreen_frame(&self.config().window),
                        self.macos_non_native_fullscreen_active,
                        viewport.maximized,
                    );
                    self.macos_non_native_fullscreen_active = next_maximized;
                    if next_maximized {
                        self.macos_non_native_fullscreen_pending_apply =
                            !apply_macos_non_native_fullscreen_presentation(&self.config().window);
                    } else {
                        restore_macos_presentation();
                        self.macos_non_native_fullscreen_pending_apply = false;
                    }
                    effects.push(AppEffect::SetFullscreen(false));
                    if !macos_handles_non_native_fullscreen_frame(&self.config().window) {
                        effects.push(AppEffect::SetMaximized(next_maximized));
                    }
                }
            }
            KeybindAction::App(AppAction::ToggleSidebarFocus) => {
                self.close_overlay_dialogs();
                if self.input_focus == InputFocus::Sidebar {
                    self.input_focus = InputFocus::Terminal;
                } else {
                    self.config_runtime.show_sidebar();
                    self.input_focus = InputFocus::Sidebar;
                    self.sidebar_hovered_session = self
                        .workspace
                        .active
                        .binding
                        .mux
                        .selected_session()
                        .and_then(|selected| self.session_target_matching(selected))
                        .or_else(|| self.session_navigation_targets().into_iter().next());
                }
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::ToggleSidebarVisibility) => {
                if !self.config_runtime.toggle_sidebar() {
                    self.input_focus = InputFocus::Terminal;
                }
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::Mux(action) => {
                self.apply_mux_key_action(action);
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::Scroll(action) => self.apply_terminal_scroll_action(action),
            KeybindAction::Write(bytes) => {
                if let Err(error) = self.workspace.active.binding.terminal.write_input(&bytes) {
                    self.last_error = Some(error.to_string());
                } else {
                    self.hide_mouse_pointer_for_terminal_typing(effects);
                }
            }
            KeybindAction::Font(action) => self.apply_font_size_action(action, effects),
            KeybindAction::Find(action) => self.apply_terminal_find_action(action, effects),
            KeybindAction::CopyToClipboard(format) => {
                self.copy_terminal_selection_or_request_copy(format, effects);
            }
            KeybindAction::CopyMode => {
                self.enter_terminal_copy_mode(effects);
            }
            KeybindAction::PasteFromClipboard => match read_clipboard_text() {
                Ok(Some(text)) => {
                    if let Err(error) = self.workspace.active.binding.terminal.write_paste(&text) {
                        self.last_error = Some(error.to_string());
                    }
                }
                Ok(None) => {}
                Err(error) => self.last_error = Some(error.to_string()),
            },
        }
    }

    fn enter_terminal_copy_mode(&mut self, effects: &mut Vec<AppEffect>) {
        let outcome = self
            .terminal_interaction
            .enter_copy_mode(&mut self.workspace.active.binding.terminal);
        if let Some(error) = outcome.last_error {
            self.last_error = Some(error);
        }
        effects.extend(outcome.effects);
    }

    fn copy_terminal_selection_or_request_copy(
        &mut self,
        format: CopyToClipboard,
        effects: &mut Vec<AppEffect>,
    ) {
        let outcome = self
            .terminal_interaction
            .copy_selection_or_request(&mut self.workspace.active.binding.terminal, format);
        if let Some(error) = outcome.last_error {
            self.last_error = Some(error);
        }
        effects.extend(outcome.effects);
    }

    pub fn reconnect_space_from_ui(&mut self, space_id: SpaceId) -> bool {
        self.workspace.reconnect_space(space_id, Instant::now())
    }
    fn close_target_pane(&mut self, target_pane_id: Option<&str>) {
        if self.uses_native_terminal_layout() {
            if let Some(pane_id) = target_pane_id
                .map(str::to_owned)
                .or_else(|| self.focused_pane())
            {
                self.close_pane(&pane_id);
            }
            return;
        }
        let session_id = self
            .workspace
            .active
            .binding
            .mux
            .selected_session()
            .unwrap_or("local")
            .to_owned();
        let mux_config = self.active_multiplexer().clone();
        self.workspace.active.binding.mux.execute_command(
            &self.repaint,
            &mux_config,
            MuxCommand::ClosePane {
                session_id,
                pane_id: target_pane_id.map(str::to_owned),
            },
        );
        self.workspace.active.binding.terminal.discard_active_pane();
    }

    /// Close a specific native pane: remove it from the backend window, kill its PTY, collapse the
    /// split layout, and re-activate the surviving focused pane this frame so it doesn't flash idle.
    fn close_pane(&mut self, pane_id: &str) {
        self.workspace
            .active
            .binding
            .close_focused_pane(&self.repaint, pane_id);
    }

    pub(super) fn mux_operation_for_action(
        &self,
        action: MuxKeyAction,
    ) -> Option<BindingOperation> {
        match action {
            MuxKeyAction::NewTab
                if self
                    .workspace
                    .active
                    .binding
                    .mux
                    .selected_session()
                    .is_none() =>
            {
                Some(BindingOperation::CreateProjectSession)
            }
            MuxKeyAction::NewTab => Some(BindingOperation::CreateWindow),
            MuxKeyAction::NextTab
            | MuxKeyAction::PreviousTab
            | MuxKeyAction::LastTab
            | MuxKeyAction::SelectTab(_) => Some(BindingOperation::NavigateWindow),
            MuxKeyAction::MoveTab(_) => Some(BindingOperation::MoveWindow),
            MuxKeyAction::SplitPane(_) => Some(BindingOperation::SplitPane),
            MuxKeyAction::SelectPane(_) | MuxKeyAction::NextPane | MuxKeyAction::PreviousPane => {
                Some(BindingOperation::NavigatePane)
            }
            MuxKeyAction::KillPane | MuxKeyAction::ClosePane => Some(BindingOperation::ClosePane),
            MuxKeyAction::TogglePaneZoom => Some(BindingOperation::TogglePaneZoom),
            MuxKeyAction::NextSession
            | MuxKeyAction::PreviousSession
            | MuxKeyAction::LastSession
            | MuxKeyAction::SelectSession(_)
            | MuxKeyAction::MoveSession(_) => None,
        }
    }

    fn apply_mux_key_action(&mut self, action: MuxKeyAction) {
        self.apply_mux_key_action_to_target(action, None, None);
    }

    fn apply_mux_key_action_to_target(
        &mut self,
        action: MuxKeyAction,
        target_window_id: Option<String>,
        target_pane_id: Option<String>,
    ) {
        if self.apply_session_navigation_action(action) {
            return;
        }
        if let MuxKeyAction::MoveSession(delta) = action {
            self.move_selected_session(delta);
            return;
        }
        if matches!(action, MuxKeyAction::ClosePane) {
            self.close_target_pane(target_pane_id.as_deref());
            return;
        }
        // On the native engine, killing a pane means removing the focused split leaf and collapsing
        // the layout, same as closing it. Other backends keep tmux/zellij kill-pane semantics.
        if self.uses_native_terminal_layout() && matches!(action, MuxKeyAction::KillPane) {
            self.close_target_pane(target_pane_id.as_deref());
            return;
        }
        if let MuxKeyAction::SplitPane(direction) = action {
            self.split_focused_pane(direction, target_pane_id.as_deref());
            return;
        }
        // On the native engine, directional pane selection moves focus geometrically across the
        // egui split layout. Other backends keep their own (cycling) pane selection.
        if let MuxKeyAction::SelectPane(direction) = action
            && self.uses_native_terminal_layout()
        {
            self.focus_pane_neighbor(direction);
            return;
        }
        // Likewise next/previous pane cycle focus across the split layout's leaves; the mux-state
        // pane selection the command path mutates is invisible to the native layout.
        if self.uses_native_terminal_layout() {
            let delta = match action {
                MuxKeyAction::NextPane => Some(1),
                MuxKeyAction::PreviousPane => Some(-1),
                _ => None,
            };
            if let Some(delta) = delta {
                self.focus_pane_relative(delta);
                return;
            }
        }
        if matches!(action, MuxKeyAction::NewTab)
            && self
                .workspace
                .active
                .binding
                .mux
                .selected_session()
                .is_none()
        {
            let cwd = new_mux_session_request_with_name(self.config(), "").cwd;
            self.create_project_session_for_cwd(cwd);
            self.sync_native_layout_terminal_now();
            return;
        }
        let selected_session = self
            .workspace
            .active
            .binding
            .mux
            .selected_session()
            .unwrap_or("local")
            .to_owned();
        let selected_cwd = terminal_cwd_for_mux_command(
            self.workspace
                .active
                .binding
                .terminal
                .current_working_directory()
                .ok()
                .flatten(),
            self.workspace
                .active
                .binding
                .mux
                .selected_session_anchor()
                .and_then(|anchor| anchor.cwd.clone()),
        );
        let command = match action {
            MuxKeyAction::NewTab => MuxCommand::NewWindow {
                session_id: selected_session,
                cwd: selected_cwd,
            },
            MuxKeyAction::NextTab => MuxCommand::ActivateNextWindow {
                session_id: selected_session,
            },
            MuxKeyAction::PreviousTab => MuxCommand::ActivatePreviousWindow {
                session_id: selected_session,
            },
            MuxKeyAction::LastTab => MuxCommand::ActivateLastWindow {
                session_id: selected_session,
            },
            MuxKeyAction::SelectTab(index) => MuxCommand::ActivateWindowIndex {
                session_id: selected_session,
                index,
            },
            MuxKeyAction::MoveTab(delta) => MuxCommand::MoveWindow {
                session_id: selected_session,
                window_id: self
                    .workspace
                    .active
                    .binding
                    .mux
                    .selected_window()
                    .map(str::to_owned),
                delta,
            },
            MuxKeyAction::SplitPane(_) => {
                unreachable!("split pane is handled before the command match")
            }
            MuxKeyAction::SelectPane(direction) => MuxCommand::SelectPane {
                session_id: selected_session,
                window_id: target_window_id.clone(),
                direction,
            },
            MuxKeyAction::NextPane => MuxCommand::SelectNextPane {
                session_id: selected_session,
                window_id: target_window_id.clone(),
            },
            MuxKeyAction::PreviousPane => MuxCommand::SelectPreviousPane {
                session_id: selected_session,
                window_id: target_window_id.clone(),
            },
            MuxKeyAction::KillPane => MuxCommand::KillPane {
                session_id: selected_session,
                pane_id: target_pane_id.clone(),
            },
            MuxKeyAction::ClosePane => {
                unreachable!("close pane is handled before the command match")
            }
            MuxKeyAction::TogglePaneZoom => MuxCommand::TogglePaneZoom {
                session_id: selected_session,
                pane_id: target_pane_id.clone(),
            },
            MuxKeyAction::NextSession
            | MuxKeyAction::PreviousSession
            | MuxKeyAction::LastSession
            | MuxKeyAction::SelectSession(_)
            | MuxKeyAction::MoveSession(_) => {
                unreachable!("session actions are handled by Bootty state")
            }
        };
        let mux_config = self.active_multiplexer().clone();
        self.workspace
            .active
            .binding
            .mux
            .execute_command(&self.repaint, &mux_config, command);
        self.sync_native_layout_terminal_now();
    }

    fn ensure_sidebar_hovered_session(&mut self) {
        if self.sidebar_hovered_index().is_some() {
            return;
        }
        self.sidebar_hovered_session = self
            .workspace
            .active
            .binding
            .mux
            .selected_session()
            .and_then(|selected| self.session_target_matching(selected))
            .or_else(|| self.session_navigation_targets().into_iter().next());
    }

    fn move_sidebar_hover(&mut self, delta: isize) {
        self.ensure_sidebar_hovered_session();
        let targets = self.session_navigation_targets();
        let Some(current) = self.sidebar_hovered_index() else {
            return;
        };
        let next = (current as isize + delta).rem_euclid(targets.len() as isize) as usize;
        self.sidebar_hovered_session = targets.get(next).cloned();
    }

    fn activate_sidebar_hovered_session(&mut self) -> bool {
        self.ensure_sidebar_hovered_session();
        let activated = self
            .sidebar_hovered_session
            .clone()
            .is_some_and(|target| self.activate_scoped_session_from_ui(&target));
        self.input_focus = InputFocus::Terminal;
        activated
    }

    fn sidebar_hovered_index(&self) -> Option<usize> {
        let hovered = self.sidebar_hovered_session.as_ref()?;
        self.session_navigation_targets()
            .iter()
            .position(|target| target == hovered)
    }

    fn session_navigation_targets(&self) -> Vec<ScopedSessionTarget> {
        self.binding_session_groups()
            .into_iter()
            .flat_map(|group| {
                group
                    .sessions
                    .into_iter()
                    .map(move |session| ScopedSessionTarget::new(group.scope, session.id))
            })
            .collect()
    }

    fn session_target_matching(&self, value: &str) -> Option<ScopedSessionTarget> {
        self.workspace
            .active
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == value || session.name == value)
            .map(|session| {
                ScopedSessionTarget::new(self.workspace.active.binding.scope, session.id.clone())
            })
    }

    fn apply_session_navigation_action(&mut self, action: MuxKeyAction) -> bool {
        let target = match action {
            MuxKeyAction::SelectSession(index) => self
                .workspace
                .active
                .binding
                .mux
                .sessions()
                .get(index.saturating_sub(1) as usize)
                .map(|session| session.id.clone()),
            MuxKeyAction::NextSession => self.relative_session(1),
            MuxKeyAction::PreviousSession => self.relative_session(-1),
            MuxKeyAction::LastSession => self
                .workspace
                .active
                .binding
                .mux
                .previous_selected_session()
                .map(str::to_owned),
            // Not a session-navigation action: let the caller route it.
            _ => return false,
        };
        // Activate when there is a target, but always report the action as handled. Missing a
        // target (e.g. last_session with no prior session) is a no-op here; falling through would
        // reach the command builder's `unreachable!` for these Bootty-owned actions and panic.
        if let Some(target) = target {
            if !self.persist_selection_before_publish(&target, None) {
                return true;
            }
            self.workspace.active.binding.mux.activate_session(&target);
            self.sync_native_layout_terminal_now();
        }
        true
    }

    fn relative_session(&self, delta: isize) -> Option<String> {
        let sessions = self.workspace.active.binding.mux.sessions();
        if sessions.is_empty() {
            return None;
        }
        let selected = self.workspace.active.binding.mux.selected_session();
        let current = selected
            .and_then(|selected| {
                sessions
                    .iter()
                    .position(|session| session.id == selected || session.name == selected)
            })
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(sessions.len() as isize) as usize;
        sessions.get(next).map(|session| session.id.clone())
    }

    fn apply_terminal_find_action(
        &mut self,
        action: TerminalFindAction,
        effects: &mut Vec<AppEffect>,
    ) {
        if self.terminal_interaction.find_action_opens_dialog(&action) {
            self.close_overlay_dialogs();
        }
        let focused_pane_id = self.focused_pane();
        let outcome = self.terminal_interaction.apply_find_action(
            &mut self.workspace.active.binding.terminal,
            action,
            focused_pane_id.as_deref(),
        );
        if let Some(error) = outcome.last_error {
            self.last_error = Some(error);
        }
        effects.extend(outcome.effects);
        self.apply_terminal_focus_intent(outcome.focus_intent);
    }

    fn apply_terminal_scroll_action(&mut self, action: TerminalScrollAction) {
        let delta = match action {
            TerminalScrollAction::Top => -1_000_000,
            TerminalScrollAction::Bottom => 1_000_000,
            TerminalScrollAction::PageUp => {
                -(self.workspace.active.binding.terminal.grid_size().1 as isize)
            }
            TerminalScrollAction::PageDown => {
                self.workspace.active.binding.terminal.grid_size().1 as isize
            }
            TerminalScrollAction::Lines(lines) => isize::from(lines),
        };
        if let Err(error) = self
            .workspace
            .active
            .binding
            .terminal
            .scroll_viewport_delta(delta)
        {
            self.last_error = Some(error.to_string());
        }
    }

    fn apply_terminal_input(
        &mut self,
        command: TerminalInputCommand,
        effects: &mut Vec<AppEffect>,
    ) {
        match command {
            TerminalInputCommand::Text(text) => {
                if let Err(error) = self
                    .workspace
                    .active
                    .binding
                    .terminal
                    .write_input(text.as_bytes())
                {
                    self.last_error = Some(error.to_string());
                } else {
                    self.hide_mouse_pointer_for_terminal_typing(effects);
                }
            }
            TerminalInputCommand::Paste(text) => {
                if let Err(error) = self.workspace.active.binding.terminal.write_paste(&text) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalInputCommand::Focus(focused) => {
                if let Err(error) = self.workspace.active.binding.terminal.encode_focus(focused) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalInputCommand::Key(input) => {
                if let Err(error) = self.workspace.active.binding.terminal.encode_key(input) {
                    self.last_error = Some(error.to_string());
                } else {
                    self.hide_mouse_pointer_for_terminal_typing(effects);
                }
            }
            TerminalInputCommand::Mouse(input) => {
                if let Err(error) = self.workspace.active.binding.terminal.encode_mouse(input) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalInputCommand::MouseWheel {
                input,
                scroll_delta,
            } => {
                if let Err(error) = self
                    .workspace
                    .active
                    .binding
                    .terminal
                    .handle_mouse_wheel(input, scroll_delta)
                {
                    self.last_error = Some(error.to_string());
                }
            }
        }
    }

    fn apply_font_size_action(&mut self, action: FontSizeAction, effects: &mut Vec<AppEffect>) {
        let default_size = BoottyConfig::default().font.size;
        let current_size = self.config().font.size;
        let next_size = match action {
            FontSizeAction::Increase(delta) => current_size + delta,
            FontSizeAction::Decrease(delta) => current_size - delta,
            FontSizeAction::Reset => default_size,
            FontSizeAction::Set(size) => size,
        }
        .max(1.0);
        self.config_runtime.set_font_size(next_size);
        let text_config = terminal_text_config(&self.config().font);
        if let Some(existing) = effects.iter_mut().rev().find_map(|effect| match effect {
            AppEffect::SetTerminalTextConfig(existing) => Some(existing),
            _ => None,
        }) {
            *existing = text_config;
        } else {
            effects.push(AppEffect::SetTerminalTextConfig(text_config));
        }
    }
}

fn should_toggle_native_fullscreen(window: &WindowConfig) -> bool {
    !window.non_native_fullscreen_enabled()
}

fn next_non_native_fullscreen_state(
    macos_handles_frame: bool,
    tracked_active: bool,
    viewport_maximized: bool,
) -> bool {
    if macos_handles_frame {
        !tracked_active
    } else {
        !viewport_maximized
    }
}

/// Run the git side of a ditch before the session is killed. The main worktree is
/// resolved up front because `cwd` stops resolving inside the repo once the linked
/// worktree is removed. Any git failure is returned (the session stays alive) so a
/// running session is never orphaned alongside half-finished cleanup.
fn run_ditch_cleanup(cwd: Option<&str>, action: &DitchAction) -> Result<(), String> {
    let Some(cwd) = cwd else {
        return Ok(());
    };
    match action {
        DitchAction::KillOnly => Ok(()),
        DitchAction::DetachWorktree => crate::git::detach_head(cwd),
        DitchAction::RemoveWorktree { force } => crate::git::remove_worktree(cwd, *force),
        DitchAction::RemoveWorktreeAndBranch {
            force,
            branch,
            repo,
        } => {
            // Skip the worktree removal when its directory is already gone: a
            // prior attempt removed it but failed to delete the branch (e.g. it
            // was checked out elsewhere). Retrying the remove would error on a
            // missing path; instead finish by deleting the branch from `repo`,
            // resolved while the worktree still existed.
            if std::path::Path::new(cwd).exists() {
                crate::git::remove_worktree(cwd, *force)?;
            }
            crate::git::delete_branch(repo, branch, *force)
        }
    }
}
