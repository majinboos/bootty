use std::{
    collections::{HashMap, HashSet},
    hash::Hasher,
    net::{IpAddr, UdpSocket},
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::Result;
use bootty_config::config::MultiplexerBackendConfig;
use eframe::egui::{self, Pos2, Rect};

mod copy_mode;
#[cfg(debug_assertions)]
mod diagnostic_actions;
mod recorded_chord;
mod selection;

use copy_mode::{
    CopyModeKeyAction, copy_mode_action_for_egui_event, copy_mode_action_for_input,
    copy_mode_egui_key_may_emit_text, copy_mode_egui_key_should_pass_to_app,
    copy_mode_input_should_pass_to_app, copy_mode_key_input_present, copy_shortcut_pressed,
    direct_copy_shortcut_pressed,
};
#[cfg(debug_assertions)]
use diagnostic_actions::{DiagnosticAction, DiagnosticActionDriver, DiagnosticRecord};
use recorded_chord::normalize_recorded_chord;
use selection::{TerminalSelectionAction, TerminalSelectionRouteContext, TerminalSelectionRouter};

use super::command_runtime::CommandRuntime;
use super::config_runtime::AppConfigRuntime;
use super::dialog_runtime::{DialogRuntime, ModalDialog};
use super::terminal_config::{
    terminal_live_config, terminal_session_config_with_side_effects, terminal_text_config,
};
use super::workspace_runtime::{
    BindingRuntime, BindingStateCandidate, PendingGeneratedName, RemoteReattach, ScopedPaneId,
    ScopedWindowId, SpaceRuntime, WorkspaceRuntime,
};

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
    diagnostics::{STATUS_METRICS_SAMPLE_INTERVAL, StabilityTraceSample, StatusMetrics},
    direct_input::{DirectKeyInput, ModifierSideState},
    geometry::{TerminalSurface, ViewTransform},
    input::{
        InputSnapshot, TerminalInputCommand, WheelScrollState,
        focus::InputFocus,
        router::{RoutedInput, route_events},
    },
    input_binding::CopyToClipboard,
    layout::{Direction, Divider, PaneLayout, SplitDirection},
    mux::{
        RepaintHandle,
        capability::BindingOperation,
        command::{MuxCommand, MuxSplitDirection},
        config::selected_backend,
        controller::{MuxController, MuxScope, SpaceId, mux_session_refresh_interval},
        snapshot::{MuxPaneAnchor, MuxSession, MuxWindow, MuxWindowProgress},
        terminal::{ActiveTerminal, TerminalRuntime, decode_scoped_pane_id},
    },
    platform::{
        apply_macos_non_native_fullscreen_presentation, macos_handles_non_native_fullscreen_frame,
        read_clipboard_text, restore_macos_presentation, show_desktop_notification,
        write_clipboard_html, write_clipboard_text,
    },
    renderer::{RendererMetrics, TerminalFrameSource},
    scheduler::{RepaintScheduler, RepaintSignal},
    terminal::{DrainStats, MouseButton, TerminalSearchDirection},
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
        terminal_find::{TerminalFindDialog, TerminalFindEvent, TerminalFindResult},
        theme_picker::{ThemePickerDialog, ThemePickerEvent},
    },
    workspace::{SpaceMuxOverride, SpaceRemoteOverride},
};
use bootty_terminal::terminal_engine::{
    TerminalCopyModeAction, TerminalLiveConfig, TerminalSelectionFormat, TerminalSideEffect,
    TerminalSideEffectEvent, encode_iterm2_report_cell_size, encode_iterm2_report_variable,
    encode_osc52_response,
};

const PRIMARY_WINDOW_STATE_KEY: &str = "main";
/// Session-finder heading for sessions running in a backend that no Space has claimed.
const UNCLAIMED_SESSIONS_LABEL: &str = "No space";

