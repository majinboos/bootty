use std::{
    collections::{HashMap, HashSet},
    hash::Hasher,
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
#[cfg(test)]
use copy_mode::{CopyModeSearchRepeat, copy_mode_action_for_char, copy_mode_action_for_egui_key};
#[cfg(debug_assertions)]
use diagnostic_actions::{DiagnosticAction, DiagnosticActionDriver, DiagnosticRecord};
use recorded_chord::normalize_recorded_chord;
use selection::{TerminalSelectionAction, TerminalSelectionRouteContext, TerminalSelectionRouter};
#[cfg(test)]
use selection::{selection_drag_scroll_delta, terminal_selection_event_clamped};

use crate::{
    app_actions::{
        AppAction, AppKeyBindings, FontSizeAction, KeybindAction, MuxKeyAction, SidebarAction,
        SidebarKeyBindings, TerminalFindAction, TerminalScrollAction,
        builtin_app_action_for_direct_key, keybind_action_for_name,
        split_app_actions_for_bindings_with_modifier_sides,
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
    layout::{Direction, Divider, PaneLayout, SplitDirection},
    modifier_remap::ModifierRemapSet,
    mux::{
        RepaintHandle,
        command::{MuxCommand, MuxSplitDirection},
        config::selected_backend,
        controller::{
            BindingId, BindingMuxController, MUX_SESSION_REFRESH_INTERVAL, MuxController, MuxScope,
            SpaceId,
        },
        snapshot::{MuxPaneAnchor, MuxSession, MuxWindow, MuxWindowProgress},
        terminal::{ActiveTerminal, TerminalRuntime},
    },
    platform::{
        apply_macos_non_native_fullscreen_presentation, macos_handles_non_native_fullscreen_frame,
        read_clipboard_text, restore_macos_presentation, show_desktop_notification,
        write_clipboard_text,
    },
    renderer::{RendererMetrics, TerminalRenderSource, TerminalWidget},
    scheduler::{RepaintScheduler, RepaintSignal},
    session_names::SessionNameStore,
    session_order::SessionOrderStore,
    terminal::{DrainStats, MouseButton, TerminalSearchDirection, TerminalSessionConfig},
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
        terminal_find::{TerminalFindDialog, TerminalFindEvent, TerminalFindResult},
        theme_picker::{ThemePickerDialog, ThemePickerEvent},
    },
    workspace::WorkspaceStore,
};
use bootty_terminal::terminal_engine::{
    TerminalColorConfig, TerminalCopyModeAction, TerminalCursorConfig, TerminalFeatureConfig,
    TerminalSelectionFormat, TerminalSideEffect, TerminalSideEffectEvent,
    encode_iterm2_report_cell_size, encode_iterm2_report_variable, encode_osc52_response,
};

#[cfg(test)]
use crate::terminal::{KeyInput, TerminalKey};
#[cfg(test)]
use bootty_terminal::terminal_engine::TerminalCopyModeMotion;

fn mux_refresh_repaint_after(config: &crate::config::MultiplexerConfig) -> Option<Duration> {
    (selected_backend(config) != MultiplexerBackendConfig::Native)
        .then_some(MUX_SESSION_REFRESH_INTERVAL)
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
    pub renderer_metrics: RendererMetrics,
    pub terminal_cell_width: f32,
    pub terminal_cell_height: f32,
    pub terminal_scale_factor: f32,
    pub terminal_view_transform: ViewTransform,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ViewportSnapshot {
    pub fullscreen: bool,
    pub maximized: bool,
    pub content_height: f32,
}

/// Host actions requested by a frame update, applied by the eframe adapter.
#[derive(Clone, Debug, PartialEq)]
pub enum AppEffect {
    CloseWindow,
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
#[derive(Clone, Debug)]
struct PendingGeneratedName {
    cwd: String,
    name: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ScopedWindowId {
    scope: MuxScope,
    session_id: String,
    window_id: String,
}

impl ScopedWindowId {
    fn new(scope: MuxScope, session_id: String, window_id: String) -> Self {
        Self {
            scope,
            session_id,
            window_id,
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ScopedPaneId {
    window: ScopedWindowId,
    pane_id: String,
}

struct BindingRuntime {
    scope: MuxScope,
    label: String,
    multiplexer: crate::config::MultiplexerConfig,
    terminal: ActiveTerminal,
    mux: BindingMuxController,
    session_order: SessionOrderStore,
    session_names: SessionNameStore,
    pending_generated_names: HashMap<String, PendingGeneratedName>,
    generated_names_signature: Option<u64>,
    terminal_side_effect_tx: mpsc::Sender<TerminalSideEffectEvent>,
    terminal_side_effect_rx: mpsc::Receiver<TerminalSideEffectEvent>,
    pane_layouts: HashMap<ScopedWindowId, PaneLayout>,
    pending_pane_split_directions: HashMap<ScopedWindowId, SplitDirection>,
    custom_tab_names: HashSet<ScopedWindowId>,
    terminal_tab_titles: HashMap<ScopedWindowId, String>,
    terminal_progress: HashMap<ScopedPaneId, TerminalProgress>,
    unscoped_terminal_progress: Option<TerminalProgress>,
}

impl BindingRuntime {
    fn new(
        scope: MuxScope,
        config: &BoottyConfig,
        variant: AppearanceVariant,
        repaint: RepaintHandle,
    ) -> Self {
        let (terminal_side_effect_tx, terminal_side_effect_rx) = mpsc::channel();
        let session_config =
            terminal_session_config_with_side_effects(config, variant, &terminal_side_effect_tx);
        Self {
            label: binding_label(scope, &config.multiplexer),
            multiplexer: config.multiplexer.clone(),
            scope,
            terminal: ActiveTerminal::new(
                TerminalWidget::initial_geometry(),
                &config.multiplexer,
                session_config,
                repaint,
            ),
            terminal_side_effect_tx,
            terminal_side_effect_rx,
            mux: BindingMuxController::default(),
            session_order: SessionOrderStore::for_binding(
                &config.config_path,
                &config.multiplexer,
                scope.binding_id().persistence_value(),
            ),
            session_names: SessionNameStore::for_binding(
                &config.config_path,
                &config.multiplexer,
                scope.binding_id().persistence_value(),
            ),
            pending_generated_names: HashMap::new(),
            generated_names_signature: None,
            pane_layouts: HashMap::new(),
            pending_pane_split_directions: HashMap::new(),
            custom_tab_names: HashSet::new(),
            terminal_tab_titles: HashMap::new(),
            terminal_progress: HashMap::new(),
            unscoped_terminal_progress: None,
        }
    }

    fn sync_session_order(&mut self) {
        let ordered_names = self.session_order.sync_sessions(
            self.mux
                .sessions()
                .iter()
                .map(|session| session.name.as_str()),
        );
        self.mux.apply_session_order(&ordered_names);
    }

    fn window_id(&self, session_id: String, window_id: String) -> ScopedWindowId {
        ScopedWindowId::new(self.scope, session_id, window_id)
    }

    fn pane_id(&self, window: ScopedWindowId, pane_id: impl Into<String>) -> ScopedPaneId {
        ScopedPaneId {
            window,
            pane_id: pane_id.into(),
        }
    }
}

fn binding_label(scope: MuxScope, multiplexer: &crate::config::MultiplexerConfig) -> String {
    let backend = match multiplexer.backend {
        crate::config::MultiplexerBackendConfig::Rmux => "Rmux",
        crate::config::MultiplexerBackendConfig::Native => "Native",
        crate::config::MultiplexerBackendConfig::Tmux => "Tmux",
        crate::config::MultiplexerBackendConfig::Zellij => "Zellij",
    };
    format!(
        "{backend} / Binding {}",
        scope.binding_id().persistence_value()
    )
}

pub struct AppState {
    binding: BindingRuntime,
    inactive_bindings: Vec<BindingRuntime>,
    repaint_scheduler: RepaintScheduler,
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
    terminal_find_dialog: Option<TerminalFindDialog>,
    terminal_find_return_focus_after_search: bool,
    last_terminal_search: String,
    last_terminal_search_direction: TerminalSearchDirection,
    theme_picker_restore_config: Option<BoottyConfig>,
    /// A command-palette choice waiting to be dispatched on the next input pass,
    /// where the viewport snapshot and effect sink are in scope.
    pending_command: Option<KeybindAction>,
    #[cfg(debug_assertions)]
    diagnostic_action_driver: Option<DiagnosticActionDriver>,
    macos_non_native_fullscreen_active: bool,
    macos_non_native_fullscreen_pending_apply: bool,
}

fn terminal_session_config_with_side_effects(
    config: &BoottyConfig,
    variant: AppearanceVariant,
    side_effect_tx: &mpsc::Sender<TerminalSideEffectEvent>,
) -> TerminalSessionConfig {
    let mut session_config = config.terminal_session_config();
    session_config.colors = config
        .colors_for_appearance(variant)
        .terminal_color_config();
    session_config.side_effect_tx = Some(side_effect_tx.clone());
    session_config
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
        mut config: BoottyConfig,
        repaint: RepaintHandle,
        direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
        modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
    ) -> Result<Self> {
        let workspace = WorkspaceStore::for_config_path(&config.config_path, &config.multiplexer);
        if let Some(binding) = workspace.binding() {
            config.multiplexer = binding.multiplexer_config();
        }
        let modifier_remaps = config.input.modifier_remaps()?;
        let macos_option_as_alt = config.input.macos_option_as_alt.into();
        let keybinds = config
            .input
            .keybinds_for_backend(config.multiplexer.backend);
        let app_key_bindings = AppKeyBindings::from_keybinds(&keybinds)?;
        let sidebar_key_bindings =
            SidebarKeyBindings::from_keybinds(&config.input.sidebar_keybind)?;
        let stability_trace = StabilityTrace::from_config(&config);
        let active_appearance_variant = config.appearance.mode.variant(AppearanceVariant::Dark);
        let mut bindings = workspace
            .bindings()
            .iter()
            .map(|workspace_binding| {
                let mut binding_config = config.clone();
                binding_config.multiplexer = workspace_binding.multiplexer_config();
                let mut runtime = BindingRuntime::new(
                    workspace_binding.mux_scope(),
                    &binding_config,
                    active_appearance_variant,
                    repaint.clone(),
                );
                runtime.label = workspace_binding.name().to_owned();
                runtime
            })
            .collect::<Vec<_>>();
        if bindings.is_empty() {
            bindings.push(BindingRuntime::new(
                MuxScope::new(SpaceId::from_persistence(0), BindingId::from_persistence(0)),
                &config,
                active_appearance_variant,
                repaint.clone(),
            ));
        }
        let binding = bindings.remove(0);
        let inactive_bindings = bindings;
        let config_hot_reload = ConfigHotReload::new(&config.config_path);
        let macos_non_native_fullscreen_active = config.window.non_native_fullscreen_enabled();
        let macos_non_native_fullscreen_applied =
            apply_macos_non_native_fullscreen_presentation(&config.window);
        let macos_non_native_fullscreen_pending_apply =
            macos_non_native_fullscreen_active && !macos_non_native_fullscreen_applied;
        #[cfg(debug_assertions)]
        let diagnostic_action_driver = DiagnosticActionDriver::from_env();

        Ok(Self {
            binding,
            inactive_bindings,
            repaint_scheduler: RepaintScheduler::default(),
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
            terminal_cursor_icon: egui::CursorIcon::Default,
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
            terminal_find_dialog: None,
            terminal_find_return_focus_after_search: false,
            last_terminal_search: String::new(),
            last_terminal_search_direction: TerminalSearchDirection::Next,
            theme_picker_restore_config: None,
            pending_command: None,
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
        &self.binding.mux
    }

    pub fn mux_scope(&self) -> MuxScope {
        self.binding.scope
    }

    pub fn binding_count(&self) -> usize {
        self.inactive_bindings.len() + 1
    }

    pub fn binding_session_groups(&self) -> Vec<BindingSessionGroup> {
        let mut bindings = std::iter::once(&self.binding)
            .chain(self.inactive_bindings.iter())
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
                BindingSessionGroup {
                    scope: binding.scope,
                    label,
                    sessions: binding.mux.sessions().to_vec(),
                    selected_session: binding.mux.selected_session().map(str::to_owned),
                    active: binding.scope == self.binding.scope,
                    can_return_to_last_session: binding.mux.previous_selected_session().is_some(),
                }
            })
            .collect()
    }

    fn binding_runtimes_mut(&mut self) -> impl Iterator<Item = &mut BindingRuntime> {
        std::iter::once(&mut self.binding).chain(self.inactive_bindings.iter_mut())
    }

    fn set_binding_terminal_colors(&mut self, colors: TerminalColorConfig) -> Result<()> {
        for binding in self.binding_runtimes_mut() {
            binding.terminal.set_colors(colors.clone())?;
        }
        Ok(())
    }

    fn set_binding_cursor_config(&mut self, cursor: TerminalCursorConfig) -> Result<()> {
        for binding in self.binding_runtimes_mut() {
            binding.terminal.set_cursor_config(cursor)?;
        }
        Ok(())
    }

    fn set_binding_feature_config(&mut self, features: TerminalFeatureConfig) -> Result<()> {
        for binding in self.binding_runtimes_mut() {
            binding.terminal.set_feature_config(features)?;
        }
        Ok(())
    }

    fn active_multiplexer(&self) -> &crate::config::MultiplexerConfig {
        &self.binding.multiplexer
    }

    pub fn multiplexer_backend(&self) -> crate::config::MultiplexerBackendConfig {
        self.binding.multiplexer.backend
    }

    pub fn terminal_transition_key(&self) -> Option<String> {
        self.binding.mux.selected_session_anchor().map(|anchor| {
            scoped_terminal_transition_key(
                self.binding.scope,
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
        self.binding.mux.last_error().or(self.last_error.as_deref())
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
        &mut self.binding.terminal
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
            .binding
            .mux
            .selected_session()
            .unwrap_or("local")
            .to_owned();
        let window = self
            .binding
            .mux
            .selected_window()
            .map(str::to_owned)
            .or_else(|| {
                self.binding
                    .mux
                    .sessions()
                    .iter()
                    .find(|candidate| candidate.id == session || candidate.name == session)
                    .and_then(|candidate| candidate.active_window_id.clone())
            })
            .unwrap_or_default();
        self.binding.window_id(session, window)
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
        self.binding
            .pending_pane_split_directions
            .remove(key)
            .or_else(|| {
                if key.window_id.is_empty() {
                    None
                } else {
                    self.binding.pending_pane_split_directions.remove(
                        &self
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
        self.binding.pane_layouts.get(&self.current_window_key())
    }

    /// Drop split layouts whose `(session, window)` no longer exists, so the map doesn't grow
    /// unbounded as the user creates and destroys native sessions and tabs. Keys are stored by
    /// whatever `current_window_key` recorded (session id, occasionally name), so accept either.
    fn prune_pane_layouts(&mut self) {
        if self.binding.pane_layouts.is_empty() {
            return;
        }
        let mut live = Vec::new();
        for session in self.binding.mux.sessions() {
            for window in &session.windows {
                live.push(
                    self.binding
                        .window_id(session.id.clone(), window.id.clone()),
                );
                live.push(
                    self.binding
                        .window_id(session.name.clone(), window.id.clone()),
                );
            }
        }
        live.push(self.current_window_key());
        self.binding
            .pane_layouts
            .retain(|key, _| live.contains(key));
    }

    /// Reconcile the active native window's split layout against the backend's pane list, then make
    /// the layout's focused pane the input runtime and keep its siblings live. Non-native backends
    /// fall back to attaching the single selected anchor.
    fn sync_terminal_panes(&mut self) -> Result<()> {
        self.prune_pane_layouts();
        let config = self.active_multiplexer().clone();
        if !self.uses_native_terminal_layout() {
            return self.binding.terminal.sync_scoped_mux_anchor(
                self.binding.scope,
                &config,
                self.binding.mux.selected_session_anchor(),
            );
        }
        let panes: Vec<MuxPaneAnchor> = self.binding.mux.selected_window_panes().to_vec();
        let pane_ids: Vec<String> = panes
            .iter()
            .filter_map(|pane| pane.pane_id.clone())
            .collect();
        if pane_ids.is_empty() {
            // Idle native session (all tabs closed): nothing to render.
            return self.binding.terminal.sync_scoped_mux_anchor(
                self.binding.scope,
                &config,
                self.binding.mux.selected_session_anchor(),
            );
        }
        let key = self.current_window_key();
        let window_id = (!key.window_id.is_empty()).then(|| key.window_id.clone());
        let selected_pane = self
            .binding
            .mux
            .selected_session_anchor()
            .and_then(|anchor| anchor.pane_id.clone());
        let server_layout = self
            .binding
            .mux
            .selected_window_layout()
            .and_then(PaneLayout::from_mux_layout)
            .filter(|layout| pane_sets_match(&layout.panes(), &pane_ids));
        let layout_missing = !self.binding.pane_layouts.contains_key(&key);
        let stale_layout = self
            .binding
            .pane_layouts
            .get(&key)
            .is_some_and(|layout| layout.panes().iter().all(|pane| !pane_ids.contains(pane)));
        let mut restored_from_server = false;
        if (layout_missing || stale_layout)
            && let Some(layout) = server_layout.clone()
        {
            self.binding.pane_layouts.insert(key.clone(), layout);
            restored_from_server = true;
        }

        let previous_panes = self
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
            self.binding.pane_layouts.insert(key.clone(), layout);
            restored_from_server = true;
        } else if pane_set_changed {
            let new_pane_direction = self
                .take_pending_pane_split_direction(&key)
                .unwrap_or(SplitDirection::Right);
            let layout = self
                .binding
                .pane_layouts
                .get_mut(&key)
                .expect("native layout should be initialized");
            layout.reconcile_with_new_pane_direction(&pane_ids, new_pane_direction);
        }
        let layout = self
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
        self.binding.terminal.sync_scoped_native_window(
            self.binding.scope,
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
        self.binding.pane_id(window, pane_id)
    }

    pub(crate) fn current_terminal_progress(&self) -> Option<TerminalProgress> {
        self.selected_window_backend_progress()
            .or_else(|| self.current_terminal_progress_from_panes())
    }

    fn selected_window_backend_progress(&self) -> Option<TerminalProgress> {
        let selected = self.mux.selected_window();
        self.mux
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
                self.binding
                    .mux
                    .selected_session_anchor()
                    .and_then(|anchor| anchor.pane_id.as_deref())
                    .and_then(|pane_id| self.pane_progress(pane_id))
            })
            .or(self.binding.unscoped_terminal_progress)
    }

    pub(crate) fn pane_progress(&self, pane_id: &str) -> Option<TerminalProgress> {
        self.binding
            .terminal_progress
            .get(&self.pane_cache_key(pane_id))
            .copied()
    }

    pub(crate) fn has_indeterminate_terminal_progress(&self) -> bool {
        self.binding
            .terminal_progress
            .values()
            .chain(self.binding.unscoped_terminal_progress.iter())
            .any(|progress| progress.state == TerminalProgressState::Indeterminate)
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
        let moved = match self.binding.pane_layouts.get_mut(&key) {
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
        if let Some(layout) = self.binding.pane_layouts.get_mut(&key) {
            layout.set_ratio_at(path, ratio, min_fraction, min_fraction);
        }
    }

    pub fn render_source_for_pane(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut (dyn TerminalRuntime + '_)> {
        self.binding.terminal.render_source_for_pane(pane_id)
    }

    pub fn pane_terminal_window_size<F>(&self, leaf_size: F) -> Option<(u16, u16)>
    where
        F: FnMut(&str) -> Option<(u16, u16)>,
    {
        self.current_pane_layout()?.terminal_window_size(leaf_size)
    }

    pub fn resize_native_layout_window(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.binding
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

    fn split_focused_pane(&mut self, direction: SplitDirection) {
        let session = self
            .binding
            .mux
            .selected_session()
            .unwrap_or("local")
            .to_owned();
        let mux_config = self.active_multiplexer().clone();
        if !self.uses_native_terminal_layout() {
            self.binding.mux.execute_command(
                &self.repaint,
                &mux_config,
                MuxCommand::SplitPane {
                    session_id: session,
                    pane_id: None,
                    direction: mux_split_direction(direction),
                },
            );
            return;
        }
        let backend = selected_backend(&mux_config);
        let key = self.current_window_key();
        let focused = self
            .binding
            .pane_layouts
            .get(&key)
            .map(|layout| layout.focused().to_owned())
            .or_else(|| {
                self.binding
                    .mux
                    .selected_session_anchor()
                    .and_then(|anchor| anchor.pane_id.clone())
            });
        self.binding.mux.execute_command(
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
            self.binding
                .pending_pane_split_directions
                .insert(key, direction);
            return;
        }

        // The native split synchronously sets the new pane active, so the refreshed anchor names it.
        let new_pane = self
            .binding
            .mux
            .selected_session_anchor()
            .and_then(|anchor| anchor.pane_id.clone());
        if let Some(new_pane) = new_pane {
            let layout = self
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
            self.binding.pending_pane_split_directions.remove(&key);
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
        let Some(layout) = self.binding.pane_layouts.get(&key) else {
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
        if target.scope != self.binding.scope {
            let Some(index) = self
                .inactive_bindings
                .iter()
                .position(|binding| binding.scope == target.scope)
            else {
                return false;
            };
            let backend = self.inactive_bindings[index].multiplexer.backend;
            let keybinds = self.config().input.keybinds_for_backend(backend);
            let app_key_bindings = match AppKeyBindings::from_keybinds(&keybinds) {
                Ok(bindings) => bindings,
                Err(error) => {
                    self.last_error = Some(error.to_string());
                    return false;
                }
            };
            std::mem::swap(&mut self.binding, &mut self.inactive_bindings[index]);
            self.app_key_bindings = app_key_bindings;
            self.terminal_surface = None;
            self.last_pane_area = None;
        }
        self.binding.mux.activate_session(&target.session_id);
        self.sync_native_layout_terminal_now();
        self.sidebar_hovered_session = Some(target.clone());
        (self.repaint)();
        true
    }

    pub fn activate_session_from_ui(&mut self, session_id: &str) {
        let target = ScopedSessionTarget::new(self.binding.scope, session_id);
        self.activate_scoped_session_from_ui(&target);
    }

    pub fn activate_relative_session_from_ui(&mut self, session_id: &str, delta: isize) -> bool {
        let sessions = self.binding.mux.sessions();
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
        self.binding
            .mux
            .activate_window(session_id, window_id, &self.repaint, &mux_config);
        self.sync_native_layout_terminal_now();
    }

    pub fn activate_relative_window_from_ui(
        &mut self,
        session_id: &str,
        window_id: &str,
        delta: isize,
    ) -> bool {
        let Some((session_id, window_id)) = self
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
        self.binding.mux.execute_command(
            &self.repaint,
            &mux_config,
            MuxCommand::ActivateLastWindow { session_id },
        );
        self.sync_native_layout_terminal_now();
        true
    }

    pub fn new_tab_for_window_from_ui(&mut self, session_id: &str, window_id: &str) -> bool {
        let selected_session = self.binding.mux.selected_session().map(str::to_owned);
        let selected_window = self.binding.mux.selected_window().map(str::to_owned);
        let Some((resolved_session_id, anchor_cwd, target_is_current)) = self
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
                self.binding
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
        self.binding.mux.execute_command(
            &self.repaint,
            &mux_config,
            MuxCommand::NewWindow { session_id, cwd },
        );
        self.sync_native_layout_terminal_now();
        true
    }

    pub fn reorder_window_before_from_ui(&mut self, source: &str, before: Option<&str>) -> bool {
        let Some(session_id) = self.binding.mux.selected_session().map(str::to_owned) else {
            return false;
        };
        if before == Some(source) {
            return false;
        }
        let windows = self.binding.mux.selected_session_windows();
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
        self.binding.mux.execute_command(
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
        let selected_session = self.binding.mux.selected_session().map(str::to_owned);
        let selected_window = self.binding.mux.selected_window().map(str::to_owned);
        let Some((session_id, position, window_count, active_window_id)) = self
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
        self.binding
            .mux
            .execute_command(&self.repaint, &mux_config, command);
        self.sync_native_layout_terminal_now();
        true
    }

    pub fn close_pane_for_window_from_ui(&mut self, session_id: &str, window_id: &str) -> bool {
        let Some((session_id, window_id, pane_id)) = self
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
        let selected_session = self.binding.mux.selected_session().map(str::to_owned);
        let current_window = self.current_window_key();
        let target_is_current = current_window.window_id == window_id
            && self
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
        self.binding
            .mux
            .close_pane(&session_id, Some(&pane_id), &self.repaint, &mux_config);
        self.binding.terminal.discard_pane(&pane_id);
        if self.uses_native_terminal_layout() {
            let key = self
                .binding
                .window_id(session_id.clone(), window_id.clone());
            if let Some(layout) = self.binding.pane_layouts.get_mut(&key) {
                layout.remove(&pane_id);
            }
            if target_is_current {
                let _ = self.sync_terminal_panes();
            }
        }
        true
    }

    fn sync_session_order(&mut self) {
        self.binding.sync_session_order();
    }
    /// Whether the generated-name reconciler needs to run for `sessions`, updating the stored
    /// fingerprint as a side effect. Reconciling forks a `git` subprocess per session (worktree
    /// lookup), so this returns `false` while no session's id, name, or cwd has changed, keeping
    /// that work off the steady-state frame path.
    fn generated_names_need_sync(&mut self, sessions: &[MuxSession]) -> bool {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for session in sessions {
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
        if self.binding.generated_names_signature == Some(signature) {
            return false;
        }
        self.binding.generated_names_signature = Some(signature);
        true
    }

    fn sync_generated_session_names(&mut self) {
        let sessions = self.binding.mux.sessions().to_vec();
        if !self.generated_names_need_sync(&sessions) {
            return;
        }
        let mut renames = Vec::new();
        self.binding
            .pending_generated_names
            .retain(|session_id, pending| {
                sessions.iter().any(|session| {
                    session.id == *session_id
                        && session
                            .anchor
                            .cwd
                            .as_deref()
                            .is_some_and(|cwd| Self::session_root(cwd) == pending.cwd)
                })
            });
        let mut planned_names = self
            .binding
            .pending_generated_names
            .values()
            .map(|pending| pending.name.clone())
            .collect::<HashSet<_>>();
        let rename_supported =
            selected_backend(self.active_multiplexer()) != MultiplexerBackendConfig::Rmux;

        for session in &sessions {
            let Some(raw_cwd) = session.anchor.cwd.as_deref() else {
                continue;
            };
            let cwd = Self::session_root(raw_cwd);
            let record = if let Some(record) =
                self.binding
                    .session_names
                    .observe_session(&session.id, &session.name, &cwd)
            {
                record
            } else {
                let legacy_name = crate::strings::session_name_for_path(&cwd);
                if session.name == legacy_name {
                    self.binding
                        .session_names
                        .remember_generated(&session.id, &cwd, &session.name);
                } else {
                    self.binding
                        .session_names
                        .mark_explicit(&session.id, &session.name, &cwd);
                }
                self.binding
                    .session_names
                    .observe_session(&session.id, &session.name, &cwd)
                    .expect("session name metadata should be observable after recording")
            };

            if let Some(pending) = self
                .binding
                .pending_generated_names
                .get(&session.id)
                .cloned()
            {
                if pending.cwd == cwd {
                    if session.name == pending.name {
                        planned_names.remove(&pending.name);
                        self.binding.session_names.remember_generated(
                            &session.id,
                            &cwd,
                            &pending.name,
                        );
                        self.binding.pending_generated_names.remove(&session.id);
                    } else if session.name != record.generated_name {
                        planned_names.remove(&pending.name);
                        self.binding.pending_generated_names.remove(&session.id);
                        self.binding
                            .session_names
                            .mark_explicit(&session.id, &session.name, &cwd);
                    }
                    continue;
                }
                self.binding.pending_generated_names.remove(&session.id);
            }
            if record.explicit {
                continue;
            }
            if session.name != record.generated_name {
                self.binding
                    .session_names
                    .mark_explicit(&session.id, &session.name, &cwd);
                continue;
            }

            let existing_names = sessions
                .iter()
                .filter(|other| other.id != session.id)
                .map(|other| other.name.as_str())
                .chain(planned_names.iter().map(String::as_str));
            let desired = crate::strings::unique_session_name(
                &crate::git::suggested_session_name(&cwd),
                existing_names,
            );
            if desired == session.name || !rename_supported {
                continue;
            }
            planned_names.insert(desired.clone());
            self.binding.pending_generated_names.insert(
                session.id.clone(),
                PendingGeneratedName {
                    cwd,
                    name: desired.clone(),
                },
            );
            renames.push((session.id.clone(), desired));
        }

        if renames.is_empty() {
            return;
        }
        let mux_config = self.active_multiplexer().clone();
        for (session_id, name) in renames {
            self.binding
                .mux
                .rename_session(&session_id, name, &self.repaint, &mux_config);
        }
    }

    fn create_project_session_for_cwd(&mut self, cwd: String) {
        let cwd = Self::session_root(&cwd);
        if let Some(session_id) = self.binding.mux.sessions().iter().find_map(|session| {
            session
                .anchor
                .cwd
                .as_deref()
                .is_some_and(|open_cwd| Self::session_root(open_cwd) == cwd)
                .then(|| session.id.clone())
        }) {
            self.activate_session_from_ui(&session_id);
            return;
        }

        let existing_names = self
            .binding
            .mux
            .sessions()
            .iter()
            .map(|session| session.name.as_str())
            .chain(
                self.binding
                    .pending_generated_names
                    .values()
                    .map(|pending| pending.name.as_str()),
            )
            .collect::<Vec<_>>();
        let session_id = crate::strings::unique_session_name(
            &crate::git::suggested_session_name(&cwd),
            existing_names,
        );
        self.binding
            .session_names
            .remember_generated(&session_id, &cwd, &session_id);
        let mux_config = self.active_multiplexer().clone();
        self.binding.mux.create_project_session(
            crate::ui::new_session_picker::NewMuxSessionRequest { session_id, cwd },
            &self.repaint,
            &mux_config,
        );
    }

    fn session_root(cwd: &str) -> String {
        let cwd = crate::git::worktree_root(cwd).unwrap_or_else(|| cwd.to_owned());
        std::fs::canonicalize(&cwd)
            .unwrap_or_else(|_| PathBuf::from(cwd))
            .to_string_lossy()
            .into_owned()
    }

    fn move_selected_session(&mut self, delta: i32) -> bool {
        let Some(selected) = self.binding.mux.selected_session().map(str::to_owned) else {
            return false;
        };
        self.move_session_from_ui(&selected, delta)
    }

    pub fn move_session_from_ui(&mut self, session_id: &str, delta: i32) -> bool {
        let Some(session_name) = self
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .map(|session| session.name.clone())
        else {
            return false;
        };
        if !self.binding.session_order.move_session(
            &session_name,
            delta,
            self.binding
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
        if !self.binding.session_order.move_session_before(
            source,
            target,
            self.binding
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
                let mux_config = self.active_multiplexer().clone();
                self.binding
                    .mux
                    .rename_session(&session_id, name, &self.repaint, &mux_config);
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
                    .binding
                    .window_id(session_id.clone(), window_id.clone());
                if name.is_empty() {
                    self.binding.custom_tab_names.remove(&key);
                    if let Some(title) = self.binding.terminal_tab_titles.get(&key).cloned() {
                        self.rename_window_for_terminal_title(&session_id, &window_id, &title);
                    }
                } else {
                    self.binding.custom_tab_names.insert(key);
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
                self.binding
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
            CommandPaletteEvent::Run(action) => {
                // Close the palette now; run the command on the next input pass,
                // where the viewport snapshot and effect sink are available.
                self.input_focus = InputFocus::Terminal;
                self.pending_command = keybind_action_for_name(action);
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
                if let Err(error) = self.binding.terminal.write_input(&response) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalSideEffect::ReportVariable(name) => {
                if let Some(response) =
                    terminal_report_variable_response(&name, self.binding.mux.selected_session())
                    && let Err(error) = self.binding.terminal.write_input(&response)
                {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalSideEffect::ConEmuProgress { state, value } => {
                self.apply_terminal_progress(source_pane_id.as_deref(), state, value);
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
                        self.binding.terminal_progress.insert(key, progress);
                    }
                    None => {
                        self.binding.terminal_progress.remove(&key);
                    }
                }
            }
            None => self.binding.unscoped_terminal_progress = progress,
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
            self.binding
                .terminal_tab_titles
                .insert(key.clone(), title.clone());
            if !self.binding.custom_tab_names.contains(&key) {
                self.rename_window_for_terminal_title(&key.session_id, &key.window_id, &title);
            }
        }
        if source_pane_id.is_none() || self.binding.terminal.focused_pane_id() == source_pane_id {
            effects.push(AppEffect::SetWindowTitle(title));
        }
    }

    fn window_key_for_pane(&self, pane_id: &str) -> Option<ScopedWindowId> {
        self.binding.mux.sessions().iter().find_map(|session| {
            session.windows.iter().find_map(|window| {
                let anchor_matches = window.anchor.pane_id.as_deref() == Some(pane_id);
                let pane_matches = window
                    .panes
                    .iter()
                    .any(|pane| pane.pane_id.as_deref() == Some(pane_id));
                (anchor_matches || pane_matches).then(|| {
                    self.binding
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
        self.binding.mux.rename_window(
            session_id,
            window_id,
            name.to_owned(),
            &self.repaint,
            &mux_config,
        );
    }

    fn window_name_for_key(&self, session_id: &str, window_id: &str) -> Option<&str> {
        self.binding
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
        let selected_session = self.binding.mux.selected_session().map(str::to_owned);
        let selected_window = self.binding.mux.selected_window().map(str::to_owned);
        let pane_count = self.binding.mux.selected_window_panes().len();
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
        let FrameInputs {
            now,
            stable_dt_ms,
            events,
            dropped_file_paths,
            modifiers,
            hover_pos,
            pressed_mouse_button,
            viewport,
            renderer_metrics,
            terminal_cell_width,
            terminal_cell_height,
            terminal_scale_factor,
            terminal_view_transform,
        } = inputs;
        let mut effects = Vec::new();

        // A command-palette choice from the previous frame runs as soon as viewport/effects are
        // available, before mux refresh can retarget selected-window actions back to backend-active.
        if let Some(action) = self.pending_command.take() {
            self.apply_keybind_action(action, viewport, &mut effects);
        }

        self.sync_macos_non_native_fullscreen_presentation();
        // Drain the focused pane plus every live sibling in the active native window so background
        // panes keep processing output. For non-native this is just the single attach surface.
        self.last_drain = self.binding.terminal.drain_native_window();
        self.drain_terminal_side_effects(
            &mut effects,
            terminal_cell_width,
            terminal_cell_height,
            terminal_scale_factor,
        );
        // A shell exiting closes its pane, collapsing the split (or cascading to the tab when it was
        // the last pane). On native, any pane's shell can exit, not just the focused one.
        if self.is_native() {
            for pane in self.binding.terminal.native_exited_panes() {
                self.close_pane(&pane);
            }
        } else {
            match self.binding.terminal.child_exited() {
                Ok(true) => self.close_active_pane(),
                Ok(false) => {}
                Err(error) => self.last_error = Some(error.to_string()),
            }
        }

        if let Some(Err(_)) = self.binding.mux.poll_command() {
            self.binding.pending_generated_names.clear();
        }
        for binding in &mut self.inactive_bindings {
            if let Some(Err(_)) = binding.mux.poll_command() {
                binding.pending_generated_names.clear();
            }
        }
        let active_config = self.binding.multiplexer.clone();
        let _ = self
            .binding
            .mux
            .refresh_sessions(&self.repaint, &active_config);
        let mut schedule_mux_refresh = mux_refresh_repaint_after(&active_config).is_some();
        for binding in &mut self.inactive_bindings {
            let _ = binding
                .mux
                .refresh_sessions(&self.repaint, &binding.multiplexer);
            binding.sync_session_order();
            schedule_mux_refresh |= mux_refresh_repaint_after(&binding.multiplexer).is_some();
        }
        if schedule_mux_refresh {
            effects.push(AppEffect::RepaintAfter(MUX_SESSION_REFRESH_INTERVAL));
        }
        self.sync_generated_session_names();
        self.sync_session_order();
        if let Err(error) = self.sync_terminal_panes() {
            self.last_error = Some(error.to_string());
        }
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

        let pending_pty_bytes = self.binding.terminal.pending_pty_len();
        let (cols, rows) = self.binding.terminal.grid_size();
        if let Some(trace) = &mut self.stability_trace {
            trace.record(StabilityTraceSample {
                elapsed_ms: trace.started_at.elapsed().as_millis(),
                selected_session: self.binding.mux.selected_session(),
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
        effects.push(AppEffect::RepaintAfter(
            repaint.after.min(CONFIG_HOT_RELOAD_INTERVAL),
        ));
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
        self.terminal_find_dialog = None;
        self.terminal_find_return_focus_after_search = false;
        restored_preview
    }

    fn open_new_mux_session_dialog(&mut self) {
        self.close_overlay_dialogs();
        self.new_mux_session_dialog = Some(NewMuxSessionDialog::open());
        self.input_focus = InputFocus::Picker;
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
        let Some(selected) = self.binding.mux.selected_session().map(str::to_owned) else {
            return;
        };
        self.open_rename_session_dialog_for(&selected);
    }

    pub fn open_rename_session_dialog_for(&mut self, session_id: &str) -> bool {
        let Some((session_id, name)) = self
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .map(|session| (session.id.clone(), session.name.clone()))
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
        let selected = self.binding.mux.selected_session()?;
        let session = self
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == selected || session.name == selected)?;
        let window_id = self
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
        let Some(selected) = self.binding.mux.selected_session().map(str::to_owned) else {
            return;
        };
        self.open_ditch_session_dialog_for(&selected);
    }

    pub fn open_ditch_session_dialog_for(&mut self, session_id: &str) -> bool {
        let Some((session_id, cwd)) = self
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
            .keybinds_for_backend(self.binding.multiplexer.backend);
        self.close_overlay_dialogs();
        self.keybind_help_dialog = Some(KeybindHelpDialog::open(&bindings));
        self.input_focus = InputFocus::Picker;
    }

    fn open_command_palette_dialog(&mut self) {
        let bindings = self
            .config()
            .input
            .keybinds_for_backend(self.binding.multiplexer.backend);
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
            && !self.lua_window_open
            && !self.settings_open
    }

    fn reload_config(&mut self, effects: &mut Vec<AppEffect>) -> bool {
        let previous = self.config().clone();
        let path = previous.config_path.clone();
        let mut next = match load_config_from_path(&path) {
            Ok(config) => config,
            Err(error) => {
                self.config_state.reject(error.to_string());
                self.last_error = self.config_state.last_error().map(str::to_owned);
                return false;
            }
        };
        let workspace = WorkspaceStore::for_config_path(&next.config_path, &next.multiplexer);
        if let Some(binding) = workspace.binding() {
            next.multiplexer = binding.multiplexer_config();
        }
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
            .keybinds_for_backend(self.binding.multiplexer.backend);
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
        self.has_new_session_config_changes = new_session_only_config_changed(&previous, &next)
            || self.has_new_session_config_changes;
        self.config_state.accept(next);
        self.set_mouse_pointer_hidden_while_typing(self.mouse_pointer_hidden_while_typing, effects);
        let config_path = self.config().config_path.clone();
        let multiplexer = self.active_multiplexer().clone();
        let binding_id = self.binding.scope.binding_id().persistence_value();
        self.binding.session_names =
            SessionNameStore::for_binding(&config_path, &multiplexer, binding_id);
        self.binding.pending_generated_names.clear();
        self.binding.session_order =
            SessionOrderStore::for_binding(&config_path, &multiplexer, binding_id);
        self.sync_session_order();
        self.last_error = if self.has_new_session_config_changes {
            Some(
                "config reloaded; session/window settings require a new window or restart"
                    .to_owned(),
            )
        } else {
            None
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
    ) -> (Vec<egui::Event>, Vec<KeybindAction>) {
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

        match TerminalRenderSource::is_mouse_tracking(&mut self.binding.terminal) {
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
            let result = match action {
                TerminalSelectionAction::Begin(event) => {
                    TerminalRenderSource::begin_selection(&mut self.binding.terminal, event)
                }
                TerminalSelectionAction::Scroll(delta) => {
                    TerminalRenderSource::scroll_viewport_delta(&mut self.binding.terminal, delta)
                }
                TerminalSelectionAction::Update(event) => {
                    TerminalRenderSource::update_selection(&mut self.binding.terminal, event)
                }
                TerminalSelectionAction::End(event) => {
                    TerminalRenderSource::end_selection(&mut self.binding.terminal, event)
                }
            };
            match result {
                Ok(()) => effects.push(AppEffect::RequestRepaint),
                Err(error) => self.last_error = Some(error.to_string()),
            }
        }
        count
    }

    fn terminal_copy_mode_active(&mut self) -> bool {
        match TerminalRenderSource::copy_mode_active(&mut self.binding.terminal) {
            Ok(active) => active,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    fn enter_terminal_copy_mode(&mut self, effects: &mut Vec<AppEffect>) {
        match TerminalRenderSource::enter_copy_mode(&mut self.binding.terminal) {
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
        match TerminalRenderSource::handle_copy_mode_action(&mut self.binding.terminal, action) {
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
        let sidebar_count = self.handle_sidebar_input(routed.ui_events);
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

        for action in actions {
            self.apply_keybind_action(action, viewport, effects);
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
        let Some(text) = bootty_winit::file_paths::format_file_paths_for_paste(
            paths.iter().map(PathBuf::as_path),
        ) else {
            return 0;
        };
        if let Err(error) = self.binding.terminal.write_paste(&text) {
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
            if direct_copy_shortcut_pressed(input) && self.copy_terminal_selection_if_any() {
                continue;
            }
            if let Some(action) = self.app_key_bindings.action_for_input(input) {
                if matches!(action, KeybindAction::PasteFromClipboard) {
                    self.suppress_next_egui_paste = true;
                }
                self.apply_keybind_action(action, viewport, effects);
                continue;
            }
            if let Some(KeybindAction::App(AppAction::NewMuxSession)) =
                builtin_app_action_for_direct_key(input)
            {
                self.open_new_mux_session_dialog();
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

    fn handle_sidebar_input(&mut self, events: Vec<egui::Event>) -> usize {
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
            let Some(action) = self.sidebar_key_bindings.action_for_key(key, modifiers) else {
                continue;
            };
            match action {
                SidebarAction::Ignore => {}
                SidebarAction::PreviousSession => {
                    self.move_sidebar_hover(-1);
                }
                SidebarAction::NextSession => {
                    self.move_sidebar_hover(1);
                }
                SidebarAction::ActivateSession => {
                    self.activate_sidebar_hovered_session();
                }
                SidebarAction::FocusTerminal => {
                    self.input_focus = InputFocus::Terminal;
                }
            }
        }
        count
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
                if let Err(error) = self.binding.terminal.write_input(&bytes) {
                    self.last_error = Some(error.to_string());
                } else {
                    self.hide_mouse_pointer_for_terminal_typing(effects);
                }
            }
            KeybindAction::Font(action) => self.apply_font_size_action(action, effects),
            KeybindAction::Find(action) => self.apply_terminal_find_action(action, effects),
            KeybindAction::CopyToClipboard => {
                self.copy_terminal_selection_or_request_copy(effects);
            }
            KeybindAction::CopyMode => {
                self.enter_terminal_copy_mode(effects);
            }
            KeybindAction::PasteFromClipboard => match read_clipboard_text() {
                Ok(Some(text)) => {
                    if let Err(error) = self.binding.terminal.write_paste(&text) {
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
        if !self.copy_terminal_selection_if_any() {
            return 0;
        }
        events.remove(index);
        1
    }

    fn copy_terminal_selection_if_any(&mut self) -> bool {
        match self
            .binding
            .terminal
            .format_selection(TerminalSelectionFormat::PlainText)
        {
            Ok(Some(bytes)) => {
                let text = String::from_utf8_lossy(&bytes);
                if let Err(error) = write_clipboard_text(&text) {
                    self.last_error = Some(error.to_string());
                }
                true
            }
            Ok(None) => false,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    fn copy_terminal_selection_or_request_copy(&mut self, effects: &mut Vec<AppEffect>) {
        if !self.copy_terminal_selection_if_any() {
            effects.push(AppEffect::RequestCopy);
        }
    }

    // Close the focused pane (cmd+w or its shell exiting) and let the mux cascade to the tab. The
    // active terminal is dropped here so its PTY is reaped; sync_mux_anchor then attaches whatever
    // pane the mux selected next (or idle when the session has no tabs left).
    fn close_active_pane(&mut self) {
        if self.uses_native_terminal_layout() {
            if let Some(focused) = self.focused_pane() {
                self.close_pane(&focused);
            }
            return;
        }
        let session_id = self
            .binding
            .mux
            .selected_session()
            .unwrap_or("local")
            .to_owned();
        let mux_config = self.active_multiplexer().clone();
        self.binding.mux.execute_command(
            &self.repaint,
            &mux_config,
            MuxCommand::ClosePane {
                session_id,
                pane_id: None,
            },
        );
        self.binding.terminal.discard_active_pane();
    }

    /// Close a specific native pane: remove it from the backend window, kill its PTY, collapse the
    /// split layout, and re-activate the surviving focused pane this frame so it doesn't flash idle.
    fn close_pane(&mut self, pane_id: &str) {
        let session_id = self
            .binding
            .mux
            .selected_session()
            .unwrap_or("local")
            .to_owned();
        let mux_config = self.active_multiplexer().clone();
        self.binding.mux.execute_command(
            &self.repaint,
            &mux_config,
            MuxCommand::ClosePane {
                session_id,
                pane_id: Some(pane_id.to_owned()),
            },
        );
        self.binding.terminal.discard_pane(pane_id);
        let key = self.current_window_key();
        if let Some(layout) = self.binding.pane_layouts.get_mut(&key) {
            layout.remove(pane_id);
        }
        let _ = self.sync_terminal_panes();
    }

    fn apply_mux_key_action(&mut self, action: MuxKeyAction) {
        if self.apply_session_navigation_action(action) {
            return;
        }
        if let MuxKeyAction::MoveSession(delta) = action {
            self.move_selected_session(delta);
            return;
        }
        if matches!(action, MuxKeyAction::ClosePane) {
            self.close_active_pane();
            return;
        }
        // On the native engine, killing a pane means removing the focused split leaf and collapsing
        // the layout, same as closing it. Other backends keep tmux/zellij kill-pane semantics.
        if self.uses_native_terminal_layout() && matches!(action, MuxKeyAction::KillPane) {
            self.close_active_pane();
            return;
        }
        if let MuxKeyAction::SplitPane(direction) = action {
            self.split_focused_pane(direction);
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
        if matches!(action, MuxKeyAction::NewTab) && self.binding.mux.selected_session().is_none() {
            let cwd = new_mux_session_request_with_name(self.config(), "").cwd;
            self.create_project_session_for_cwd(cwd);
            self.sync_native_layout_terminal_now();
            return;
        }
        let selected_session = self
            .binding
            .mux
            .selected_session()
            .unwrap_or("local")
            .to_owned();
        let selected_cwd = terminal_cwd_for_mux_command(
            self.binding
                .terminal
                .current_working_directory()
                .ok()
                .flatten(),
            self.binding
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
                window_id: self.binding.mux.selected_window().map(str::to_owned),
                delta,
            },
            MuxKeyAction::SplitPane(_) => {
                unreachable!("split pane is handled before the command match")
            }
            MuxKeyAction::SelectPane(direction) => MuxCommand::SelectPane {
                session_id: selected_session,
                direction,
            },
            MuxKeyAction::NextPane => MuxCommand::SelectNextPane {
                session_id: selected_session,
            },
            MuxKeyAction::PreviousPane => MuxCommand::SelectPreviousPane {
                session_id: selected_session,
            },
            MuxKeyAction::KillPane => MuxCommand::KillPane {
                session_id: selected_session,
                pane_id: None,
            },
            MuxKeyAction::ClosePane => {
                unreachable!("close pane is handled before the command match")
            }
            MuxKeyAction::TogglePaneZoom => MuxCommand::TogglePaneZoom {
                session_id: selected_session,
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
        self.binding
            .mux
            .execute_command(&self.repaint, &mux_config, command);
        self.sync_native_layout_terminal_now();
    }

    fn ensure_sidebar_hovered_session(&mut self) {
        if self.sidebar_hovered_index().is_some() {
            return;
        }
        self.sidebar_hovered_session = self
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

    fn activate_sidebar_hovered_session(&mut self) {
        self.ensure_sidebar_hovered_session();
        if let Some(target) = self.sidebar_hovered_session.clone() {
            self.activate_scoped_session_from_ui(&target);
        }
        self.input_focus = InputFocus::Terminal;
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
        self.binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == value || session.name == value)
            .map(|session| ScopedSessionTarget::new(self.binding.scope, session.id.clone()))
    }

    fn apply_session_navigation_action(&mut self, action: MuxKeyAction) -> bool {
        let target = match action {
            MuxKeyAction::SelectSession(index) => self
                .binding
                .mux
                .sessions()
                .get(index.saturating_sub(1) as usize)
                .map(|session| session.id.clone()),
            MuxKeyAction::NextSession => self.relative_session(1),
            MuxKeyAction::PreviousSession => self.relative_session(-1),
            MuxKeyAction::LastSession => self
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
            self.binding.mux.activate_session(&target);
            self.sync_native_layout_terminal_now();
        }
        true
    }

    fn relative_session(&self, delta: isize) -> Option<String> {
        let sessions = self.binding.mux.sessions();
        if sessions.is_empty() {
            return None;
        }
        let selected = self.binding.mux.selected_session();
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
        match self.binding.terminal.search_viewport(query, direction) {
            Ok(found) => self.terminal_find_result_from_frame(found),
            Err(error) => {
                self.last_error = Some(error.to_string());
                TerminalFindResult::default()
            }
        }
    }

    fn search_copy_mode_terminal(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
    ) -> TerminalFindResult {
        match TerminalRenderSource::handle_copy_mode_action(
            &mut self.binding.terminal,
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
            TerminalScrollAction::PageUp => -(self.binding.terminal.grid_size().1 as isize),
            TerminalScrollAction::PageDown => self.binding.terminal.grid_size().1 as isize,
            TerminalScrollAction::Lines(lines) => isize::from(lines),
        };
        if let Err(error) = self.binding.terminal.scroll_viewport_delta(delta) {
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
                if let Err(error) = self.binding.terminal.write_input(text.as_bytes()) {
                    self.last_error = Some(error.to_string());
                } else {
                    self.hide_mouse_pointer_for_terminal_typing(effects);
                }
            }
            TerminalInputCommand::Paste(text) => {
                if let Err(error) = self.binding.terminal.write_paste(&text) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalInputCommand::Focus(focused) => {
                if let Err(error) = self.binding.terminal.encode_focus(focused) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalInputCommand::Key(input) => {
                if let Err(error) = self.binding.terminal.encode_key(input) {
                    self.last_error = Some(error.to_string());
                } else {
                    self.hide_mouse_pointer_for_terminal_typing(effects);
                }
            }
            TerminalInputCommand::Mouse(input) => {
                if let Err(error) = self.binding.terminal.encode_mouse(input) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalInputCommand::MouseWheel {
                input,
                scroll_delta,
            } => {
                if let Err(error) = self
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
        effects.push(AppEffect::SetTerminalTextConfig(
            self.config().font.terminal_text_config(),
        ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MultiplexerBackendConfig, WindowFullscreen};
    use anyhow::Context;
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        thread,
        time::{Duration, Instant},
    };

    #[test]
    fn recorded_chord_lowercases_letters_but_keeps_named_keys() {
        // The physical-key serializer emits uppercase letters; recorded chords are lowercased to
        // match the default keybind convention (cmd+alt+x), while named keys keep their casing so
        // they still parse and match.
        assert_eq!(
            normalize_recorded_chord("cmd+alt+X".to_owned()),
            "cmd+alt+x"
        );
        assert_eq!(normalize_recorded_chord("cmd+V".to_owned()), "cmd+v");
        assert_eq!(normalize_recorded_chord("ctrl+KeyV".to_owned()), "ctrl+v");
        assert_eq!(
            normalize_recorded_chord("ctrl+shift+Digit1".to_owned()),
            "ctrl+shift+1"
        );
        assert_eq!(normalize_recorded_chord("ctrl+Tab".to_owned()), "ctrl+Tab");
        assert_eq!(normalize_recorded_chord("cmd+F5".to_owned()), "cmd+F5");
        assert_eq!(normalize_recorded_chord("cmd+=".to_owned()), "cmd+=");
    }

    static TEST_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_test_id() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let sequence = TEST_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("{nanos}-{sequence}")
    }

    fn route_selection_test_events(
        events: Vec<egui::Event>,
        context: TerminalSelectionRouteContext<'_>,
    ) -> (
        Vec<egui::Event>,
        Vec<TerminalSelectionAction>,
        TerminalSelectionRouter,
    ) {
        let mut router = TerminalSelectionRouter::default();
        let (terminal_events, selection_actions) = router.route_events(events, context);
        (terminal_events, selection_actions, router)
    }

    #[test]
    fn remove_first_paste_event_removes_only_one_paste_event() {
        let mut events = vec![
            egui::Event::Text("before".to_owned()),
            egui::Event::Paste("first".to_owned()),
            egui::Event::Paste("second".to_owned()),
        ];

        assert!(remove_first_paste_event(&mut events));
        assert_eq!(
            events,
            vec![
                egui::Event::Text("before".to_owned()),
                egui::Event::Paste("second".to_owned())
            ]
        );
    }

    #[test]
    fn find_bar_focus_keeps_text_in_ui_but_routes_terminal_pointer_events() {
        let find_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 40.0));
        let outside_press = egui::Event::PointerButton {
            pos: egui::Pos2::new(120.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        };
        let routed = route_find_modeless_events(
            InputFocus::Picker,
            vec![outside_press.clone(), egui::Event::Text("a".to_owned())],
            Some(find_rect),
            None,
        );

        assert_eq!(routed.terminal_events, vec![outside_press]);
        assert_eq!(routed.ui_events, vec![egui::Event::Text("a".to_owned())]);
    }

    #[test]
    fn terminal_focus_does_not_route_find_bar_pointer_events_to_terminal() {
        let find_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 40.0));
        let inside_press = egui::Event::PointerButton {
            pos: egui::Pos2::new(20.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        };
        let outside_press = egui::Event::PointerButton {
            pos: egui::Pos2::new(120.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        };
        let routed = route_find_modeless_events(
            InputFocus::Terminal,
            vec![inside_press.clone(), outside_press.clone()],
            Some(find_rect),
            None,
        );

        assert_eq!(routed.ui_events, vec![inside_press]);
        assert_eq!(routed.terminal_events, vec![outside_press]);
    }

    #[test]
    fn bootty_selection_drag_is_not_sent_to_terminal_input() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 80.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let events = vec![
            egui::Event::PointerButton {
                pos: egui::Pos2::new(10.0, 10.0),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: shift,
            },
            egui::Event::PointerMoved(egui::Pos2::new(20.0, 10.0)),
            egui::Event::PointerButton {
                pos: egui::Pos2::new(20.0, 10.0),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::Text("x".to_owned()),
        ];

        let (terminal_events, selection_actions, router) = route_selection_test_events(
            events,
            TerminalSelectionRouteContext {
                surface: Some(surface),
                view: ViewTransform::IDENTITY,
                mouse_tracking: false,
                frame_modifiers: egui::Modifiers::default(),
                chrome_handle_rects: &[],
            },
        );

        assert_eq!(terminal_events, vec![egui::Event::Text("x".to_owned())]);
        assert_eq!(selection_actions.len(), 3);
        assert!(matches!(
            selection_actions[0],
            TerminalSelectionAction::Begin(_)
        ));
        assert!(matches!(
            selection_actions[1],
            TerminalSelectionAction::Update(_)
        ));
        assert!(matches!(
            selection_actions[2],
            TerminalSelectionAction::End(_)
        ));
        assert!(!router.is_active());
    }

    #[test]
    fn selection_drag_above_terminal_scrolls_and_updates_at_viewport_edge() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 80.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let events = vec![
            egui::Event::PointerButton {
                pos: egui::Pos2::new(10.0, 10.0),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: shift,
            },
            egui::Event::PointerMoved(egui::Pos2::new(20.0, -25.0)),
        ];

        let (terminal_events, selection_actions, router) = route_selection_test_events(
            events,
            TerminalSelectionRouteContext {
                surface: Some(surface),
                view: ViewTransform::IDENTITY,
                mouse_tracking: false,
                frame_modifiers: egui::Modifiers::default(),
                chrome_handle_rects: &[],
            },
        );

        assert!(terminal_events.is_empty());
        assert!(matches!(
            selection_actions[0],
            TerminalSelectionAction::Begin(_)
        ));
        assert_eq!(selection_actions.len(), 1);
        let scroll_actions = router.autoscroll_actions(
            Some(surface),
            ViewTransform::IDENTITY,
            egui::Modifiers::default(),
        );
        assert_eq!(scroll_actions[0], TerminalSelectionAction::Scroll(-2));
        let TerminalSelectionAction::Update(event) = scroll_actions[1] else {
            panic!("expected edge update after scroll");
        };
        assert_eq!(event.position.y, 0.0);
        assert!(router.is_active());
    }

    #[test]
    fn selection_drag_below_terminal_scrolls_and_updates_at_viewport_edge() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(220.0, 160.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let events = vec![
            egui::Event::PointerButton {
                pos: egui::Pos2::new(10.0, 30.0),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: shift,
            },
            egui::Event::PointerMoved(egui::Pos2::new(20.0, 205.0)),
        ];

        let (terminal_events, selection_actions, router) = route_selection_test_events(
            events,
            TerminalSelectionRouteContext {
                surface: Some(surface),
                view: ViewTransform::IDENTITY,
                mouse_tracking: false,
                frame_modifiers: egui::Modifiers::default(),
                chrome_handle_rects: &[],
            },
        );

        assert!(terminal_events.is_empty());
        assert!(matches!(
            selection_actions[0],
            TerminalSelectionAction::Begin(_)
        ));
        assert_eq!(selection_actions.len(), 1);
        let scroll_actions = router.autoscroll_actions(
            Some(surface),
            ViewTransform::IDENTITY,
            egui::Modifiers::default(),
        );
        assert_eq!(scroll_actions[0], TerminalSelectionAction::Scroll(3));
        let TerminalSelectionAction::Update(event) = scroll_actions[1] else {
            panic!("expected edge update after scroll");
        };
        assert!(event.position.y < 160.0);
        assert!(event.position.y >= 140.0);
        assert!(router.is_active());
    }

    #[test]
    fn held_selection_below_terminal_repeats_downward_scroll_without_pointer_motion() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(220.0, 160.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );

        let mut router = TerminalSelectionRouter::default();
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let _ = router.route_events(
            vec![
                egui::Event::PointerButton {
                    pos: egui::Pos2::new(10.0, 30.0),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: shift,
                },
                egui::Event::PointerMoved(egui::Pos2::new(20.0, 205.0)),
            ],
            TerminalSelectionRouteContext {
                surface: Some(surface),
                view: ViewTransform::IDENTITY,
                mouse_tracking: false,
                frame_modifiers: egui::Modifiers::default(),
                chrome_handle_rects: &[],
            },
        );
        let actions = router.autoscroll_actions(
            Some(surface),
            ViewTransform::IDENTITY,
            egui::Modifiers::default(),
        );

        assert_eq!(actions[0], TerminalSelectionAction::Scroll(3));
        let TerminalSelectionAction::Update(event) = actions[1] else {
            panic!("expected edge update after repeated scroll");
        };
        assert!(event.position.y < 160.0);
        assert!(event.position.y >= 140.0);
    }

    #[test]
    fn selection_press_only_near_edge_does_not_autoscroll_until_drag_moves() {
        let mut state = test_state();
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(220.0, 160.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        state.record_surface(surface);

        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let effects = state.update_frame(test_frame_inputs(
            vec![egui::Event::PointerButton {
                pos: egui::Pos2::new(10.0, 155.0),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: shift,
            }],
            Some(egui::Pos2::new(10.0, 155.0)),
        ));

        assert!(state.terminal_selection.is_active());
        assert_eq!(
            effects
                .iter()
                .filter(|effect| matches!(effect, AppEffect::RequestRepaint))
                .count(),
            1
        );
    }

    #[test]
    fn copy_mode_key_layer_supports_tmux_vim_navigation_and_selection() {
        fn terminal(action: TerminalCopyModeAction) -> Option<CopyModeKeyAction> {
            Some(CopyModeKeyAction::Terminal(action))
        }

        assert_eq!(
            copy_mode_action_for_egui_key(egui::Key::J, egui::Modifiers::default()),
            terminal(TerminalCopyModeAction::Move(TerminalCopyModeMotion::Down))
        );
        assert_eq!(
            copy_mode_action_for_char('n'),
            Some(CopyModeKeyAction::SearchRepeat(
                CopyModeSearchRepeat::SameDirection
            ))
        );
        assert_eq!(
            copy_mode_action_for_char('N'),
            Some(CopyModeKeyAction::SearchRepeat(
                CopyModeSearchRepeat::OppositeDirection
            ))
        );
        assert_eq!(
            copy_mode_action_for_input(KeyInput {
                key: TerminalKey::N,
                mods: crate::terminal::KeyMods::default(),
                repeat: false,
                utf8: Some("n"),
                unshifted: Some('n'),
            }),
            Some(CopyModeKeyAction::SearchRepeat(
                CopyModeSearchRepeat::SameDirection
            ))
        );

        let mut suppress_next_text = false;
        assert_eq!(
            copy_mode_action_for_egui_event(
                &key_event(egui::Key::J, egui::Modifiers::default()),
                &mut suppress_next_text,
            ),
            terminal(TerminalCopyModeAction::Move(TerminalCopyModeMotion::Down))
        );
        assert_eq!(
            copy_mode_action_for_egui_event(
                &egui::Event::Text("j".to_owned()),
                &mut suppress_next_text,
            ),
            None
        );
        assert_eq!(
            copy_mode_action_for_egui_event(
                &egui::Event::Text("/".to_owned()),
                &mut suppress_next_text,
            ),
            Some(CopyModeKeyAction::SearchPrompt(
                TerminalSearchDirection::Next
            ))
        );
        assert_eq!(
            copy_mode_action_for_egui_key(egui::Key::ArrowUp, egui::Modifiers::default()),
            terminal(TerminalCopyModeAction::Move(TerminalCopyModeMotion::Up))
        );
        assert_eq!(
            copy_mode_action_for_egui_key(egui::Key::Space, egui::Modifiers::default()),
            terminal(TerminalCopyModeAction::BeginSelection)
        );
        assert_eq!(
            copy_mode_action_for_egui_key(egui::Key::V, egui::Modifiers::default()),
            terminal(TerminalCopyModeAction::ToggleSelection)
        );
        assert_eq!(
            copy_mode_action_for_egui_key(
                egui::Key::V,
                egui::Modifiers {
                    ctrl: true,
                    ..Default::default()
                }
            ),
            terminal(TerminalCopyModeAction::ToggleRectangle)
        );

        assert_eq!(
            copy_mode_action_for_egui_key(
                egui::Key::V,
                egui::Modifiers {
                    shift: true,
                    ..Default::default()
                }
            ),
            terminal(TerminalCopyModeAction::SelectLine)
        );
        assert_eq!(
            copy_mode_action_for_char('v'),
            terminal(TerminalCopyModeAction::ToggleSelection)
        );
        assert_eq!(
            copy_mode_action_for_input(KeyInput {
                key: TerminalKey::V,
                mods: crate::terminal::KeyMods::default(),
                repeat: false,
                utf8: Some("v"),
                unshifted: Some('v'),
            }),
            terminal(TerminalCopyModeAction::ToggleSelection)
        );
        assert_eq!(
            copy_mode_action_for_char('$'),
            terminal(TerminalCopyModeAction::Move(
                TerminalCopyModeMotion::EndOfLine
            ))
        );
        assert_eq!(
            copy_mode_action_for_char('/'),
            Some(CopyModeKeyAction::SearchPrompt(
                TerminalSearchDirection::Next
            ))
        );
        assert_eq!(
            copy_mode_action_for_char('?'),
            Some(CopyModeKeyAction::SearchPrompt(
                TerminalSearchDirection::Previous
            ))
        );
        assert_eq!(
            copy_mode_action_for_char('*'),
            Some(CopyModeKeyAction::SearchWord(TerminalSearchDirection::Next))
        );
        assert_eq!(
            copy_mode_action_for_char('#'),
            Some(CopyModeKeyAction::SearchWord(
                TerminalSearchDirection::Previous
            ))
        );
    }

    #[test]
    fn copy_mode_search_repeat_uses_the_direction_that_started_search() {
        let mut state = test_state();
        let mut effects = Vec::new();

        state.apply_copy_mode_key_action(
            CopyModeKeyAction::SearchPrompt(TerminalSearchDirection::Previous),
            &mut effects,
        );
        assert_eq!(
            state.last_terminal_search_direction,
            TerminalSearchDirection::Previous
        );

        state.last_terminal_search = "needle".to_owned();
        state.apply_copy_mode_key_action(
            CopyModeKeyAction::SearchRepeat(CopyModeSearchRepeat::SameDirection),
            &mut effects,
        );
        assert_eq!(
            state.last_terminal_search_direction,
            TerminalSearchDirection::Previous
        );

        state.apply_copy_mode_key_action(
            CopyModeKeyAction::SearchRepeat(CopyModeSearchRepeat::OppositeDirection),
            &mut effects,
        );
        assert_eq!(
            state.last_terminal_search_direction,
            TerminalSearchDirection::Previous,
            "opposite repeat must not change the sticky search mode"
        );

        state.apply_copy_mode_key_action(
            CopyModeKeyAction::SearchRepeat(CopyModeSearchRepeat::SameDirection),
            &mut effects,
        );
        assert_eq!(
            state.last_terminal_search_direction,
            TerminalSearchDirection::Previous,
            "next same-direction repeat should still follow the original backward search mode"
        );

        state.apply_copy_mode_key_action(
            CopyModeKeyAction::SearchPrompt(TerminalSearchDirection::Next),
            &mut effects,
        );
        assert_eq!(
            state.last_terminal_search_direction,
            TerminalSearchDirection::Next,
            "a new explicit search prompt should replace the sticky search mode"
        );
    }

    #[test]
    fn copy_mode_egui_question_mark_opens_backward_search_prompt() {
        let mut suppress_next_text = false;
        assert_eq!(
            copy_mode_action_for_egui_event(
                &key_event(egui::Key::Questionmark, egui::Modifiers::default()),
                &mut suppress_next_text,
            ),
            Some(CopyModeKeyAction::SearchPrompt(
                TerminalSearchDirection::Previous
            ))
        );

        let mut suppress_next_text = false;
        assert_eq!(
            copy_mode_action_for_egui_event(
                &key_event(
                    egui::Key::Slash,
                    egui::Modifiers {
                        shift: true,
                        ..Default::default()
                    },
                ),
                &mut suppress_next_text,
            ),
            Some(CopyModeKeyAction::SearchPrompt(
                TerminalSearchDirection::Previous
            ))
        );
    }

    #[test]
    fn copy_mode_search_submit_returns_focus_to_terminal_for_repeat_keys() {
        let mut state = test_state();
        let mut effects = Vec::new();
        state.apply_copy_mode_key_action(
            CopyModeKeyAction::SearchPrompt(TerminalSearchDirection::Previous),
            &mut effects,
        );
        assert_eq!(state.input_focus, InputFocus::Picker);

        state.apply_terminal_find_event(
            TerminalFindDialog::open_with_direction(
                "needle".to_owned(),
                TerminalSearchDirection::Previous,
            ),
            TerminalFindEvent::Search {
                query: "needle".to_owned(),
                direction: TerminalSearchDirection::Previous,
            },
        );
        assert_eq!(state.input_focus, InputFocus::Terminal);
        assert!(state.terminal_find_dialog.is_some());

        assert_eq!(
            state.last_terminal_search_direction,
            TerminalSearchDirection::Previous,
            "submitting a backward copy-mode search keeps backward repeat mode"
        );
    }

    #[test]
    fn default_app_bindings_leave_alt_s_and_alt_enter_for_terminal_input() {
        let mut bindings = AppKeyBindings::from_config(&BoottyConfig::default().input)
            .expect("default app bindings");
        let alt = egui::Modifiers {
            alt: true,
            ..Default::default()
        };
        let (terminal_events, actions) = split_app_actions_for_bindings_with_modifier_sides(
            &mut bindings,
            vec![
                key_event(egui::Key::S, alt),
                egui::Event::Text("s".to_owned()),
                key_event(egui::Key::Enter, alt),
            ],
            ModifierSideState {
                left_alt: true,
                ..Default::default()
            },
        );

        assert!(actions.is_empty());
        assert_eq!(terminal_events.len(), 3);
    }

    #[test]
    fn selection_drag_inside_bottom_hot_zone_scrolls_downward() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(220.0, 160.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );

        assert_eq!(
            selection_drag_scroll_delta(surface, egui::Pos2::new(20.0, 155.0)),
            1
        );
        assert_eq!(
            selection_drag_scroll_delta(surface, egui::Pos2::new(20.0, 150.0)),
            0
        );
    }

    #[test]
    fn update_frame_repeats_selection_downscroll_without_new_pointer_events() {
        let mut state = test_state();
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(220.0, 160.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        state.record_surface(surface);
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };

        let first_frame = state.update_frame(test_frame_inputs(
            vec![
                egui::Event::PointerButton {
                    pos: egui::Pos2::new(10.0, 30.0),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: shift,
                },
                egui::Event::PointerMoved(egui::Pos2::new(20.0, 155.0)),
            ],
            Some(egui::Pos2::new(20.0, 155.0)),
        ));
        assert!(state.terminal_selection.is_active());
        assert_eq!(
            first_frame
                .iter()
                .filter(|effect| matches!(effect, AppEffect::RequestRepaint))
                .count(),
            3
        );

        let repeat_frame = state.update_frame(test_frame_inputs(Vec::new(), None));
        assert_eq!(
            repeat_frame
                .iter()
                .filter(|effect| matches!(effect, AppEffect::RequestRepaint))
                .count(),
            2
        );
    }

    #[test]
    fn selection_drag_into_partial_bottom_cell_scrolls_downward() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(220.0, 165.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let pos = egui::Pos2::new(20.0, 162.0);

        assert_eq!(selection_drag_scroll_delta(surface, pos), 1);
        let event = terminal_selection_event_clamped(surface, ViewTransform::IDENTITY, pos, false)
            .expect("clamped selection event");

        assert!(event.position.y < 160.0);
        assert!(event.position.y >= 140.0);
    }

    #[test]
    fn selection_drag_below_small_pane_uses_widget_edge_not_minimum_grid_edge() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 80.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let pos = egui::Pos2::new(20.0, 125.0);

        assert_eq!(selection_drag_scroll_delta(surface, pos), 3);
        let event = terminal_selection_event_clamped(surface, ViewTransform::IDENTITY, pos, false)
            .expect("clamped selection event");

        assert!(event.position.y < 80.0);
        assert!(event.position.y >= 60.0);
    }

    #[test]
    fn press_over_chrome_handle_does_not_begin_selection() {
        // Dragging a resize handle (sidebar edge / pane divider) that overlaps the terminal must
        // not start a text selection, even with no mouse tracking active.
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 80.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let handle =
            egui::Rect::from_min_size(egui::Pos2::new(4.0, 0.0), egui::Vec2::new(8.0, 80.0));
        let press_pos = egui::Pos2::new(8.0, 10.0);
        assert!(surface.rect.contains(press_pos));
        assert!(handle.contains(press_pos));
        let events = vec![
            egui::Event::PointerButton {
                pos: press_pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::PointerMoved(egui::Pos2::new(40.0, 10.0)),
        ];

        let (_, selection_actions, router) = route_selection_test_events(
            events,
            TerminalSelectionRouteContext {
                surface: Some(surface),
                view: ViewTransform::IDENTITY,
                mouse_tracking: false,
                frame_modifiers: egui::Modifiers::default(),
                chrome_handle_rects: &[handle],
            },
        );

        assert!(selection_actions.is_empty());
        assert!(!router.is_active());
    }

    #[test]
    fn plain_mouse_drag_stays_available_for_terminal_mouse_reporting() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 80.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let events = vec![egui::Event::PointerButton {
            pos: egui::Pos2::new(10.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        }];
        let original = events.clone();

        let (terminal_events, selection_actions, router) = route_selection_test_events(
            events,
            TerminalSelectionRouteContext {
                surface: Some(surface),
                view: ViewTransform::IDENTITY,
                mouse_tracking: true,
                frame_modifiers: egui::Modifiers::default(),
                chrome_handle_rects: &[],
            },
        );

        assert_eq!(terminal_events, original);
        assert!(selection_actions.is_empty());
        assert!(!router.is_active());
    }

    #[test]
    fn plain_mouse_drag_starts_selection_when_mouse_reporting_is_inactive() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 80.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let press = egui::Event::PointerButton {
            pos: egui::Pos2::new(10.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        };
        let motion = egui::Event::PointerMoved(egui::Pos2::new(20.0, 10.0));
        let release = egui::Event::PointerButton {
            pos: egui::Pos2::new(20.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        };
        let events = vec![press.clone(), motion.clone(), release.clone()];

        let (terminal_events, selection_actions, router) = route_selection_test_events(
            events,
            TerminalSelectionRouteContext {
                surface: Some(surface),
                view: ViewTransform::IDENTITY,
                mouse_tracking: false,
                frame_modifiers: egui::Modifiers::default(),
                chrome_handle_rects: &[],
            },
        );

        assert_eq!(terminal_events, vec![press, motion, release]);
        assert_eq!(selection_actions.len(), 3);
        assert!(matches!(
            selection_actions[0],
            TerminalSelectionAction::Begin(_)
        ));
        assert!(matches!(
            selection_actions[2],
            TerminalSelectionAction::End(_)
        ));
        assert!(!router.is_active());
    }

    #[test]
    fn shift_drag_overrides_mouse_reporting_for_bootty_selection() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 80.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let events = vec![egui::Event::PointerButton {
            pos: egui::Pos2::new(10.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: shift,
        }];

        let (terminal_events, selection_actions, router) = route_selection_test_events(
            events,
            TerminalSelectionRouteContext {
                surface: Some(surface),
                view: ViewTransform::IDENTITY,
                mouse_tracking: true,
                frame_modifiers: shift,
                chrome_handle_rects: &[],
            },
        );
        assert!(terminal_events.is_empty());
        assert_eq!(selection_actions.len(), 1);
        assert!(matches!(
            selection_actions[0],
            TerminalSelectionAction::Begin(_)
        ));
        assert!(router.is_active());
    }

    #[test]
    fn frame_shift_overrides_mouse_reporting_when_pointer_event_lacks_modifiers() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 80.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let events = vec![egui::Event::PointerButton {
            pos: egui::Pos2::new(10.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        }];

        let (terminal_events, selection_actions, router) = route_selection_test_events(
            events,
            TerminalSelectionRouteContext {
                surface: Some(surface),
                view: ViewTransform::IDENTITY,
                mouse_tracking: true,
                frame_modifiers: shift,
                chrome_handle_rects: &[],
            },
        );
        assert!(terminal_events.is_empty());
        assert_eq!(selection_actions.len(), 1);
        assert!(matches!(
            selection_actions[0],
            TerminalSelectionAction::Begin(_)
        ));
        assert!(router.is_active());
    }

    #[test]
    fn command_c_is_detected_as_copy_shortcut_for_selection_override() {
        assert!(copy_shortcut_pressed(&egui::Event::Key {
            key: egui::Key::C,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                command: true,
                mac_cmd: true,
                ..Default::default()
            },
        }));
        assert!(!copy_shortcut_pressed(&egui::Event::Key {
            key: egui::Key::C,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                ctrl: true,
                ..Default::default()
            },
        }));
    }

    #[test]
    fn direct_command_c_is_detected_as_copy_shortcut_for_selection_override() {
        assert!(direct_copy_shortcut_pressed(KeyInput {
            key: TerminalKey::C,
            mods: crate::terminal::KeyMods {
                command: true,
                ..Default::default()
            },
            repeat: false,
            utf8: Some("c"),
            unshifted: Some('c'),
        }));
        assert!(!direct_copy_shortcut_pressed(KeyInput {
            key: TerminalKey::C,
            mods: crate::terminal::KeyMods {
                ctrl: true,
                ..Default::default()
            },
            repeat: false,
            utf8: Some("c"),
            unshifted: Some('c'),
        }));
    }

    #[test]
    fn mouse_shape_side_effect_maps_common_cursor_names() {
        assert_eq!(
            terminal_cursor_icon_for_mouse_shape("shape=pointing_hand"),
            Some(egui::CursorIcon::PointingHand)
        );
        assert_eq!(
            terminal_cursor_icon_for_mouse_shape("ew-resize"),
            Some(egui::CursorIcon::ResizeHorizontal)
        );
        assert_eq!(
            terminal_cursor_icon_for_mouse_shape("not-a-known-cursor"),
            None
        );
    }

    #[test]
    fn terminal_typing_hides_mouse_pointer_until_pointer_moves() {
        let mut state = test_state();
        state.terminal_cursor_icon = egui::CursorIcon::PointingHand;
        let mut effects = Vec::new();

        state.apply_terminal_input(TerminalInputCommand::Text("x".to_owned()), &mut effects);

        assert_eq!(
            effects,
            vec![AppEffect::SetTerminalCursorIcon(egui::CursorIcon::None)]
        );

        effects.clear();
        state.restore_mouse_pointer_after_pointer_moved(
            &[egui::Event::PointerMoved(egui::Pos2::new(1.0, 1.0))],
            Some(egui::Pos2::new(1.0, 1.0)),
            &mut effects,
        );

        assert_eq!(
            effects,
            vec![AppEffect::SetTerminalCursorIcon(
                egui::CursorIcon::PointingHand
            )]
        );
    }

    #[test]
    fn terminal_typing_restores_mouse_pointer_when_hover_position_changes_without_event() {
        let mut state = test_state();
        state.terminal_cursor_icon = egui::CursorIcon::Text;
        state.last_mouse_hover_pos = Some(egui::Pos2::new(1.0, 1.0));
        let mut effects = Vec::new();

        state.apply_terminal_input(TerminalInputCommand::Text("x".to_owned()), &mut effects);
        effects.clear();

        state.restore_mouse_pointer_after_pointer_moved(
            &[],
            Some(egui::Pos2::new(2.0, 1.0)),
            &mut effects,
        );

        assert_eq!(
            effects,
            vec![AppEffect::SetTerminalCursorIcon(egui::CursorIcon::Text)]
        );
    }

    #[test]
    fn hide_mouse_pointer_while_typing_setting_can_disable_typing_hide() {
        let mut state = test_state_with_config(|config| {
            config.input.hide_mouse_pointer_while_typing = false;
        });
        let mut effects = Vec::new();

        state.apply_terminal_input(TerminalInputCommand::Text("x".to_owned()), &mut effects);

        assert!(effects.is_empty());
    }

    #[test]
    fn bell_side_effect_requests_host_bell() {
        let mut state = test_state();
        let mut effects = Vec::new();

        state.apply_terminal_side_effect_event(
            TerminalSideEffectEvent::unscoped(TerminalSideEffect::Bell),
            &mut effects,
            10.0,
            20.0,
            1.0,
        );

        assert_eq!(effects, vec![AppEffect::Bell]);
    }

    #[test]
    fn report_variable_response_returns_selected_session_name() {
        assert_eq!(
            terminal_report_variable_response("session.name", Some("local")),
            Some(encode_iterm2_report_variable("local"))
        );
    }

    #[test]
    fn report_variable_response_ignores_unknown_variables() {
        assert_eq!(
            terminal_report_variable_response("user.missing", Some("local")),
            None
        );
    }

    #[test]
    fn default_fullscreen_config_toggles_native_fullscreen() {
        let config = BoottyConfig::default();

        assert!(should_toggle_native_fullscreen(&config.window));
    }

    #[test]
    fn appkit_handled_non_native_fullscreen_toggles_tracked_state() {
        assert!(!next_non_native_fullscreen_state(true, true, false));
        assert!(next_non_native_fullscreen_state(true, false, false));
    }

    #[test]
    fn viewport_handled_non_native_fullscreen_toggles_maximized_state() {
        assert!(!next_non_native_fullscreen_state(false, false, true));
        assert!(next_non_native_fullscreen_state(false, true, false));
    }

    #[test]
    fn non_native_fullscreen_config_toggles_non_native_fullscreen() {
        let mut config = BoottyConfig::default();
        config.window.fullscreen = WindowFullscreen::NonNative;

        assert!(!should_toggle_native_fullscreen(&config.window));
    }

    #[test]
    fn external_mux_backends_schedule_frequent_refresh_repaints() {
        let mut config = BoottyConfig::default();
        assert_eq!(mux_refresh_repaint_after(&config.multiplexer), None);

        config.multiplexer.backend = MultiplexerBackendConfig::Zellij;

        assert_eq!(
            mux_refresh_repaint_after(&config.multiplexer),
            Some(MUX_SESSION_REFRESH_INTERVAL)
        );
        assert!(MUX_SESSION_REFRESH_INTERVAL <= Duration::from_millis(250));

        config.multiplexer.backend = MultiplexerBackendConfig::Tmux;
        assert_eq!(
            mux_refresh_repaint_after(&config.multiplexer),
            if cfg!(windows) {
                None
            } else {
                Some(MUX_SESSION_REFRESH_INTERVAL)
            }
        );
    }

    #[test]
    fn new_mux_session_request_uses_configured_working_directory() {
        let mut config = BoottyConfig::default();
        config.session.working_directory = Some("tmp/bootty-project".into());

        let request = new_mux_session_request_with_name(&config, "review-session");

        assert_eq!(request.session_id, "review-session");
        assert_eq!(request.cwd, "tmp/bootty-project");
    }

    #[test]
    fn new_mux_session_request_defaults_to_home_working_directory() {
        let config = BoottyConfig::default();
        let expected_home = crate::config::default_working_directory()
            .expect("home directory should be discoverable");

        let request = new_mux_session_request_with_name(&config, "home-session");

        assert_eq!(request.session_id, "home-session");
        assert_eq!(request.cwd, expected_home.to_string_lossy().as_ref());
    }

    #[test]
    fn mux_command_cwd_prefers_live_osc7_directory_over_snapshot_anchor() {
        assert_eq!(
            terminal_cwd_for_mux_command(
                Some("file://host/Users/me/project%20space".to_owned()),
                Some("/old".to_owned()),
            ),
            Some("/Users/me/project space".to_owned())
        );
        assert_eq!(
            terminal_cwd_for_mux_command(None, Some("/fallback".to_owned())),
            Some("/fallback".to_owned())
        );
    }

    #[test]
    fn new_window_action_opens_new_session_picker() {
        let mut state = test_state();
        let mut effects = Vec::new();

        state.apply_keybind_action(
            KeybindAction::App(AppAction::NewWindow),
            ViewportSnapshot::default(),
            &mut effects,
        );

        assert!(state.take_dialog().is_some());
    }

    fn test_frame_inputs(events: Vec<egui::Event>, hover_pos: Option<egui::Pos2>) -> FrameInputs {
        FrameInputs {
            now: Instant::now(),
            stable_dt_ms: 16.0,
            events,
            dropped_file_paths: Vec::new(),
            modifiers: egui::Modifiers::default(),
            hover_pos,
            pressed_mouse_button: None,
            viewport: ViewportSnapshot::default(),
            renderer_metrics: RendererMetrics::default(),
            terminal_cell_width: 10.0,
            terminal_cell_height: 20.0,
            terminal_scale_factor: 1.0,
            terminal_view_transform: ViewTransform::IDENTITY,
        }
    }

    fn test_state() -> AppState {
        test_state_with_config(|_| {})
    }

    fn session_with(id: &str, name: &str, cwd: &str) -> MuxSession {
        MuxSession {
            id: id.to_owned(),
            name: name.to_owned(),
            active: true,
            anchor: MuxPaneAnchor {
                session_id: id.to_owned(),
                pane_id: Some(format!("{id}-pane")),
                cwd: Some(cwd.to_owned()),
                process: None,
            },
            active_window_id: None,
            windows: Vec::new(),
        }
    }

    #[test]
    fn generated_name_sync_skips_unchanged_sessions_and_reruns_on_change() {
        // Guards the fix for the per-frame `git` fork: the reconciler must not repeat its
        // per-session worktree lookups while the session set is unchanged, but must re-run when a
        // session's name or cwd changes so generated names stay current.
        let mut state = test_state();
        let sessions = vec![session_with("s1", "alpha", "/repo/alpha")];

        assert!(
            state.generated_names_need_sync(&sessions),
            "first observation of a session set must reconcile"
        );
        assert!(
            !state.generated_names_need_sync(&sessions),
            "an unchanged session set must be skipped (no per-frame git forks)"
        );

        let renamed = vec![session_with("s1", "beta", "/repo/alpha")];
        assert!(
            state.generated_names_need_sync(&renamed),
            "a session rename must trigger reconciliation"
        );

        let moved = vec![session_with("s1", "beta", "/repo/beta")];
        assert!(
            state.generated_names_need_sync(&moved),
            "a session cwd change must trigger reconciliation"
        );
        assert!(
            !state.generated_names_need_sync(&moved),
            "reconciliation must settle again once the session set stops changing"
        );
    }

    fn test_state_with_config(mutate: impl FnOnce(&mut BoottyConfig)) -> AppState {
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-test-{unique}"));
        std::fs::create_dir_all(&config_dir).expect("create app state test config dir");
        let mut config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        mutate(&mut config);
        AppState::new(config, repaint, None, None).expect("state")
    }

    fn test_binding_runtime(scope: MuxScope) -> BindingRuntime {
        let config = BoottyConfig::default();
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        BindingRuntime::new(scope, &config, AppearanceVariant::Dark, repaint)
    }

    #[test]
    fn binding_runtimes_isolate_overlapping_layout_progress_and_terminal_target_identity() {
        let mut first = test_binding_runtime(MuxScope::new(
            SpaceId::from_persistence(1),
            BindingId::from_persistence(10),
        ));
        let mut second = test_binding_runtime(MuxScope::new(
            SpaceId::from_persistence(1),
            BindingId::from_persistence(20),
        ));
        let first_window = first.window_id("$1".to_owned(), "@1".to_owned());
        let second_window = second.window_id("$1".to_owned(), "@1".to_owned());
        let first_pane = first.pane_id(first_window.clone(), "%1");
        let second_pane = second.pane_id(second_window.clone(), "%1");
        let first_transition =
            scoped_terminal_transition_key(first.scope, MultiplexerBackendConfig::Tmux, "$1", Some("%1"));
        let second_transition =
            scoped_terminal_transition_key(second.scope, MultiplexerBackendConfig::Tmux, "$1", Some("%1"));

        first
            .terminal_side_effect_tx
            .send(TerminalSideEffectEvent::new(
                Some("%1".to_owned()),
                TerminalSideEffect::WindowTitle("first title".to_owned()),
            ))
            .expect("send first binding side effect");

        first
            .pane_layouts
            .insert(first_window.clone(), PaneLayout::single("%1".to_owned()));
        second
            .pane_layouts
            .insert(second_window.clone(), PaneLayout::single("%1".to_owned()));
        first.terminal_progress.insert(
            first_pane.clone(),
            TerminalProgress::from_conemu("normal", Some(25)).expect("progress"),
        );
        second.terminal_progress.insert(
            second_pane.clone(),
            TerminalProgress::from_conemu("error", Some(75)).expect("progress"),
        );
        first.mux.set_error(Some("first failed".to_owned()));
        assert_eq!(
            first
                .terminal_side_effect_rx
                .try_recv()
                .expect("first binding side effect")
                .effect,
            TerminalSideEffect::WindowTitle("first title".to_owned())
        );

        assert!(matches!(
            second.terminal_side_effect_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        first.pane_layouts.remove(&first_window);
        first.terminal_progress.remove(&first_pane);

        assert_ne!(first_window, second_window);
        assert_ne!(first_pane, second_pane);
        assert_ne!(first_transition, second_transition);
        assert!(first.pane_layouts.is_empty());
        assert!(first.terminal_progress.is_empty());
        assert!(second.pane_layouts.contains_key(&second_window));
        assert_eq!(second.terminal_progress[&second_pane].percent(), Some(75));
        assert_eq!(first.mux.last_error(), Some("first failed"));
        assert_eq!(second.mux.last_error(), None);
    }

    #[test]
    fn switching_bindings_updates_backend_specific_keybindings_and_render_policy() {
        let mut state = test_state_with_config(|config| {
            config.input.keybind.clear();
            config.input.backend_keybinds.native = vec!["f1=next_tab".to_owned()];
            config.input.backend_keybinds.tmux = vec!["f1=previous_tab".to_owned()];
        });
        assert_eq!(
            state.app_key_bindings.action_for_key_with_modifier_sides(
                egui::Key::F1,
                egui::Modifiers::NONE,
                ModifierSideState::default(),
            ),
            Some(KeybindAction::Mux(MuxKeyAction::NextTab))
        );
        let remote_scope = MuxScope::new(
            state.binding.scope.space_id(),
            BindingId::from_persistence(
                state
                    .binding
                    .scope
                    .binding_id()
                    .persistence_value()
                    .saturating_add(1000),
            ),
        );
        let mut remote = BindingRuntime::new(
            remote_scope,
            state.config(),
            state.active_appearance_variant,
            state.repaint.clone(),
        );
        let native_config = remote.multiplexer.clone();
        remote.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: "$1".to_owned(),
                cwd: std::env::temp_dir().to_string_lossy().into_owned(),
            },
            &state.repaint,
            &native_config,
        );
        remote.multiplexer.backend = crate::config::MultiplexerBackendConfig::Tmux;
        state.inactive_bindings.push(remote);

        assert!(
            state.activate_scoped_session_from_ui(&ScopedSessionTarget::new(remote_scope, "$1",))
        );

        assert_eq!(
            state.app_key_bindings.action_for_key_with_modifier_sides(
                egui::Key::F1,
                egui::Modifiers::NONE,
                ModifierSideState::default(),
            ),
            Some(KeybindAction::Mux(MuxKeyAction::PreviousTab))
        );
        assert_eq!(
            state.multiplexer_backend(),
            crate::config::MultiplexerBackendConfig::Tmux
        );
        assert!(!state.uses_native_terminal_layout());
    }

    #[test]
    fn inactive_binding_refresh_applies_its_own_persisted_session_order() {
        let mut state = test_state();
        let remote_scope = MuxScope::new(
            state.binding.scope.space_id(),
            BindingId::from_persistence(
                state
                    .binding
                    .scope
                    .binding_id()
                    .persistence_value()
                    .saturating_add(1000),
            ),
        );
        let mut remote = BindingRuntime::new(
            remote_scope,
            state.config(),
            state.active_appearance_variant,
            state.repaint.clone(),
        );
        let first = format!("inactive-order-a-{}", unique_test_id());
        let second = format!("inactive-order-b-{}", unique_test_id());
        let remote_config = remote.multiplexer.clone();
        for session_id in [&first, &second] {
            remote.mux.create_project_session(
                crate::mux::controller::NewMuxSessionRequest {
                    session_id: session_id.clone(),
                    cwd: std::env::temp_dir().to_string_lossy().into_owned(),
                },
                &state.repaint,
                &remote_config,
            );
        }
        assert!(
            remote
                .session_order
                .move_session(&second, -1, [first.as_str(), second.as_str()],)
        );
        state.inactive_bindings.push(remote);

        state.update_frame(test_frame_inputs(Vec::new(), None));

        let remote_sessions = state
            .binding_session_groups()
            .into_iter()
            .find(|group| group.scope == remote_scope)
            .expect("remote binding group")
            .sessions;
        let first_index = remote_sessions
            .iter()
            .position(|session| session.id == first)
            .expect("first session");
        let second_index = remote_sessions
            .iter()
            .position(|session| session.id == second)
            .expect("second session");
        assert!(second_index < first_index);
    }

    #[test]
    fn scoped_sidebar_navigation_routes_colliding_ids_without_resetting_other_binding() {
        let mut state = test_state();
        state.binding.label = "Local".to_owned();
        let local_scope = state.binding.scope;
        let remote_scope = MuxScope::new(
            local_scope.space_id(),
            BindingId::from_persistence(
                local_scope
                    .binding_id()
                    .persistence_value()
                    .saturating_add(1000),
            ),
        );
        let mut remote = BindingRuntime::new(
            remote_scope,
            state.config(),
            state.active_appearance_variant,
            state.repaint.clone(),
        );
        remote.label = "Remote".to_owned();
        let local_config = state.binding.multiplexer.clone();
        for session_id in ["$1", "$2"] {
            state.binding.mux.create_project_session(
                crate::mux::controller::NewMuxSessionRequest {
                    session_id: session_id.to_owned(),
                    cwd: std::env::temp_dir().to_string_lossy().into_owned(),
                },
                &state.repaint,
                &local_config,
            );
        }
        state.binding.mux.activate_session("$2");
        let remote_config = remote.multiplexer.clone();
        remote.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: "$1".to_owned(),
                cwd: std::env::temp_dir().to_string_lossy().into_owned(),
            },
            &state.repaint,
            &remote_config,
        );
        state.inactive_bindings.push(remote);

        let groups = state.binding_session_groups();
        assert_eq!(groups.len(), 2);
        assert!(
            groups
                .iter()
                .all(|group| group.sessions.iter().any(|session| session.id == "$1"))
        );
        assert!(
            groups
                .iter()
                .find(|group| group.scope == local_scope)
                .is_some_and(|group| group.can_return_to_last_session)
        );
        assert!(
            groups
                .iter()
                .find(|group| group.scope == remote_scope)
                .is_some_and(|group| !group.can_return_to_last_session)
        );

        let remote_target = ScopedSessionTarget::new(remote_scope, "$1");
        assert!(state.activate_scoped_session_from_ui(&remote_target));
        assert_eq!(state.mux_scope(), remote_scope);
        assert_eq!(state.mux().selected_session(), Some("$1"));
        let local = state
            .inactive_bindings
            .iter()
            .find(|binding| binding.scope == local_scope)
            .expect("local binding remains live");
        assert_eq!(local.mux.selected_session(), Some("$2"));

        let targets = state.session_navigation_targets();
        let remote_index = targets
            .iter()
            .position(|target| target == &remote_target)
            .expect("remote target is keyboard navigable");
        let previous_index = (remote_index + targets.len() - 1) % targets.len();
        state.sidebar_hovered_session = Some(targets[previous_index].clone());
        state.move_sidebar_hover(1);
        assert_eq!(state.sidebar_hovered_session.as_ref(), Some(&remote_target));
        state.activate_sidebar_hovered_session();
        assert_eq!(state.mux_scope(), remote_scope);
    }

    #[test]
    fn startup_restores_all_bindings_for_grouped_navigation() {
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-multi-binding-{unique}"));
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        let config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        let workspace = WorkspaceStore::for_config_path(&config.config_path, &config.multiplexer);
        let space_id = workspace
            .binding()
            .expect("default binding")
            .mux_scope()
            .space_id()
            .persistence_value();
        let conn = crate::workspace::open_db(workspace.path()).expect("open workspace database");
        conn.execute(
            "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![space_id, "Default Binding", "native", 0_i64],
        )
        .expect("insert remote binding");
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});

        let state = AppState::new(config, repaint, None, None).expect("state");
        let groups = state.binding_session_groups();

        assert_eq!(state.binding_count(), 2);
        assert_eq!(groups.len(), 2);
        assert!(groups[0].label.starts_with("Default Binding / Binding "));
        assert!(groups[1].label.starts_with("Default Binding / Binding "));
        assert_ne!(groups[0].label, groups[1].label);
        assert!(groups[0].active);
        assert!(!groups[1].active);
    }

    #[test]
    fn native_startup_waits_for_user_to_open_first_session() {
        let state = test_state();

        assert!(
            state.binding.mux.sessions().is_empty(),
            "startup must not open a session before the user asks for one"
        );
        assert_eq!(state.binding.mux.selected_session(), None);
    }

    fn sync_initial_native_terminal(state: &mut AppState) {
        let mux_config = state.config_state.current().multiplexer.clone();
        if let Some(error) = state
            .binding
            .mux
            .refresh_sessions(&state.repaint, &mux_config)
        {
            panic!("initial native mux refresh failed: {error}");
        }
        state
            .sync_terminal_panes()
            .expect("initial native terminal sync");
    }

    fn key_event(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn sidebar_keybinds_map_configured_navigation_without_default_escape() {
        let bindings =
            SidebarKeyBindings::from_keybinds(&BoottyConfig::default().input.sidebar_keybind)
                .expect("default sidebar keybinds");

        assert_eq!(
            bindings.action_for_key(egui::Key::J, egui::Modifiers::NONE),
            Some(SidebarAction::NextSession)
        );
        assert_eq!(
            bindings.action_for_key(egui::Key::ArrowUp, egui::Modifiers::NONE),
            Some(SidebarAction::PreviousSession)
        );
        assert_eq!(
            bindings.action_for_key(
                egui::Key::N,
                egui::Modifiers {
                    ctrl: true,
                    ..Default::default()
                }
            ),
            Some(SidebarAction::NextSession)
        );
        assert_eq!(
            bindings.action_for_key(egui::Key::Enter, egui::Modifiers::NONE),
            Some(SidebarAction::ActivateSession)
        );
        assert_eq!(
            bindings.action_for_key(egui::Key::Escape, egui::Modifiers::NONE),
            None
        );
    }

    #[test]
    fn pane_widget_key_namespaces_same_pane_id_by_session_and_window() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_a = format!("widget-a-{}", unique_test_id());
        let session_b = format!("widget-b-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_a.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        let key_a = state.pane_widget_key("pane-1");
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_b,
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        let key_b = state.pane_widget_key("pane-1");

        assert_ne!(key_a, key_b);
        assert!(key_a.contains(&session_a));
    }

    #[test]
    fn sidebar_focus_consumes_keys_and_enter_returns_terminal_focus() {
        let mut state = test_state();
        state.input_focus = InputFocus::Sidebar;

        assert_eq!(
            state.handle_sidebar_input(vec![
                key_event(egui::Key::J, egui::Modifiers::NONE),
                egui::Event::Text("j".to_owned()),
            ]),
            2
        );
        assert_eq!(state.input_focus, InputFocus::Sidebar);

        assert_eq!(
            state.handle_sidebar_input(vec![key_event(egui::Key::Escape, egui::Modifiers::NONE)]),
            1
        );
        assert_eq!(state.input_focus, InputFocus::Sidebar);

        assert_eq!(
            state.handle_sidebar_input(vec![key_event(egui::Key::Enter, egui::Modifiers::NONE)]),
            1
        );
        assert_eq!(state.input_focus, InputFocus::Terminal);
    }

    #[test]
    fn prune_pane_layouts_drops_dead_sessions_but_keeps_the_active_window() {
        let mut state = test_state();
        let current = state.current_window_key();
        let ghost = state
            .binding
            .window_id("ghost-session".to_owned(), "@9".to_owned());
        state
            .binding
            .pane_layouts
            .insert(current.clone(), PaneLayout::single("p1".to_owned()));
        state
            .binding
            .pane_layouts
            .insert(ghost.clone(), PaneLayout::single("p2".to_owned()));

        state.prune_pane_layouts();

        assert!(
            state.binding.pane_layouts.contains_key(&current),
            "active window's layout must survive pruning"
        );
        assert!(
            !state.binding.pane_layouts.contains_key(&ghost),
            "layout for a session that no longer exists must be reclaimed"
        );
    }

    #[test]
    fn native_layout_reconcile_keeps_local_focus_when_server_anchor_is_stale() {
        assert_eq!(
            focus_after_native_layout_reconcile(false, &[], Some("%1")),
            None,
            "refreshes must not let a stale rmux active-pane anchor overwrite Bootty focus"
        );
    }

    #[test]
    fn native_layout_reconcile_focuses_new_or_restored_server_pane() {
        assert_eq!(
            focus_after_native_layout_reconcile(true, &[], Some("%2")),
            Some("%2".to_owned())
        );
        assert_eq!(
            focus_after_native_layout_reconcile(false, &["%2".to_owned()], Some("%2")),
            Some("%2".to_owned())
        );
        assert_eq!(
            focus_after_native_layout_reconcile(false, &["%2".to_owned()], Some("%1")),
            Some("%2".to_owned())
        );
    }

    #[test]
    fn native_new_tab_command_syncs_terminal_before_next_frame() {
        let mut state = test_state_with_config(|config| {
            config.session.shell = Some("/usr/bin/true".to_owned());
        });
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        let previous = state
            .binding
            .terminal
            .focused_pane_id()
            .map(str::to_owned)
            .expect("first native tab focused pane");

        state.apply_mux_key_action(MuxKeyAction::NewTab);

        let selected = state
            .binding
            .mux
            .selected_session_anchor()
            .and_then(|anchor| anchor.pane_id.as_deref())
            .map(str::to_owned)
            .expect("new tab selected pane");
        assert_eq!(
            state.binding.terminal.focused_pane_id(),
            Some(selected.as_str())
        );
        assert_ne!(selected, previous);
    }

    #[test]
    fn native_session_activation_syncs_terminal_before_next_frame() {
        let mut state = test_state_with_config(|config| {
            config.session.shell = Some("/usr/bin/true".to_owned());
        });
        sync_initial_native_terminal(&mut state);
        let mux_config = state.config().multiplexer.clone();
        let session_a = format!("native-a-{}", unique_test_id());
        let session_b = format!("native-b-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_a.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.sync_native_layout_terminal_now();
        let first_pane = state
            .binding
            .terminal
            .focused_pane_id()
            .map(str::to_owned)
            .expect("first focused pane");
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_b,
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.sync_native_layout_terminal_now();
        let second_pane = state
            .binding
            .terminal
            .focused_pane_id()
            .map(str::to_owned)
            .expect("second focused pane");
        assert_ne!(second_pane, first_pane);

        state.activate_session_from_ui(&session_a);

        assert_eq!(
            state.binding.terminal.focused_pane_id(),
            Some(first_pane.as_str())
        );
    }

    #[test]
    #[ignore = "requires an rmux binary; set RMUX_TMPDIR to isolate the daemon"]
    fn rmux_live_app_state_session_and_tab_activation_stay_interactive() -> Result<()> {
        std::env::var_os("RMUX_TMPDIR")
            .context("set RMUX_TMPDIR to an empty temporary directory before running this test")?;
        use crate::mux::rmux::{RmuxSessionClient, SdkRmuxClient};

        let client = SdkRmuxClient::new();
        let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
        let session_a = format!("bootty-app-perf-a-{}", std::process::id());
        let session_b = format!("bootty-app-perf-b-{}", std::process::id());
        client.ensure_session(&session_a, &cwd)?;
        client.ensure_session(&session_b, &cwd)?;
        client.new_window(&session_a, Some(&cwd))?;
        client.new_window(&session_a, Some(&cwd))?;
        client.new_window(&session_b, Some(&cwd))?;

        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Rmux;
        });
        let refresh_start = Instant::now();
        let deadline = refresh_start + Duration::from_secs(5);
        loop {
            let mux_config = state.config_state.current().multiplexer.clone();
            if let Some(error) = state
                .binding
                .mux
                .refresh_sessions(&state.repaint, &mux_config)
            {
                anyhow::bail!(error);
            }
            if state
                .binding
                .mux
                .sessions()
                .iter()
                .any(|session| session.id == session_a)
                && state
                    .binding
                    .mux
                    .sessions()
                    .iter()
                    .any(|session| session.id == session_b)
            {
                break;
            }
            if Instant::now() >= deadline {
                anyhow::bail!("timed out waiting for rmux app-state snapshot");
            }
            thread::sleep(Duration::from_millis(10));
        }
        let refresh_elapsed = refresh_start.elapsed();

        let session_start = Instant::now();
        state.activate_session_from_ui(&session_b);
        state.sync_terminal_panes()?;
        let session_elapsed = session_start.elapsed();

        let window_id = state
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_a)
            .and_then(|session| session.windows.get(1))
            .map(|window| window.id.clone())
            .context("app perf target tab should exist")?;
        let tab_start = Instant::now();
        state.activate_window_from_ui(&session_a, &window_id);
        state.sync_terminal_panes()?;
        let tab_elapsed = tab_start.elapsed();

        eprintln!(
            "rmux app-state perf probe: refresh={refresh_elapsed:?} session={session_elapsed:?} tab={tab_elapsed:?}"
        );

        client.kill_session(&session_a)?;
        client.kill_session(&session_b)?;
        let _ = std::process::Command::new("rmux")
            .arg("kill-server")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        assert!(
            session_elapsed < Duration::from_millis(100),
            "app-state rmux session activation should not block: {session_elapsed:?}"
        );
        assert!(
            tab_elapsed < Duration::from_millis(100),
            "app-state rmux tab activation should not block: {tab_elapsed:?}"
        );
        Ok(())
    }

    #[test]
    fn pending_pane_split_direction_survives_window_id_materialization() {
        let mut state = test_state();
        let pending = state
            .binding
            .window_id("rmux-session".to_owned(), String::new());
        state
            .binding
            .pending_pane_split_directions
            .insert(pending, SplitDirection::Down);
        let materialized = state
            .binding
            .window_id("rmux-session".to_owned(), "@1".to_owned());

        let direction = state.take_pending_pane_split_direction(&materialized);

        assert_eq!(direction, Some(SplitDirection::Down));
        assert!(state.binding.pending_pane_split_directions.is_empty());
    }

    #[test]
    fn rmux_split_layout_defers_when_selected_anchor_is_still_old_pane() {
        let mut state = test_state();
        let key = state
            .binding
            .window_id("rmux-session".to_owned(), "@1".to_owned());
        state
            .binding
            .pane_layouts
            .insert(key.clone(), PaneLayout::single("%1".to_owned()));

        state.apply_split_layout_after_command(
            key.clone(),
            Some("%1".to_owned()),
            SplitDirection::Down,
            MultiplexerBackendConfig::Rmux,
        );

        assert_eq!(
            state.take_pending_pane_split_direction(&key),
            Some(SplitDirection::Down)
        );
        assert_eq!(
            state.binding.pane_layouts.get(&key).map(PaneLayout::panes),
            Some(vec!["%1".to_owned()])
        );
    }

    #[test]
    fn direct_input_suppression_tracks_terminal_ownership() {
        let mut state = test_state();

        assert!(state.direct_input_suppresses_egui_events());

        state.apply_keybind_action(
            KeybindAction::App(AppAction::ToggleSidebarFocus),
            ViewportSnapshot::default(),
            &mut Vec::new(),
        );
        assert!(!state.direct_input_suppresses_egui_events());

        state.apply_keybind_action(
            KeybindAction::App(AppAction::SessionPicker),
            ViewportSnapshot::default(),
            &mut Vec::new(),
        );
        assert!(!state.direct_input_suppresses_egui_events());
    }

    #[test]
    fn last_session_toggles_bootty_selected_session() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: "local".to_owned(),
                cwd: ".".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: "project".to_owned(),
                cwd: "repo".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );

        state.activate_session_from_ui("local");
        state.activate_session_from_ui("project");
        state.apply_mux_key_action(MuxKeyAction::LastSession);
        assert_eq!(state.binding.mux.selected_session(), Some("local"));

        state.apply_mux_key_action(MuxKeyAction::LastSession);
        assert_eq!(state.binding.mux.selected_session(), Some("project"));
    }

    #[test]
    fn last_session_without_a_prior_session_is_a_no_op_not_a_panic() {
        // A fresh state has only the initial session and no previous selection; last_session must be
        // consumed silently instead of falling through to the command builder's `unreachable!`.
        let mut state = test_state();
        let before = state.binding.mux.selected_session().map(str::to_owned);
        state.apply_mux_key_action(MuxKeyAction::LastSession);
        assert_eq!(
            state.binding.mux.selected_session().map(str::to_owned),
            before
        );
    }

    #[test]
    fn context_session_commands_open_their_picker_or_navigate_the_active_session() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let first = format!("context-session-command-first-{}", unique_test_id());
        let second = format!("context-session-command-second-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: first.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: second.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.activate_session_from_ui(&second);

        assert!(state.open_new_session_dialog_from_ui());
        assert!(state.take_dialog().is_some());
        assert_eq!(state.binding.mux.selected_session(), Some(second.as_str()));

        assert!(state.open_session_picker_dialog_from_ui());
        assert!(state.take_session_picker_dialog().is_some());
        assert_eq!(state.binding.mux.selected_session(), Some(second.as_str()));

        assert!(state.activate_relative_session_from_ui(&second, -1));
        assert_ne!(state.binding.mux.selected_session(), Some(second.as_str()));

        assert!(state.activate_last_session_from_ui());
        assert_eq!(state.binding.mux.selected_session(), Some(second.as_str()));
    }

    #[test]
    fn context_session_navigation_anchors_to_the_clicked_session() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let unique = unique_test_id();
        let first = format!("context-session-first-{unique}");
        let clicked = format!("context-session-clicked-{unique}");
        let next = format!("context-session-next-{unique}");
        for session_id in [&first, &clicked, &next] {
            state.binding.mux.create_project_session(
                crate::mux::controller::NewMuxSessionRequest {
                    session_id: (*session_id).clone(),
                    cwd: "/tmp".to_owned(),
                },
                &state.repaint,
                &mux_config,
            );
        }
        state.activate_session_from_ui(&first);
        let sessions = state.binding.mux.sessions();
        let clicked_index = sessions
            .iter()
            .position(|session| session.id == clicked)
            .expect("clicked session is present");
        let selected_index = sessions
            .iter()
            .position(|session| session.id == first)
            .expect("selected session is present");
        let expected_clicked_next = sessions[(clicked_index + 1) % sessions.len()].id.clone();
        let selected_next = sessions[(selected_index + 1) % sessions.len()].id.clone();
        assert_ne!(expected_clicked_next, selected_next);

        assert!(state.activate_relative_session_from_ui(&clicked, 1));

        assert_eq!(
            state.binding.mux.selected_session(),
            Some(expected_clicked_next.as_str())
        );
    }

    #[test]
    fn move_session_reorders_bootty_owned_session_order() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let unique = unique_test_id();
        let alpha = format!("alpha-{unique}");
        let beta = format!("beta-{unique}");
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: alpha.clone(),
                cwd: "repo/a".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: beta.clone(),
                cwd: "repo/b".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );

        assert!(state.binding.session_order.move_session(
            &beta,
            -1,
            [alpha.as_str(), beta.as_str()],
        ));
        let ordered = state
            .binding
            .session_order
            .sync_sessions([alpha.as_str(), beta.as_str()]);

        assert_eq!(ordered, vec![beta, alpha]);
    }

    #[test]
    fn context_rename_session_targets_the_clicked_inactive_session() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let first = format!("context-session-first-{}", unique_test_id());
        let second = format!("context-session-second-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: first.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: second.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );

        state.open_rename_session_dialog_for(&first);

        let dialog = state
            .take_rename_session_dialog()
            .expect("clicked session should open its rename dialog");
        assert_eq!(
            dialog,
            RenameSessionDialog::open(first.clone(), first.clone())
        );
        state.apply_rename_session_event(
            dialog,
            RenameSessionEvent::Rename {
                session_id: first.clone(),
                name: "renamed-from-context".to_owned(),
            },
        );

        assert_eq!(state.mux().selected_session(), Some(second.as_str()));
        assert_eq!(
            state
                .mux()
                .sessions()
                .iter()
                .find(|session| session.id == first)
                .map(|session| session.name.as_str()),
            Some("renamed-from-context")
        );
    }

    #[test]
    fn context_ditch_keeps_the_other_session_selected() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let first = format!("context-ditch-first-{}", unique_test_id());
        let second = format!("context-ditch-second-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: first.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: second.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );

        state.apply_ditch_session_event(
            DitchSessionDialog::open(first.clone(), None),
            DitchSessionEvent::Ditch {
                session_id: first.clone(),
                cwd: None,
                action: DitchAction::KillOnly,
            },
        );

        assert_eq!(state.mux().selected_session(), Some(second.as_str()));
        assert!(
            state
                .mux()
                .sessions()
                .iter()
                .all(|session| session.id != first)
        );
    }

    #[test]
    fn context_move_session_reorders_the_clicked_inactive_session() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let first = format!("context-move-session-first-{}", unique_test_id());
        let second = format!("context-move-session-second-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: first.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: second.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        let before = state
            .binding
            .mux
            .sessions()
            .iter()
            .map(|session| session.id.clone())
            .collect::<Vec<_>>();
        let before_index = before
            .iter()
            .position(|session_id| session_id == &first)
            .expect("clicked session should be present");
        assert!(
            before_index + 1 < before.len(),
            "clicked session should have a following session: {before:?}"
        );

        assert!(state.move_session_from_ui(&first, 1));

        assert_eq!(
            state
                .mux()
                .sessions()
                .iter()
                .position(|session| session.id == first),
            Some(before_index + 1)
        );
    }

    #[test]
    fn close_action_emits_close_window_effect() {
        let mut state = test_state();
        let mut effects = Vec::new();

        state.apply_keybind_action(
            KeybindAction::App(AppAction::Close),
            ViewportSnapshot::default(),
            &mut effects,
        );

        assert_eq!(effects, vec![AppEffect::CloseWindow]);
    }

    #[test]
    fn new_tab_action_adds_a_window() {
        let mut state = test_state();
        let before = state.binding.mux.selected_session_windows().len();
        let selected = state.binding.mux.selected_session().map(str::to_owned);

        state.apply_mux_key_action(MuxKeyAction::NewTab);

        let after = state.binding.mux.selected_session_windows().len();
        assert!(
            after > before,
            "before={before} after={after} selected={selected:?}"
        );
    }

    #[test]
    fn move_tab_action_reorders_selected_session_windows() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("move-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id,
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        let moved = state
            .binding
            .mux
            .selected_window()
            .map(str::to_owned)
            .expect("new tab selected");
        let before = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        let before_index = before
            .iter()
            .position(|id| id == &moved)
            .expect("selected tab is in window list");
        assert!(
            before_index > 0,
            "new tab should be movable left: {before:?}"
        );

        state.apply_mux_key_action(MuxKeyAction::MoveTab(-1));

        let after = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(after[before_index - 1], moved);
    }

    #[test]
    fn context_rename_tab_targets_the_clicked_inactive_tab() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("context-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        let windows = state.binding.mux.selected_session_windows();
        let clicked = windows[0].clone();
        let selected = windows[1].id.clone();

        state.open_rename_tab_dialog_for(&session_id, &clicked.id);

        assert_eq!(
            state.take_rename_tab_dialog(),
            Some(RenameTabDialog::open(session_id, clicked.id, clicked.name,))
        );
        assert_eq!(state.mux().selected_window(), Some(selected.as_str()));
    }

    #[test]
    fn context_new_tab_for_an_inactive_tab_uses_its_anchor_cwd() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("context-tab-cwd-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: "/context/tab-one".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.binding.mux.execute_command(
            &state.repaint,
            &mux_config,
            MuxCommand::NewWindow {
                session_id: session_id.clone(),
                cwd: Some("/context/tab-two".to_owned()),
            },
        );
        let clicked = state.binding.mux.selected_session_windows()[0].id.clone();

        assert!(state.new_tab_for_window_from_ui(&session_id, &clicked));

        assert_eq!(
            state
                .mux()
                .sessions()
                .iter()
                .find(|session| session.id == session_id)
                .and_then(|session| session.windows.last())
                .and_then(|window| window.anchor.cwd.as_deref()),
            Some("/context/tab-one")
        );
    }

    #[test]
    fn context_close_pane_closes_the_clicked_inactive_tab() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("context-close-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.binding.mux.execute_command(
            &state.repaint,
            &mux_config,
            MuxCommand::NewWindow {
                session_id: session_id.clone(),
                cwd: None,
            },
        );
        let clicked = state.binding.mux.selected_session_windows()[0].id.clone();
        let selected = state.binding.mux.selected_session_windows()[1].id.clone();

        assert!(state.close_pane_for_window_from_ui(&session_id, &clicked));

        let remaining = state
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
            .expect("target session should stay open")
            .windows
            .iter()
            .map(|window| window.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(remaining, vec![selected.as_str()]);
        assert_eq!(state.mux().selected_window(), Some(selected.as_str()));
    }

    #[test]
    fn context_move_tab_reorders_the_clicked_inactive_tab() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("context-move-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        let before = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        let clicked = before[0].clone();
        let active = before[2].clone();

        assert!(state.move_window_from_ui(&session_id, &clicked, 1));

        assert_eq!(
            state
                .mux()
                .selected_session_windows()
                .iter()
                .map(|window| window.id.clone())
                .collect::<Vec<_>>(),
            vec![before[1].clone(), before[0].clone(), before[2].clone()]
        );
        assert_eq!(state.mux().selected_window(), Some(active.as_str()));
    }

    #[test]
    fn context_tab_navigation_anchors_to_the_clicked_tab() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("context-navigate-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        let tabs = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        let clicked = tabs[1].clone();
        state.activate_window_from_ui(&session_id, &tabs[0]);

        assert!(state.activate_relative_window_from_ui(&session_id, &clicked, 1));

        assert_eq!(state.mux().selected_window(), Some(tabs[2].as_str()));
    }

    #[test]
    fn window_reorder_from_ui_moves_non_active_tab_to_end() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("drag-move-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id,
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        let before = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        let moved = before[0].clone();

        assert!(state.reorder_window_before_from_ui(&moved, None));

        let after = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            after,
            vec![before[1].clone(), before[2].clone(), before[0].clone()]
        );
    }

    #[test]
    fn window_reorder_from_ui_ignores_self_drop() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("self-drop-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id,
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        let before = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        let moved = before[0].clone();

        assert!(!state.reorder_window_before_from_ui(&moved, Some(&moved)));

        let after = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(after, before);
    }

    #[test]
    fn command_palette_move_tab_action_reorders_selected_session_windows() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("palette-move-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id,
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        let moved = state
            .binding
            .mux
            .selected_window()
            .map(str::to_owned)
            .expect("new tab selected");
        let before = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        let before_index = before
            .iter()
            .position(|id| id == &moved)
            .expect("selected tab is in window list");
        assert!(
            before_index > 0,
            "new tab should be movable left: {before:?}"
        );

        state.apply_command_palette_event(
            CommandPaletteDialog::open(&[]),
            CommandPaletteEvent::Run("move_tab:-1"),
        );
        let effects = state.update_frame(test_frame_inputs(Vec::new(), None));
        assert!(
            effects.contains(&AppEffect::RequestRepaint),
            "palette move-tab must schedule an immediate repaint so status tabs re-render"
        );

        let after = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(after[before_index - 1], moved);
    }

    #[test]
    fn copy_mode_leaves_global_shortcuts_for_app_keybindings() {
        let alt_shift = egui::Modifiers {
            alt: true,
            shift: true,
            ..Default::default()
        };
        assert!(copy_mode_egui_key_should_pass_to_app(
            egui::Key::Comma,
            alt_shift
        ));
        assert!(copy_mode_input_should_pass_to_app(KeyInput {
            key: TerminalKey::Comma,
            mods: crate::terminal::KeyMods {
                alt: true,
                shift: true,
                ..Default::default()
            },
            repeat: false,
            utf8: Some("<"),
            unshifted: Some(','),
        }));

        assert!(!copy_mode_egui_key_should_pass_to_app(
            egui::Key::J,
            egui::Modifiers::default()
        ));
        assert!(!copy_mode_input_should_pass_to_app(KeyInput {
            key: TerminalKey::J,
            mods: crate::terminal::KeyMods::default(),
            repeat: false,
            utf8: Some("j"),
            unshifted: Some('j'),
        }));

        let command = egui::Modifiers {
            command: true,
            ..Default::default()
        };
        assert!(!copy_mode_egui_key_should_pass_to_app(
            egui::Key::C,
            command
        ));
        assert!(copy_mode_egui_key_should_pass_to_app(egui::Key::F, command));
        assert!(!copy_mode_input_should_pass_to_app(KeyInput {
            key: TerminalKey::C,
            mods: crate::terminal::KeyMods {
                command: true,
                ..Default::default()
            },
            repeat: false,
            utf8: None,
            unshifted: Some('c'),
        }));
    }

    #[test]
    fn rename_tab_action_opens_dialog_and_renames_selected_tab() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("rename-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: "repo".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        let window_id = state.binding.mux.selected_session_windows()[0].id.clone();
        let mut effects = Vec::new();

        state.apply_keybind_action(
            KeybindAction::App(AppAction::RenameTab),
            ViewportSnapshot::default(),
            &mut effects,
        );
        let dialog = state
            .take_rename_tab_dialog()
            .expect("rename tab action should open the dialog");

        state.apply_rename_tab_event(
            dialog,
            RenameTabEvent::Rename {
                session_id,
                window_id: window_id.clone(),
                name: "build".to_owned(),
            },
        );

        let window = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .find(|window| window.id == window_id)
            .expect("renamed tab should remain present");
        assert_eq!(window.name, "build");
        assert_eq!(effects, vec![AppEffect::RequestRepaint]);
    }

    #[test]
    fn unscoped_window_title_side_effect_renames_selected_tab() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("title-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id,
                cwd: "repo".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        let window_id = state.binding.mux.selected_session_windows()[0].id.clone();
        let mut effects = Vec::new();

        state.apply_terminal_side_effect_event(
            TerminalSideEffectEvent::unscoped(TerminalSideEffect::WindowTitle(
                "⠼ agents".to_owned(),
            )),
            &mut effects,
            8.0,
            16.0,
            1.0,
        );

        let window = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .find(|window| window.id == window_id)
            .expect("selected window should remain present");
        assert_eq!(window.name, "⠼ agents");
        assert_eq!(
            effects,
            vec![AppEffect::SetWindowTitle("⠼ agents".to_owned())]
        );
    }

    #[test]
    fn scoped_window_title_side_effect_renames_source_tab_not_selected_tab() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("scoped-title-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: "repo".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        let first_window_id = state.binding.mux.selected_session_windows()[0].id.clone();
        let first_original_name = state.binding.mux.selected_session_windows()[0].name.clone();
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        let second_window = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .find(|window| window.id != first_window_id)
            .expect("second tab should be present")
            .clone();
        let second_pane_id = second_window
            .anchor
            .pane_id
            .clone()
            .expect("native tab should have a source pane id");
        state.activate_window_from_ui(&session_id, &first_window_id);
        let mut effects = Vec::new();

        state.apply_terminal_side_effect_event(
            TerminalSideEffectEvent::new(
                Some(second_pane_id),
                TerminalSideEffect::WindowTitle("⠼ agents".to_owned()),
            ),
            &mut effects,
            8.0,
            16.0,
            1.0,
        );

        let windows = state.binding.mux.selected_session_windows();
        let first_window = windows
            .iter()
            .find(|window| window.id == first_window_id)
            .expect("selected tab should remain present");
        let second_window = windows
            .iter()
            .find(|window| window.id == second_window.id)
            .expect("source tab should remain present");
        assert_eq!(first_window.name, first_original_name);
        assert_eq!(second_window.name, "⠼ agents");
        assert_eq!(
            state.binding.mux.selected_window(),
            Some(first_window_id.as_str())
        );
        assert_eq!(effects, Vec::<AppEffect>::new());
    }

    #[test]
    fn scoped_terminal_progress_updates_and_clears_its_inactive_window_indicator() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("progress-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: "repo".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        let first_window_id = state.binding.mux.selected_session_windows()[0].id.clone();
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        let second_window = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .find(|window| window.id != first_window_id)
            .expect("second tab should be present")
            .clone();
        let second_pane_id = second_window
            .anchor
            .pane_id
            .clone()
            .expect("native tab should have a source pane id");
        state.activate_window_from_ui(&session_id, &first_window_id);

        state.apply_terminal_side_effect_event(
            TerminalSideEffectEvent::new(
                Some(second_pane_id.clone()),
                TerminalSideEffect::ConEmuProgress {
                    state: "normal".to_owned(),
                    value: Some(42),
                },
            ),
            &mut Vec::new(),
            8.0,
            16.0,
            1.0,
        );

        assert_eq!(state.window_progress(&second_window), Some(42));

        state.apply_terminal_side_effect_event(
            TerminalSideEffectEvent::new(
                Some(second_pane_id.clone()),
                TerminalSideEffect::ConEmuProgress {
                    state: "indeterminate".to_owned(),
                    value: None,
                },
            ),
            &mut Vec::new(),
            8.0,
            16.0,
            1.0,
        );

        assert!(state.has_indeterminate_terminal_progress());
        assert_eq!(state.window_progress(&second_window), Some(50));
        assert!(state.window_has_indeterminate_progress(&second_window));

        state.apply_terminal_side_effect_event(
            TerminalSideEffectEvent::new(
                Some(second_pane_id),
                TerminalSideEffect::ConEmuProgress {
                    state: "inactive".to_owned(),
                    value: Some(0),
                },
            ),
            &mut Vec::new(),
            8.0,
            16.0,
            1.0,
        );

        assert_eq!(state.window_progress(&second_window), None);
        assert!(!state.has_indeterminate_terminal_progress());
        assert!(!state.window_has_indeterminate_progress(&second_window));
    }

    #[test]
    fn manually_renamed_tab_ignores_terminal_title_renames() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("manual-title-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: "repo".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        let window_id = state.binding.mux.selected_session_windows()[0].id.clone();
        state.apply_rename_tab_event(
            RenameTabDialog::open(session_id.clone(), window_id.clone(), "tab-1".to_owned()),
            RenameTabEvent::Rename {
                session_id,
                window_id: window_id.clone(),
                name: "build".to_owned(),
            },
        );
        let mut effects = Vec::new();

        state.apply_terminal_side_effect_event(
            TerminalSideEffectEvent::unscoped(TerminalSideEffect::WindowTitle("editor".to_owned())),
            &mut effects,
            8.0,
            16.0,
            1.0,
        );

        let window = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .find(|window| window.id == window_id)
            .expect("selected window should remain present");
        assert_eq!(window.name, "build");
        assert_eq!(
            effects,
            vec![AppEffect::SetWindowTitle("editor".to_owned())]
        );
    }

    #[test]
    fn blank_tab_rename_restores_terminal_title_following() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("blank-title-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: "repo".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        let window_id = state.binding.mux.selected_session_windows()[0].id.clone();
        state.apply_terminal_side_effect_event(
            TerminalSideEffectEvent::unscoped(TerminalSideEffect::WindowTitle("editor".to_owned())),
            &mut Vec::new(),
            8.0,
            16.0,
            1.0,
        );
        state.apply_rename_tab_event(
            RenameTabDialog::open(session_id.clone(), window_id.clone(), "tab-1".to_owned()),
            RenameTabEvent::Rename {
                session_id: session_id.clone(),
                window_id: window_id.clone(),
                name: "build".to_owned(),
            },
        );
        state.apply_terminal_side_effect_event(
            TerminalSideEffectEvent::unscoped(TerminalSideEffect::WindowTitle("server".to_owned())),
            &mut Vec::new(),
            8.0,
            16.0,
            1.0,
        );
        state.apply_rename_tab_event(
            RenameTabDialog::open(session_id.clone(), window_id.clone(), "build".to_owned()),
            RenameTabEvent::Rename {
                session_id,
                window_id: window_id.clone(),
                name: String::new(),
            },
        );

        let window = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .find(|window| window.id == window_id)
            .expect("selected window should remain present");
        assert_eq!(window.name, "server");
    }

    #[test]
    fn copy_action_emits_request_copy_effect() {
        let mut state = test_state();
        let mut effects = Vec::new();

        state.apply_keybind_action(
            KeybindAction::CopyToClipboard,
            ViewportSnapshot::default(),
            &mut effects,
        );

        assert_eq!(effects, vec![AppEffect::RequestCopy]);
    }

    #[test]
    fn toggle_sidebar_visibility_flips_config_and_requests_repaint() {
        let mut state = test_state();
        let before = state.config().chrome.sidebar;
        let mut effects = Vec::new();

        state.apply_keybind_action(
            KeybindAction::App(AppAction::ToggleSidebarVisibility),
            ViewportSnapshot::default(),
            &mut effects,
        );

        assert_eq!(state.config().chrome.sidebar, !before);
        assert_eq!(effects, vec![AppEffect::RequestRepaint]);
    }

    #[test]
    fn font_size_decrease_clamps_at_one_and_emits_text_config() {
        let mut state = test_state();
        let mut effects = Vec::new();

        state.apply_keybind_action(
            KeybindAction::Font(FontSizeAction::Decrease(10_000.0)),
            ViewportSnapshot::default(),
            &mut effects,
        );

        assert_eq!(state.config().font.size, 1.0);
        assert!(matches!(
            effects.as_slice(),
            [AppEffect::SetTerminalTextConfig(_)]
        ));
    }

    #[test]
    fn reload_with_unreadable_config_rejects_and_keeps_previous_config() {
        let mut state = test_state();
        let previous_title = state.config().window.title.clone();
        let mut effects = Vec::new();

        // Default config_path points at a location the test never writes, so
        // the reload must take the rejection path.
        let reloaded = state.reload_config(&mut effects);

        if reloaded {
            // A real user config exists on this machine; the reload accepting
            // it is correct behavior, nothing to assert against.
            return;
        }
        assert!(state.last_error().is_some());
        assert_eq!(state.config().window.title, previous_title);
        assert!(effects.is_empty());
    }

    #[test]
    fn reload_applies_window_title_change_as_effect() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").expect("write empty config");

        let config = BoottyConfig {
            config_path: path.clone(),
            ..BoottyConfig::default()
        };
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let mut state = AppState::new(config, repaint, None, None).expect("state");

        std::fs::write(&path, "[window]\ntitle = \"renamed\"\n").expect("write config");
        let mut effects = Vec::new();
        let reloaded = state.reload_config(&mut effects);

        assert!(reloaded);
        assert!(
            effects.contains(&AppEffect::SetWindowTitle("renamed".to_owned())),
            "{effects:?}"
        );
        assert_eq!(state.config().window.title, "renamed");
    }
}
