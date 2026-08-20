use std::{
    collections::{HashMap, HashSet},
    hash::Hasher,
    net::{IpAddr, UdpSocket},
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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

use super::workspace_runtime::{
    BindingRuntime, NativeTerminalOwner, PendingGeneratedName, RemoteReattach, ScopedPaneId,
    ScopedWindowId, SpaceRuntime, SpaceTransition, WorkspaceRuntime,
    binding_runtime_for_multiplexer, terminal_session_config_with_side_effects,
};

use crate::{
    app_actions::{
        AppAction, AppKeyBindings, FontSizeAction, KeybindAction, MuxKeyAction, SidebarAction,
        SidebarKeyBindings, TerminalFindAction, TerminalScrollAction,
        builtin_app_invocation_for_direct_key, split_app_actions_for_bindings_with_modifier_sides,
    },
    commands::{
        AppCommandReceiver, AppCommandSender, BoundAppCommandSender, Caller, CommandCancellation,
        CommandCatalog, CommandInvocation, CommandOutcome, CommandTarget, CoreCommandExecutor,
        MutationClass, ResourceKind, app_command_channel_with_repaint,
    },
    config::{
        AppearanceMode, AppearanceVariant, BoottyConfig, ConfigState, WindowConfig,
        load_config_from_path, load_or_create_config_document,
    },
    config_reload::{CONFIG_HOT_RELOAD_INTERVAL, ConfigHotReload, new_session_only_config_changed},
    diagnostics::{
        STATUS_METRICS_SAMPLE_INTERVAL, StabilityTrace, StabilityTraceSample, StatusMetrics,
    },
    direct_input::{DirectKeyInput, ModifierSideState},
    geometry::{TerminalSurface, ViewTransform},
    input::{
        InputSnapshot, TerminalInputCommand, WheelScrollState,
        focus::InputFocus,
        router::{RoutedInput, route_events},
        terminal_input_commands_with_wheel_state,
    },
    input_binding::CopyToClipboard,
    layout::{Direction, Divider, PaneLayout, SplitDirection},
    modifier_remap::ModifierRemapSet,
    mux::{
        RepaintHandle,
        capability::{BindingOperation, BindingOperationOutcome},
        command::{MuxCommand, MuxSplitDirection},
        config::selected_backend,
        controller::{
            MuxCommandCompletion, MuxCommandError, MuxCommandResult, MuxController, MuxScope,
            SpaceId, mux_session_refresh_interval,
        },
        snapshot::{MuxPaneAnchor, MuxSession, MuxWindow, MuxWindowProgress},
        terminal::{ActiveTerminal, TerminalRuntime, decode_scoped_pane_id},
    },
    platform::{
        apply_macos_non_native_fullscreen_presentation, macos_handles_non_native_fullscreen_frame,
        read_clipboard_text, restore_macos_presentation, show_desktop_notification,
        write_clipboard_html, write_clipboard_text,
    },
    renderer::{RendererMetrics, TerminalRenderSource},
    scheduler::{RepaintScheduler, RepaintSignal},
    session_names::SessionNameStore,
    session_order::SessionOrderStore,
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
    workspace::{SpaceMuxOverride, SpaceRemoteOverride, WorkspaceRepository},
};
use bootty_terminal::terminal_engine::{
    TerminalColorConfig, TerminalCopyModeAction, TerminalCursorConfig, TerminalFeatureConfig,
    TerminalSelectionFormat, TerminalSideEffect, TerminalSideEffectEvent,
    encode_iterm2_report_cell_size, encode_iterm2_report_variable, encode_osc52_response,
};

const PRIMARY_WINDOW_STATE_KEY: &str = "main";
static NEXT_WINDOW_COMMAND_GENERATION: AtomicU64 = AtomicU64::new(1);

fn process_command_handle() -> String {
    static HANDLE: OnceLock<String> = OnceLock::new();
    HANDLE
        .get_or_init(|| {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            format!("{}:{nanos:032x}", std::process::id())
        })
        .clone()
}

fn next_window_command_generation() -> u64 {
    NEXT_WINDOW_COMMAND_GENERATION.fetch_add(1, Ordering::Relaxed)
}
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

fn command_outcome_message(outcome: &CommandOutcome) -> Option<String> {
    match outcome {
        CommandOutcome::Success { .. } => None,
        CommandOutcome::Unsupported { message }
        | CommandOutcome::Unavailable { message }
        | CommandOutcome::Denied { message }
        | CommandOutcome::StaleTarget { message }
        | CommandOutcome::Failed { message, .. } => Some(message.clone()),
        CommandOutcome::ConfirmationRequired { .. } => {
            Some("command requires confirmation".to_owned())
        }
    }
}

fn command_outcome_for_binding_operation(
    outcome: BindingOperationOutcome<()>,
) -> Option<CommandOutcome> {
    match outcome {
        BindingOperationOutcome::Supported(()) => None,
        BindingOperationOutcome::Unsupported => Some(CommandOutcome::Unsupported {
            message: "mux operation is unsupported".to_owned(),
        }),
        BindingOperationOutcome::Unavailable => Some(CommandOutcome::Unavailable {
            message: "mux operation is unavailable".to_owned(),
        }),
        BindingOperationOutcome::Stale => Some(CommandOutcome::StaleTarget {
            message: "mux operation capability is stale".to_owned(),
        }),
    }
}

enum PendingCommandResult {
    Mux {
        command: MuxCommand,
        result: mpsc::Receiver<MuxCommandResult>,
    },
    Outcome(mpsc::Receiver<CommandOutcome>),
}

enum CommandDispatch {
    Complete(CommandOutcome),
    Pending(PendingCommandResult),
}

struct PendingAppCommand {
    deadline: Instant,
    cancellation: CommandCancellation,
    response: mpsc::Sender<CommandOutcome>,
    result: PendingCommandResult,
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
    window_state_key: String,
    command_instance_handle: String,
    command_instance_generation: u64,
    command_window_generation: u64,
    workspace: WorkspaceRuntime,
    repaint_scheduler: RepaintScheduler,
    network_change_detector: NetworkChangeDetector,
    last_error: Option<String>,
    last_drain: DrainStats,
    last_frame_dt_ms: f32,
    status_metrics: StatusMetrics,
    last_status_metrics_sample: Instant,
    terminal_surface: Option<TerminalSurface>,
    /// The full terminal area the panes were last laid out within, for geometric neighbor lookup.
    last_pane_area: Option<Rect>,
    terminal_view_transform: ViewTransform,
    config_state: ConfigState,
    active_appearance_variant: AppearanceVariant,
    input_focus: InputFocus,
    app_key_bindings: AppKeyBindings,
    sidebar_key_bindings: SidebarKeyBindings,
    has_new_session_config_changes: bool,
    repaint: RepaintHandle,
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
    lua_window_open: bool,
    terminal_selection: TerminalSelectionRouter,
    /// Screen rects of chrome resize handles (sidebar edge, pane dividers) registered during the
    /// previous frame's UI build. A primary press inside one of these must not begin a terminal
    /// text selection — the handle owns that drag. Populated each frame in `show_fixed_layout`.
    chrome_handle_rects: Vec<egui::Rect>,
    wheel_scroll_state: WheelScrollState,
    modifier_remaps: ModifierRemapSet,
    terminal_cursor_icon: egui::CursorIcon,
    mouse_pointer_hidden_while_typing: bool,
    last_mouse_hover_pos: Option<Pos2>,
    macos_option_as_alt: crate::terminal::MacosOptionAsAlt,
    stability_trace: Option<StabilityTrace>,
    config_hot_reload: ConfigHotReload,
    new_mux_session_dialog: Option<NewMuxSessionDialog>,
    sidebar_hovered_session: Option<ScopedSessionTarget>,
    session_picker_dialog: Option<SessionPickerDialog>,
    rename_session_dialog: Option<RenameSessionDialog>,
    rename_tab_dialog: Option<RenameTabDialog>,
    ditch_session_dialog: Option<DitchSessionDialog>,
    keybind_help_dialog: Option<KeybindHelpDialog>,
    command_palette_dialog: Option<CommandPaletteDialog>,
    theme_picker_dialog: Option<ThemePickerDialog>,
    space_editor_dialog: Option<SpaceEditorDialog>,
    terminal_find_dialog: Option<TerminalFindDialog>,
    terminal_find_return_focus_after_search: bool,
    last_terminal_search: String,
    last_terminal_search_direction: TerminalSearchDirection,
    theme_picker_restore_config: Option<BoottyConfig>,
    /// A command-palette choice waiting for the next frame's viewport and effect sink.
    pending_command: Option<CommandInvocation>,
    app_command_tx: AppCommandSender,
    app_command_rx: AppCommandReceiver,
    command_catalog: Arc<CommandCatalog>,
    pending_app_commands: Vec<PendingAppCommand>,
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

fn mux_split_direction(direction: SplitDirection) -> MuxSplitDirection {
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

fn new_mux_session_request_with_name(
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

fn terminal_cwd_for_mux_command(
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
        let modifier_remaps = config.input.modifier_remaps()?;
        let macos_option_as_alt = config.input.macos_option_as_alt.into();
        let sidebar_key_bindings =
            SidebarKeyBindings::from_keybinds(&config.input.sidebar_keybind)?;
        let stability_trace = StabilityTrace::from_config(&config);
        let active_appearance_variant = config.appearance.mode.variant(AppearanceVariant::Dark);
        let workspace = WorkspaceRuntime::open(
            &config,
            &window_state_key,
            active_appearance_variant,
            repaint.clone(),
        )?;
        let keybinds = config
            .input
            .keybinds_for_backend(workspace.multiplexer_backend());
        let app_key_bindings = AppKeyBindings::from_keybinds(&keybinds)?;
        let config_hot_reload = ConfigHotReload::new(&config.config_path);
        let macos_non_native_fullscreen_active = config.window.non_native_fullscreen_enabled();
        let macos_non_native_fullscreen_applied =
            apply_macos_non_native_fullscreen_presentation(&config.window);
        let macos_non_native_fullscreen_pending_apply =
            macos_non_native_fullscreen_active && !macos_non_native_fullscreen_applied;
        #[cfg(debug_assertions)]
        let diagnostic_action_driver = DiagnosticActionDriver::from_env();
        let (app_command_tx, app_command_rx) =
            app_command_channel_with_repaint(64, repaint.clone());
        let command_instance_handle = process_command_handle();
        let command_instance_generation = 1;
        let command_window_generation = next_window_command_generation();
        let command_catalog = Arc::new(CommandCatalog::default());

        Ok(Self {
            window_state_key,
            command_instance_handle,
            command_instance_generation,
            command_window_generation,
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
            config_state: ConfigState::new(config),
            active_appearance_variant,
            input_focus: InputFocus::Terminal,
            app_key_bindings,
            sidebar_key_bindings,
            has_new_session_config_changes: false,
            repaint,
            direct_input_rx,
            modifier_side_rx,
            modifier_sides: ModifierSideState::default(),
            pending_direct_input: Vec::new(),
            suppress_next_egui_paste: false,
            settings_open: false,
            lua_window_open: false,
            terminal_selection: TerminalSelectionRouter::default(),
            wheel_scroll_state: WheelScrollState::default(),
            modifier_remaps,
            terminal_cursor_icon: egui::CursorIcon::Text,
            mouse_pointer_hidden_while_typing: false,
            last_mouse_hover_pos: None,
            macos_option_as_alt,
            stability_trace,
            config_hot_reload,
            new_mux_session_dialog: None,
            sidebar_hovered_session: None,
            session_picker_dialog: None,
            rename_session_dialog: None,
            rename_tab_dialog: None,
            command_palette_dialog: None,
            theme_picker_dialog: None,
            space_editor_dialog: None,
            terminal_find_dialog: None,
            terminal_find_return_focus_after_search: false,
            last_terminal_search: String::new(),
            last_terminal_search_direction: TerminalSearchDirection::Next,
            theme_picker_restore_config: None,
            pending_command: None,
            pending_app_commands: Vec::new(),
            app_command_tx,
            app_command_rx,
            command_catalog,
            ditch_session_dialog: None,
            keybind_help_dialog: None,
            #[cfg(debug_assertions)]
            diagnostic_action_driver,
            macos_non_native_fullscreen_active,
            macos_non_native_fullscreen_pending_apply,
        })
    }

    pub fn config(&self) -> &BoottyConfig {
        self.config_state.current()
    }

    fn prepare_native_terminal_transition(&mut self, target: &mut BindingRuntime) {
        let active_is_native = selected_backend(&self.workspace.binding.multiplexer)
            == MultiplexerBackendConfig::Native;
        let target_is_native =
            selected_backend(&target.multiplexer) == MultiplexerBackendConfig::Native;

        match (active_is_native, target_is_native) {
            (true, true) => {
                std::mem::swap(&mut self.workspace.binding.terminal, &mut target.terminal);
                std::mem::swap(
                    &mut self.workspace.binding.terminal_side_effect_tx,
                    &mut target.terminal_side_effect_tx,
                );
                std::mem::swap(
                    &mut self.workspace.binding.terminal_side_effect_rx,
                    &mut target.terminal_side_effect_rx,
                );
            }
            (true, false) => {
                let mut binding_config = self.config().clone();
                binding_config.multiplexer = self.workspace.binding.multiplexer.clone();
                let replacement = NativeTerminalOwner::new(
                    &binding_config,
                    self.active_appearance_variant,
                    self.repaint.clone(),
                );
                let native_terminal =
                    NativeTerminalOwner::replace_binding(&mut self.workspace.binding, replacement);
                debug_assert!(self.workspace.parked_native_terminal.is_none());
                self.workspace.parked_native_terminal = Some(native_terminal);
            }
            (false, true) => {
                if let Some(mut native_terminal) = self.workspace.parked_native_terminal.take() {
                    native_terminal.swap_with_binding(target);
                }
            }
            (false, false) => {}
        }
    }

    /// Apply a dragged sidebar width to the live config without touching disk, so the layout
    /// tracks the pointer each frame. [`Self::persist_sidebar_width`] writes the final value.
    pub fn set_sidebar_width_live(&mut self, width: f32) {
        self.config_state.current_mut().chrome.sidebar_width = width;
    }

    /// Persist the sidebar width to `config.toml` on drag release. The live value already matches,
    /// so the hot-reload baseline is refreshed to skip the redundant reload the write would trigger.
    pub fn persist_sidebar_width(&mut self, width: f32) {
        let path = self.config().config_path.clone();
        let result = (|| {
            let mut document = load_or_create_config_document(&path)?;
            document.set_item(
                &["chrome", "sidebar-width"],
                bootty_config::toml_edit::value(f64::from(width)),
            )?;
            document.write_to_disk()
        })();
        match result {
            Ok(()) => self.config_hot_reload.refresh_after_reload(&path),
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
        let result = (|| {
            let mut document = load_or_create_config_document(&path)?;
            document.set_item(
                &["appearance", "mode"],
                bootty_config::toml_edit::value(token),
            )?;
            document.write_to_disk()
        })();
        match result {
            Ok(()) => {
                self.config_hot_reload.refresh_after_reload(&path);
                self.reload_config(effects);
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
        let result = (|| {
            let mut document = load_or_create_config_document(&path)?;
            document.set_item(
                &["appearance", branch, "theme"],
                bootty_config::toml_edit::value(theme),
            )?;
            document.write_to_disk()
        })();
        match result {
            Ok(()) => {
                self.config_hot_reload.refresh_after_reload(&path);
                self.reload_config(effects);
            }
            Err(error) => self.last_error = Some(error.to_string()),
        }
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
        let config = self.config_state.current_mut();
        let branch = match variant {
            AppearanceVariant::Light => &mut config.appearance.light,
            AppearanceVariant::Dark => &mut config.appearance.dark,
        };
        branch.theme = Some(theme.to_owned());
        branch.colors = resolved.colors;
        let colors = self
            .config()
            .colors_for_appearance(variant)
            .terminal_color_config();
        match self.set_binding_terminal_colors(colors) {
            Ok(()) => effects.push(AppEffect::RequestRepaint),
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn restore_theme_picker_preview(&mut self) -> bool {
        let Some(config) = self.theme_picker_restore_config.clone() else {
            return false;
        };
        self.config_state.accept(config);
        let colors = self
            .config()
            .colors_for_appearance(self.active_appearance_variant)
            .terminal_color_config();
        match self.set_binding_terminal_colors(colors) {
            Ok(()) => true,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    pub fn theme_picker_preview_active(&self) -> bool {
        self.theme_picker_restore_config.is_some() && self.theme_picker_dialog.is_some()
    }

    pub fn set_appearance_variant(&mut self, variant: AppearanceVariant) {
        if self.active_appearance_variant == variant {
            return;
        }
        let colors = self
            .config()
            .colors_for_appearance(variant)
            .terminal_color_config();
        match self.set_binding_terminal_colors(colors) {
            Ok(()) => {
                self.active_appearance_variant = variant;
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
            }
        }
    }

    pub fn active_appearance_variant(&self) -> AppearanceVariant {
        self.active_appearance_variant
    }

    pub fn ui_theme(&self) -> bootty_ui::Theme {
        theme_from_config(self.config(), self.active_appearance_variant)
    }

    pub fn mux(&self) -> &MuxController {
        &self.workspace.binding.mux
    }

    pub fn mux_scope(&self) -> MuxScope {
        self.workspace.binding.scope
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

    fn space_backend_override(
        &self,
        space_id: SpaceId,
    ) -> Option<Option<MultiplexerBackendConfig>> {
        self.workspace.backend_override(space_id)
    }

    fn space_remote_override(&self, space_id: SpaceId) -> Option<SpaceRemoteOverride> {
        self.workspace.remote_override(space_id)
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
        let mut repository = WorkspaceRepository::for_config_path(&config.config_path);
        let space = match repository.create_space(
            name,
            icon,
            color,
            tint_sidebar,
            mux,
            &config.multiplexer,
        ) {
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
        if space_id == self.workspace.active_space_id {
            let neighbor = spaces
                .get(index + 1)
                .or_else(|| index.checked_sub(1).and_then(|index| spaces.get(index)));
            if !neighbor.is_some_and(|space| self.activate_space_from_ui(space.id)) {
                return false;
            }
        }
        let config_path = self.config().config_path.clone();
        let mut workspace = WorkspaceRepository::for_config_path(&config_path);
        match workspace.delete_space(space_id) {
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
        let SpaceMuxOverride {
            backend: backend_override,
            remote: remote_override,
        } = mux.clone();
        let Some(previous_override) = self.space_backend_override(space_id) else {
            return false;
        };
        let previous_remote = self.space_remote_override(space_id);
        let resolved_backend = backend_override.unwrap_or(self.config().multiplexer.backend);
        let app_key_bindings = if space_id == self.workspace.active_space_id {
            let keybinds = self.config().input.keybinds_for_backend(resolved_backend);
            match AppKeyBindings::from_keybinds(&keybinds) {
                Ok(bindings) => Some(bindings),
                Err(error) => {
                    self.last_error = Some(error.to_string());
                    return false;
                }
            }
        } else {
            None
        };
        // The remote decides which machine the binding's sessions live on, so a change to it needs
        // the same rebuild a backend change does.
        let backend_changed = previous_override != backend_override
            || previous_remote.as_ref() != Some(&remote_override);
        let config_path = self.config().config_path.clone();
        let mut workspace = WorkspaceRepository::for_config_path(&config_path);
        let runtime_config = self.config().clone();
        let active_appearance_variant = self.active_appearance_variant;
        let repaint = self.repaint.clone();
        match workspace.update_space(space_id, name, icon, color, tint_sidebar, mux) {
            Ok(true) => {
                if space_id == self.workspace.active_space_id {
                    self.workspace.active_space_name = name.trim().to_owned();
                    self.workspace.active_space_icon = icon.trim().to_owned();
                    self.workspace.active_space_color = color;
                    self.workspace.active_space_tint_sidebar = tint_sidebar;
                    if backend_changed {
                        let scope = self.workspace.binding.scope;
                        let label = self.workspace.binding.label.clone();
                        self.workspace.binding = binding_runtime_for_multiplexer(
                            &runtime_config,
                            scope,
                            label,
                            backend_override,
                            remote_override.clone(),
                            active_appearance_variant,
                            repaint.clone(),
                        );
                        self.app_key_bindings =
                            app_key_bindings.expect("active backend bindings were validated");
                        self.terminal_surface = None;
                        self.last_pane_area = None;
                        if let Err(error) = self.sync_terminal_panes() {
                            self.last_error = Some(error.to_string());
                        }
                    }
                } else if let Some(space) = self
                    .workspace
                    .inactive_spaces
                    .iter_mut()
                    .find(|space| space.id == space_id)
                {
                    space.name = name.trim().to_owned();
                    space.icon = icon.trim().to_owned();
                    space.color = color;
                    space.tint_sidebar = tint_sidebar;
                    if backend_changed {
                        let scope = space.binding.scope;
                        let label = space.binding.label.clone();
                        space.binding = binding_runtime_for_multiplexer(
                            &runtime_config,
                            scope,
                            label,
                            backend_override,
                            remote_override.clone(),
                            active_appearance_variant,
                            repaint.clone(),
                        );
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

    fn persist_active_binding_restore_state(&mut self) {
        let selected_session = self
            .workspace
            .binding
            .mux
            .selected_session()
            .map(str::to_owned);
        let selected_window = self
            .workspace
            .binding
            .mux
            .selected_window()
            .map(str::to_owned);
        let mut workspace = WorkspaceRepository::for_config_path(&self.config().config_path);
        if let Err(error) = workspace.set_binding_restore_state(
            self.workspace.binding.scope,
            self.workspace.binding.mux.last_error().is_some(),
            selected_session.as_deref(),
            selected_window.as_deref(),
        ) {
            self.last_error = Some(error.to_string());
        }
    }
    fn persist_rmux_restore_state(&mut self) {
        if selected_backend(&self.workspace.binding.multiplexer) == MultiplexerBackendConfig::Rmux {
            self.persist_active_binding_restore_state();
        }
    }

    pub fn activate_space_from_ui(&mut self, space_id: SpaceId) -> bool {
        if space_id == self.workspace.active_space_id {
            return false;
        }
        let Some(index) = self
            .workspace
            .inactive_spaces
            .iter()
            .position(|space| space.id == space_id)
        else {
            return false;
        };
        let backend = self.workspace.inactive_spaces[index]
            .binding
            .multiplexer
            .backend;
        let keybinds = self.config().input.keybinds_for_backend(backend);
        let app_key_bindings = match AppKeyBindings::from_keybinds(&keybinds) {
            Ok(bindings) => bindings,
            Err(error) => {
                self.last_error = Some(error.to_string());
                return false;
            }
        };
        let switch_started = crate::diagnostics::latency_start();
        self.persist_active_binding_restore_state();
        crate::diagnostics::trace_phase("space.persist_restore_state", switch_started);
        // Leave the outgoing space's tmux overrides in place. It keeps a live runtime, so its
        // status bar should stay hidden, and its terminal carries the bookkeeping to restore on
        // drop. Restoring here cost a tmux fork per pane and session, then the incoming binding
        // immediately paid to set them again.
        let phase = crate::diagnostics::latency_start();
        let mut target = self.workspace.inactive_spaces.remove(index);
        self.workspace.binding.discard_terminal_side_effects();
        for binding in &mut self.workspace.inactive_bindings {
            binding.discard_terminal_side_effects();
        }
        for binding in target.bindings_mut() {
            binding.discard_terminal_side_effects();
        }
        if let Some(owner) = &mut self.workspace.parked_native_terminal {
            owner.discard_side_effects();
        }
        self.prepare_native_terminal_transition(&mut target.binding);
        crate::diagnostics::trace_phase("space.prepare_transition", phase);
        let phase = crate::diagnostics::latency_start();
        let current = SpaceRuntime {
            id: std::mem::replace(&mut self.workspace.active_space_id, target.id),
            name: std::mem::replace(&mut self.workspace.active_space_name, target.name),
            icon: std::mem::replace(&mut self.workspace.active_space_icon, target.icon),
            color: std::mem::replace(&mut self.workspace.active_space_color, target.color),
            tint_sidebar: std::mem::replace(
                &mut self.workspace.active_space_tint_sidebar,
                target.tint_sidebar,
            ),
            position: std::mem::replace(&mut self.workspace.active_space_position, target.position),
            binding: std::mem::replace(&mut self.workspace.binding, target.binding),
            inactive_bindings: std::mem::replace(
                &mut self.workspace.inactive_bindings,
                target.inactive_bindings,
            ),
        };
        if !self
            .workspace
            .binding
            .session_order
            .session_names()
            .is_empty()
        {
            self.workspace.binding.mux.refresh_on_next_frame();
            let active_config = self.workspace.binding.multiplexer.clone();
            let _ = self
                .workspace
                .binding
                .mux
                .refresh_sessions(&self.repaint, &active_config);
            crate::diagnostics::trace_phase("space.refresh_sessions", phase);
            let phase = crate::diagnostics::latency_start();
            self.sync_session_order();
            crate::diagnostics::trace_phase("space.sync_session_order", phase);
            if selected_backend(&active_config) == MultiplexerBackendConfig::Native {
                self.workspace.binding.persisted_sessions_restored = false;
                self.workspace
                    .binding
                    .restore_persisted_sessions(&self.repaint);
            }
        }
        let previous_space_id = current.id;
        self.workspace.inactive_spaces.push(current);
        self.workspace
            .inactive_spaces
            .sort_by_key(|space| space.position);
        self.workspace.space_transition = Some(SpaceTransition {
            from: previous_space_id,
            to: self.workspace.active_space_id,
            started: Instant::now(),
        });
        let phase = crate::diagnostics::latency_start();
        let workspace = WorkspaceRepository::for_config_path(&self.config().config_path);
        if let Err(error) =
            workspace.set_selected_space(&self.window_state_key, self.workspace.active_space_id)
        {
            self.last_error = Some(error.to_string());
        }
        crate::diagnostics::trace_phase("space.persist_selected_space", phase);
        self.app_key_bindings = app_key_bindings;
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
        self.new_mux_session_dialog = None;
        self.sidebar_hovered_session = None;
        self.session_picker_dialog = None;
        self.rename_session_dialog = None;
        self.rename_tab_dialog = None;
        self.ditch_session_dialog = None;
        self.space_editor_dialog = None;
    }

    pub fn binding_session_groups(&self) -> Vec<BindingSessionGroup> {
        let mut bindings = std::iter::once(&self.workspace.binding)
            .chain(self.workspace.inactive_bindings.iter())
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
                    active: binding.scope == self.workspace.binding.scope,
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
            self.workspace.active_space_position,
            self.workspace.active_space_name.as_str(),
            std::iter::once(&self.workspace.binding)
                .chain(self.workspace.inactive_bindings.iter())
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
                    active: binding.scope == self.workspace.binding.scope,
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
                scope: self.workspace.binding.scope,
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

    fn binding_runtimes_mut(&mut self) -> impl Iterator<Item = &mut BindingRuntime> {
        std::iter::once(&mut self.workspace.binding)
            .chain(self.workspace.inactive_bindings.iter_mut())
            .chain(
                self.workspace
                    .inactive_spaces
                    .iter_mut()
                    .flat_map(SpaceRuntime::bindings_mut),
            )
    }

    fn set_binding_terminal_colors(&mut self, colors: TerminalColorConfig) -> Result<()> {
        if let Some(owner) = &mut self.workspace.parked_native_terminal {
            owner.terminal.set_colors(colors.clone())?;
        }
        for binding in self.binding_runtimes_mut() {
            binding.terminal.set_colors(colors.clone())?;
        }
        Ok(())
    }

    fn set_binding_cursor_config(&mut self, cursor: TerminalCursorConfig) -> Result<()> {
        if let Some(owner) = &mut self.workspace.parked_native_terminal {
            owner.terminal.set_cursor_config(cursor)?;
        }
        for binding in self.binding_runtimes_mut() {
            binding.terminal.set_cursor_config(cursor)?;
        }
        Ok(())
    }

    fn set_binding_feature_config(&mut self, features: TerminalFeatureConfig) -> Result<()> {
        if let Some(owner) = &mut self.workspace.parked_native_terminal {
            owner.terminal.set_feature_config(features)?;
        }
        for binding in self.binding_runtimes_mut() {
            binding.terminal.set_feature_config(features)?;
        }
        Ok(())
    }

    fn active_multiplexer(&self) -> &crate::config::MultiplexerConfig {
        &self.workspace.binding.multiplexer
    }

    pub fn multiplexer_backend(&self) -> crate::config::MultiplexerBackendConfig {
        self.workspace.binding.multiplexer.backend
    }

    pub fn terminal_transition_key(&self) -> Option<String> {
        self.workspace
            .binding
            .mux
            .selected_session_anchor()
            .map(|anchor| {
                scoped_terminal_transition_key(
                    self.workspace.binding.scope,
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
            .binding
            .mux
            .last_error()
            .or(self.last_error.as_deref())
    }

    pub fn clear_last_error(&mut self) {
        self.workspace.binding.mux.set_error(None);
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
    pub fn set_lua_window_open(&mut self, open: bool) {
        self.lua_window_open = open;
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
        &mut self.workspace.binding.terminal
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

    fn uses_native_terminal_layout(&self) -> bool {
        matches!(
            self.active_multiplexer().backend,
            crate::config::MultiplexerBackendConfig::Native
                | crate::config::MultiplexerBackendConfig::Rmux
        )
    }

    fn current_window_key(&self) -> ScopedWindowId {
        let session = self
            .workspace
            .binding
            .mux
            .selected_session()
            .unwrap_or("local")
            .to_owned();
        let window = self
            .workspace
            .binding
            .mux
            .selected_window()
            .map(str::to_owned)
            .or_else(|| {
                self.workspace
                    .binding
                    .mux
                    .sessions()
                    .iter()
                    .find(|candidate| candidate.id == session || candidate.name == session)
                    .and_then(|candidate| candidate.active_window_id.clone())
            })
            .unwrap_or_default();
        self.workspace.binding.window_id(session, window)
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
            .binding
            .pending_pane_split_directions
            .remove(key)
            .or_else(|| {
                if key.window_id.is_empty() {
                    None
                } else {
                    self.workspace.binding.pending_pane_split_directions.remove(
                        &self
                            .workspace
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
            .binding
            .pane_layouts
            .get(&self.current_window_key())
    }

    /// Drop split layouts whose `(session, window)` no longer exists, so the map doesn't grow
    /// unbounded as the user creates and destroys native sessions and tabs. Keys are stored by
    /// whatever `current_window_key` recorded (session id, occasionally name), so accept either.
    fn prune_pane_layouts(&mut self) {
        if self.workspace.binding.pane_layouts.is_empty() {
            return;
        }
        let mut live = Vec::new();
        for session in self.workspace.binding.mux.sessions() {
            for window in &session.windows {
                live.push(
                    self.workspace
                        .binding
                        .window_id(session.id.clone(), window.id.clone()),
                );
                live.push(
                    self.workspace
                        .binding
                        .window_id(session.name.clone(), window.id.clone()),
                );
            }
        }
        live.push(self.current_window_key());
        self.workspace
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
            let result = self.workspace.binding.terminal.sync_scoped_mux_anchor(
                self.workspace.binding.scope,
                &config,
                self.workspace.binding.mux.selected_session_anchor(),
            );
            crate::diagnostics::trace_slow("panes.sync_scoped_mux_anchor", phase, 2.0);
            return result;
        }
        let panes: Vec<MuxPaneAnchor> = self.workspace.binding.mux.selected_window_panes().to_vec();
        let pane_ids: Vec<String> = panes
            .iter()
            .filter_map(|pane| pane.pane_id.clone())
            .collect();
        if pane_ids.is_empty() {
            // Idle native session (all tabs closed): nothing to render.
            return self.workspace.binding.terminal.sync_scoped_mux_anchor(
                self.workspace.binding.scope,
                &config,
                self.workspace.binding.mux.selected_session_anchor(),
            );
        }
        let key = self.current_window_key();
        let window_id = (!key.window_id.is_empty()).then(|| key.window_id.clone());
        let selected_pane = self
            .workspace
            .binding
            .mux
            .selected_session_anchor()
            .and_then(|anchor| anchor.pane_id.clone());
        let server_layout = self
            .workspace
            .binding
            .mux
            .selected_window_layout()
            .and_then(PaneLayout::from_mux_layout)
            .filter(|layout| pane_sets_match(&layout.panes(), &pane_ids));
        let layout_missing = !self.workspace.binding.pane_layouts.contains_key(&key);
        let stale_layout = self
            .workspace
            .binding
            .pane_layouts
            .get(&key)
            .is_some_and(|layout| layout.panes().iter().all(|pane| !pane_ids.contains(pane)));
        let mut restored_from_server = false;
        if (layout_missing || stale_layout)
            && let Some(layout) = server_layout.clone()
        {
            self.workspace
                .binding
                .pane_layouts
                .insert(key.clone(), layout);
            restored_from_server = true;
        }

        let previous_panes = self
            .workspace
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
                .binding
                .pane_layouts
                .get_mut(&key)
                .expect("native layout should be initialized");
            layout.reconcile_with_new_pane_direction(&pane_ids, new_pane_direction);
        }
        let layout = self
            .workspace
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
        self.workspace.binding.terminal.sync_scoped_native_window(
            self.workspace.binding.scope,
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
        self.workspace.binding.pane_id(window, pane_id)
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
                    .binding
                    .mux
                    .selected_session_anchor()
                    .and_then(|anchor| anchor.pane_id.as_deref())
                    .and_then(|pane_id| self.pane_progress(pane_id))
            })
            .or(self.workspace.binding.unscoped_terminal_progress)
    }

    pub(crate) fn pane_progress(&self, pane_id: &str) -> Option<TerminalProgress> {
        self.workspace
            .binding
            .terminal_progress
            .get(&self.pane_cache_key(pane_id))
            .copied()
    }

    pub(crate) fn pane_ports(&self, pane_id: &str) -> Option<&[u16]> {
        self.workspace
            .binding
            .terminal_ports
            .get(&self.pane_cache_key(pane_id))
            .map(Vec::as_slice)
    }

    pub(crate) fn session_ports(&self, session: &MuxSession) -> Vec<u16> {
        let selected = self.workspace.binding.mux.selected_session();
        let mut ports =
            if selected == Some(session.id.as_str()) || selected == Some(session.name.as_str()) {
                self.workspace.binding.unscoped_terminal_ports.clone()
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
            .binding
            .terminal_progress
            .values()
            .chain(self.workspace.binding.unscoped_terminal_progress.iter())
            .any(|progress| progress.state == TerminalProgressState::Indeterminate)
            || self.workspace.binding.mux.sessions().iter().any(|session| {
                session
                    .windows
                    .iter()
                    .any(|window| self.window_has_indeterminate_progress(window))
            })
    }

    /// The names the active binding shows for `sessions`, in the same order.
    pub(crate) fn session_display_names(&self, sessions: &[MuxSession]) -> Vec<String> {
        self.workspace.binding.session_display_names(sessions)
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
        let moved = match self.workspace.binding.pane_layouts.get_mut(&key) {
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
        if let Some(layout) = self.workspace.binding.pane_layouts.get_mut(&key) {
            layout.set_ratio_at(path, ratio, min_fraction, min_fraction);
        }
    }

    pub fn render_source_for_pane(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut (dyn TerminalRuntime + '_)> {
        self.workspace
            .binding
            .terminal
            .render_source_for_pane(pane_id)
    }

    pub fn pane_terminal_window_size<F>(&self, leaf_size: F) -> Option<(u16, u16)>
    where
        F: FnMut(&str) -> Option<(u16, u16)>,
    {
        self.current_pane_layout()?.terminal_window_size(leaf_size)
    }

    pub fn resize_native_layout_window(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.workspace
            .binding
            .terminal
            .resize_native_layout_window(cols, rows)
    }

    fn sync_native_layout_terminal_now(&mut self) {
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
            .binding
            .mux
            .selected_session()
            .unwrap_or("local")
            .to_owned();
        let mux_config = self.active_multiplexer().clone();
        if !self.uses_native_terminal_layout() {
            self.workspace.binding.mux.execute_command(
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
                .binding
                .pane_layouts
                .get(&key)
                .map(|layout| layout.focused().to_owned())
                .or_else(|| {
                    self.workspace
                        .binding
                        .mux
                        .selected_session_anchor()
                        .and_then(|anchor| anchor.pane_id.clone())
                })
        });
        self.workspace.binding.mux.execute_command(
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
                .binding
                .pending_pane_split_directions
                .insert(key, direction);
            return;
        }

        // The native split synchronously sets the new pane active, so the refreshed anchor names it.
        let new_pane = self
            .workspace
            .binding
            .mux
            .selected_session_anchor()
            .and_then(|anchor| anchor.pane_id.clone());
        if let Some(new_pane) = new_pane {
            let layout = self
                .workspace
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
        let Some(layout) = self.workspace.binding.pane_layouts.get(&key) else {
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
        if target.scope.space_id() != self.workspace.active_space_id
            && !self.activate_space_from_ui(target.scope.space_id())
        {
            return false;
        }
        if target.scope != self.workspace.binding.scope {
            let Some(index) = self
                .workspace
                .inactive_bindings
                .iter()
                .position(|binding| binding.scope == target.scope)
            else {
                return false;
            };
            let backend = self.workspace.inactive_bindings[index].multiplexer.backend;
            let keybinds = self.config().input.keybinds_for_backend(backend);
            let app_key_bindings = match AppKeyBindings::from_keybinds(&keybinds) {
                Ok(bindings) => bindings,
                Err(error) => {
                    self.last_error = Some(error.to_string());
                    return false;
                }
            };
            // Same as the space switch: the outgoing binding stays live and restores its own tmux
            // overrides on drop, so skip the fork-per-option restore the next attach would undo.
            let mut target_binding = self.workspace.inactive_bindings.remove(index);
            self.workspace.binding.discard_terminal_side_effects();
            target_binding.discard_terminal_side_effects();
            if let Some(owner) = &mut self.workspace.parked_native_terminal {
                owner.discard_side_effects();
            }
            self.prepare_native_terminal_transition(&mut target_binding);
            let current_binding = std::mem::replace(&mut self.workspace.binding, target_binding);
            self.workspace
                .inactive_bindings
                .insert(index, current_binding);
            if !self
                .workspace
                .binding
                .session_order
                .session_names()
                .is_empty()
            {
                self.workspace.binding.mux.refresh_on_next_frame();
                let active_config = self.workspace.binding.multiplexer.clone();
                let _ = self
                    .workspace
                    .binding
                    .mux
                    .refresh_sessions(&self.repaint, &active_config);
                self.sync_session_order();
                if selected_backend(&active_config) == MultiplexerBackendConfig::Native {
                    self.workspace.binding.persisted_sessions_restored = false;
                    self.workspace
                        .binding
                        .restore_persisted_sessions(&self.repaint);
                }
            }
            self.app_key_bindings = app_key_bindings;
            self.terminal_surface = None;
            self.last_pane_area = None;
        }
        self.workspace
            .binding
            .mux
            .activate_session(&target.session_id);
        self.persist_rmux_restore_state();
        self.sync_native_layout_terminal_now();
        self.sidebar_hovered_session = Some(target.clone());
        (self.repaint)();
        true
    }

    pub fn activate_session_from_ui(&mut self, session_id: &str) {
        let target = ScopedSessionTarget::new(self.workspace.binding.scope, session_id);
        self.activate_scoped_session_from_ui(&target);
    }

    pub fn activate_relative_session_from_ui(&mut self, session_id: &str, delta: isize) -> bool {
        let sessions = self.workspace.binding.mux.sessions();
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
        let mux_config = self.active_multiplexer().clone();
        self.workspace.binding.mux.activate_window(
            session_id,
            window_id,
            &self.repaint,
            &mux_config,
        );
        self.persist_rmux_restore_state();
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
        self.workspace.binding.mux.execute_command(
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
            .binding
            .mux
            .selected_session()
            .map(str::to_owned);
        let selected_window = self
            .workspace
            .binding
            .mux
            .selected_window()
            .map(str::to_owned);
        let Some((resolved_session_id, anchor_cwd, target_is_current)) = self
            .workspace
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
        self.workspace.binding.mux.execute_command(
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
        let windows = self.workspace.binding.mux.selected_session_windows();
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
        self.workspace.binding.mux.execute_command(
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
            .binding
            .mux
            .selected_session()
            .map(str::to_owned);
        let selected_window = self
            .workspace
            .binding
            .mux
            .selected_window()
            .map(str::to_owned);
        let Some((session_id, position, window_count, active_window_id)) = self
            .workspace
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
            .binding
            .mux
            .execute_command(&self.repaint, &mux_config, command);
        self.sync_native_layout_terminal_now();
        true
    }

    pub fn close_pane_for_window_from_ui(&mut self, session_id: &str, window_id: &str) -> bool {
        let Some((session_id, window_id, pane_id)) = self
            .workspace
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
            .binding
            .mux
            .selected_session()
            .map(str::to_owned);
        let current_window = self.current_window_key();
        let target_is_current = current_window.window_id == window_id
            && self
                .workspace
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
        self.workspace.binding.mux.close_pane(
            &session_id,
            Some(&pane_id),
            &self.repaint,
            &mux_config,
        );
        self.workspace.binding.terminal.discard_pane(&pane_id);
        if self.uses_native_terminal_layout() {
            let key = self
                .workspace
                .binding
                .window_id(session_id.clone(), window_id.clone());
            if let Some(layout) = self.workspace.binding.pane_layouts.get_mut(&key) {
                layout.remove(&pane_id);
            }
            if target_is_current {
                let _ = self.sync_terminal_panes();
            }
        }
        true
    }

    fn sync_session_order(&mut self) {
        self.workspace.binding.sync_session_order();
    }
    /// Whether the generated-name reconciler needs to run, updating the stored fingerprint as a
    /// side effect. Reconciling forks up to four `git` subprocesses per session (a worktree lookup,
    /// then a suggested name), so this returns `false` while nothing relevant has changed, keeping
    /// that work off the steady-state frame path.
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
    fn generated_names_need_sync(&mut self) -> bool {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for session in self.workspace.binding.mux.all_sessions() {
            hasher.write(session.id.as_bytes());
            hasher.write_u8(0);
            hasher.write(session.name.as_bytes());
            hasher.write_u8(0);
            if let Some(cwd) = session.anchor.cwd.as_deref() {
                hasher.write(cwd.as_bytes());
            }
            hasher.write_u8(1);
        }
        let signature = hasher.finish();
        if self.workspace.binding.generated_names_signature == Some(signature) {
            return false;
        }
        self.workspace.binding.generated_names_signature = Some(signature);
        true
    }

    fn sync_generated_session_names(&mut self) {
        let remote = self.active_multiplexer().remote.is_some();
        // Preserve membership before `observe_session` records the backend's new names below.
        self.workspace.binding.carry_renamed_members();
        if selected_backend(self.active_multiplexer()) == MultiplexerBackendConfig::Rmux {
            return;
        }
        if !self.generated_names_need_sync() {
            return;
        }
        // Reconcile only this binding's sessions. Generating names for the whole backend list
        // renames sessions that belong to other Spaces.
        let sessions = self.workspace.binding.mux.sessions().to_vec();
        let mut renames = Vec::new();
        self.workspace
            .binding
            .pending_generated_names
            .retain(|session_id, pending| {
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
        let mut planned_names = self
            .workspace
            .binding
            .pending_generated_names
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
            let mut record = if let Some(record) = self
                .workspace
                .binding
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
                    self.workspace.binding.session_names.remember_generated(
                        &session.id,
                        &cwd,
                        &session.name,
                        &session.name,
                    );
                } else {
                    self.workspace.binding.session_names.mark_explicit(
                        &session.id,
                        &session.name,
                        &session.name,
                        &cwd,
                    );
                }
                self.workspace
                    .binding
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
                    self.workspace
                        .binding
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
                self.workspace
                    .binding
                    .session_names
                    .set_display_name(&session.id, &display_name);
                record.display_name = display_name;
            }

            if let Some(pending) = self
                .workspace
                .binding
                .pending_generated_names
                .get(&session.id)
                .cloned()
            {
                if pending.cwd == cwd {
                    if session.name == pending.name {
                        planned_names.remove(&pending.name);
                        self.workspace.binding.session_names.remember_generated(
                            &session.id,
                            &cwd,
                            &pending.name,
                            &pending.display_name,
                        );
                        self.workspace
                            .binding
                            .pending_generated_names
                            .remove(&session.id);
                    } else if session.name != record.generated_name {
                        planned_names.remove(&pending.name);
                        self.workspace
                            .binding
                            .pending_generated_names
                            .remove(&session.id);
                        self.workspace.binding.session_names.mark_explicit(
                            &session.id,
                            &session.name,
                            &session.name,
                            &cwd,
                        );
                    }
                    continue;
                }
                self.workspace
                    .binding
                    .pending_generated_names
                    .remove(&session.id);
            }
            if record.explicit {
                continue;
            }
            if session.name != record.generated_name {
                self.workspace.binding.session_names.mark_explicit(
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
            self.workspace.binding.pending_generated_names.insert(
                session.id.clone(),
                PendingGeneratedName {
                    cwd,
                    name: desired.clone(),
                    display_name,
                },
            );
            renames.push((session.id.clone(), desired));
        }

        if renames.is_empty() {
            return;
        }
        let mux_config = self.active_multiplexer().clone();
        for (session_id, name) in renames {
            self.workspace.binding.mux.rename_session(
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
    fn taken_session_names(&self, keep: Option<&str>) -> Vec<String> {
        std::iter::once(&self.workspace.binding)
            .chain(self.workspace.inactive_bindings.iter())
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
        self.workspace.binding.pending_generated_names.insert(
            session_id.clone(),
            PendingGeneratedName {
                cwd: cwd.clone(),
                name: session_id.clone(),
                display_name: display_name.clone(),
            },
        );
        self.workspace.binding.session_names.remember_generated(
            &session_id,
            &cwd,
            &session_id,
            &display_name,
        );
        self.workspace
            .binding
            .session_order
            .add_session(&session_id);
        let mux_config = self.active_multiplexer().clone();
        self.workspace.binding.mux.create_project_session(
            crate::ui::new_session_picker::NewMuxSessionRequest { session_id, cwd },
            &self.repaint,
            &mux_config,
        );
        self.persist_rmux_restore_state();
        self.input_focus = InputFocus::Terminal;
    }

    fn session_cwd(cwd: &str, remote: bool) -> String {
        if remote {
            cwd.to_owned()
        } else {
            Self::session_root(cwd)
        }
    }

    fn suggested_session_name(cwd: &str, remote: bool) -> String {
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
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .map(|session| session.name.clone())
        else {
            return false;
        };
        if !self.workspace.binding.session_order.move_session(
            &session_name,
            delta,
            self.workspace
                .binding
                .mux
                .sessions()
                .iter()
                .map(|session| session.name.as_str()),
        ) {
            return false;
        }
        self.sync_session_order();
        true
    }

    pub fn reorder_session_before(&mut self, source: &str, target: Option<&str>) -> bool {
        // Per-session anchors: a drag reorders within a group when source and target share one,
        // and moves the whole group across groups.
        if !self.workspace.binding.session_order.move_session_before(
            source,
            target,
            self.workspace
                .binding
                .mux
                .sessions()
                .iter()
                .map(|session| session.name.as_str()),
        ) {
            return false;
        }
        self.sync_session_order();
        true
    }

    pub fn take_dialog(&mut self) -> Option<NewMuxSessionDialog> {
        self.new_mux_session_dialog.take()
    }
    pub fn take_space_editor_dialog(&mut self) -> Option<SpaceEditorDialog> {
        self.space_editor_dialog.take()
    }

    pub fn apply_space_editor_event(&mut self, dialog: SpaceEditorDialog, event: SpaceEditorEvent) {
        match event {
            SpaceEditorEvent::None => self.space_editor_dialog = Some(dialog),
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
                    self.space_editor_dialog = Some(dialog);
                }
            }
        }
    }

    pub fn detach_scoped_session_from_space(&mut self, target: &ScopedSessionTarget) -> bool {
        let Some(binding) = self
            .binding_runtimes_mut()
            .find(|binding| binding.scope == target.scope)
        else {
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
        if !binding.session_order.remove_session(&name) {
            return false;
        }
        binding.sync_session_order();
        (self.repaint)();
        true
    }

    pub fn take_session_picker_dialog(&mut self) -> Option<SessionPickerDialog> {
        self.session_picker_dialog.take()
    }

    pub fn apply_session_picker_event(
        &mut self,
        dialog: SessionPickerDialog,
        event: SessionPickerEvent,
    ) {
        match event {
            SessionPickerEvent::None => {
                self.session_picker_dialog = Some(dialog);
            }
            SessionPickerEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            SessionPickerEvent::ActivateSession(target) => {
                self.input_focus = InputFocus::Terminal;
                if let Some(binding) = self
                    .binding_runtimes_mut()
                    .find(|binding| binding.scope == target.scope)
                    && let Some(name) = binding
                        .mux
                        .all_sessions()
                        .iter()
                        .find(|session| {
                            session.id == target.session_id || session.name == target.session_id
                        })
                        .map(|session| session.name.clone())
                {
                    binding.session_order.add_session(&name);
                    binding.sync_session_order();
                }
                self.activate_scoped_session_from_ui(&target);
            }
        }
    }

    pub fn take_rename_session_dialog(&mut self) -> Option<RenameSessionDialog> {
        self.rename_session_dialog.take()
    }

    pub fn apply_rename_session_event(
        &mut self,
        dialog: RenameSessionDialog,
        event: RenameSessionEvent,
    ) {
        match event {
            RenameSessionEvent::None => {
                self.rename_session_dialog = Some(dialog);
            }
            RenameSessionEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            RenameSessionEvent::Rename { session_id, name } => {
                let name = name.trim().to_owned();
                let session = self
                    .workspace
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
                    self.workspace
                        .binding
                        .session_order
                        .rename_session(&session.name, &backend_name);
                    self.workspace.binding.pending_generated_names.insert(
                        backend_name.clone(),
                        PendingGeneratedName {
                            cwd: cwd.clone(),
                            name: backend_name.clone(),
                            display_name: name.clone(),
                        },
                    );
                    self.workspace.binding.session_names.mark_explicit(
                        &session.id,
                        &backend_name,
                        &name,
                        &cwd,
                    );
                    let mux_config = self.active_multiplexer().clone();
                    self.workspace.binding.mux.rename_session(
                        &session.id,
                        backend_name,
                        &self.repaint,
                        &mux_config,
                    );
                }
                self.input_focus = InputFocus::Terminal;
            }
        }
    }

    pub fn take_rename_tab_dialog(&mut self) -> Option<RenameTabDialog> {
        self.rename_tab_dialog.take()
    }

    pub fn apply_rename_tab_event(&mut self, dialog: RenameTabDialog, event: RenameTabEvent) {
        match event {
            RenameTabEvent::None => {
                self.rename_tab_dialog = Some(dialog);
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
                    .binding
                    .window_id(session_id.clone(), window_id.clone());
                if name.is_empty() {
                    self.workspace.binding.custom_tab_names.remove(&key);
                    if let Some(title) = self
                        .workspace
                        .binding
                        .terminal_tab_titles
                        .get(&key)
                        .cloned()
                    {
                        self.rename_window_for_terminal_title(&session_id, &window_id, &title);
                    }
                } else {
                    self.workspace.binding.custom_tab_names.insert(key);
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
        self.ditch_session_dialog.take()
    }

    pub fn apply_ditch_session_event(
        &mut self,
        dialog: DitchSessionDialog,
        event: DitchSessionEvent,
    ) {
        match event {
            DitchSessionEvent::None => {
                self.ditch_session_dialog = Some(dialog);
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
                    self.ditch_session_dialog = Some(dialog);
                    return;
                }
                let mux_config = self.active_multiplexer().clone();
                self.workspace
                    .binding
                    .mux
                    .ditch_session(&session_id, &self.repaint, &mux_config);
                self.input_focus = InputFocus::Terminal;
            }
        }
    }

    pub fn take_keybind_help_dialog(&mut self) -> Option<KeybindHelpDialog> {
        self.keybind_help_dialog.take()
    }

    pub fn apply_keybind_help_event(&mut self, dialog: KeybindHelpDialog, event: KeybindHelpEvent) {
        match event {
            KeybindHelpEvent::None => {
                self.keybind_help_dialog = Some(dialog);
            }
            KeybindHelpEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
        }
    }

    pub fn take_command_palette_dialog(&mut self) -> Option<CommandPaletteDialog> {
        self.command_palette_dialog.take()
    }

    pub fn apply_command_palette_event(
        &mut self,
        dialog: CommandPaletteDialog,
        event: CommandPaletteEvent,
    ) {
        match event {
            CommandPaletteEvent::None => {
                self.command_palette_dialog = Some(dialog);
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
                    .command_catalog
                    .describe(&invocation.command)
                    .and_then(|descriptor| descriptor.target)
                {
                    let Some(target) = self.current_command_target_for(&invocation.command, kind)
                    else {
                        self.pending_command = None;
                        self.last_error = Some(format!("no current {kind:?} target is available"));
                        return;
                    };
                    invocation.target = Some(target);
                }
                self.pending_command = Some(invocation);
            }
        }
    }

    pub fn take_theme_picker_dialog(&mut self) -> Option<ThemePickerDialog> {
        self.theme_picker_dialog.take()
    }

    pub fn apply_theme_picker_event(
        &mut self,
        dialog: ThemePickerDialog,
        event: ThemePickerEvent,
        effects: &mut Vec<AppEffect>,
    ) {
        match event {
            ThemePickerEvent::None => {
                self.theme_picker_dialog = Some(dialog);
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
                self.theme_picker_dialog = Some(dialog);
            }
            ThemePickerEvent::Preview(theme) => {
                self.preview_active_theme(&theme, effects);
                self.theme_picker_dialog = Some(dialog);
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
                self.new_mux_session_dialog = Some(dialog);
            }
            NewSessionPickerEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            NewSessionPickerEvent::Error(error) => {
                self.last_error = Some(error);
                self.new_mux_session_dialog = Some(dialog);
            }
            NewSessionPickerEvent::CreateWorktree { repo, branch } => {
                match crate::git::add_worktree(&repo, &branch) {
                    Ok(path) => {
                        self.create_project_session_for_cwd(path);
                        self.input_focus = InputFocus::Terminal;
                    }
                    Err(error) => {
                        self.last_error = Some(format!("worktree: {error}"));
                        self.new_mux_session_dialog = Some(dialog);
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
                    if scope != self.workspace.binding.scope {
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
                if let Err(error) = self.workspace.binding.terminal.write_input(&response) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalSideEffect::ReportVariable(name) => {
                if let Some(response) = terminal_report_variable_response(
                    &name,
                    self.workspace.binding.mux.selected_session(),
                ) && let Err(error) = self.workspace.binding.terminal.write_input(&response)
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
                            .binding
                            .terminal_progress
                            .insert(key, progress);
                    }
                    None => {
                        self.workspace.binding.terminal_progress.remove(&key);
                    }
                }
            }
            None => self.workspace.binding.unscoped_terminal_progress = progress,
        }
    }

    fn apply_terminal_ports(&mut self, source_pane_id: Option<&str>, ports: Vec<u16>) {
        match source_pane_id {
            Some(pane_id) => {
                let key = self.pane_cache_key(pane_id);
                self.workspace.binding.terminal_ports.insert(key, ports);
            }
            None => self.workspace.binding.unscoped_terminal_ports = ports,
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
                .binding
                .terminal_tab_titles
                .insert(key.clone(), title.clone());
            if !self.workspace.binding.custom_tab_names.contains(&key) {
                self.rename_window_for_terminal_title(&key.session_id, &key.window_id, &title);
            }
        }
        if source_pane_id.is_none()
            || self.workspace.binding.terminal.focused_pane_id() == source_pane_id
        {
            effects.push(AppEffect::SetWindowTitle(title));
        }
    }

    fn window_key_for_pane(&self, pane_id: &str) -> Option<ScopedWindowId> {
        self.workspace
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
        self.workspace.binding.mux.rename_window(
            session_id,
            window_id,
            name.to_owned(),
            &self.repaint,
            &mux_config,
        );
    }

    fn window_name_for_key(&self, session_id: &str, window_id: &str) -> Option<&str> {
        self.workspace
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
            .binding
            .mux
            .selected_session()
            .map(str::to_owned);
        let selected_window = self
            .workspace
            .binding
            .mux
            .selected_window()
            .map(str::to_owned);
        let pane_count = self.workspace.binding.mux.selected_window_panes().len();
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
        if let Some(invocation) = self.pending_command.take() {
            let _ = self.dispatch_command(invocation, viewport, &mut effects);
        }

        self.sync_macos_non_native_fullscreen_presentation();
        // Drain the focused pane plus every live sibling in the active native window so background
        // panes keep processing output. For non-native this is just the single attach surface.
        self.last_drain = self.workspace.binding.terminal.drain_native_window();
        for binding in &mut self.workspace.inactive_bindings {
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
            for pane in self.workspace.binding.terminal.native_exited_panes() {
                self.close_pane(&pane);
            }
        } else {
            match self.workspace.binding.terminal.child_exited() {
                Ok(true) => self.handle_attach_client_exit(now),
                Ok(false) => self.note_attach_client_alive(now),
                Err(error) => self.last_error = Some(error.to_string()),
            }
            self.start_due_reattach(now, &mut effects);
        }

        if let Some(Err(_)) = self.workspace.binding.mux.poll_command() {
            self.workspace.binding.pending_generated_names.clear();
        }
        for binding in &mut self.workspace.inactive_bindings {
            if let Some(Err(_)) = binding.mux.poll_command() {
                binding.pending_generated_names.clear();
            }
        }
        for space in &mut self.workspace.inactive_spaces {
            for binding in space.bindings_mut() {
                if let Some(Err(_)) = binding.mux.poll_command() {
                    binding.pending_generated_names.clear();
                }
            }
        }
        let active_config = self.workspace.binding.multiplexer.clone();
        self.workspace
            .binding
            .mux
            .set_refresh_interval(mux_session_refresh_interval(window_focused));
        let _ = self
            .workspace
            .binding
            .mux
            .refresh_sessions(&self.repaint, &active_config);
        self.workspace
            .binding
            .restore_persisted_sessions(&self.repaint);
        let refresh_completed = self.workspace.binding.mux.take_refresh_completed();
        self.resolve_remote_attach_exit_after_refresh(refresh_completed);
        let mux_refresh_after = mux_refresh_repaint_after(&active_config, window_focused);
        for binding in &mut self.workspace.inactive_bindings {
            binding.restore_persisted_sessions(&self.repaint);
            binding.sync_session_order();
        }
        for space in &mut self.workspace.inactive_spaces {
            for binding in space.bindings_mut() {
                binding.restore_persisted_sessions(&self.repaint);
                binding.sync_session_order();
            }
        }
        if let Some(after) = mux_refresh_after {
            effects.push(AppEffect::RepaintAfter(after));
        }
        self.sync_generated_session_names();
        self.sync_session_order();
        let phase = crate::diagnostics::latency_start();
        let waiting_to_reattach = self
            .workspace
            .binding
            .reattach
            .is_some_and(|reattach| !reattach.started);
        if !waiting_to_reattach && let Err(error) = self.sync_terminal_panes() {
            if self.workspace.binding.multiplexer.remote.is_some() {
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

        let pending_pty_bytes = self.workspace.binding.terminal.pending_pty_len();
        let (cols, rows) = self.workspace.binding.terminal.grid_size();
        if let Some(trace) = &mut self.stability_trace {
            trace.record(StabilityTraceSample {
                elapsed_ms: trace.started_at.elapsed().as_millis(),
                selected_session: self.workspace.binding.mux.selected_session(),
                cols,
                rows,
                pending_pty_bytes,
                drain_bytes: self.last_drain.bytes,
                drain_elapsed_us: self.last_drain.elapsed_us,
                text_runs: renderer_metrics.text_runs,
                last_error: self.last_error.as_deref(),
            });
        }
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
        let repaint_after = repaint.after.min(CONFIG_HOT_RELOAD_INTERVAL);
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
        self.new_mux_session_dialog = None;
        self.session_picker_dialog = None;
        self.rename_session_dialog = None;
        self.rename_tab_dialog = None;
        self.ditch_session_dialog = None;
        self.keybind_help_dialog = None;
        self.command_palette_dialog = None;
        self.theme_picker_dialog = None;
        self.space_editor_dialog = None;
        self.terminal_find_dialog = None;
        self.terminal_find_return_focus_after_search = false;
        restored_preview
    }

    fn open_new_mux_session_dialog(&mut self) {
        self.close_overlay_dialogs();
        self.new_mux_session_dialog = Some(
            self.active_multiplexer()
                .remote
                .clone()
                .map(|remote| NewMuxSessionDialog::open_remote(remote, self.repaint.clone()))
                .unwrap_or_else(NewMuxSessionDialog::open),
        );
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
        self.space_editor_dialog = Some(
            SpaceEditorDialog::new_space(
                default_space_icon(&existing_icons),
                SpaceMuxOverride::default(),
            )
            .with_profiles(profiles.into_iter()),
        );
        self.input_focus = InputFocus::Picker;
        true
    }

    pub fn open_edit_space_dialog_from_ui(&mut self, space_id: SpaceId) -> bool {
        let backend = self.space_backend_override(space_id);
        let Some((space, backend)) = self
            .space_summaries()
            .into_iter()
            .find(|space| space.id == space_id)
            .zip(backend)
        else {
            return false;
        };
        self.close_overlay_dialogs();
        // Save only this Space's remote override.
        let remote = self
            .space_remote_override(space.id)
            .expect("a listed Space has a remote source");
        let profiles = self
            .config()
            .ssh_profiles
            .iter()
            .map(|(id, profile)| (id.clone(), profile.clone()))
            .collect::<Vec<_>>();
        self.space_editor_dialog = Some(
            SpaceEditorDialog::edit_space(
                space.id,
                space.name,
                space.icon,
                space.color,
                space.tint_sidebar,
                SpaceMuxOverride { backend, remote },
            )
            .with_profiles(profiles.into_iter()),
        );
        self.input_focus = InputFocus::Picker;
        true
    }

    pub fn open_new_session_dialog_from_ui(&mut self) -> bool {
        self.open_new_mux_session_dialog();
        true
    }

    fn open_session_picker_dialog(&mut self) {
        self.close_overlay_dialogs();
        self.session_picker_dialog = Some(SessionPickerDialog::open());
        self.input_focus = InputFocus::Picker;
    }

    pub fn open_session_picker_dialog_from_ui(&mut self) -> bool {
        self.open_session_picker_dialog();
        true
    }

    fn toggle_session_picker_dialog(&mut self) {
        if self.session_picker_dialog.is_some() {
            self.session_picker_dialog = None;
            self.input_focus = InputFocus::Terminal;
        } else {
            self.open_session_picker_dialog();
        }
    }

    fn open_rename_session_dialog(&mut self) {
        let Some(selected) = self
            .workspace
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
        self.rename_session_dialog = Some(RenameSessionDialog::open(session_id, name));
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
        self.rename_tab_dialog = Some(RenameTabDialog::open(session_id, window_id, name));
        self.input_focus = InputFocus::Picker;
        true
    }

    fn selected_window_for_rename(&self) -> Option<(String, String, String)> {
        let selected = self.workspace.binding.mux.selected_session()?;
        let session = self
            .workspace
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == selected || session.name == selected)?;
        let window_id = self
            .workspace
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
        self.ditch_session_dialog = Some(DitchSessionDialog::open(session_id, cwd));
        self.input_focus = InputFocus::Picker;
        true
    }

    fn open_keybind_help_dialog(&mut self) {
        let bindings = self
            .config()
            .input
            .keybinds_for_backend(self.workspace.binding.multiplexer.backend);
        self.close_overlay_dialogs();
        self.keybind_help_dialog = Some(KeybindHelpDialog::open(&bindings));
        self.input_focus = InputFocus::Picker;
    }

    fn open_command_palette_dialog(&mut self) {
        let bindings = self
            .config()
            .input
            .keybinds_for_backend(self.workspace.binding.multiplexer.backend);
        self.close_overlay_dialogs();
        self.command_palette_dialog = Some(CommandPaletteDialog::open(&bindings));
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
        self.theme_picker_dialog = Some(ThemePickerDialog::open(
            &config_path,
            current.as_deref(),
            branch,
        ));
        self.input_focus = InputFocus::Picker;
    }

    fn direct_terminal_input_enabled(&self) -> bool {
        self.input_focus.terminal_owns_input()
            && self.new_mux_session_dialog.is_none()
            && self.session_picker_dialog.is_none()
            && self.rename_session_dialog.is_none()
            && self.rename_tab_dialog.is_none()
            && self.ditch_session_dialog.is_none()
            && self.keybind_help_dialog.is_none()
            && self.command_palette_dialog.is_none()
            && self.theme_picker_dialog.is_none()
            && self.space_editor_dialog.is_none()
            && !self.lua_window_open
            && !self.settings_open
    }

    fn rebuild_profile_bindings(&mut self, config: &BoottyConfig) {
        let repaint = self.repaint.clone();
        let variant = self.active_appearance_variant;
        for binding in self.binding_runtimes_mut() {
            if !matches!(binding.remote_override, SpaceRemoteOverride::Profile(_)) {
                continue;
            }
            *binding = binding_runtime_for_multiplexer(
                config,
                binding.scope,
                binding.label.clone(),
                binding.backend_override,
                binding.remote_override.clone(),
                variant,
                repaint.clone(),
            );
        }
    }

    fn reload_config(&mut self, effects: &mut Vec<AppEffect>) -> bool {
        let previous = self.config().clone();
        let path = previous.config_path.clone();
        let next = match load_config_from_path(&path) {
            Ok(config) => config,
            Err(error) => {
                self.config_state.reject(error.to_string());
                self.last_error = self.config_state.last_error().map(str::to_owned);
                return false;
            }
        };
        let compatibility_warning = (!next.compatibility_warnings.is_empty())
            .then(|| next.compatibility_warnings.join("; "));
        let modifier_remaps = match next.input.modifier_remaps() {
            Ok(remaps) => remaps,
            Err(error) => {
                self.config_state.reject(error.to_string());
                self.last_error = self.config_state.last_error().map(str::to_owned);
                return false;
            }
        };
        let keybinds = next
            .input
            .keybinds_for_backend(self.workspace.binding.multiplexer.backend);
        let app_key_bindings = match AppKeyBindings::from_keybinds(&keybinds) {
            Ok(bindings) => bindings,
            Err(error) => {
                self.config_state.reject(error.to_string());
                self.last_error = self.config_state.last_error().map(str::to_owned);
                return false;
            }
        };
        let sidebar_key_bindings =
            match SidebarKeyBindings::from_keybinds(&next.input.sidebar_keybind) {
                Ok(bindings) => bindings,
                Err(error) => {
                    self.config_state.reject(error.to_string());
                    self.last_error = self.config_state.last_error().map(str::to_owned);
                    return false;
                }
            };

        let previous_colors = previous.colors_for_appearance(self.active_appearance_variant);
        let next_colors = next.colors_for_appearance(self.active_appearance_variant);
        if previous_colors != next_colors
            && let Err(error) =
                self.set_binding_terminal_colors(next_colors.terminal_color_config())
        {
            self.config_state.reject(error.to_string());
            self.last_error = self.config_state.last_error().map(str::to_owned);
            return false;
        }
        if previous.cursor != next.cursor
            && let Err(error) = self.set_binding_cursor_config(next.cursor.terminal_cursor_config())
        {
            self.config_state.reject(error.to_string());
            self.last_error = self.config_state.last_error().map(str::to_owned);
            return false;
        }
        if previous.session.glyph_protocol != next.session.glyph_protocol
            && let Err(error) =
                self.set_binding_feature_config(next.session.terminal_feature_config())
        {
            self.config_state.reject(error.to_string());
            self.last_error = self.config_state.last_error().map(str::to_owned);
            return false;
        }
        if previous.font != next.font {
            effects.push(AppEffect::SetTerminalTextConfig(
                next.font.terminal_text_config(),
            ));
            if previous.font.ui_families() != next.font.ui_families() {
                effects.push(AppEffect::SetUiFonts(next.font.ui_families().to_vec()));
            }
        }
        if previous.window.title != next.window.title {
            effects.push(AppEffect::SetWindowTitle(next.window.title.clone()));
        }
        if previous.diagnostics != next.diagnostics {
            self.stability_trace = StabilityTrace::from_config(&next);
        }

        self.modifier_remaps = modifier_remaps;
        self.macos_option_as_alt = next.input.macos_option_as_alt.into();
        self.app_key_bindings = app_key_bindings;
        self.sidebar_key_bindings = sidebar_key_bindings;
        let active_appearance_variant = self.active_appearance_variant;
        if previous.ssh_profiles != next.ssh_profiles {
            self.rebuild_profile_bindings(&next);
        }
        for binding in self.binding_runtimes_mut() {
            let mut binding_config = next.clone();
            binding_config.multiplexer = binding.multiplexer.clone();
            let session_config = terminal_session_config_with_side_effects(
                &binding_config,
                active_appearance_variant,
                &binding.terminal_side_effect_tx,
            );
            binding.terminal.set_terminal_config(session_config);
        }
        if let Some(owner) = &mut self.workspace.parked_native_terminal {
            let mut owner_config = next.clone();
            owner_config.multiplexer.backend = crate::config::MultiplexerBackendConfig::Native;
            let session_config = terminal_session_config_with_side_effects(
                &owner_config,
                active_appearance_variant,
                &owner.terminal_side_effect_tx,
            );
            owner.terminal.set_terminal_config(session_config);
        }
        self.has_new_session_config_changes = new_session_only_config_changed(&previous, &next)
            || self.has_new_session_config_changes;
        self.config_state.accept(next);
        self.set_mouse_pointer_hidden_while_typing(self.mouse_pointer_hidden_while_typing, effects);
        let config_path = self.config().config_path.clone();
        let binding_id = self
            .workspace
            .binding
            .scope
            .binding_id()
            .persistence_value();
        self.workspace.binding.session_names =
            SessionNameStore::for_binding(&config_path, binding_id);
        self.workspace.binding.pending_generated_names.clear();
        self.workspace.binding.session_order =
            SessionOrderStore::for_binding(&config_path, binding_id);
        self.sync_session_order();
        self.last_error = match (self.has_new_session_config_changes, compatibility_warning) {
            (true, Some(warning)) => Some(format!(
                "config reloaded; session/window settings require a new window or restart; {warning}"
            )),
            (true, None) => Some(
                "config reloaded; session/window settings require a new window or restart"
                    .to_owned(),
            ),
            (false, warning) => warning,
        };
        effects.push(AppEffect::RequestRepaint);
        true
    }

    fn hot_reload_config_if_changed(&mut self, effects: &mut Vec<AppEffect>, now: Instant) {
        if !self.config_hot_reload.changed(now) {
            return;
        }
        let path = self.config().config_path.clone();
        if self.reload_config(effects) {
            self.config_hot_reload.refresh_after_reload(&path);
        }
    }

    fn split_app_actions(
        &mut self,
        events: Vec<egui::Event>,
    ) -> (Vec<egui::Event>, Vec<CommandInvocation>) {
        split_app_actions_for_bindings_with_modifier_sides(
            &mut self.app_key_bindings,
            events,
            self.modifier_sides,
        )
    }

    /// While the command palette is open, find and remove the configure-keybinding
    /// chord (`cmd+shift+,` on macOS, `ctrl+shift+,` elsewhere) from `events` so it
    /// doesn't also trigger whatever global binding shares that chord. Returns
    /// whether one was consumed.
    fn take_configure_keybind_chord(&self, events: &mut Vec<egui::Event>) -> bool {
        if self.command_palette_dialog.is_none() {
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

        match TerminalRenderSource::is_mouse_tracking(self.workspace.binding.terminal.as_mut()) {
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
                TerminalSelectionAction::Begin(event) => TerminalRenderSource::begin_selection(
                    self.workspace.binding.terminal.as_mut(),
                    event,
                ),
                TerminalSelectionAction::Scroll(delta) => {
                    TerminalRenderSource::scroll_viewport_delta(
                        self.workspace.binding.terminal.as_mut(),
                        delta,
                    )
                }
                TerminalSelectionAction::Update(event) => TerminalRenderSource::update_selection(
                    self.workspace.binding.terminal.as_mut(),
                    event,
                ),
                TerminalSelectionAction::End(event) => TerminalRenderSource::end_selection(
                    self.workspace.binding.terminal.as_mut(),
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
        match TerminalRenderSource::copy_mode_active(self.workspace.binding.terminal.as_mut()) {
            Ok(active) => active,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    fn enter_terminal_copy_mode(&mut self, effects: &mut Vec<AppEffect>) {
        match TerminalRenderSource::enter_copy_mode(self.workspace.binding.terminal.as_mut()) {
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
        match TerminalRenderSource::handle_copy_mode_action(
            self.workspace.binding.terminal.as_mut(),
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
                .command_palette_dialog
                .as_ref()
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
        let commands = terminal_input_commands_with_wheel_state(
            snapshot,
            &self.modifier_remaps,
            self.macos_option_as_alt,
            &mut self.wheel_scroll_state,
        );
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
        if self.workspace.binding.multiplexer.remote.is_some() {
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
        if let Err(error) = self.workspace.binding.terminal.write_paste(&text) {
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
            input.mods = self.modifier_remaps.apply(input.mods);
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
            if let Some(invocation) = self.app_key_bindings.invocation_for_input(input) {
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
            let Some(invocation) = self.sidebar_key_bindings.invocation_for_key(key, modifiers)
            else {
                continue;
            };
            self.dispatch_command(invocation, viewport, effects);
        }
        count
    }

    /// Returns a non-blocking sender for producers outside the UI-owner call stack.
    ///
    /// UI code dispatches directly and must not synchronously wait on this channel's response.
    pub fn app_command_sender(&self, caller: Caller) -> BoundAppCommandSender {
        self.app_command_tx.for_caller(caller)
    }

    pub fn command_catalog(&self) -> Arc<CommandCatalog> {
        Arc::clone(&self.command_catalog)
    }

    fn drain_app_commands(&mut self, viewport: ViewportSnapshot, effects: &mut Vec<AppEffect>) {
        self.drain_pending_app_commands(Instant::now());
        let mut drained = 0;
        for _ in 0..32 {
            let request = match self.app_command_rx.try_recv() {
                Ok(request) => request,
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
            };
            drained += 1;
            let now = Instant::now();
            let dispatch = if request.cancellation.is_cancelled() {
                CommandDispatch::Complete(CommandOutcome::cancelled())
            } else if now >= request.deadline {
                request.cancellation.cancel();
                CommandDispatch::Complete(CommandOutcome::deadline_exceeded())
            } else {
                self.dispatch_command_with_execution(
                    request.invocation,
                    viewport,
                    effects,
                    Some((request.deadline, request.cancellation.clone())),
                )
            };
            match dispatch {
                CommandDispatch::Complete(outcome) => {
                    let _ = request.response.send(outcome);
                }
                CommandDispatch::Pending(result) => {
                    self.pending_app_commands.push(PendingAppCommand {
                        deadline: request.deadline,
                        cancellation: request.cancellation,
                        response: request.response,
                        result,
                    });
                }
            }
        }
        if drained == 32 {
            effects.push(AppEffect::RequestRepaint);
        }
    }

    fn drain_pending_app_commands(&mut self, now: Instant) {
        for pending in std::mem::take(&mut self.pending_app_commands) {
            let outcome = if pending.cancellation.is_cancelled() {
                CommandOutcome::cancelled()
            } else if now >= pending.deadline && pending.cancellation.cancel() {
                CommandOutcome::deadline_exceeded()
            } else {
                match &pending.result {
                    PendingCommandResult::Mux { command, result } => match result.try_recv() {
                        Ok(result) => self.command_outcome_for_mux_result(command, result),
                        Err(mpsc::TryRecvError::Empty) => {
                            self.pending_app_commands.push(pending);
                            continue;
                        }
                        Err(mpsc::TryRecvError::Disconnected) => CommandOutcome::Failed {
                            code: "backend_worker_stopped".to_owned(),
                            message: "mux command worker stopped".to_owned(),
                        },
                    },
                    PendingCommandResult::Outcome(result) => match result.try_recv() {
                        Ok(outcome) => outcome,
                        Err(mpsc::TryRecvError::Empty) => {
                            self.pending_app_commands.push(pending);
                            continue;
                        }
                        Err(mpsc::TryRecvError::Disconnected) => CommandOutcome::Failed {
                            code: "command_worker_stopped".to_owned(),
                            message: "command worker stopped".to_owned(),
                        },
                    },
                }
            };
            let _ = pending.response.send(outcome);
        }
    }

    fn dispatch_command(
        &mut self,
        invocation: CommandInvocation,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) -> CommandOutcome {
        match self.dispatch_command_with_execution(invocation, viewport, effects, None) {
            CommandDispatch::Complete(outcome) => outcome,
            CommandDispatch::Pending(_) => {
                unreachable!("UI-owned command dispatch cannot return a pending backend result")
            }
        }
    }

    fn dispatch_command_with_execution(
        &mut self,
        invocation: CommandInvocation,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let extension = match self.command_catalog.resolve_extension(invocation.clone()) {
            Ok(extension) => extension,
            Err(outcome) => return CommandDispatch::Complete(outcome),
        };
        if let Some(mut resolved) = extension {
            let target = match self.resolve_command_target(
                &resolved.invocation.command,
                resolved.descriptor.target,
                resolved.invocation.target.as_ref(),
            ) {
                Ok(target) => target,
                Err(outcome) => return CommandDispatch::Complete(outcome),
            };
            resolved.invocation.target = target;
            if resolved.descriptor.mutation == MutationClass::Destructive
                && resolved.invocation.confirmation.as_ref()
                    != Some(&resolved.invocation.confirmation())
            {
                return CommandDispatch::Complete(CommandOutcome::ConfirmationRequired {
                    confirmation: Box::new(resolved.invocation.confirmation()),
                });
            }
            let (deadline, cancellation) = execution.unwrap_or_else(|| {
                (
                    Instant::now() + Duration::from_secs(10),
                    CommandCancellation::new(),
                )
            });
            let result = (resolved.handler)(resolved.invocation, deadline, cancellation);
            return CommandDispatch::Pending(PendingCommandResult::Outcome(result));
        }
        let mut resolved = match self.command_catalog.resolve(invocation) {
            Ok(resolved) => resolved,
            Err(outcome) => {
                self.last_error = command_outcome_message(&outcome);
                return CommandDispatch::Complete(outcome);
            }
        };
        let target = match self.resolve_command_target(
            &resolved.invocation.command,
            resolved.descriptor.target,
            resolved.invocation.target.as_ref(),
        ) {
            Ok(target) => target,
            Err(outcome) => {
                self.last_error = command_outcome_message(&outcome);
                return CommandDispatch::Complete(outcome);
            }
        };
        resolved.invocation.target = target;
        if let Some(outcome) = self.preflight_command(&resolved.executor) {
            self.last_error = command_outcome_message(&outcome);
            return CommandDispatch::Complete(outcome);
        }
        if resolved.descriptor.mutation == MutationClass::Destructive
            && matches!(
                resolved.invocation.caller,
                Caller::Cli | Caller::Socket | Caller::Luau
            )
            && resolved.invocation.confirmation.as_ref()
                != Some(&resolved.invocation.confirmation())
        {
            return CommandDispatch::Complete(CommandOutcome::ConfirmationRequired {
                confirmation: Box::new(resolved.invocation.confirmation()),
            });
        }
        let caller = resolved.invocation.caller;
        let target = resolved.invocation.target;
        self.dispatch_resolved_command(
            resolved.executor,
            target.as_ref(),
            caller,
            viewport,
            effects,
            execution,
        )
    }

    fn preflight_command(&self, executor: &CoreCommandExecutor) -> Option<CommandOutcome> {
        let CoreCommandExecutor::Keybind(KeybindAction::Mux(action)) = executor else {
            return None;
        };
        let operation = self.mux_operation_for_action(*action)?;
        if let Some(message) = self.workspace.binding.mux.unavailable_reason() {
            return Some(CommandOutcome::Unavailable {
                message: message.to_owned(),
            });
        }
        command_outcome_for_binding_operation(
            self.workspace
                .binding
                .mux
                .operation_outcome(&self.workspace.binding.multiplexer, operation),
        )
    }

    fn resolve_command_target(
        &self,
        command: &str,
        expected: Option<ResourceKind>,
        supplied: Option<&CommandTarget>,
    ) -> Result<Option<CommandTarget>, CommandOutcome> {
        let Some(expected) = expected else {
            return if supplied.is_none() {
                Ok(None)
            } else {
                Err(CommandOutcome::Denied {
                    message: "command does not accept a target".to_owned(),
                })
            };
        };
        if supplied.is_some_and(|target| target.kind != expected) {
            return Err(CommandOutcome::Denied {
                message: format!("command requires a {expected:?} target"),
            });
        }
        let Some(current) = self.current_command_target_for(command, expected) else {
            return Err(CommandOutcome::Unavailable {
                message: format!("no current {expected:?} target is available"),
            });
        };
        if supplied.is_some_and(|target| target != &current) {
            return Err(CommandOutcome::StaleTarget {
                message: format!("the {expected:?} target is stale"),
            });
        }
        Ok(Some(current))
    }

    fn current_command_target_for(
        &self,
        command: &str,
        kind: ResourceKind,
    ) -> Option<CommandTarget> {
        let target = self.current_command_target(kind);
        if target.is_some() || command != "new_tab" || kind != ResourceKind::Session {
            return target;
        }
        self.current_command_target(ResourceKind::Binding)
            .map(|binding| CommandTarget {
                kind,
                handle: serde_json::to_string(&("no-session", &binding.handle))
                    .expect("serialize empty session target"),
                generation: binding.generation,
            })
    }

    fn current_command_target(&self, kind: ResourceKind) -> Option<CommandTarget> {
        let process = self.command_instance_handle.clone();
        let window = &self.window_state_key;
        let scope = self.workspace.binding.scope;
        let space = scope.space_id().persistence_value().to_string();
        let binding = scope.binding_id().persistence_value().to_string();
        let binding_generation = self.workspace.binding.mux.binding_generation();
        let binding_handle = serde_json::to_string(&(
            &process,
            window,
            self.command_window_generation,
            &space,
            &binding,
            binding_generation,
        ))
        .expect("serialize target");
        let (session, mux_window, pane) = self.selected_mux_resource_path();
        let target = match kind {
            ResourceKind::Instance => CommandTarget {
                kind,
                handle: process,
                generation: self.command_instance_generation,
            },
            ResourceKind::ApplicationWindow => CommandTarget {
                kind,
                handle: serde_json::to_string(&[&process, window]).expect("serialize target"),
                generation: self.command_window_generation,
            },
            ResourceKind::Binding => CommandTarget {
                kind,
                handle: binding_handle,
                generation: binding_generation,
            },
            ResourceKind::Session => {
                let session = session?;
                CommandTarget {
                    kind,
                    handle: serde_json::to_string(&[&binding_handle, &session])
                        .expect("serialize target"),
                    generation: self.workspace.binding.mux.session_generation(&session)?,
                }
            }
            ResourceKind::MuxWindow => {
                let (session, mux_window) = (session?, mux_window?);
                CommandTarget {
                    kind,
                    handle: serde_json::to_string(&[&binding_handle, &session, &mux_window])
                        .expect("serialize target"),
                    generation: self
                        .workspace
                        .binding
                        .mux
                        .window_generation(&session, &mux_window)?,
                }
            }
            ResourceKind::Pane => {
                let (session, mux_window, pane) = (session?, mux_window?, pane?);
                CommandTarget {
                    kind,
                    handle: serde_json::to_string(&[&binding_handle, &session, &mux_window, &pane])
                        .expect("serialize target"),
                    generation: self.workspace.binding.mux.pane_generation(
                        &session,
                        &mux_window,
                        &pane,
                    )?,
                }
            }
            ResourceKind::Terminal => {
                let (handle, generation) = match (session, mux_window, pane) {
                    (Some(session), Some(mux_window), Some(pane)) => (
                        serde_json::to_string(&(&binding_handle, &session, &mux_window, &pane))
                            .expect("serialize target"),
                        self.workspace.binding.mux.terminal_generation(
                            &session,
                            &mux_window,
                            &pane,
                        )?,
                    ),
                    (Some(session), _, _) => (
                        serde_json::to_string(&(&binding_handle, &session))
                            .expect("serialize target"),
                        self.workspace.binding.mux.session_generation(&session)?,
                    ),
                    (None, _, _) => (
                        serde_json::to_string(&(&binding_handle, "active_terminal"))
                            .expect("serialize target"),
                        binding_generation,
                    ),
                };
                CommandTarget {
                    kind,
                    handle,
                    generation,
                }
            }
        };
        Some(target)
    }

    fn selected_mux_resource_path(&self) -> (Option<String>, Option<String>, Option<String>) {
        let Some(anchor) = self.workspace.binding.mux.selected_session_anchor() else {
            return (None, None, None);
        };
        let session = anchor.session_id.clone();
        let mux_window = self
            .workspace
            .binding
            .mux
            .selected_window()
            .map(str::to_owned)
            .or_else(|| {
                self.workspace
                    .binding
                    .mux
                    .sessions()
                    .iter()
                    .find(|candidate| candidate.id == session)
                    .and_then(|candidate| candidate.active_window_id.clone())
            });
        let pane = if self.uses_native_terminal_layout() {
            self.workspace
                .binding
                .terminal
                .focused_pane_id()
                .map(|pane_id| {
                    decode_scoped_pane_id(pane_id).map_or_else(
                        || pane_id.to_owned(),
                        |(scope, pane_id)| {
                            debug_assert_eq!(scope, self.workspace.binding.scope);
                            pane_id
                        },
                    )
                })
        } else {
            anchor.pane_id.clone()
        };
        (Some(session), mux_window, pane)
    }

    fn read_active_terminal(&mut self) -> CommandOutcome {
        match self.workspace.binding.terminal.extract_frame() {
            Ok(frame) => CommandOutcome::Success {
                value: serde_json::json!({
                    "cols": frame.cols,
                    "rows": frame.rows,
                    "text": frame.text_rows().join("\n"),
                    "cursor": frame.cursor.map(|cursor| serde_json::json!({
                        "x": cursor.x,
                        "y": cursor.y,
                    })),
                }),
                warnings: Vec::new(),
            },
            Err(error) => CommandOutcome::Failed {
                code: "terminal_read_failed".to_owned(),
                message: error.to_string(),
            },
        }
    }

    fn dispatch_resolved_command(
        &mut self,
        executor: CoreCommandExecutor,
        target: Option<&CommandTarget>,
        caller: Caller,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        match executor {
            CoreCommandExecutor::Keybind(KeybindAction::App(AppAction::ReloadConfig)) => {
                if let Err(outcome) = Self::begin_synchronous_command(execution) {
                    return CommandDispatch::Complete(outcome);
                }
                let reloaded = self.reload_config(effects);
                let outcome = if reloaded {
                    let path = self.config().config_path.clone();
                    self.config_hot_reload.refresh_after_reload(&path);
                    self.last_error
                        .clone()
                        .map_or_else(CommandOutcome::success, |warning| {
                            CommandOutcome::success_with_warning("configuration_warning", warning)
                        })
                } else {
                    CommandOutcome::Failed {
                        code: "execution_failed".to_owned(),
                        message: self
                            .last_error
                            .clone()
                            .unwrap_or_else(|| "configuration reload failed".to_owned()),
                    }
                };
                CommandDispatch::Complete(outcome)
            }
            CoreCommandExecutor::Keybind(action) => self.dispatch_resolved_keybind_command(
                action, target, caller, viewport, effects, execution,
            ),
            CoreCommandExecutor::Sidebar(action) => {
                if let Err(outcome) = Self::begin_synchronous_command(execution) {
                    return CommandDispatch::Complete(outcome);
                }
                self.apply_sidebar_action(action);
                CommandDispatch::Complete(CommandOutcome::success())
            }
            CoreCommandExecutor::ReadTerminal => {
                if let Err(outcome) = Self::begin_synchronous_command(execution) {
                    return CommandDispatch::Complete(outcome);
                }
                CommandDispatch::Complete(self.read_active_terminal())
            }
        }
    }

    fn begin_synchronous_command(
        execution: Option<(Instant, CommandCancellation)>,
    ) -> Result<(), CommandOutcome> {
        let Some((deadline, cancellation)) = execution else {
            return Ok(());
        };
        if Instant::now() >= deadline && cancellation.cancel() {
            return Err(CommandOutcome::deadline_exceeded());
        }
        if !cancellation.try_start() {
            return Err(CommandOutcome::cancelled());
        }
        Ok(())
    }

    fn dispatch_resolved_keybind_command(
        &mut self,
        action: KeybindAction,
        target: Option<&CommandTarget>,
        caller: Caller,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let mut return_native_mux_focus = false;
        if matches!(caller, Caller::Cli | Caller::Socket | Caller::Luau)
            && let KeybindAction::Mux(mux_action) = action
        {
            if let Some(operation) = self.mux_operation_for_action(mux_action)
                && let Some(outcome) = command_outcome_for_binding_operation(
                    self.workspace
                        .binding
                        .mux
                        .operation_outcome(&self.workspace.binding.multiplexer, operation),
                )
            {
                self.last_error = command_outcome_message(&outcome);
                return CommandDispatch::Complete(outcome);
            }
            let native_local_action = selected_backend(self.active_multiplexer())
                == MultiplexerBackendConfig::Native
                && Self::native_mux_action_uses_local_layout(mux_action);
            if native_local_action {
                return_native_mux_focus = true;
            } else if let Some(command) = self.mux_command_for_command(mux_action, target) {
                let config = self.active_multiplexer().clone();
                let (deadline, cancellation) = execution.unwrap_or_else(|| {
                    (
                        Instant::now() + Duration::from_secs(10),
                        CommandCancellation::new(),
                    )
                });
                let result = self.workspace.binding.mux.execute_command_authoritatively(
                    &self.repaint,
                    &config,
                    command.clone(),
                    deadline,
                    cancellation,
                );
                return CommandDispatch::Pending(PendingCommandResult::Mux { command, result });
            }
        }
        if let Err(outcome) = Self::begin_synchronous_command(execution) {
            return CommandDispatch::Complete(outcome);
        }
        let previous_error = self.last_error.take();
        self.apply_resolved_keybind_action(action, target, viewport, effects);
        let outcome = match self.last_error.clone() {
            Some(message) => CommandOutcome::Failed {
                code: "execution_failed".to_owned(),
                message,
            },
            None => {
                self.last_error = previous_error;
                if return_native_mux_focus {
                    CommandOutcome::Success {
                        value: self.current_mux_focus_value(),
                        warnings: Vec::new(),
                    }
                } else {
                    CommandOutcome::success()
                }
            }
        };
        CommandDispatch::Complete(outcome)
    }
    fn native_mux_action_uses_local_layout(action: MuxKeyAction) -> bool {
        matches!(
            action,
            MuxKeyAction::NextSession
                | MuxKeyAction::PreviousSession
                | MuxKeyAction::LastSession
                | MuxKeyAction::SelectSession(_)
                | MuxKeyAction::MoveSession(_)
                | MuxKeyAction::SplitPane(_)
                | MuxKeyAction::SelectPane(_)
                | MuxKeyAction::NextPane
                | MuxKeyAction::PreviousPane
                | MuxKeyAction::KillPane
                | MuxKeyAction::ClosePane
        )
    }

    fn current_mux_focus_value(&self) -> serde_json::Value {
        let focused = self
            .current_command_target(ResourceKind::Pane)
            .or_else(|| self.current_command_target(ResourceKind::MuxWindow))
            .or_else(|| self.current_command_target(ResourceKind::Session));
        focused.map_or_else(
            || serde_json::json!({}),
            |focused| serde_json::json!({ "focused": focused }),
        )
    }

    fn mux_command_for_command(
        &mut self,
        action: MuxKeyAction,
        target: Option<&CommandTarget>,
    ) -> Option<MuxCommand> {
        if matches!(
            action,
            MuxKeyAction::NextSession
                | MuxKeyAction::PreviousSession
                | MuxKeyAction::LastSession
                | MuxKeyAction::SelectSession(_)
                | MuxKeyAction::MoveSession(_)
        ) {
            return None;
        }

        let target = target.expect("mux command target was resolved");
        let path = serde_json::from_str::<Vec<String>>(&target.handle)
            .expect("resolved mux command target has a resource path");
        if action == MuxKeyAction::NewTab && path.first().is_some_and(|part| part == "no-session") {
            let remote = self.active_multiplexer().remote.is_some();
            let cwd = new_mux_session_request_with_name(self.config(), "").cwd;
            let cwd = Self::session_cwd(&cwd, remote);
            let display_name = Self::suggested_session_name(&cwd, remote);
            let session_id = crate::strings::unique_session_name(
                &display_name,
                self.taken_session_names(None).iter().map(String::as_str),
            );
            return Some(MuxCommand::CreateProjectSession { session_id, cwd });
        }

        let session_id = path
            .get(1)
            .expect("resolved mux target includes a session")
            .clone();
        let window_id = (target.kind == ResourceKind::MuxWindow).then(|| {
            path.get(2)
                .expect("mux window target includes a window")
                .clone()
        });
        let pane_id = (target.kind == ResourceKind::Pane)
            .then(|| path.get(3).expect("pane target includes a pane").clone());
        let cwd = terminal_cwd_for_mux_command(
            self.workspace
                .binding
                .terminal
                .current_working_directory()
                .ok()
                .flatten(),
            self.workspace
                .binding
                .mux
                .selected_session_anchor()
                .and_then(|anchor| anchor.cwd.clone()),
        );
        let command = match action {
            MuxKeyAction::NewTab => MuxCommand::NewWindow { session_id, cwd },
            MuxKeyAction::NextTab => MuxCommand::ActivateNextWindow { session_id },
            MuxKeyAction::PreviousTab => MuxCommand::ActivatePreviousWindow { session_id },
            MuxKeyAction::LastTab => MuxCommand::ActivateLastWindow { session_id },
            MuxKeyAction::SelectTab(index) => MuxCommand::ActivateWindowIndex { session_id, index },
            MuxKeyAction::MoveTab(delta) => MuxCommand::MoveWindow {
                session_id,
                window_id: self
                    .workspace
                    .binding
                    .mux
                    .selected_window()
                    .map(str::to_owned),
                delta,
            },
            MuxKeyAction::SplitPane(direction) => MuxCommand::SplitPane {
                session_id,
                pane_id,
                direction: mux_split_direction(direction),
            },
            MuxKeyAction::SelectPane(direction) => MuxCommand::SelectPane {
                session_id,
                window_id,
                direction,
            },
            MuxKeyAction::NextPane => MuxCommand::SelectNextPane {
                session_id,
                window_id,
            },
            MuxKeyAction::PreviousPane => MuxCommand::SelectPreviousPane {
                session_id,
                window_id,
            },
            MuxKeyAction::KillPane => MuxCommand::KillPane {
                session_id,
                pane_id,
            },
            MuxKeyAction::ClosePane => MuxCommand::ClosePane {
                session_id,
                pane_id,
            },
            MuxKeyAction::TogglePaneZoom => MuxCommand::TogglePaneZoom {
                session_id,
                pane_id,
            },
            MuxKeyAction::NextSession
            | MuxKeyAction::PreviousSession
            | MuxKeyAction::LastSession
            | MuxKeyAction::SelectSession(_)
            | MuxKeyAction::MoveSession(_) => unreachable!("handled before command construction"),
        };
        Some(command)
    }

    fn command_outcome_for_mux_result(
        &mut self,
        command: &MuxCommand,
        result: MuxCommandResult,
    ) -> CommandOutcome {
        let config = self.active_multiplexer().clone();
        match self
            .workspace
            .binding
            .mux
            .complete_authoritative_command(result, &config)
        {
            Ok(completion) => {
                self.sync_native_layout_terminal_now();
                CommandOutcome::Success {
                    value: self.mux_command_completion_value(command, &completion),
                    warnings: Vec::new(),
                }
            }
            Err(error) => {
                let message = error.to_string();
                let outcome = match error {
                    MuxCommandError::Cancelled => CommandOutcome::Failed {
                        code: "cancelled".to_owned(),
                        message,
                    },
                    MuxCommandError::DeadlineExceeded => CommandOutcome::Failed {
                        code: "deadline_exceeded".to_owned(),
                        message,
                    },
                    MuxCommandError::Unsupported => CommandOutcome::Unsupported { message },
                    MuxCommandError::Unavailable => CommandOutcome::Unavailable { message },
                    MuxCommandError::Stale => CommandOutcome::StaleTarget { message },
                    MuxCommandError::Failed(_) => CommandOutcome::Failed {
                        code: "execution_failed".to_owned(),
                        message,
                    },
                };
                self.last_error = command_outcome_message(&outcome);
                outcome
            }
        }
    }

    fn mux_command_completion_value(
        &self,
        command: &MuxCommand,
        completion: &MuxCommandCompletion,
    ) -> serde_json::Value {
        let mut value = serde_json::Map::new();
        if let Some(session_id) = match command {
            MuxCommand::CreateProjectSession { session_id, .. }
            | MuxCommand::CreateWorktreeSession { session_id, .. } => Some(session_id.as_str()),
            _ => None,
        } {
            value.insert(
                "created".to_owned(),
                serde_json::to_value(self.mux_resource_target(
                    ResourceKind::Session,
                    session_id,
                    None,
                ))
                .expect("serialize command target"),
            );
        }
        if let (Some(session_id), Some(window_id)) = (
            completion.selected_session.as_deref(),
            completion.selected_window.as_deref(),
        ) {
            value.insert(
                "focused".to_owned(),
                serde_json::to_value(self.mux_resource_target(
                    ResourceKind::MuxWindow,
                    session_id,
                    Some(window_id),
                ))
                .expect("serialize command target"),
            );
        }
        if !value.contains_key("focused")
            && let Some(session_id) = completion.selected_session.as_deref()
        {
            value.insert(
                "focused".to_owned(),
                serde_json::to_value(self.mux_resource_target(
                    ResourceKind::Session,
                    session_id,
                    None,
                ))
                .expect("serialize command target"),
            );
        }
        serde_json::Value::Object(value)
    }

    fn mux_resource_target(
        &self,
        kind: ResourceKind,
        session_id: &str,
        window_id: Option<&str>,
    ) -> CommandTarget {
        let binding = self
            .current_command_target(ResourceKind::Binding)
            .expect("mux completion requires a binding target")
            .handle;
        match kind {
            ResourceKind::Session => CommandTarget {
                kind,
                handle: serde_json::to_string(&[&binding, session_id]).expect("serialize target"),
                generation: self
                    .workspace
                    .binding
                    .mux
                    .session_generation(session_id)
                    .unwrap_or(1),
            },
            ResourceKind::MuxWindow => {
                let window_id = window_id.expect("mux window target requires a window id");
                CommandTarget {
                    kind,
                    handle: serde_json::to_string(&[&binding, session_id, window_id])
                        .expect("serialize target"),
                    generation: self
                        .workspace
                        .binding
                        .mux
                        .window_generation(session_id, window_id)
                        .unwrap_or(1),
                }
            }
            _ => unreachable!("mux completion only returns session and window targets"),
        }
    }
    fn apply_resolved_keybind_action(
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

    fn apply_sidebar_action(&mut self, action: SidebarAction) -> bool {
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
                if self.reload_config(effects) {
                    let path = self.config().config_path.clone();
                    self.config_hot_reload.refresh_after_reload(&path);
                }
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
                self.open_edit_space_dialog_from_ui(self.workspace.active_space_id);
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
                if !self.close_space_from_ui(self.workspace.active_space_id) {
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
                    self.config_state.current_mut().chrome.sidebar = true;
                    self.input_focus = InputFocus::Sidebar;
                    self.sidebar_hovered_session = self
                        .workspace
                        .binding
                        .mux
                        .selected_session()
                        .and_then(|selected| self.session_target_matching(selected))
                        .or_else(|| self.session_navigation_targets().into_iter().next());
                }
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::ToggleSidebarVisibility) => {
                let chrome = &mut self.config_state.current_mut().chrome;
                chrome.sidebar = !chrome.sidebar;
                if !chrome.sidebar {
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
                if let Err(error) = self.workspace.binding.terminal.write_input(&bytes) {
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
                    if let Err(error) = self.workspace.binding.terminal.write_paste(&text) {
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
        let mut selection = |format| self.workspace.binding.terminal.format_selection(format);
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
        let Some(remote) = self.workspace.binding.multiplexer.remote.clone() else {
            self.close_active_pane();
            return;
        };
        if self
            .workspace
            .binding
            .reattach
            .is_some_and(|reattach| !reattach.started)
        {
            return;
        }
        let attached_for = self
            .workspace
            .binding
            .remote_attach_started
            .map(|started| now.saturating_duration_since(started));
        let reattach =
            RemoteReattach::after_failure(self.workspace.binding.reattach, attached_for, now);
        let error = format!(
            "lost the connection to {}; reconnecting (attempt {})",
            remote.host, reattach.attempts
        );
        self.last_error = Some(error.clone());
        self.workspace
            .binding
            .mux
            .set_availability_error(Some(error));
        self.workspace.binding.reattach = Some(reattach);
    }

    fn handle_attach_start_failure(&mut self, now: Instant, detail: &str) {
        let Some(remote) = self.workspace.binding.multiplexer.remote.clone() else {
            return;
        };
        let reattach = RemoteReattach::after_failure(self.workspace.binding.reattach, None, now);
        let error = format!(
            "could not connect to {}: {detail}; reconnecting (attempt {})",
            remote.host, reattach.attempts
        );
        self.last_error = Some(error.clone());
        self.workspace
            .binding
            .mux
            .set_availability_error(Some(error));
        self.workspace.binding.reattach = Some(reattach);
    }

    fn resolve_remote_attach_exit_after_refresh(&mut self, refresh_completed: bool) {
        if self
            .workspace
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
            .binding
            .remote_attach_started
            .is_some_and(|started| {
                now.saturating_duration_since(started) >= RemoteReattach::STABLE_AFTER
            });
        if established
            && self
                .workspace
                .binding
                .reattach
                .is_some_and(|reattach| reattach.started)
        {
            self.workspace.binding.reattach = None;
            self.workspace.binding.mux.set_availability_error(None);
        }
    }

    /// Drop the dead attach client once its backoff has passed. Clearing the pane's target is what
    /// asks for a new one: this frame's pane sync starts a fresh client for the same session.
    fn start_due_reattach(&mut self, now: Instant, effects: &mut Vec<AppEffect>) {
        let Some(mut reattach) = self.workspace.binding.reattach else {
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
        self.workspace.binding.reattach = Some(reattach);
        self.workspace.binding.remote_attach_started = Some(now);
        self.workspace.binding.terminal.discard_active_pane();
    }

    pub fn reconnect_space_from_ui(&mut self, space_id: SpaceId) -> bool {
        let now = Instant::now();
        if space_id == self.workspace.active_space_id {
            let mut restarted = Self::restart_remote_binding(&mut self.workspace.binding, now);
            for binding in &mut self.workspace.inactive_bindings {
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
        self.workspace.binding.reattach.is_some()
            || self
                .workspace
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
        if self.workspace.binding.reattach.is_some() {
            Self::restart_remote_binding(&mut self.workspace.binding, now);
        }
        for binding in &mut self.workspace.inactive_bindings {
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
            .binding
            .mux
            .selected_session()
            .unwrap_or("local")
            .to_owned();
        let mux_config = self.active_multiplexer().clone();
        self.workspace.binding.mux.execute_command(
            &self.repaint,
            &mux_config,
            MuxCommand::ClosePane {
                session_id,
                pane_id: target_pane_id.map(str::to_owned),
            },
        );
        self.workspace.binding.terminal.discard_active_pane();
    }

    /// Close a specific native pane: remove it from the backend window, kill its PTY, collapse the
    /// split layout, and re-activate the surviving focused pane this frame so it doesn't flash idle.
    fn close_pane(&mut self, pane_id: &str) {
        let session_id = self
            .workspace
            .binding
            .mux
            .selected_session()
            .unwrap_or("local")
            .to_owned();
        let mux_config = self.active_multiplexer().clone();
        self.workspace.binding.mux.execute_command(
            &self.repaint,
            &mux_config,
            MuxCommand::ClosePane {
                session_id,
                pane_id: Some(pane_id.to_owned()),
            },
        );
        self.workspace.binding.terminal.discard_pane(pane_id);
        let key = self.current_window_key();
        if let Some(layout) = self.workspace.binding.pane_layouts.get_mut(&key) {
            layout.remove(pane_id);
        }
        let _ = self.sync_terminal_panes();
    }

    fn mux_operation_for_action(&self, action: MuxKeyAction) -> Option<BindingOperation> {
        match action {
            MuxKeyAction::NewTab if self.workspace.binding.mux.selected_session().is_none() => {
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
            && self.workspace.binding.mux.selected_session().is_none()
        {
            let cwd = new_mux_session_request_with_name(self.config(), "").cwd;
            self.create_project_session_for_cwd(cwd);
            self.sync_native_layout_terminal_now();
            return;
        }
        let selected_session = self
            .workspace
            .binding
            .mux
            .selected_session()
            .unwrap_or("local")
            .to_owned();
        let selected_cwd = terminal_cwd_for_mux_command(
            self.workspace
                .binding
                .terminal
                .current_working_directory()
                .ok()
                .flatten(),
            self.workspace
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
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == value || session.name == value)
            .map(|session| {
                ScopedSessionTarget::new(self.workspace.binding.scope, session.id.clone())
            })
    }

    fn apply_session_navigation_action(&mut self, action: MuxKeyAction) -> bool {
        let target = match action {
            MuxKeyAction::SelectSession(index) => self
                .workspace
                .binding
                .mux
                .sessions()
                .get(index.saturating_sub(1) as usize)
                .map(|session| session.id.clone()),
            MuxKeyAction::NextSession => self.relative_session(1),
            MuxKeyAction::PreviousSession => self.relative_session(-1),
            MuxKeyAction::LastSession => self
                .workspace
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
            self.workspace.binding.mux.activate_session(&target);
            self.persist_rmux_restore_state();
            self.sync_native_layout_terminal_now();
        }
        true
    }

    fn relative_session(&self, delta: isize) -> Option<String> {
        let sessions = self.workspace.binding.mux.sessions();
        if sessions.is_empty() {
            return None;
        }
        let selected = self.workspace.binding.mux.selected_session();
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
                .binding
                .terminal
                .focused_render_source(&pane_id)
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
            .binding
            .terminal
            .search_viewport(query, direction)?;
        let frame = self.workspace.binding.terminal.extract_frame()?;
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
        match TerminalRenderSource::handle_copy_mode_action(
            self.workspace.binding.terminal.as_mut(),
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
                -(self.workspace.binding.terminal.grid_size().1 as isize)
            }
            TerminalScrollAction::PageDown => {
                self.workspace.binding.terminal.grid_size().1 as isize
            }
            TerminalScrollAction::Lines(lines) => isize::from(lines),
        };
        if let Err(error) = self.workspace.binding.terminal.scroll_viewport_delta(delta) {
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
                if let Err(error) = self.workspace.binding.terminal.write_input(text.as_bytes()) {
                    self.last_error = Some(error.to_string());
                } else {
                    self.hide_mouse_pointer_for_terminal_typing(effects);
                }
            }
            TerminalInputCommand::Paste(text) => {
                if let Err(error) = self.workspace.binding.terminal.write_paste(&text) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalInputCommand::Focus(focused) => {
                if let Err(error) = self.workspace.binding.terminal.encode_focus(focused) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalInputCommand::Key(input) => {
                if let Err(error) = self.workspace.binding.terminal.encode_key(input) {
                    self.last_error = Some(error.to_string());
                } else {
                    self.hide_mouse_pointer_for_terminal_typing(effects);
                }
            }
            TerminalInputCommand::Mouse(input) => {
                if let Err(error) = self.workspace.binding.terminal.encode_mouse(input) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalInputCommand::MouseWheel {
                input,
                scroll_delta,
            } => {
                if let Err(error) = self
                    .workspace
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
        self.config_state.current_mut().font.size = next_size;
        let text_config = self.config().font.terminal_text_config();
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