/// How soon to wake up for the next session poll, for backends that only report through polling.
/// Native sessions live in-process and report themselves, so they schedule nothing.
fn mux_refresh_repaint_after(
    config: &crate::config::MultiplexerConfig,
    window_focused: bool,
) -> Option<Duration> {
    (selected_backend(config) != MultiplexerBackendConfig::Native)
        .then(|| mux_session_refresh_interval(window_focused))
}
/// Per-frame snapshot of everything the state machine needs from the host.
/// Captured once at frame start; `egui::Context` never enters this module.
#[derive(Clone, Debug)]
pub struct FrameInputs {
    pub now: Instant,
    pub stable_dt_ms: f32,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TerminalProgressState {
    Normal,
    Error,
    Indeterminate,
    Warning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TerminalProgress {
    pub state: TerminalProgressState,
    pub value: Option<u8>,
}

impl TerminalProgress {
    fn from_conemu(state: &str, value: Option<u8>) -> Option<Self> {
        let state = match state {
            "normal" => TerminalProgressState::Normal,
            "error" => TerminalProgressState::Error,
            "indeterminate" => TerminalProgressState::Indeterminate,
            "warning" => TerminalProgressState::Warning,
            "inactive" => return None,
            _ => return None,
        };
        Some(Self { state, value })
    }

    fn from_mux(progress: &MuxWindowProgress) -> Option<Self> {
        Self::from_conemu(&progress.state, progress.percent)
    }

    pub(crate) fn fraction(self) -> Option<f32> {
        self.value
            .map(|value| f32::from(value) / 100.0)
            .or((self.state == TerminalProgressState::Indeterminate).then_some(0.5))
    }

    fn percent(self) -> Option<u8> {
        self.value
            .or((self.state == TerminalProgressState::Indeterminate).then_some(50))
    }
}

struct NetworkChangeDetector {
    next_check: Instant,
    signature: Option<IpAddr>,
}

impl NetworkChangeDetector {
    const INTERVAL: Duration = Duration::from_secs(2);

    fn new(now: Instant) -> Self {
        Self {
            next_check: now + Self::INTERVAL,
            signature: network_signature(),
        }
    }

    fn changed(&mut self, now: Instant) -> bool {
        self.changed_to(now, network_signature())
    }

    fn changed_to(&mut self, now: Instant, signature: Option<IpAddr>) -> bool {
        if now < self.next_check {
            return false;
        }
        self.next_check = now + Self::INTERVAL;
        let changed = signature != self.signature;
        self.signature = signature;
        changed
    }
}

fn network_signature() -> Option<IpAddr> {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("1.1.1.1", 80)).ok()?;
    socket.local_addr().ok().map(|address| address.ip())
}

pub struct AppState {
    pub(super) window_state_key: String,
    pub(super) commands: CommandRuntime,
    pub(super) workspace: WorkspaceRuntime,
    repaint_scheduler: RepaintScheduler,
    network_change_detector: NetworkChangeDetector,
    pub(super) last_error: Option<String>,
    last_drain: DrainStats,
    last_frame_dt_ms: f32,
    status_metrics: StatusMetrics,
    last_status_metrics_sample: Instant,
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
    suppress_next_egui_paste: bool,
    /// While the settings overlay is open the terminal behind it must receive no input, so the
    /// direct (winit) input path is gated on this just like it is on the modal mux dialogs.
    settings_open: bool,
    /// Mirrors whether a Luau-opened floating window is showing. That window lives on `BoottyApp`
    /// rather than here, so input gating reads this mirror to stop feeding the terminal behind it.
    extension_overlay_open: bool,
    terminal_selection: TerminalSelectionRouter,
    /// Screen rects of chrome resize handles (sidebar edge, pane dividers) registered during the
    /// previous frame's UI build. A primary press inside one of these must not begin a terminal
    /// text selection — the handle owns that drag. Populated each frame in `show_fixed_layout`.
    chrome_handle_rects: Vec<egui::Rect>,
    wheel_scroll_state: WheelScrollState,
    terminal_cursor_icon: egui::CursorIcon,
    mouse_pointer_hidden_while_typing: bool,
    last_mouse_hover_pos: Option<Pos2>,
    deferred_profile_binding_rebuilds: HashSet<MuxScope>,
    dialogs: DialogRuntime,
    sidebar_hovered_session: Option<ScopedSessionTarget>,
    terminal_find_dialog: Option<TerminalFindDialog>,
    terminal_find_return_focus_after_search: bool,
    last_terminal_search: String,
    last_terminal_search_direction: TerminalSearchDirection,
    theme_picker_restore_config: Option<BoottyConfig>,
    /// A command-palette choice waiting for the next frame's viewport and effect sink.
    #[cfg(debug_assertions)]
    diagnostic_action_driver: Option<DiagnosticActionDriver>,
    macos_non_native_fullscreen_active: bool,
    macos_non_native_fullscreen_pending_apply: bool,
}

fn remove_first_paste_event(events: &mut Vec<egui::Event>) -> bool {
    if let Some(index) = events
        .iter()
        .position(|event| matches!(event, egui::Event::Paste(_)))
    {
        events.remove(index);
        true
    } else {
        false
    }
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

fn layout_direction(direction: crate::mux::command::MuxDirection) -> Direction {
    use crate::mux::command::MuxDirection;
    match direction {
        MuxDirection::Left => Direction::Left,
        MuxDirection::Right => Direction::Right,
        MuxDirection::Up => Direction::Up,
        MuxDirection::Down => Direction::Down,
    }
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

pub(super) fn mux_split_direction(direction: SplitDirection) -> MuxSplitDirection {
    match direction {
        SplitDirection::Right => MuxSplitDirection::Right,
        SplitDirection::Down => MuxSplitDirection::Down,
    }
}

fn pane_sets_match(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().all(|pane| b.contains(pane))
}

fn focus_after_native_layout_reconcile(
    restored_from_server: bool,
    new_panes: &[String],
    selected_pane: Option<&str>,
) -> Option<String> {
    if restored_from_server {
        return selected_pane.map(str::to_owned);
    }
    if let Some(selected_pane) = selected_pane
        && new_panes.iter().any(|pane| pane == selected_pane)
    {
        return Some(selected_pane.to_owned());
    }
    new_panes.first().cloned()
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
        repaint: RepaintHandle,
        direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
        modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
    ) -> Result<Self> {
        Self::new_for_window(
            config,
            PRIMARY_WINDOW_STATE_KEY.to_owned(),
            repaint,
            direct_input_rx,
            modifier_side_rx,
        )
    }

    pub fn new_for_window(
        config: BoottyConfig,
        window_state_key: String,
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
        #[cfg(debug_assertions)]
        let diagnostic_action_driver = DiagnosticActionDriver::from_env();
        let commands = CommandRuntime::new(repaint.clone());

        Ok(Self {
            window_state_key,
            commands,
            workspace,
            repaint_scheduler: RepaintScheduler::default(),
            network_change_detector: NetworkChangeDetector::new(Instant::now()),
            last_error: None,
            last_drain: DrainStats::default(),
            last_frame_dt_ms: 0.0,
            status_metrics: StatusMetrics::default(),
            last_status_metrics_sample: Instant::now() - STATUS_METRICS_SAMPLE_INTERVAL,
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
            suppress_next_egui_paste: false,
            settings_open: false,
            extension_overlay_open: false,
            terminal_selection: TerminalSelectionRouter::default(),
            wheel_scroll_state: WheelScrollState::default(),
            terminal_cursor_icon: egui::CursorIcon::Text,
            mouse_pointer_hidden_while_typing: false,
            last_mouse_hover_pos: None,
            deferred_profile_binding_rebuilds: HashSet::new(),
            dialogs: DialogRuntime::default(),
            sidebar_hovered_session: None,
            terminal_find_dialog: None,
            terminal_find_return_focus_after_search: false,
            last_terminal_search: String::new(),
            last_terminal_search_direction: TerminalSearchDirection::Next,
            theme_picker_restore_config: None,
            #[cfg(debug_assertions)]
            diagnostic_action_driver,
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
    pub fn create_space_from_ui(
        &mut self,
        name: &str,
        icon: &str,
        color: [u8; 3],
        tint_sidebar: bool,
    ) -> bool {
        self.create_space_with_backend_from_ui(
            name,
            icon,
            color,
            tint_sidebar,
            SpaceMuxOverride::default(),
        )
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

    fn persist_rmux_selection_before_publish(
        &mut self,
        session_id: &str,
        window_id: Option<&str>,
    ) -> bool {
        if selected_backend(&self.workspace.active.binding.multiplexer)
            != MultiplexerBackendConfig::Rmux
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
        let mut bindings = std::iter::once(&self.workspace.active.binding)
            .chain(self.workspace.active.inactive_bindings.iter())
            .collect::<Vec<_>>();
        bindings.sort_by_key(|binding| binding.scope.binding_id().persistence_value());
        bindings
            .iter()
            .map(|binding| {
                let duplicate_label = bindings
                    .iter()
                    .filter(|candidate| candidate.label == binding.label)
                    .count()
                    > 1;
                let label = if duplicate_label {
                    format!(
                        "{} / Binding {}",
                        binding.label,
                        binding.scope.binding_id().persistence_value()
                    )
                } else {
                    binding.label.clone()
                };
                let sessions = binding.mux.sessions().to_vec();
                BindingSessionGroup {
                    scope: binding.scope,
                    label,
                    display_names: binding.session_display_name_map(&sessions),
                    sessions,
                    selected_session: binding.mux.selected_session().map(str::to_owned),
                    active: binding.scope == self.workspace.active.binding.scope,
                    can_return_to_last_session: binding.mux.previous_selected_session().is_some(),
                }
            })
            .collect()
    }

    /// Every session the workspace can reach, grouped by the Space that owns it, with a trailing
    /// group for the sessions no Space claims. The finder needs the owner to know whether selecting a
    /// session means switching Spaces or adopting the session into the current one; the sidebar stays
    /// on `binding_session_groups`, which is this Space only.
    pub fn session_finder_groups(&self) -> Vec<BindingSessionGroup> {
        let mut spaces = vec![(
            self.workspace.active.position,
            self.workspace.active.name.as_str(),
            std::iter::once(&self.workspace.active.binding)
                .chain(self.workspace.active.inactive_bindings.iter())
                .collect::<Vec<_>>(),
        )];
        spaces.extend(self.workspace.inactive_spaces.iter().map(|space| {
            (
                space.position,
                space.name.as_str(),
                space.bindings().collect::<Vec<_>>(),
            )
        }));
        spaces.sort_by_key(|(position, ..)| *position);

        // One entry per session name: only the active binding refreshes, so a Space that has not been
        // visited this run has no snapshot of its own and has to borrow the shared backend's view of
        // its members. Names are what membership is keyed by, so names are the identity here.
        let mut sessions_across_spaces = Vec::<&MuxSession>::new();
        for binding in spaces.iter().flat_map(|(_, _, bindings)| bindings) {
            for session in binding.mux.all_sessions() {
                if !sessions_across_spaces
                    .iter()
                    .any(|known| known.name == session.name)
                {
                    sessions_across_spaces.push(session);
                }
            }
        }

        let mut claimed = HashSet::new();
        let mut groups = Vec::new();
        for (_, space_name, bindings) in &spaces {
            for binding in bindings {
                let members = binding.session_order.session_names();
                let sessions = members
                    .iter()
                    .filter_map(|name| {
                        // The owner's own snapshot first: session ids are per backend, and the id is
                        // what activation targets.
                        binding
                            .mux
                            .all_sessions()
                            .iter()
                            .chain(sessions_across_spaces.iter().copied())
                            .find(|session| session.name == *name)
                            .cloned()
                    })
                    .collect::<Vec<_>>();
                claimed.extend(members);
                if sessions.is_empty() {
                    continue;
                }
                groups.push(BindingSessionGroup {
                    scope: binding.scope,
                    label: if bindings.len() > 1 {
                        format!("{space_name} / {}", binding.label)
                    } else {
                        (*space_name).to_owned()
                    },
                    display_names: binding.session_display_name_map(&sessions),
                    sessions,
                    selected_session: binding.mux.selected_session().map(str::to_owned),
                    active: binding.scope == self.workspace.active.binding.scope,
                    can_return_to_last_session: binding.mux.previous_selected_session().is_some(),
                });
            }
        }

        let unclaimed = sessions_across_spaces
            .into_iter()
            .filter(|session| !claimed.contains(&session.name))
            .cloned()
            .collect::<Vec<_>>();
        if !unclaimed.is_empty() {
            groups.push(BindingSessionGroup {
                // Activating one of these adopts it into the current Space.
                scope: self.workspace.active.binding.scope,
                label: UNCLAIMED_SESSIONS_LABEL.to_owned(),
                sessions: unclaimed,
                selected_session: None,
                active: false,
                can_return_to_last_session: false,
                // No Space owns these, so bootty has no name of its own for them.
                display_names: HashMap::new(),
            });
        }
        groups
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

    pub fn status_metrics(&self) -> StatusMetrics {
        self.status_metrics
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

    fn is_native(&self) -> bool {
        matches!(
            self.active_multiplexer().backend,
            crate::config::MultiplexerBackendConfig::Native
        )
    }

    pub(super) fn uses_native_terminal_layout(&self) -> bool {
        matches!(
            self.active_multiplexer().backend,
            crate::config::MultiplexerBackendConfig::Native
                | crate::config::MultiplexerBackendConfig::Rmux
        )
    }

    fn current_window_key(&self) -> ScopedWindowId {
        let session = self
            .workspace
            .active
            .binding
            .mux
            .selected_session()
            .unwrap_or("local")
            .to_owned();
        let window = self
            .workspace
            .active
            .binding
            .mux
            .selected_window()
            .map(str::to_owned)
            .or_else(|| {
                self.workspace
                    .active
                    .binding
                    .mux
                    .sessions()
                    .iter()
                    .find(|candidate| candidate.id == session || candidate.name == session)
                    .and_then(|candidate| candidate.active_window_id.clone())
            })
            .unwrap_or_default();
        self.workspace.active.binding.window_id(session, window)
    }
    pub fn pane_widget_key(&self, pane_id: &str) -> String {
        let window = self.current_window_key();
        let backend = selected_backend(self.active_multiplexer());
        format!(
            "{}:{}:{backend:?}:{}:{}:{pane_id}",
            window.scope.space_id().persistence_value(),
            window.scope.binding_id().persistence_value(),
            window.session_id,
            window.window_id,
        )
    }

    fn take_pending_pane_split_direction(
        &mut self,
        key: &ScopedWindowId,
    ) -> Option<SplitDirection> {
        self.workspace
            .active
            .binding
            .pending_pane_split_directions
            .remove(key)
            .or_else(|| {
                if key.window_id.is_empty() {
                    None
                } else {
                    self.workspace
                        .active
                        .binding
                        .pending_pane_split_directions
                        .remove(
                            &self
                                .workspace
                                .active
                                .binding
                                .window_id(key.session_id.clone(), String::new()),
                        )
                }
            })
    }

    fn current_pane_layout(&self) -> Option<&PaneLayout> {
        if !self.uses_native_terminal_layout() {
            return None;
        }
        self.workspace
            .active
            .binding
            .pane_layouts
            .get(&self.current_window_key())
    }

    /// Drop split layouts whose `(session, window)` no longer exists, so the map doesn't grow
    /// unbounded as the user creates and destroys native sessions and tabs. Keys are stored by
    /// whatever `current_window_key` recorded (session id, occasionally name), so accept either.
    fn prune_pane_layouts(&mut self) {
        if self.workspace.active.binding.pane_layouts.is_empty() {
            return;
        }
        let mut live = Vec::new();
        for session in self.workspace.active.binding.mux.sessions() {
            for window in &session.windows {
                live.push(
                    self.workspace
                        .active
                        .binding
                        .window_id(session.id.clone(), window.id.clone()),
                );
                live.push(
                    self.workspace
                        .active
                        .binding
                        .window_id(session.name.clone(), window.id.clone()),
                );
            }
        }
        live.push(self.current_window_key());
        self.workspace
            .active
            .binding
            .pane_layouts
            .retain(|key, _| live.contains(key));
    }

    /// Reconcile the active native window's split layout against the backend's pane list, then make
    /// the layout's focused pane the input runtime and keep its siblings live. Non-native backends
    /// fall back to attaching the single selected anchor.
    fn sync_terminal_panes(&mut self) -> Result<()> {
        let phase = crate::diagnostics::latency_start();
        self.prune_pane_layouts();
        crate::diagnostics::trace_slow("panes.prune_pane_layouts", phase, 2.0);
        let phase = crate::diagnostics::latency_start();
        let config = self.active_multiplexer().clone();
        crate::diagnostics::trace_slow("panes.clone_config", phase, 2.0);
        if !self.uses_native_terminal_layout() {
            let phase = crate::diagnostics::latency_start();
            let result = self
                .workspace
                .active
                .binding
                .terminal
                .sync_scoped_mux_anchor(
                    self.workspace.active.binding.scope,
                    &config,
                    self.workspace.active.binding.mux.selected_session_anchor(),
                );
            crate::diagnostics::trace_slow("panes.sync_scoped_mux_anchor", phase, 2.0);
            return result;
        }
        let panes: Vec<MuxPaneAnchor> = self
            .workspace
            .active
            .binding
            .mux
            .selected_window_panes()
            .to_vec();
        let pane_ids: Vec<String> = panes
            .iter()
            .filter_map(|pane| pane.pane_id.clone())
            .collect();
        if pane_ids.is_empty() {
            // Idle native session (all tabs closed): nothing to render.
            return self
                .workspace
                .active
                .binding
                .terminal
                .sync_scoped_mux_anchor(
                    self.workspace.active.binding.scope,
                    &config,
                    self.workspace.active.binding.mux.selected_session_anchor(),
                );
        }
        let key = self.current_window_key();
        let window_id = (!key.window_id.is_empty()).then(|| key.window_id.clone());
        let selected_pane = self
            .workspace
            .active
            .binding
            .mux
            .selected_session_anchor()
            .and_then(|anchor| anchor.pane_id.clone());
        let server_layout = self
            .workspace
            .active
            .binding
            .mux
            .selected_window_layout()
            .and_then(PaneLayout::from_mux_layout)
            .filter(|layout| pane_sets_match(&layout.panes(), &pane_ids));
        let layout_missing = !self
            .workspace
            .active
            .binding
            .pane_layouts
            .contains_key(&key);
        let stale_layout = self
            .workspace
            .active
            .binding
            .pane_layouts
            .get(&key)
            .is_some_and(|layout| layout.panes().iter().all(|pane| !pane_ids.contains(pane)));
        let mut restored_from_server = false;
        if (layout_missing || stale_layout)
            && let Some(layout) = server_layout.clone()
        {
            self.workspace
                .active
                .binding
                .pane_layouts
                .insert(key.clone(), layout);
            restored_from_server = true;
        }

        let previous_panes = self
            .workspace
            .active
            .binding
            .pane_layouts
            .get(&key)
            .map(PaneLayout::panes)
            .unwrap_or_default();
        let new_panes = pane_ids
            .iter()
            .filter(|pane| !previous_panes.contains(pane))
            .cloned()
            .collect::<Vec<_>>();
        let has_new_pane = !new_panes.is_empty();
        {
            let layout = self
                .workspace
                .active
                .binding
                .pane_layouts
                .entry(key.clone())
                .or_insert_with(|| PaneLayout::single(pane_ids[0].clone()));
            // A window id can be reused after its window is closed (native names tabs `tab-N`). If none
            // of the cached layout's panes still exist, it belongs to the old window -- start fresh.
            if layout.panes().iter().all(|pane| !pane_ids.contains(pane)) {
                *layout = PaneLayout::single(pane_ids[0].clone());
            }
        }
        let removed_panes = previous_panes
            .iter()
            .filter(|pane| !pane_ids.contains(pane))
            .cloned()
            .collect::<Vec<_>>();
        let pane_set_changed = has_new_pane || !removed_panes.is_empty();
        if pane_set_changed && let Some(layout) = server_layout {
            self.workspace
                .active
                .binding
                .pane_layouts
                .insert(key.clone(), layout);
            restored_from_server = true;
        } else if pane_set_changed {
            let new_pane_direction = self
                .take_pending_pane_split_direction(&key)
                .unwrap_or(SplitDirection::Right);
            let layout = self
                .workspace
                .active
                .binding
                .pane_layouts
                .get_mut(&key)
                .expect("native layout should be initialized");
            layout.reconcile_with_new_pane_direction(&pane_ids, new_pane_direction);
        }
        let layout = self
            .workspace
            .active
            .binding
            .pane_layouts
            .get_mut(&key)
            .expect("native layout should be initialized");
        if let Some(focus) = focus_after_native_layout_reconcile(
            restored_from_server,
            &new_panes,
            selected_pane.as_deref(),
        ) {
            layout.set_focus(&focus);
        }
        let focused_id = layout.focused().to_owned();
        let focused_anchor = panes
            .iter()
            .find(|pane| pane.pane_id.as_deref() == Some(focused_id.as_str()))
            .cloned();
        self.workspace
            .active
            .binding
            .terminal
            .sync_scoped_native_window(
                self.workspace.active.binding.scope,
                &panes,
                focused_anchor.as_ref(),
                window_id.as_deref(),
                selected_backend(&config),
                config.hide_tmux_status,
            )
    }

    /// True when the active native window holds more than one pane and should render as a split.
    pub fn native_multi_pane(&self) -> bool {
        self.current_pane_layout()
            .is_some_and(|layout| !layout.is_single())
    }

    pub fn focused_pane(&self) -> Option<String> {
        self.current_pane_layout()
            .map(|layout| layout.focused().to_owned())
    }

    fn pane_cache_key(&self, pane_id: &str) -> ScopedPaneId {
        let window = self
            .window_key_for_pane(pane_id)
            .unwrap_or_else(|| self.current_window_key());
        self.workspace.active.binding.pane_id(window, pane_id)
    }

    pub(crate) fn current_terminal_progress(&self) -> Option<TerminalProgress> {
        self.selected_window_backend_progress()
            .or_else(|| self.current_terminal_progress_from_panes())
    }

    fn selected_window_backend_progress(&self) -> Option<TerminalProgress> {
        let selected = self.mux().selected_window();
        self.mux()
            .selected_session_windows()
            .iter()
            .find(|window| match selected {
                Some(selected) => window.id == selected,
                None => window.active,
            })
            .and_then(|window| self.backend_window_progress(window))
    }

    fn current_terminal_progress_from_panes(&self) -> Option<TerminalProgress> {
        self.focused_pane()
            .as_deref()
            .and_then(|pane_id| self.pane_progress(pane_id))
            .or_else(|| {
                self.workspace
                    .active
                    .binding
                    .mux
                    .selected_session_anchor()
                    .and_then(|anchor| anchor.pane_id.as_deref())
                    .and_then(|pane_id| self.pane_progress(pane_id))
            })
            .or(self.workspace.active.binding.unscoped_terminal_progress)
    }

    pub(crate) fn pane_progress(&self, pane_id: &str) -> Option<TerminalProgress> {
        self.workspace
            .active
            .binding
            .terminal_progress
            .get(&self.pane_cache_key(pane_id))
            .copied()
    }

    pub(crate) fn pane_ports(&self, pane_id: &str) -> Option<&[u16]> {
        self.workspace
            .active
            .binding
            .terminal_ports
            .get(&self.pane_cache_key(pane_id))
            .map(Vec::as_slice)
    }

    pub(crate) fn session_ports(&self, session: &MuxSession) -> Vec<u16> {
        let selected = self.workspace.active.binding.mux.selected_session();
        let mut ports =
            if selected == Some(session.id.as_str()) || selected == Some(session.name.as_str()) {
                self.workspace
                    .active
                    .binding
                    .unscoped_terminal_ports
                    .clone()
            } else {
                Vec::new()
            };
        for pane in session
            .windows
            .iter()
            .flat_map(|window| window.panes.iter().chain(std::iter::once(&window.anchor)))
            .filter_map(|pane| pane.pane_id.as_deref())
        {
            if let Some(reported) = self.pane_ports(pane) {
                for port in reported {
                    if !ports.contains(port) {
                        ports.push(*port);
                    }
                }
            }
        }
        ports
    }

    pub(crate) fn has_indeterminate_terminal_progress(&self) -> bool {
        self.workspace
            .active
            .binding
            .terminal_progress
            .values()
            .chain(
                self.workspace
                    .active
                    .binding
                    .unscoped_terminal_progress
                    .iter(),
            )
            .any(|progress| progress.state == TerminalProgressState::Indeterminate)
            || self
                .workspace
                .active
                .binding
                .mux
                .sessions()
                .iter()
                .any(|session| {
                    session
                        .windows
                        .iter()
                        .any(|window| self.window_has_indeterminate_progress(window))
                })
    }

    /// The names the active binding shows for `sessions`, in the same order.
    pub(crate) fn session_display_names(&self, sessions: &[MuxSession]) -> Vec<String> {
        self.workspace
            .active
            .binding
            .session_display_names(sessions)
    }

    pub(crate) fn window_has_indeterminate_progress(&self, window: &MuxWindow) -> bool {
        if let Some(progress) = self.backend_window_progress(window) {
            return progress.state == TerminalProgressState::Indeterminate;
        }
        window
            .panes
            .iter()
            .chain(std::iter::once(&window.anchor))
            .filter_map(|pane| pane.pane_id.as_deref())
            .filter_map(|pane_id| self.pane_progress(pane_id))
            .any(|progress| progress.state == TerminalProgressState::Indeterminate)
    }

    pub(crate) fn window_progress(&self, window: &MuxWindow) -> Option<u8> {
        if let Some(progress) = self.backend_window_progress(window) {
            return progress.percent();
        }
        window
            .panes
            .iter()
            .chain(std::iter::once(&window.anchor))
            .filter_map(|pane| pane.pane_id.as_deref())
            .filter_map(|pane_id| self.pane_progress(pane_id))
            .filter_map(TerminalProgress::percent)
            .max()
    }

    /// An attached client forwards OSC 9;4 only for the pane it is currently showing, so its own
    /// per-window bookkeeping is the only source that can speak for a background window.
    fn backend_window_progress(&self, window: &MuxWindow) -> Option<TerminalProgress> {
        window
            .progress
            .as_ref()
            .and_then(TerminalProgress::from_mux)
    }

    pub fn pane_rects(&self, area: Rect, gap: f32) -> Vec<(String, Rect)> {
        self.current_pane_layout()
            .map(|layout| layout.rects(area, gap))
            .unwrap_or_default()
    }

    pub fn pane_dividers(&self, area: Rect, gap: f32) -> Vec<Divider> {
        self.current_pane_layout()
            .map(|layout| layout.dividers(area, gap))
            .unwrap_or_default()
    }

    pub fn focus_pane(&mut self, pane_id: &str) {
        let key = self.current_window_key();
        let moved = match self.workspace.active.binding.pane_layouts.get_mut(&key) {
            Some(layout) if layout.focused() != pane_id => layout.set_focus(pane_id),
            _ => false,
        };
        // Make the new pane the input runtime this frame so its rect doesn't briefly render the
        // previously focused pane (the deref runtime would otherwise lag until the next frame's sync).
        if moved {
            let _ = self.sync_terminal_panes();
        }
    }

    pub fn set_pane_ratio(&mut self, path: &[u8], ratio: f32, min_fraction: f32) {
        let key = self.current_window_key();
        if let Some(layout) = self.workspace.active.binding.pane_layouts.get_mut(&key) {
            layout.set_ratio_at(path, ratio, min_fraction, min_fraction);
        }
    }

    pub fn terminal_runtime_for_pane(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut (dyn TerminalRuntime + '_)> {
        self.workspace
            .active
            .binding
            .terminal
            .terminal_runtime_for_pane(pane_id)
    }

    pub fn pane_terminal_window_size<F>(&self, leaf_size: F) -> Option<(u16, u16)>
    where
        F: FnMut(&str) -> Option<(u16, u16)>,
    {
        self.current_pane_layout()?.terminal_window_size(leaf_size)
    }

    pub fn resize_native_layout_window(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.workspace
            .active
            .binding
            .terminal
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
        let session = self
            .workspace
            .active
            .binding
            .mux
            .selected_session()
            .unwrap_or("local")
            .to_owned();
        let mux_config = self.active_multiplexer().clone();
        if !self.uses_native_terminal_layout() {
            self.workspace.active.binding.mux.execute_command(
                &self.repaint,
                &mux_config,
                MuxCommand::SplitPane {
                    session_id: session,
                    pane_id: target_pane_id.map(str::to_owned),
                    direction: mux_split_direction(direction),
                },
            );
            return;
        }
        let backend = selected_backend(&mux_config);
        let key = self.current_window_key();
        let focused = target_pane_id.map(str::to_owned).or_else(|| {
            self.workspace
                .active
                .binding
                .pane_layouts
                .get(&key)
                .map(|layout| layout.focused().to_owned())
                .or_else(|| {
                    self.workspace
                        .active
                        .binding
                        .mux
                        .selected_session_anchor()
                        .and_then(|anchor| anchor.pane_id.clone())
                })
        });
        self.workspace.active.binding.mux.execute_command(
            &self.repaint,
            &mux_config,
            MuxCommand::SplitPane {
                session_id: session,
                pane_id: focused.clone(),
                direction: mux_split_direction(direction),
            },
        );
        self.apply_split_layout_after_command(key, focused, direction, backend);
    }

    fn apply_split_layout_after_command(
        &mut self,
        key: ScopedWindowId,
        focused: Option<String>,
        direction: SplitDirection,
        backend: MultiplexerBackendConfig,
    ) {
        if backend == MultiplexerBackendConfig::Rmux {
            self.workspace
                .active
                .binding
                .pending_pane_split_directions
                .insert(key, direction);
            return;
        }

        // The native split synchronously sets the new pane active, so the refreshed anchor names it.
        let new_pane = self
            .workspace
            .active
            .binding
            .mux
            .selected_session_anchor()
            .and_then(|anchor| anchor.pane_id.clone());
        if let Some(new_pane) = new_pane {
            let layout = self
                .workspace
                .active
                .binding
                .pane_layouts
                .entry(key.clone())
                .or_insert_with(|| PaneLayout::single(new_pane.clone()));
            if let Some(focused) = &focused {
                layout.set_focus(focused);
            }
            if !layout.contains(&new_pane) {
                layout.split_focused(new_pane, direction);
            }
            self.workspace
                .active
                .binding
                .pending_pane_split_directions
                .remove(&key);
            let _ = self.sync_terminal_panes();
        }
    }

    pub fn record_pane_area(&mut self, area: Rect) {
        self.last_pane_area = Some(area);
    }

    fn focus_pane_neighbor(&mut self, direction: Direction) {
        let key = self.current_window_key();
        let Some(area) = self.last_pane_area else {
            return;
        };
        let gap = self.config().chrome.pane_divider_width;
        let neighbor = self
            .workspace
            .active
            .binding
            .pane_layouts
            .get(&key)
            .and_then(|layout| layout.neighbor(layout.focused(), direction, area, gap));
        if let Some(neighbor) = neighbor {
            self.focus_pane(&neighbor);
        }
    }

    fn focus_pane_relative(&mut self, delta: isize) {
        let key = self.current_window_key();
        let Some(layout) = self.workspace.active.binding.pane_layouts.get(&key) else {
            return;
        };
        let panes = layout.panes();
        if panes.len() < 2 {
            return;
        }
        let Some(index) = panes.iter().position(|pane| pane == layout.focused()) else {
            return;
        };
        let next = (index as isize + delta).rem_euclid(panes.len() as isize) as usize;
        let pane = panes[next].clone();
        self.focus_pane(&pane);
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
        if !self.persist_rmux_selection_before_publish(&target.session_id, None) {
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
        let sessions = self.workspace.active.binding.mux.sessions();
        let Some(current) = sessions
            .iter()
            .position(|session| session.id == session_id || session.name == session_id)
        else {
            return false;
        };
        let next = (current as isize + delta).rem_euclid(sessions.len() as isize) as usize;
        let session_id = sessions[next].id.clone();
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
        let Some(session_id) = self
            .workspace
            .active
            .binding
            .mux
            .previous_selected_session()
            .map(str::to_owned)
        else {
            return false;
        };
        self.activate_session_from_ui(&session_id);
        true
    }

    pub fn activate_window_from_ui(&mut self, session_id: &str, window_id: &str) {
        if !self.persist_rmux_selection_before_publish(session_id, Some(window_id)) {
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
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .and_then(|session| {
                let mut windows = session.windows.iter().collect::<Vec<_>>();
                windows.sort_by_key(|window| window.index);
                let current = windows.iter().position(|window| window.id == window_id)?;
                let next = (current as isize + delta).rem_euclid(windows.len() as isize) as usize;
                Some((session.id.clone(), windows[next].id.clone()))
            })
        else {
            return false;
        };
        self.activate_window_from_ui(&session_id, &window_id);
        true
    }

    pub fn activate_last_window_from_ui(&mut self, session_id: &str) -> bool {
        let Some(session_id) = self
            .workspace
            .active
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .filter(|session| session.windows.len() > 1)
            .map(|session| session.id.clone())
        else {
            return false;
        };
        let mux_config = self.active_multiplexer().clone();
        self.workspace.active.binding.mux.execute_command(
            &self.repaint,
            &mux_config,
            MuxCommand::ActivateLastWindow { session_id },
        );
        self.sync_native_layout_terminal_now();
        true
    }

    pub fn new_tab_for_window_from_ui(&mut self, session_id: &str, window_id: &str) -> bool {
        let selected_session = self
            .workspace
            .active
            .binding
            .mux
            .selected_session()
            .map(str::to_owned);
        let selected_window = self
            .workspace
            .active
            .binding
            .mux
            .selected_window()
            .map(str::to_owned);
        let Some((resolved_session_id, anchor_cwd, target_is_current)) = self
            .workspace
            .active
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .and_then(|session| {
                let window = session
                    .windows
                    .iter()
                    .find(|window| window.id == window_id)?;
                let session_is_current = selected_session
                    .as_deref()
                    .is_some_and(|selected| selected == session.id || selected == session.name);
                let window_is_current = selected_window.as_deref().map_or_else(
                    || session.active_window_id.as_deref() == Some(window_id),
                    |selected| selected == window_id,
                );
                Some((
                    session.id.clone(),
                    window
                        .anchor
                        .cwd
                        .clone()
                        .or_else(|| session.anchor.cwd.clone()),
                    session_is_current && window_is_current,
                ))
            })
        else {
            return false;
        };
        let live_terminal_cwd = target_is_current
            .then(|| {
                self.workspace
                    .active
                    .binding
                    .terminal
                    .current_working_directory()
                    .ok()
                    .flatten()
            })
            .flatten();
        self.new_tab_from_ui(
            resolved_session_id,
            terminal_cwd_for_mux_command(live_terminal_cwd, anchor_cwd),
        )
    }

    fn new_tab_from_ui(&mut self, session_id: String, cwd: Option<String>) -> bool {
        let mux_config = self.active_multiplexer().clone();
        self.workspace.active.binding.mux.execute_command(
            &self.repaint,
            &mux_config,
            MuxCommand::NewWindow { session_id, cwd },
        );
        self.sync_native_layout_terminal_now();
        true
    }

    pub fn reorder_window_before_from_ui(&mut self, source: &str, before: Option<&str>) -> bool {
        let Some(session_id) = self
            .workspace
            .active
            .binding
            .mux
            .selected_session()
            .map(str::to_owned)
        else {
            return false;
        };
        if before == Some(source) {
            return false;
        }
        let windows = self.workspace.active.binding.mux.selected_session_windows();
        let Some(from) = windows.iter().position(|window| window.id == source) else {
            return false;
        };
        let mut target_ids = windows
            .iter()
            .map(|window| window.id.as_str())
            .filter(|id| *id != source)
            .collect::<Vec<_>>();
        let to = before
            .and_then(|before| target_ids.iter().position(|id| *id == before))
            .unwrap_or(target_ids.len());
        target_ids.insert(to, source);
        let Some(to) = target_ids.iter().position(|id| *id == source) else {
            return false;
        };
        let delta = to as i32 - from as i32;
        if delta == 0 {
            return false;
        }

        let mux_config = self.active_multiplexer().clone();
        self.workspace.active.binding.mux.execute_command(
            &self.repaint,
            &mux_config,
            MuxCommand::MoveWindow {
                session_id,
                window_id: Some(source.to_owned()),
                delta,
            },
        );
        self.sync_native_layout_terminal_now();
        true
    }

    pub fn move_window_from_ui(&mut self, session_id: &str, window_id: &str, delta: i32) -> bool {
        let selected_session = self
            .workspace
            .active
            .binding
            .mux
            .selected_session()
            .map(str::to_owned);
        let selected_window = self
            .workspace
            .active
            .binding
            .mux
            .selected_window()
            .map(str::to_owned);
        let Some((session_id, position, window_count, active_window_id)) = self
            .workspace
            .active
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .and_then(|session| {
                let mut windows = session.windows.iter().collect::<Vec<_>>();
                windows.sort_by_key(|window| window.index);
                let active_window_id = (selected_session
                    .as_deref()
                    .is_some_and(|selected| selected == session.id || selected == session.name))
                .then_some(selected_window.as_deref())
                .flatten()
                .filter(|selected| windows.iter().any(|window| window.id == *selected))
                .map(str::to_owned)
                .or_else(|| session.active_window_id.clone());
                windows
                    .iter()
                    .position(|window| window.id == window_id)
                    .map(|position| {
                        (
                            session.id.clone(),
                            position,
                            windows.len(),
                            active_window_id,
                        )
                    })
            })
        else {
            return false;
        };
        let target = (position as i32 + delta).clamp(0, window_count as i32 - 1) as usize;
        if target == position {
            return false;
        }

        let mux_config = self.active_multiplexer().clone();
        let command = match active_window_id {
            Some(selected_window_id) if selected_window_id.as_str() != window_id => {
                MuxCommand::MoveWindowPreservingSelection {
                    session_id,
                    window_id: window_id.to_owned(),
                    delta,
                    selected_window_id,
                }
            }
            _ => MuxCommand::MoveWindow {
                session_id,
                window_id: Some(window_id.to_owned()),
                delta,
            },
        };
        self.workspace
            .active
            .binding
            .mux
            .execute_command(&self.repaint, &mux_config, command);
        self.sync_native_layout_terminal_now();
        true
    }

    pub fn close_pane_for_window_from_ui(&mut self, session_id: &str, window_id: &str) -> bool {
        let Some((session_id, window_id, pane_id)) = self
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
                    .and_then(|window| {
                        window
                            .anchor
                            .pane_id
                            .clone()
                            .map(|pane_id| (session.id.clone(), window.id.clone(), pane_id))
                    })
            })
        else {
            return false;
        };
        let selected_session = self
            .workspace
            .active
            .binding
            .mux
            .selected_session()
            .map(str::to_owned);
        let current_window = self.current_window_key();
        let target_is_current = current_window.window_id == window_id
            && self
                .workspace
                .active
                .binding
                .mux
                .sessions()
                .iter()
                .find(|session| session.id == session_id)
                .is_some_and(|session| {
                    selected_session
                        .as_deref()
                        .is_some_and(|selected| selected == session.id || selected == session.name)
                });
        let mux_config = self.active_multiplexer().clone();
        self.workspace.active.binding.mux.close_pane(
            &session_id,
            Some(&pane_id),
            &self.repaint,
            &mux_config,
        );
        self.workspace
            .active
            .binding
            .terminal
            .discard_pane(&pane_id);
        if self.uses_native_terminal_layout() {
            let key = self
                .workspace
                .active
                .binding
                .window_id(session_id.clone(), window_id.clone());
            if let Some(layout) = self.workspace.active.binding.pane_layouts.get_mut(&key) {
                layout.remove(&pane_id);
            }
            if target_is_current {
                let _ = self.sync_terminal_panes();
            }
        }
        true
    }

    fn sync_session_order(&mut self) {
        if let Err(error) = self.workspace.reconcile_binding_states() {
            self.last_error = Some(error.to_string());
        }
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

    fn begin_active_binding_membership_mutation(
        &mut self,
        command: &MuxCommand,
        naming: Option<&PendingGeneratedName>,
    ) -> bool {
        match self
            .workspace
            .begin_active_binding_membership_mutation(command, naming)
        {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }
    /// Fingerprint the backend facts used by the generated-name reconciler. Reconciling forks up to
    /// four `git` subprocesses per session, so an unchanged fingerprint keeps that work off the
    /// steady-state frame path.
    ///
    /// Fingerprints the whole backend list, which changes only when the backend really did.
    /// `mux.sessions()` cannot be used: it is narrowed to this binding's membership, and it is
    /// unstable *within* a frame, because `apply_snapshot` resets it to the full backend list on
    /// every refresh and `sync_session_order` narrows it again later in the same frame. Hashing it
    /// reconciled several times a second forever, which is a `git` fork per session per refresh.
    ///
    /// Membership is left out on purpose. Including it would let a newly attached session take its
    /// generated name immediately, rather than waiting for the next backend change, but the extra
    /// reconciles it causes reach the cwd-keyed `SessionNameStore` collision between bindings often
    /// enough to fail Space membership tests. Include it once that store is keyed by session id.
    fn generated_names_signature(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for session in self.workspace.active.binding.mux.all_sessions() {
            hasher.write(session.id.as_bytes());
            hasher.write_u8(0);
            hasher.write(session.name.as_bytes());
            hasher.write_u8(0);
            if let Some(cwd) = session.anchor.cwd.as_deref() {
                hasher.write(cwd.as_bytes());
            }
            hasher.write_u8(1);
        }
        hasher.finish()
    }

    fn sync_generated_session_names(&mut self) {
        let remote = self.active_multiplexer().remote.is_some();
        let mut candidate = self.workspace.active_reconciled_binding_state_candidate();
        let mut pending_generated_names = self
            .workspace
            .active
            .binding
            .pending_generated_names
            .clone();
        if selected_backend(self.active_multiplexer()) == MultiplexerBackendConfig::Rmux {
            self.commit_binding_state_candidate(candidate);
            return;
        }
        let signature = self.generated_names_signature();
        if self.workspace.active.binding.generated_names_signature == Some(signature) {
            self.commit_binding_state_candidate(candidate);
            return;
        }
        // Reconcile only this binding's sessions. Generating names for the whole backend list
        // renames sessions that belong to other Spaces.
        let sessions = self.workspace.active.binding.mux.sessions().to_vec();
        let mut renames = Vec::new();
        pending_generated_names.retain(|session_id, pending| {
            // A pending name the backend already reports has served its purpose: it exists to
            // keep the name alive for membership and uniqueness until the rename or create lands.
            // Renames record it under the new name rather than a session id, so the id lookup
            // below never prunes those and they would otherwise be held forever.
            if sessions.iter().any(|session| session.name == pending.name) {
                return false;
            }
            sessions
                .iter()
                .find(|session| session.id == *session_id)
                .is_none_or(|session| {
                    session
                        .anchor
                        .cwd
                        .as_deref()
                        .is_some_and(|cwd| Self::session_cwd(cwd, remote) == pending.cwd)
                })
        });
        let mut planned_names = pending_generated_names
            .values()
            .map(|pending| pending.name.clone())
            .collect::<HashSet<_>>();
        let rename_supported =
            selected_backend(self.active_multiplexer()) != MultiplexerBackendConfig::Rmux;
        // A generated name has to clear every session on the server, not just this binding's members:
        // asking for one another Space or a hand-made session already holds is a rename the backend
        // rejects, leaving bootty asking for it again on every change.
        let taken_names = self.taken_session_names(None);

        for session in &sessions {
            let Some(raw_cwd) = session.anchor.cwd.as_deref() else {
                continue;
            };
            let cwd = Self::session_cwd(raw_cwd, remote);
            let mut record = if let Some(record) =
                candidate
                    .session_names
                    .observe_session(&session.id, &session.name, &cwd)
            {
                record
            } else {
                let legacy_name = if remote {
                    crate::strings::session_name_for_remote_path(&cwd)
                } else {
                    crate::strings::session_name_for_path(&cwd)
                };
                if session.name == legacy_name {
                    candidate.session_names.remember_generated(
                        &session.id,
                        &cwd,
                        &session.name,
                        &session.name,
                    );
                } else {
                    candidate.session_names.mark_explicit(
                        &session.id,
                        &session.name,
                        &session.name,
                        &cwd,
                    );
                }
                candidate
                    .session_names
                    .observe_session(&session.id, &session.name, &cwd)
                    .expect("session name metadata should be observable after recording")
            };

            // Records written before display names existed have none, and only those need one worked
            // out: from here on, creating and renaming both record what bootty means to show, so a
            // name someone typed is never something to second-guess.
            if record.display_name.is_empty() {
                let generated_suffix = session.name != record.generated_name
                    && crate::strings::is_uniquified_session_name(
                        &session.name,
                        &record.generated_name,
                    );
                if record.explicit && generated_suffix {
                    // Bootty generated `generated_name`, then asked the backend for that name plus a
                    // uniqueness suffix — which the old reconciler read back as somebody's rename.
                    candidate
                        .session_names
                        .reclaim_generated(&session.id, &session.name);
                    record.generated_name = session.name.clone();
                    record.explicit = false;
                }
                let display_name = if record.explicit {
                    session.name.clone()
                } else {
                    // The name bootty means for this worktree, whenever the backend name is that name
                    // or that name plus the suffix it needed to clear the server.
                    let suggested = Self::suggested_session_name(&cwd, remote);
                    if crate::strings::is_uniquified_session_name(&session.name, &suggested) {
                        suggested
                    } else {
                        session.name.clone()
                    }
                };
                candidate
                    .session_names
                    .set_display_name(&session.id, &display_name);
                record.display_name = display_name;
            }

            if let Some(pending) = pending_generated_names.get(&session.id).cloned() {
                if pending.cwd == cwd {
                    if session.name == pending.name {
                        planned_names.remove(&pending.name);
                        if pending.explicit {
                            candidate.session_names.mark_explicit(
                                &session.id,
                                &pending.name,
                                &pending.display_name,
                                &cwd,
                            );
                        } else {
                            candidate.session_names.remember_generated(
                                &session.id,
                                &cwd,
                                &pending.name,
                                &pending.display_name,
                            );
                        }
                        pending_generated_names.remove(&session.id);
                    } else if session.name != record.generated_name {
                        planned_names.remove(&pending.name);
                        pending_generated_names.remove(&session.id);
                        candidate.session_names.mark_explicit(
                            &session.id,
                            &session.name,
                            &session.name,
                            &cwd,
                        );
                    }
                    continue;
                }
                pending_generated_names.remove(&session.id);
            }
            if record.explicit {
                continue;
            }
            if session.name != record.generated_name {
                candidate.session_names.mark_explicit(
                    &session.id,
                    &session.name,
                    &session.name,
                    &cwd,
                );
                continue;
            }

            let existing_names = taken_names
                .iter()
                .map(String::as_str)
                .filter(|name| *name != session.name)
                .chain(planned_names.iter().map(String::as_str));
            let display_name = Self::suggested_session_name(&cwd, remote);
            let desired = crate::strings::unique_session_name(&display_name, existing_names);
            if desired == session.name || !rename_supported {
                continue;
            }
            planned_names.insert(desired.clone());
            pending_generated_names.insert(
                session.id.clone(),
                PendingGeneratedName {
                    cwd,
                    name: desired.clone(),
                    display_name,
                    explicit: false,
                },
            );
            renames.push((session.id.clone(), desired));
        }

        if !self.commit_binding_state_candidate(candidate) {
            return;
        }
        self.workspace.active.binding.pending_generated_names = pending_generated_names;
        self.workspace.active.binding.generated_names_signature = Some(signature);
        if renames.is_empty() {
            return;
        }
        let mux_config = self.active_multiplexer().clone();
        for (session_id, name) in renames {
            self.workspace.active.binding.mux.rename_session(
                &session_id,
                name,
                &self.repaint,
                &mux_config,
            );
        }
    }

    /// Every session name the backend already answers to, plus the names bootty has asked it for and
    /// is still waiting on. `keep` is the name of the session being renamed, which must not count as
    /// taken against itself.
    pub(super) fn taken_session_names(&self, keep: Option<&str>) -> Vec<String> {
        std::iter::once(&self.workspace.active.binding)
            .chain(self.workspace.active.inactive_bindings.iter())
            .chain(
                self.workspace
                    .inactive_spaces
                    .iter()
                    .flat_map(SpaceRuntime::bindings),
            )
            .flat_map(|binding| {
                binding.mux.backend_session_names().iter().cloned().chain(
                    binding
                        .pending_generated_names
                        .values()
                        .map(|pending| pending.name.clone()),
                )
            })
            .filter(|name| Some(name.as_str()) != keep)
            .collect()
    }

    fn create_project_session_for_cwd(&mut self, cwd: String) {
        let remote = self.active_multiplexer().remote.is_some();
        let cwd = Self::session_cwd(&cwd, remote);

        let existing_names = self.taken_session_names(None);
        // The backend name has to clear every session on the server, including sessions bootty does
        // not own; the display name is the one bootty meant, before that uniqueness pass.
        let display_name = Self::suggested_session_name(&cwd, remote);
        let session_id = crate::strings::unique_session_name(
            &display_name,
            existing_names.iter().map(String::as_str),
        );
        let command = MuxCommand::CreateProjectSession {
            session_id: session_id.clone(),
            cwd: cwd.clone(),
        };
        let pending_name = PendingGeneratedName {
            cwd: cwd.clone(),
            name: session_id.clone(),
            display_name: display_name.clone(),
            explicit: false,
        };
        if !self.begin_active_binding_membership_mutation(&command, Some(&pending_name)) {
            return;
        }
        self.workspace
            .active
            .binding
            .pending_generated_names
            .insert(session_id.clone(), pending_name);
        let mux_config = self.active_multiplexer().clone();
        self.workspace.active.binding.mux.create_project_session(
            crate::ui::new_session_picker::NewMuxSessionRequest { session_id, cwd },
            &self.repaint,
            &mux_config,
        );
        if selected_backend(&mux_config) == MultiplexerBackendConfig::Native {
            self.workspace
                .active
                .binding
                .membership_reconciliation_ready = true;
        }
        self.input_focus = InputFocus::Terminal;
    }

    pub(super) fn session_cwd(cwd: &str, remote: bool) -> String {
        if remote {
            cwd.to_owned()
        } else {
            Self::session_root(cwd)
        }
    }

    pub(super) fn suggested_session_name(cwd: &str, remote: bool) -> String {
        if remote {
            crate::strings::session_name_for_remote_path(cwd)
        } else {
            crate::git::suggested_session_name(cwd)
        }
    }

    fn session_root(cwd: &str) -> String {
        let cwd = crate::git::worktree_root(cwd).unwrap_or_else(|| cwd.to_owned());
        std::fs::canonicalize(&cwd)
            .unwrap_or_else(|_| PathBuf::from(cwd))
            .to_string_lossy()
            .into_owned()
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

    pub fn take_dialog(&mut self) -> Option<NewMuxSessionDialog> {
        self.dialogs.take_new_session()
    }
    pub fn take_space_editor_dialog(&mut self) -> Option<SpaceEditorDialog> {
        self.dialogs.take_space_editor()
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

    pub fn take_session_picker_dialog(&mut self) -> Option<SessionPickerDialog> {
        self.dialogs.take_session_picker()
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

    pub fn take_rename_session_dialog(&mut self) -> Option<RenameSessionDialog> {
        self.dialogs.take_rename_session()
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
                let session = self
                    .workspace
                    .active
                    .binding
                    .mux
                    .sessions()
                    .iter()
                    .find(|session| session.id == session_id || session.name == session_id)
                    .cloned();
                if let Some(session) = session {
                    let cwd = session
                        .anchor
                        .cwd
                        .as_deref()
                        .map(Self::session_root)
                        .unwrap_or_default();
                    // The typed name is what bootty shows. The backend still needs a name no other
                    // session on the server holds, so it may carry a suffix the sidebar never shows.
                    let taken = self.taken_session_names(Some(session.name.as_str()));
                    let backend_name = crate::strings::unique_session_name(
                        &name,
                        taken.iter().map(String::as_str),
                    );
                    let command = MuxCommand::RenameSession {
                        session_id: session.id.clone(),
                        name: backend_name.clone(),
                    };
                    let pending_name = PendingGeneratedName {
                        cwd: cwd.clone(),
                        name: backend_name.clone(),
                        display_name: name.clone(),
                        explicit: true,
                    };
                    if !self.begin_active_binding_membership_mutation(&command, Some(&pending_name))
                    {
                        self.dialogs.open(ModalDialog::RenameSession(dialog));
                        return;
                    }
                    self.workspace
                        .active
                        .binding
                        .pending_generated_names
                        .insert(session.id.clone(), pending_name);
                    let mux_config = self.active_multiplexer().clone();
                    self.workspace.active.binding.mux.rename_session(
                        &session.id,
                        backend_name,
                        &self.repaint,
                        &mux_config,
                    );
                    if selected_backend(&mux_config) == MultiplexerBackendConfig::Native {
                        self.workspace
                            .active
                            .binding
                            .membership_reconciliation_ready = true;
                    }
                }
                self.input_focus = InputFocus::Terminal;
            }
        }
    }

    pub fn take_rename_tab_dialog(&mut self) -> Option<RenameTabDialog> {
        self.dialogs.take_rename_tab()
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
                let key = self
                    .workspace
                    .active
                    .binding
                    .window_id(session_id.clone(), window_id.clone());
                if name.is_empty() {
                    self.workspace.active.binding.custom_tab_names.remove(&key);
                    if let Some(title) = self
                        .workspace
                        .active
                        .binding
                        .terminal_tab_titles
                        .get(&key)
                        .cloned()
                    {
                        self.rename_window_for_terminal_title(&session_id, &window_id, &title);
                    }
                } else {
                    self.workspace.active.binding.custom_tab_names.insert(key);
                    self.rename_window(&session_id, &window_id, name);
                }
                self.input_focus = InputFocus::Terminal;
            }
        }
    }

    pub fn take_terminal_find_dialog(&mut self) -> Option<TerminalFindDialog> {
        self.terminal_find_dialog.take()
    }

    pub fn apply_terminal_find_event(
        &mut self,
        mut dialog: TerminalFindDialog,
        event: TerminalFindEvent,
    ) {
        match event {
            TerminalFindEvent::None => {
                self.terminal_find_dialog = Some(dialog);
            }
            TerminalFindEvent::Close => {
                self.input_focus = InputFocus::Terminal;
                self.clear_terminal_search();
                self.terminal_find_return_focus_after_search = false;
            }
            TerminalFindEvent::FocusFind => {
                self.input_focus = InputFocus::Picker;
                self.terminal_find_dialog = Some(dialog);
            }
            TerminalFindEvent::FocusTerminal => {
                self.input_focus = InputFocus::Terminal;
                self.terminal_find_dialog = Some(dialog);
            }
            TerminalFindEvent::Search { query, direction } => {
                let result = self.search_terminal(&query, direction);
                dialog.set_result(result);
                if direction != TerminalSearchDirection::Current
                    && self.terminal_find_return_focus_after_search
                {
                    self.input_focus = InputFocus::Terminal;
                }
                self.terminal_find_dialog = Some(dialog);
            }
        }
    }

    pub fn take_ditch_session_dialog(&mut self) -> Option<DitchSessionDialog> {
        self.dialogs.take_ditch_session()
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
                if !self.begin_active_binding_membership_mutation(&command, None) {
                    self.dialogs.open(ModalDialog::DitchSession(dialog));
                    return;
                }
                self.workspace.active.binding.mux.ditch_session(
                    &session_id,
                    &self.repaint,
                    &mux_config,
                );
                if selected_backend(&mux_config) == MultiplexerBackendConfig::Native {
                    self.workspace
                        .active
                        .binding
                        .membership_reconciliation_ready = true;
                }
                self.input_focus = InputFocus::Terminal;
            }
        }
    }

    pub fn take_keybind_help_dialog(&mut self) -> Option<KeybindHelpDialog> {
        self.dialogs.take_keybind_help()
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

    pub fn take_command_palette_dialog(&mut self) -> Option<CommandPaletteDialog> {
        self.dialogs.take_command_palette()
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

    pub fn take_theme_picker_dialog(&mut self) -> Option<ThemePickerDialog> {
        self.dialogs.take_theme_picker()
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
        effects: &mut Vec<AppEffect>,
        terminal_cell_width: f32,
        terminal_cell_height: f32,
        terminal_scale_factor: f32,
    ) {
        let side_effects = self
            .workspace
            .active
            .binding
            .terminal_side_effect_rx
            .try_iter()
            .collect::<Vec<_>>();
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
                self.apply_terminal_progress(source_pane_id.as_deref(), state, value);
                effects.push(AppEffect::RequestRepaint);
            }
            TerminalSideEffect::Iterm2UserVarPorts(ports) => {
                self.apply_terminal_ports(source_pane_id.as_deref(), ports);
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

    fn apply_terminal_progress(
        &mut self,
        source_pane_id: Option<&str>,
        state: String,
        value: Option<u8>,
    ) {
        if state == "unknown" {
            return;
        }
        // A tmux client reports progress for every window through its own bookkeeping, and
        // forwards OSC 9;4 only for the pane it currently shows. Recording the forwarded copy
        // would credit it to whichever pane the attach started on, painting a bar on the wrong
        // window and never clearing it.
        if selected_backend(&self.config().multiplexer) == MultiplexerBackendConfig::Tmux {
            return;
        }
        let progress = TerminalProgress::from_conemu(&state, value);
        match source_pane_id {
            Some(pane_id) => {
                let key = self.pane_cache_key(pane_id);
                match progress {
                    Some(progress) => {
                        self.workspace
                            .active
                            .binding
                            .terminal_progress
                            .insert(key, progress);
                    }
                    None => {
                        self.workspace.active.binding.terminal_progress.remove(&key);
                    }
                }
            }
            None => self.workspace.active.binding.unscoped_terminal_progress = progress,
        }
    }

    fn apply_terminal_ports(&mut self, source_pane_id: Option<&str>, ports: Vec<u16>) {
        match source_pane_id {
            Some(pane_id) => {
                let key = self.pane_cache_key(pane_id);
                self.workspace
                    .active
                    .binding
                    .terminal_ports
                    .insert(key, ports);
            }
            None => self.workspace.active.binding.unscoped_terminal_ports = ports,
        }
    }

    fn apply_terminal_window_title(
        &mut self,
        source_pane_id: Option<&str>,
        title: String,
        effects: &mut Vec<AppEffect>,
    ) {
        let window_key = source_pane_id
            .and_then(|pane_id| self.window_key_for_pane(pane_id))
            .or_else(|| source_pane_id.is_none().then(|| self.current_window_key()))
            .filter(|key| !key.window_id.is_empty());
        if let Some(key) = window_key {
            self.workspace
                .active
                .binding
                .terminal_tab_titles
                .insert(key.clone(), title.clone());
            if !self
                .workspace
                .active
                .binding
                .custom_tab_names
                .contains(&key)
            {
                self.rename_window_for_terminal_title(&key.session_id, &key.window_id, &title);
            }
        }
        if source_pane_id.is_none()
            || self.workspace.active.binding.terminal.focused_pane_id() == source_pane_id
        {
            effects.push(AppEffect::SetWindowTitle(title));
        }
    }

    fn window_key_for_pane(&self, pane_id: &str) -> Option<ScopedWindowId> {
        self.workspace
            .active
            .binding
            .mux
            .sessions()
            .iter()
            .find_map(|session| {
                session.windows.iter().find_map(|window| {
                    let anchor_matches = window.anchor.pane_id.as_deref() == Some(pane_id);
                    let pane_matches = window
                        .panes
                        .iter()
                        .any(|pane| pane.pane_id.as_deref() == Some(pane_id));
                    (anchor_matches || pane_matches).then(|| {
                        self.workspace
                            .active
                            .binding
                            .window_id(session.id.clone(), window.id.clone())
                    })
                })
            })
    }

    fn rename_window_for_terminal_title(&mut self, session_id: &str, window_id: &str, title: &str) {
        if self.window_name_for_key(session_id, window_id) == Some(title) {
            return;
        }
        self.rename_window(session_id, window_id, title);
    }

    fn rename_window(&mut self, session_id: &str, window_id: &str, name: &str) {
        let mux_config = self.active_multiplexer().clone();
        self.workspace.active.binding.mux.rename_window(
            session_id,
            window_id,
            name.to_owned(),
            &self.repaint,
            &mux_config,
        );
    }

    fn window_name_for_key(&self, session_id: &str, window_id: &str) -> Option<&str> {
        self.workspace
            .active
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)?
            .windows
            .iter()
            .find(|window| window.id == window_id)
            .map(|window| window.name.as_str())
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

    #[cfg(debug_assertions)]
    fn drive_diagnostic_actions(&mut self, now: Instant, effects: &mut Vec<AppEffect>) -> usize {
        let actions = self
            .diagnostic_action_driver
            .as_mut()
            .map(|driver| driver.due_actions(now))
            .unwrap_or_default();
        let action_count = actions.len();
        for action in actions {
            self.record_diagnostic_action("start", action, 0);
            let start = Instant::now();
            self.apply_mux_key_action(action.mux_action());
            self.record_diagnostic_action("done", action, start.elapsed().as_micros());
            effects.push(AppEffect::RequestRepaint);
        }
        action_count
    }

    #[cfg(not(debug_assertions))]
    fn drive_diagnostic_actions(&mut self, _now: Instant, _effects: &mut Vec<AppEffect>) -> usize {
        0
    }

    #[cfg(debug_assertions)]
    fn record_diagnostic_action(
        &mut self,
        phase: &str,
        action: DiagnosticAction,
        action_elapsed_us: u128,
    ) {
        let selected_session = self
            .workspace
            .active
            .binding
            .mux
            .selected_session()
            .map(str::to_owned);
        let selected_window = self
            .workspace
            .active
            .binding
            .mux
            .selected_window()
            .map(str::to_owned);
        let pane_count = self
            .workspace
            .active
            .binding
            .mux
            .selected_window_panes()
            .len();
        let last_error = self.last_error.clone();
        if let Some(driver) = &mut self.diagnostic_action_driver {
            driver.record(DiagnosticRecord {
                phase,
                action,
                action_elapsed_us,
                selected_session: selected_session.as_deref(),
                selected_window: selected_window.as_deref(),
                pane_count,
                last_error: last_error.as_deref(),
            });
        }
    }

    pub fn update_frame(&mut self, inputs: FrameInputs) -> Vec<AppEffect> {
        let frame_started = crate::diagnostics::latency_start();
        let FrameInputs {
            now,
            stable_dt_ms,
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
        // Drain the focused pane plus every live sibling in the active native window so background
        // panes keep processing output. For non-native this is just the single attach surface.
        self.last_drain = self.workspace.active.binding.terminal.drain_native_window();
        for binding in &mut self.workspace.active.inactive_bindings {
            binding.terminal.drain_native_window();
            binding.discard_terminal_side_effects();
        }
        for space in &mut self.workspace.inactive_spaces {
            for binding in space.bindings_mut() {
                binding.terminal.drain_native_window();
                binding.discard_terminal_side_effects();
            }
        }
        if let Some(owner) = &mut self.workspace.parked_native_terminal {
            owner.drain_inactive();
        }
        self.drain_terminal_side_effects(
            &mut effects,
            terminal_cell_width,
            terminal_cell_height,
            terminal_scale_factor,
        );
        if self.has_degraded_remote() && self.network_change_detector.changed(now) {
            self.reset_remote_reconnects(now);
        }
        // A shell exiting closes its pane, collapsing the split (or cascading to the tab when it was
        // the last pane). On native, any pane's shell can exit, not just the focused one.
        if self.is_native() {
            for pane in self.workspace.active.binding.terminal.native_exited_panes() {
                self.close_pane(&pane);
            }
        } else {
            match self.workspace.active.binding.terminal.child_exited() {
                Ok(true) => self.handle_attach_client_exit(now),
                Ok(false) => self.note_attach_client_alive(now),
                Err(error) => self.last_error = Some(error.to_string()),
            }
            self.start_due_reattach(now, &mut effects);
        }

        for binding in self.workspace.bindings_mut() {
            if let Some(result) = binding.mux.poll_command() {
                if result.is_err() {
                    binding.pending_generated_names.clear();
                    binding.membership_reconciliation_waiting_for_refresh = true;
                    binding.mux.refresh_on_next_frame();
                } else {
                    binding.membership_reconciliation_ready = true;
                }
            }
        }
        let active_config = self.workspace.active.binding.multiplexer.clone();
        self.workspace
            .active
            .binding
            .mux
            .set_refresh_interval(mux_session_refresh_interval(window_focused));
        let _ = self
            .workspace
            .active
            .binding
            .mux
            .refresh_sessions(&self.repaint, &active_config);
        self.workspace
            .active
            .binding
            .restore_persisted_sessions(&self.repaint);
        let refresh_completed = self.workspace.active.binding.mux.take_refresh_completed();
        if refresh_completed
            && self
                .workspace
                .active
                .binding
                .membership_reconciliation_waiting_for_refresh
        {
            self.workspace
                .active
                .binding
                .membership_reconciliation_ready = true;
        }
        self.resolve_remote_attach_exit_after_refresh(refresh_completed);
        let mux_refresh_after = mux_refresh_repaint_after(&active_config, window_focused);
        for binding in &mut self.workspace.active.inactive_bindings {
            binding.restore_persisted_sessions(&self.repaint);
        }
        for space in &mut self.workspace.inactive_spaces {
            for binding in space.bindings_mut() {
                binding.restore_persisted_sessions(&self.repaint);
            }
        }
        if let Some(after) = mux_refresh_after {
            effects.push(AppEffect::RepaintAfter(after));
        }
        if let Err(error) = self.workspace.reconcile_binding_membership_mutations() {
            self.last_error = Some(error.to_string());
        }
        if !self.deferred_profile_binding_rebuilds.is_empty() {
            let requested_scopes = self.deferred_profile_binding_rebuilds.clone();
            let config = self.config().clone();
            if let Err(error) = self.rebuild_profile_bindings(&config, Some(&requested_scopes)) {
                self.last_error = Some(error.to_string());
            }
        }
        self.sync_generated_session_names();
        self.sync_session_order();
        let phase = crate::diagnostics::latency_start();
        let waiting_to_reattach = self
            .workspace
            .active
            .binding
            .reattach
            .is_some_and(|reattach| !reattach.started);
        if !waiting_to_reattach && let Err(error) = self.sync_terminal_panes() {
            if self.workspace.active.binding.multiplexer.remote.is_some() {
                self.handle_attach_start_failure(now, &error.to_string());
            } else {
                self.last_error = Some(error.to_string());
            }
        }
        crate::diagnostics::trace_slow("frame.sync_terminal_panes", phase, 4.0);
        self.hot_reload_config_if_changed(&mut effects, now);
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
            + self.handle_dropped_file_paths(dropped_file_paths)
            + self.drive_diagnostic_actions(now, &mut effects);
        self.last_frame_dt_ms = stable_dt_ms;

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
        if now.duration_since(self.last_status_metrics_sample) >= STATUS_METRICS_SAMPLE_INTERVAL {
            self.status_metrics = StatusMetrics {
                drain: self.last_drain,
                renderer: renderer_metrics,
                cols,
                rows,
            };
            self.last_status_metrics_sample = now;
        }
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
        self.terminal_find_dialog = None;
        self.terminal_find_return_focus_after_search = false;
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

    fn open_terminal_find_dialog(&mut self) {
        self.open_terminal_find_dialog_with_direction(TerminalSearchDirection::Next);
    }

    fn open_terminal_find_dialog_with_direction(&mut self, direction: TerminalSearchDirection) {
        let query = self.last_terminal_search.clone();
        self.close_overlay_dialogs();
        let mut dialog = TerminalFindDialog::open_with_direction(query.clone(), direction);
        if !query.trim().is_empty() {
            let result = self.search_terminal(&query, TerminalSearchDirection::Current);
            dialog.set_result(result);
        }
        self.terminal_find_dialog = Some(dialog);
        self.terminal_find_return_focus_after_search = false;
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

    fn rebuild_profile_bindings(
        &mut self,
        config: &BoottyConfig,
        requested_scopes: Option<&HashSet<MuxScope>>,
    ) -> Result<(), crate::workspace::WorkspacePersistenceError> {
        let repaint = self.repaint.clone();
        let variant = self.active_appearance_variant;
        let profile_scopes = self
            .workspace
            .binding_scopes()
            .filter(|scope| requested_scopes.is_none_or(|scopes| scopes.contains(scope)))
            .filter(|scope| {
                self.workspace
                    .binding_placement(*scope)
                    .is_some_and(|placement| {
                        matches!(placement.remote, SpaceRemoteOverride::Profile(_))
                    })
            })
            .collect::<Vec<_>>();
        let mut pending_scopes = HashSet::new();
        for scope in &profile_scopes {
            match self
                .workspace
                .binding_has_pending_membership_operation(*scope)
            {
                Ok(true) => {
                    pending_scopes.insert(*scope);
                }
                Ok(false) => {}
                Err(error) => {
                    self.deferred_profile_binding_rebuilds
                        .extend(profile_scopes.iter().copied());
                    return Err(error);
                }
            }
        }
        self.deferred_profile_binding_rebuilds
            .extend(pending_scopes.iter().copied());
        let mut rebuilt_scopes = Vec::new();
        for binding in self.workspace.bindings_mut() {
            if !profile_scopes.contains(&binding.scope) {
                continue;
            }
            if pending_scopes.contains(&binding.scope) {
                continue;
            }
            let placement = binding.placement().clone();
            binding.rebuild(config, placement, variant, repaint.clone());
            rebuilt_scopes.push(binding.scope);
        }
        for scope in rebuilt_scopes {
            self.deferred_profile_binding_rebuilds.remove(&scope);
        }
        Ok(())
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
            self.rebuild_profile_bindings(&change.config, None)
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
            .pending_generated_names
            .clear();
        self.sync_session_order();
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

    fn terminal_mouse_tracking_for_selection(
        &mut self,
        events: &[egui::Event],
        terminal_input_enabled: bool,
        pressed_mouse_button: Option<MouseButton>,
    ) -> bool {
        let primary_drag_active = pressed_mouse_button == Some(MouseButton::Left);
        if !terminal_input_enabled
            || !events.iter().any(|event| match event {
                egui::Event::PointerButton {
                    button: egui::PointerButton::Primary,
                    ..
                } => true,
                egui::Event::PointerMoved(_) => primary_drag_active,
                _ => false,
            })
        {
            return false;
        }

        match TerminalRuntime::is_mouse_tracking(self.workspace.active.binding.terminal.as_mut()) {
            Ok(mouse_tracking) => mouse_tracking,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    fn apply_terminal_selection_actions(
        &mut self,
        actions: Vec<TerminalSelectionAction>,
        effects: &mut Vec<AppEffect>,
    ) -> usize {
        let count = actions.len();
        for action in actions {
            let copy_on_select = self.config().input.copy_on_select
                && matches!(&action, TerminalSelectionAction::End(_));
            let result = match action {
                TerminalSelectionAction::Begin(event) => TerminalRuntime::begin_selection(
                    self.workspace.active.binding.terminal.as_mut(),
                    event,
                ),
                TerminalSelectionAction::Scroll(delta) => TerminalRuntime::scroll_viewport_delta(
                    self.workspace.active.binding.terminal.as_mut(),
                    delta,
                ),
                TerminalSelectionAction::Update(event) => TerminalRuntime::update_selection(
                    self.workspace.active.binding.terminal.as_mut(),
                    event,
                ),
                TerminalSelectionAction::End(event) => TerminalRuntime::end_selection(
                    self.workspace.active.binding.terminal.as_mut(),
                    event,
                ),
            };
            match result {
                Ok(()) => {
                    effects.push(AppEffect::RequestRepaint);
                    if copy_on_select {
                        self.copy_terminal_selection_if_any(CopyToClipboard::Mixed);
                    }
                }
                Err(error) => self.last_error = Some(error.to_string()),
            }
        }
        count
    }

    fn terminal_copy_mode_active(&mut self) -> bool {
        match TerminalRuntime::copy_mode_active(self.workspace.active.binding.terminal.as_mut()) {
            Ok(active) => active,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    fn enter_terminal_copy_mode(&mut self, effects: &mut Vec<AppEffect>) {
        match TerminalRuntime::enter_copy_mode(self.workspace.active.binding.terminal.as_mut()) {
            Ok(()) => effects.push(AppEffect::RequestRepaint),
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn apply_copy_mode_key_action(
        &mut self,
        action: CopyModeKeyAction,
        effects: &mut Vec<AppEffect>,
    ) -> bool {
        match action {
            CopyModeKeyAction::Terminal(action) => {
                self.apply_terminal_copy_mode_action(action, effects)
            }
            CopyModeKeyAction::SearchPrompt(direction) => {
                self.record_terminal_search_direction(direction);
                self.open_terminal_find_dialog_with_direction(direction);
                self.terminal_find_return_focus_after_search = true;
                effects.push(AppEffect::RequestRepaint);
                true
            }
            CopyModeKeyAction::SearchWord(direction) => self.apply_terminal_copy_mode_action(
                TerminalCopyModeAction::SearchWord(direction),
                effects,
            ),
            CopyModeKeyAction::SearchRepeat(repeat) => {
                let direction = repeat.direction(self.last_terminal_search_direction);
                let query = self.last_terminal_search.clone();
                if !query.trim().is_empty() {
                    let result =
                        self.search_terminal_with_direction_recording(&query, direction, false);
                    if let Some(dialog) = self.terminal_find_dialog.as_mut() {
                        dialog.set_result(result);
                    }
                    effects.push(AppEffect::RequestRepaint);
                }
                true
            }
        }
    }

    fn record_terminal_search_direction(&mut self, direction: TerminalSearchDirection) {
        if direction != TerminalSearchDirection::Current {
            self.last_terminal_search_direction = direction;
        }
    }

    fn apply_terminal_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
        effects: &mut Vec<AppEffect>,
    ) -> bool {
        let search_direction = match &action {
            TerminalCopyModeAction::Search { direction, .. }
            | TerminalCopyModeAction::SearchWord(direction) => Some(*direction),
            _ => None,
        };
        match TerminalRuntime::handle_copy_mode_action(
            self.workspace.active.binding.terminal.as_mut(),
            action,
        ) {
            Ok(outcome) => {
                if let Some(bytes) = outcome.copied {
                    let text = String::from_utf8_lossy(&bytes);
                    if let Err(error) = write_clipboard_text(&text) {
                        self.last_error = Some(error.to_string());
                    }
                }
                let search_result = outcome.search.map(|search| {
                    self.last_terminal_search = search.query;
                    if let Some(direction) = search_direction {
                        self.record_terminal_search_direction(direction);
                    }
                    self.terminal_find_result_from_frame(search.found)
                });
                if let Some(result) = search_result
                    && let Some(dialog) = self.terminal_find_dialog.as_mut()
                {
                    dialog.set_result(result);
                }
                effects.push(AppEffect::RequestRepaint);
                outcome.active
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    fn consume_copy_mode_egui_events(
        &mut self,
        events: &mut Vec<egui::Event>,
        effects: &mut Vec<AppEffect>,
        terminal_input_enabled: bool,
    ) -> usize {
        if !terminal_input_enabled
            || (self.terminal_find_dialog.is_some() && self.input_focus != InputFocus::Terminal)
            || !copy_mode_key_input_present(events)
            || !self.terminal_copy_mode_active()
        {
            return 0;
        }

        let mut count = 0;
        let mut retained = Vec::with_capacity(events.len());
        let mut suppress_next_text = false;
        let mut pass_next_text_to_app = false;
        for event in events.drain(..) {
            match &event {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } if copy_mode_egui_key_should_pass_to_app(*key, *modifiers) => {
                    pass_next_text_to_app = copy_mode_egui_key_may_emit_text(*key);
                    retained.push(event);
                }
                egui::Event::Text(_) if std::mem::take(&mut pass_next_text_to_app) => {
                    retained.push(event);
                }
                _ if matches!(event, egui::Event::Key { .. } | egui::Event::Text(_)) => {
                    pass_next_text_to_app = false;
                    count += 1;
                    if let Some(action) =
                        copy_mode_action_for_egui_event(&event, &mut suppress_next_text)
                    {
                        self.apply_copy_mode_key_action(action, effects);
                    }
                }
                _ => {
                    pass_next_text_to_app = false;
                    retained.push(event);
                }
            }
        }
        *events = retained;
        count
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
        let suppress_next_egui_paste = std::mem::take(&mut self.suppress_next_egui_paste);
        let mut events = events;
        if suppress_next_egui_paste {
            remove_first_paste_event(&mut events);
        }
        let terminal_input_enabled = self.direct_terminal_input_enabled();
        let selection_surface = terminal_input_enabled
            .then_some(self.terminal_surface)
            .flatten();
        let mouse_tracking = self.terminal_mouse_tracking_for_selection(
            &events,
            terminal_input_enabled,
            pressed_mouse_button,
        );
        let mut chrome_handle_rects = self.chrome_handle_rects.clone();
        if let Some(rect) = self
            .terminal_find_dialog
            .as_ref()
            .and_then(TerminalFindDialog::last_rect)
        {
            chrome_handle_rects.push(rect);
        }
        let (mut events, mut selection_actions) = self.terminal_selection.route_events(
            events,
            TerminalSelectionRouteContext {
                surface: selection_surface,
                view: self.terminal_view_transform,
                mouse_tracking,
                frame_modifiers: modifiers,
                chrome_handle_rects: &chrome_handle_rects,
            },
        );
        selection_actions.extend(self.terminal_selection.autoscroll_actions(
            selection_surface,
            self.terminal_view_transform,
            modifiers,
        ));
        let selection_count = self.apply_terminal_selection_actions(selection_actions, effects);
        let copy_mode_count =
            self.consume_copy_mode_egui_events(&mut events, effects, terminal_input_enabled);
        let copy_selection_count = self.consume_copy_shortcut_for_terminal_selection(&mut events);
        // `cmd+shift+,` over a palette row jumps to that command's keybinding editor.
        // Consume it here so it doesn't also fire its own global binding.
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
        let routed = if self.terminal_find_dialog.is_some() {
            route_find_modeless_events(
                self.input_focus,
                events,
                self.terminal_find_dialog
                    .as_ref()
                    .and_then(TerminalFindDialog::last_rect),
                hover_pos,
            )
        } else {
            route_events(self.input_focus, events)
        };
        let sidebar_count = self.handle_sidebar_input(routed.ui_events, viewport, effects);
        let events = if terminal_input_enabled || self.terminal_find_dialog.is_some() {
            routed.terminal_events
        } else {
            Vec::new()
        };
        let snapshot = InputSnapshot {
            events,
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
        let count = commands.len()
            + actions.len()
            + sidebar_count
            + selection_count
            + copy_mode_count
            + copy_selection_count;

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

        let mut copy_mode_active = self.terminal_copy_mode_active();
        for input in inputs {
            let mut input = input.input();
            input.mods = self.config_runtime.remap_mods(input.mods);
            if copy_mode_active {
                if let Some(action) = copy_mode_action_for_input(input) {
                    copy_mode_active = self.apply_copy_mode_key_action(action, effects);
                    continue;
                }
                if !copy_mode_input_should_pass_to_app(input) {
                    continue;
                }
            }
            if direct_copy_shortcut_pressed(input)
                && self.copy_terminal_selection_if_any(CopyToClipboard::Mixed)
            {
                continue;
            }
            if let Some(invocation) = self.config_runtime.invocation_for_input(input) {
                if invocation.command == "paste_from_clipboard" {
                    self.suppress_next_egui_paste = true;
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

    fn consume_copy_shortcut_for_terminal_selection(
        &mut self,
        events: &mut Vec<egui::Event>,
    ) -> usize {
        let Some(index) = events.iter().position(copy_shortcut_pressed) else {
            return 0;
        };
        if !self.copy_terminal_selection_if_any(CopyToClipboard::Mixed) {
            return 0;
        }
        events.remove(index);
        1
    }

    fn write_terminal_selection_to_clipboard(&mut self, format: CopyToClipboard) -> Result<bool> {
        let mut selection = |format| {
            self.workspace
                .active
                .binding
                .terminal
                .format_selection(format)
        };
        match format {
            CopyToClipboard::Plain => {
                let Some(bytes) = selection(TerminalSelectionFormat::PlainText)? else {
                    return Ok(false);
                };
                write_clipboard_text(&String::from_utf8_lossy(&bytes))?;
            }
            CopyToClipboard::Vt => {
                let Some(bytes) = selection(TerminalSelectionFormat::Vt)? else {
                    return Ok(false);
                };
                write_clipboard_text(&String::from_utf8_lossy(&bytes))?;
            }
            CopyToClipboard::Html => {
                let Some(bytes) = selection(TerminalSelectionFormat::Html)? else {
                    return Ok(false);
                };
                write_clipboard_html(&String::from_utf8_lossy(&bytes), None)?;
            }
            CopyToClipboard::Mixed => {
                let Some(plain) = selection(TerminalSelectionFormat::PlainText)? else {
                    return Ok(false);
                };
                let Some(html) = selection(TerminalSelectionFormat::Html)? else {
                    return Ok(false);
                };
                write_clipboard_html(
                    &String::from_utf8_lossy(&html),
                    Some(&String::from_utf8_lossy(&plain)),
                )?;
            }
        }
        Ok(true)
    }

    fn copy_terminal_selection_if_any(&mut self, format: CopyToClipboard) -> bool {
        match self.write_terminal_selection_to_clipboard(format) {
            Ok(copied) => copied,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    fn copy_terminal_selection_or_request_copy(
        &mut self,
        format: CopyToClipboard,
        effects: &mut Vec<AppEffect>,
    ) {
        if !self.copy_terminal_selection_if_any(format) {
            effects.push(AppEffect::RequestCopy);
        }
    }

    /// The attach client exited. For a local binding that means the pane it was showing ended, so
    /// the pane closes. For a remote one it means either that or a dropped connection, and the two
    /// look identical from here — so bootty reconnects instead of closing. The sessions live on the
    /// other host and outlive the link; closing on a network blip would kill work the user still
    /// has. A pane that really did end is gone from the next snapshot, which closes it properly.
    fn handle_attach_client_exit(&mut self, now: Instant) {
        let Some(remote) = self.workspace.active.binding.multiplexer.remote.clone() else {
            self.close_active_pane();
            return;
        };
        if self
            .workspace
            .active
            .binding
            .reattach
            .is_some_and(|reattach| !reattach.started)
        {
            return;
        }
        let attached_for = self
            .workspace
            .active
            .binding
            .remote_attach_started
            .map(|started| now.saturating_duration_since(started));
        let reattach = RemoteReattach::after_failure(
            self.workspace.active.binding.reattach,
            attached_for,
            now,
        );
        let error = format!(
            "lost the connection to {}; reconnecting (attempt {})",
            remote.host, reattach.attempts
        );
        self.last_error = Some(error.clone());
        self.workspace
            .active
            .binding
            .mux
            .set_availability_error(Some(error));
        self.workspace.active.binding.reattach = Some(reattach);
    }

    fn handle_attach_start_failure(&mut self, now: Instant, detail: &str) {
        let Some(remote) = self.workspace.active.binding.multiplexer.remote.clone() else {
            return;
        };
        let reattach =
            RemoteReattach::after_failure(self.workspace.active.binding.reattach, None, now);
        let error = format!(
            "could not connect to {}: {detail}; reconnecting (attempt {})",
            remote.host, reattach.attempts
        );
        self.last_error = Some(error.clone());
        self.workspace
            .active
            .binding
            .mux
            .set_availability_error(Some(error));
        self.workspace.active.binding.reattach = Some(reattach);
    }

    fn resolve_remote_attach_exit_after_refresh(&mut self, refresh_completed: bool) {
        if self
            .workspace
            .active
            .binding
            .resolve_empty_remote_after_attach_exit(refresh_completed)
            && self
                .last_error
                .as_deref()
                .is_some_and(|error| error.starts_with("lost the connection to "))
        {
            self.last_error = None;
        }
    }
    /// A remote attach client that has been alive long enough proves the connection is back, so the
    /// next outage starts its backoff from the beginning rather than from where this one left off.
    fn note_attach_client_alive(&mut self, now: Instant) {
        let established = self
            .workspace
            .active
            .binding
            .remote_attach_started
            .is_some_and(|started| {
                now.saturating_duration_since(started) >= RemoteReattach::STABLE_AFTER
            });
        if established
            && self
                .workspace
                .active
                .binding
                .reattach
                .is_some_and(|reattach| reattach.started)
        {
            self.workspace.active.binding.reattach = None;
            self.workspace
                .active
                .binding
                .mux
                .set_availability_error(None);
        }
    }

    /// Drop the dead attach client once its backoff has passed. Clearing the pane's target is what
    /// asks for a new one: this frame's pane sync starts a fresh client for the same session.
    fn start_due_reattach(&mut self, now: Instant, effects: &mut Vec<AppEffect>) {
        let Some(mut reattach) = self.workspace.active.binding.reattach else {
            return;
        };
        if !reattach.due(now) {
            // Nothing else is guaranteed to wake the frame loop while a pane sits disconnected, so
            // the wait itself asks for the frame that ends it.
            if !reattach.started {
                effects.push(AppEffect::RepaintAfter(
                    reattach.retry_at.saturating_duration_since(now),
                ));
            }
            return;
        }
        reattach.started = true;
        self.workspace.active.binding.reattach = Some(reattach);
        self.workspace.active.binding.remote_attach_started = Some(now);
        self.workspace.active.binding.terminal.discard_active_pane();
    }

    pub fn reconnect_space_from_ui(&mut self, space_id: SpaceId) -> bool {
        let now = Instant::now();
        if space_id == self.workspace.active.id {
            let mut restarted =
                Self::restart_remote_binding(&mut self.workspace.active.binding, now);
            for binding in &mut self.workspace.active.inactive_bindings {
                restarted |= Self::restart_remote_binding(binding, now);
            }
            return restarted;
        }
        let Some(space) = self
            .workspace
            .inactive_spaces
            .iter_mut()
            .find(|space| space.id == space_id)
        else {
            return false;
        };
        let mut restarted = false;
        for binding in space.bindings_mut() {
            restarted |= Self::restart_remote_binding(binding, now);
        }
        restarted
    }

    fn has_degraded_remote(&self) -> bool {
        self.workspace.active.binding.reattach.is_some()
            || self
                .workspace
                .active
                .inactive_bindings
                .iter()
                .any(|binding| binding.reattach.is_some())
            || self
                .workspace
                .inactive_spaces
                .iter()
                .flat_map(SpaceRuntime::bindings)
                .any(|binding| binding.reattach.is_some())
    }

    fn reset_remote_reconnects(&mut self, now: Instant) {
        if self.workspace.active.binding.reattach.is_some() {
            Self::restart_remote_binding(&mut self.workspace.active.binding, now);
        }
        for binding in &mut self.workspace.active.inactive_bindings {
            if binding.reattach.is_some() {
                Self::restart_remote_binding(binding, now);
            }
        }
        for space in &mut self.workspace.inactive_spaces {
            for binding in space.bindings_mut() {
                if binding.reattach.is_some() {
                    Self::restart_remote_binding(binding, now);
                }
            }
        }
    }

    fn restart_remote_binding(binding: &mut BindingRuntime, now: Instant) -> bool {
        let Some(remote) = binding.multiplexer.remote.as_ref() else {
            return false;
        };
        binding.reattach = Some(RemoteReattach {
            retry_at: now,
            attempts: 1,
            started: true,
        });
        binding.remote_attach_started = Some(now);
        binding
            .mux
            .set_availability_error(Some(format!("reconnecting to {}", remote.host)));
        binding.terminal.discard_active_pane();
        true
    }
    // Close the focused pane (cmd+w or its shell exiting) and let the mux cascade to the tab. The
    // active terminal is dropped here so its PTY is reaped; sync_mux_anchor then attaches whatever
    // pane the mux selected next (or idle when the session has no tabs left).
    fn close_active_pane(&mut self) {
        self.close_target_pane(None);
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
                pane_id: Some(pane_id.to_owned()),
            },
        );
        self.workspace.active.binding.terminal.discard_pane(pane_id);
        let key = self.current_window_key();
        if let Some(layout) = self.workspace.active.binding.pane_layouts.get_mut(&key) {
            layout.remove(pane_id);
        }
        let _ = self.sync_terminal_panes();
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
            self.focus_pane_neighbor(layout_direction(direction));
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
            if !self.persist_rmux_selection_before_publish(&target, None) {
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
        match action {
            TerminalFindAction::Prompt => {
                self.open_terminal_find_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            TerminalFindAction::Close => {
                self.terminal_find_dialog = None;
                self.clear_terminal_search();
                self.input_focus = InputFocus::Terminal;
                effects.push(AppEffect::RequestRepaint);
            }
            TerminalFindAction::Search(query) => {
                self.search_terminal(&query, TerminalSearchDirection::Current);
                effects.push(AppEffect::RequestRepaint);
            }
            TerminalFindAction::SearchSelection => {
                if let Some(query) = self.selected_terminal_text() {
                    self.search_terminal(&query, TerminalSearchDirection::Current);
                    effects.push(AppEffect::RequestRepaint);
                }
            }
            TerminalFindAction::Previous => {
                let query = self.last_terminal_search.clone();
                if query.is_empty() {
                    self.open_terminal_find_dialog();
                } else {
                    self.search_terminal(&query, TerminalSearchDirection::Previous);
                }
                effects.push(AppEffect::RequestRepaint);
            }
            TerminalFindAction::Next => {
                let query = self.last_terminal_search.clone();
                if query.is_empty() {
                    self.open_terminal_find_dialog();
                } else {
                    self.search_terminal(&query, TerminalSearchDirection::Next);
                }
                effects.push(AppEffect::RequestRepaint);
            }
        }
    }

    fn selected_terminal_text(&mut self) -> Option<String> {
        match self
            .workspace
            .active
            .binding
            .terminal
            .format_selection(TerminalSelectionFormat::PlainText)
        {
            Ok(Some(bytes)) => Some(String::from_utf8_lossy(&bytes).trim().to_owned())
                .filter(|text| !text.is_empty()),
            Ok(None) => None,
            Err(error) => {
                self.last_error = Some(error.to_string());
                None
            }
        }
    }

    fn clear_terminal_search(&mut self) {
        if let Err(error) = self
            .workspace
            .active
            .binding
            .terminal
            .search_viewport("", TerminalSearchDirection::Current)
        {
            self.last_error = Some(error.to_string());
        }
    }

    fn search_terminal(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
    ) -> TerminalFindResult {
        self.search_terminal_with_direction_recording(query, direction, true)
    }

    fn search_terminal_with_direction_recording(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
        record_direction: bool,
    ) -> TerminalFindResult {
        let query = query.trim();
        if query.is_empty() {
            self.clear_terminal_search();
            return TerminalFindResult::default();
        }
        self.last_terminal_search = query.to_owned();
        if record_direction {
            self.record_terminal_search_direction(direction);
        }
        if self.terminal_copy_mode_active() {
            return self.search_copy_mode_terminal(query, direction);
        }
        match self.search_focused_terminal_runtime(query, direction) {
            Ok(result) => result,
            Err(error) => {
                self.last_error = Some(error.to_string());
                TerminalFindResult::default()
            }
        }
    }

    fn search_focused_terminal_runtime(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
    ) -> Result<TerminalFindResult> {
        if let Some(pane_id) = self.focused_pane()
            && let Some(source) = self
                .workspace
                .active
                .binding
                .terminal
                .focused_terminal_runtime(&pane_id)
        {
            let found = source.search_viewport(query, direction)?;
            let frame = source.extract_frame()?;
            return Ok(TerminalFindResult {
                found,
                active_index: frame.active_search_match_index,
                match_count: frame.search_match_count,
            });
        }

        let found = self
            .workspace
            .active
            .binding
            .terminal
            .search_viewport(query, direction)?;
        let frame = self.workspace.active.binding.terminal.extract_frame()?;
        Ok(TerminalFindResult {
            found,
            active_index: frame.active_search_match_index,
            match_count: frame.search_match_count,
        })
    }

    fn search_copy_mode_terminal(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
    ) -> TerminalFindResult {
        match TerminalRuntime::handle_copy_mode_action(
            self.workspace.active.binding.terminal.as_mut(),
            TerminalCopyModeAction::Search {
                query: query.to_owned(),
                direction,
            },
        ) {
            Ok(outcome) => outcome
                .search
                .map_or_else(TerminalFindResult::default, |search| {
                    self.terminal_find_result_from_frame(search.found)
                }),
            Err(error) => {
                self.last_error = Some(error.to_string());
                TerminalFindResult::default()
            }
        }
    }

    fn terminal_find_result_from_frame(&mut self, found: bool) -> TerminalFindResult {
        let (active_index, match_count) = self
            .workspace
            .active
            .binding
            .terminal
            .extract_frame()
            .map(|frame| (frame.active_search_match_index, frame.search_match_count))
            .unwrap_or_else(|error| {
                self.last_error = Some(error.to_string());
                (None, 0)
            });
        TerminalFindResult {
            found,
            active_index,
            match_count,
        }
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
