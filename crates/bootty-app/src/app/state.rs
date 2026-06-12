use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::Result;
use eframe::egui::{self, Pos2};

use crate::{
    app_actions::{
        AppAction, AppKeyBindings, FontSizeAction, KeybindAction, MuxKeyAction, SidebarAction,
        SidebarKeyBindings, TerminalScrollAction, builtin_app_action_for_direct_key,
        split_app_actions_for_bindings,
    },
    config::{BoottyConfig, ConfigState, WindowConfig, load_config_from_path},
    config_reload::{CONFIG_HOT_RELOAD_INTERVAL, ConfigHotReload, new_session_only_config_changed},
    diagnostics::{
        STATUS_METRICS_SAMPLE_INTERVAL, StabilityTrace, StabilityTraceSample, StatusMetrics,
        should_sample_status_metrics,
    },
    direct_input::{DirectKeyInput, ModifierSideState},
    geometry::TerminalSurface,
    input::{
        InputSnapshot, TerminalInputCommand, focus::InputFocus, router::route_events,
        terminal_input_commands_with_options,
    },
    modifier_remap::ModifierRemapSet,
    mux::{
        RepaintHandle,
        command::MuxCommand,
        controller::MuxController,
        sidebar_meta::{
            SidebarMetadata, SidebarMetadataSession, collect_sidebar_metadata,
            sidebar_metadata_sessions_for_prefix,
        },
        terminal::ActiveTerminal,
    },
    platform::{
        apply_macos_non_native_fullscreen_presentation, install_macos_app_icon,
        macos_handles_non_native_fullscreen_frame, read_clipboard_text, restore_macos_presentation,
        spawn_new_window,
    },
    renderer::{RendererMetrics, TerminalWidget},
    scheduler::{RepaintScheduler, RepaintSignal},
    session_order::SessionOrderStore,
    terminal::{DrainStats, MouseButton},
    terminal_text::TerminalTextConfig,
    theme::theme_from_config,
    ui::{
        chrome,
        new_session_picker::{NewMuxSessionDialog, NewSessionPickerEvent},
        session_picker::{SessionPickerDialog, SessionPickerEvent},
    },
};

const SIDEBAR_METADATA_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const SIDEBAR_METADATA_INITIAL_DELAY: Duration = Duration::from_secs(1);
const MACOS_APP_ICON_INITIAL_DELAY: Duration = Duration::from_secs(1);
const MACOS_APP_ICON_RETRY_INTERVAL: Duration = Duration::from_secs(5);

/// Per-frame snapshot of everything the state machine needs from the host.
/// Captured once at frame start; `egui::Context` never enters this module.
#[derive(Clone, Debug)]
pub struct FrameInputs {
    pub now: Instant,
    pub stable_dt_ms: f32,
    pub events: Vec<egui::Event>,
    pub modifiers: egui::Modifiers,
    pub hover_pos: Option<Pos2>,
    pub pressed_mouse_button: Option<MouseButton>,
    pub viewport: ViewportSnapshot,
    pub renderer_metrics: RendererMetrics,
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
    RepaintAfter(Duration),
    SetTerminalTextConfig(TerminalTextConfig),
}

pub struct AppState {
    terminal: ActiveTerminal,
    repaint_scheduler: RepaintScheduler,
    last_error: Option<String>,
    last_drain: DrainStats,
    last_frame_dt_ms: f32,
    status_metrics: StatusMetrics,
    last_status_metrics_sample: Instant,
    terminal_surface: Option<TerminalSurface>,
    config_state: ConfigState,
    input_focus: InputFocus,
    app_key_bindings: AppKeyBindings,
    sidebar_key_bindings: SidebarKeyBindings,
    has_new_session_config_changes: bool,
    mux: MuxController,
    repaint: RepaintHandle,
    direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
    modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
    modifier_sides: ModifierSideState,
    pending_direct_input: Vec<DirectKeyInput>,
    suppress_next_egui_paste: bool,
    modifier_remaps: ModifierRemapSet,
    macos_option_as_alt: crate::terminal::MacosOptionAsAlt,
    stability_trace: Option<StabilityTrace>,
    config_hot_reload: ConfigHotReload,
    session_order: SessionOrderStore,
    sidebar_metadata: SidebarMetadata,
    last_sidebar_metadata_refresh: Instant,
    sidebar_metadata_tx: Option<mpsc::Sender<Vec<SidebarMetadataSession>>>,
    sidebar_metadata_rx: Option<mpsc::Receiver<SidebarMetadata>>,
    sidebar_metadata_pending: bool,
    new_mux_session_dialog: Option<NewMuxSessionDialog>,
    sidebar_hovered_session: Option<String>,
    session_picker_dialog: Option<SessionPickerDialog>,
    macos_app_icon_installed: bool,
    next_macos_app_icon_install: Instant,
    macos_non_native_fullscreen_active: bool,
}

fn initial_sidebar_metadata_refresh_mark(started_at: Instant) -> Instant {
    started_at - SIDEBAR_METADATA_REFRESH_INTERVAL + SIDEBAR_METADATA_INITIAL_DELAY
}

fn sidebar_metadata_refresh_due(last_refresh: Instant, now: Instant, pending: bool) -> bool {
    !pending && now.duration_since(last_refresh) >= SIDEBAR_METADATA_REFRESH_INTERVAL
}

fn initial_macos_app_icon_install_after(started_at: Instant) -> Instant {
    started_at + MACOS_APP_ICON_INITIAL_DELAY
}

fn macos_app_icon_install_due(installed: bool, next_attempt: Instant, now: Instant) -> bool {
    !installed && now >= next_attempt
}

fn next_macos_app_icon_retry(now: Instant) -> Instant {
    now + MACOS_APP_ICON_RETRY_INTERVAL
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

#[cfg(test)]
fn new_mux_session_request_with_name(
    config: &BoottyConfig,
    name: impl Into<String>,
) -> crate::ui::new_session_picker::NewMuxSessionRequest {
    let cwd = config
        .session
        .working_directory
        .clone()
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

impl AppState {
    pub fn new(
        config: BoottyConfig,
        repaint: RepaintHandle,
        direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
        modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
    ) -> Result<Self> {
        let modifier_remaps = config.input.modifier_remaps()?;
        let macos_option_as_alt = config.input.macos_option_as_alt.into();
        let keybinds = config
            .input
            .keybinds_for_backend(config.multiplexer.backend);
        let app_key_bindings = AppKeyBindings::from_keybinds(&keybinds)?;
        let sidebar_key_bindings =
            SidebarKeyBindings::from_keybinds(&config.input.sidebar_keybind)?;
        let stability_trace = StabilityTrace::from_config(&config);
        let session_config = config.terminal_session_config();
        let config_hot_reload = ConfigHotReload::new(&config.config_path);
        let session_order = SessionOrderStore::for_config_path(&config.config_path);
        let macos_non_native_fullscreen_active = config.window.non_native_fullscreen_enabled();
        apply_macos_non_native_fullscreen_presentation(&config.window);

        Ok(Self {
            terminal: ActiveTerminal::new(
                TerminalWidget::initial_geometry(),
                &config.multiplexer,
                session_config,
                repaint.clone(),
            ),
            repaint_scheduler: RepaintScheduler::default(),
            last_error: None,
            last_drain: DrainStats::default(),
            last_frame_dt_ms: 0.0,
            status_metrics: StatusMetrics::default(),
            last_status_metrics_sample: Instant::now() - STATUS_METRICS_SAMPLE_INTERVAL,
            terminal_surface: None,
            config_state: ConfigState::new(config),
            input_focus: InputFocus::Terminal,
            app_key_bindings,
            sidebar_key_bindings,
            has_new_session_config_changes: false,
            mux: MuxController::new(),
            repaint,
            direct_input_rx,
            modifier_side_rx,
            modifier_sides: ModifierSideState::default(),
            pending_direct_input: Vec::new(),
            suppress_next_egui_paste: false,
            modifier_remaps,
            macos_option_as_alt,
            stability_trace,
            config_hot_reload,
            session_order,
            sidebar_metadata: SidebarMetadata::default(),
            last_sidebar_metadata_refresh: initial_sidebar_metadata_refresh_mark(Instant::now()),
            sidebar_metadata_tx: None,
            sidebar_metadata_rx: None,
            sidebar_metadata_pending: false,
            new_mux_session_dialog: None,
            sidebar_hovered_session: None,
            session_picker_dialog: None,
            macos_app_icon_installed: false,
            next_macos_app_icon_install: initial_macos_app_icon_install_after(Instant::now()),
            macos_non_native_fullscreen_active,
        })
    }

    pub fn config(&self) -> &BoottyConfig {
        self.config_state.current()
    }

    pub fn ui_theme(&self) -> bootty_ui::Theme {
        theme_from_config(self.config())
    }

    pub fn mux(&self) -> &MuxController {
        &self.mux
    }

    pub fn status_metrics(&self) -> StatusMetrics {
        self.status_metrics
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn sidebar_metadata(&self) -> &SidebarMetadata {
        &self.sidebar_metadata
    }

    pub fn sidebar_focused(&self) -> bool {
        self.input_focus == InputFocus::Sidebar
    }

    pub fn terminal_focused(&self) -> bool {
        self.direct_terminal_input_enabled()
    }

    pub fn sidebar_hovered_session(&self) -> Option<&str> {
        self.sidebar_hovered_session.as_deref()
    }
    pub fn direct_input_suppresses_egui_events(&self) -> bool {
        self.direct_terminal_input_enabled()
    }

    pub fn macos_non_native_fullscreen_active(&self) -> bool {
        self.macos_non_native_fullscreen_active
    }

    pub fn terminal_mut(&mut self) -> &mut ActiveTerminal {
        &mut self.terminal
    }

    pub fn record_surface(&mut self, surface: TerminalSurface) {
        self.terminal_surface = Some(surface);
    }

    pub fn record_render_error(&mut self, error: impl ToString) {
        self.last_error = Some(error.to_string());
    }

    pub fn activate_session_from_ui(&mut self, session_id: &str) {
        self.mux.activate_session(session_id);
        self.sidebar_hovered_session = Some(session_id.to_owned());
    }

    pub fn activate_window_from_ui(&mut self, session_id: &str, window_id: &str) {
        let mux_config = self.config().multiplexer.clone();
        self.mux
            .activate_window(session_id, window_id, &self.repaint, &mux_config);
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

    fn move_selected_session(&mut self, delta: i32) -> bool {
        let Some(selected) = self.mux.selected_session() else {
            return false;
        };
        let Some(selected_name) = self
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == selected || session.name == selected)
            .map(|session| session.name.clone())
        else {
            return false;
        };
        if !self.session_order.move_session(
            &selected_name,
            delta,
            self.mux
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
        if !self.session_order.move_block_before(
            source,
            target,
            self.mux
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
            SessionPickerEvent::ActivateSession(session_id) => {
                self.input_focus = InputFocus::Terminal;
                self.activate_session_from_ui(&session_id);
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
            NewSessionPickerEvent::NewWorktreeUnavailable => {
                self.last_error = Some("new worktree creation is not wired yet".to_owned());
                self.new_mux_session_dialog = Some(dialog);
            }
            NewSessionPickerEvent::CreateSession(request) => {
                let mux_config = self.config().multiplexer.clone();
                self.mux
                    .create_project_session(request, &self.repaint, &mux_config);
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

    pub fn pending_direct_input(&self) -> &[DirectKeyInput] {
        &self.pending_direct_input
    }

    pub fn update_frame(&mut self, inputs: FrameInputs) -> Vec<AppEffect> {
        let FrameInputs {
            now,
            stable_dt_ms,
            events,
            modifiers,
            hover_pos,
            pressed_mouse_button,
            viewport,
            renderer_metrics,
        } = inputs;
        let mut effects = Vec::new();

        if macos_app_icon_install_due(
            self.macos_app_icon_installed,
            self.next_macos_app_icon_install,
            now,
        ) {
            self.macos_app_icon_installed = install_macos_app_icon();
            if !self.macos_app_icon_installed {
                self.next_macos_app_icon_install = next_macos_app_icon_retry(now);
            }
        }
        self.last_drain = self.terminal.drain_pty();
        match self.terminal.child_exited() {
            Ok(true) => {
                effects.push(AppEffect::CloseWindow);
                return effects;
            }
            Ok(false) => {}
            Err(error) => self.last_error = Some(error.to_string()),
        }

        if let Some(error) = self
            .mux
            .refresh_sessions(&self.repaint, &self.config_state.current().multiplexer)
        {
            self.last_error = Some(error);
        }
        if let Some(result) = self.mux.poll_command() {
            self.last_error = result.err();
        }
        self.sync_session_order();
        if self.config_state.current().chrome.sidebar {
            self.refresh_sidebar_metadata(viewport);
        }
        if let Err(error) = self.terminal.sync_mux_anchor(
            &self.config_state.current().multiplexer,
            self.mux.selected_session_anchor(),
        ) {
            self.last_error = Some(error.to_string());
        }
        self.hot_reload_config_if_changed(&mut effects);
        let input_commands = self.handle_direct_input(viewport, &mut effects)
            + self.handle_egui_input(
                events,
                modifiers,
                hover_pos,
                pressed_mouse_button,
                viewport,
                &mut effects,
            );
        self.last_frame_dt_ms = stable_dt_ms;

        let pending_pty_bytes = self.terminal.pending_pty_len();
        let (cols, rows) = self.terminal.grid_size();
        if let Some(trace) = &mut self.stability_trace {
            trace.record(StabilityTraceSample {
                elapsed_ms: trace.started_at.elapsed().as_millis(),
                selected_session: self.mux.selected_session(),
                cols,
                rows,
                pending_pty_bytes,
                drain_bytes: self.last_drain.bytes,
                drain_elapsed_us: self.last_drain.elapsed_us,
                text_runs: renderer_metrics.text_runs,
                last_error: self.last_error.as_deref(),
            });
        }
        if should_sample_status_metrics(self.last_status_metrics_sample.elapsed()) {
            self.status_metrics = StatusMetrics {
                drain: self.last_drain,
                renderer: renderer_metrics,
                cols,
                rows,
            };
            self.last_status_metrics_sample = Instant::now();
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

    fn refresh_sidebar_metadata(&mut self, viewport: ViewportSnapshot) {
        if let Some(rx) = &self.sidebar_metadata_rx {
            match rx.try_recv() {
                Ok(metadata) => {
                    self.sidebar_metadata = metadata;
                    self.sidebar_metadata_pending = false;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.sidebar_metadata_tx = None;
                    self.sidebar_metadata_rx = None;
                    self.sidebar_metadata_pending = false;
                }
            }
        }

        if !sidebar_metadata_refresh_due(
            self.last_sidebar_metadata_refresh,
            Instant::now(),
            self.sidebar_metadata_pending,
        ) {
            return;
        }

        self.ensure_sidebar_metadata_worker();
        let Some(tx) = &self.sidebar_metadata_tx else {
            return;
        };
        let max_sessions = self.sidebar_metadata_session_budget(viewport);
        match tx.send(sidebar_metadata_sessions_for_prefix(
            self.mux.sessions(),
            max_sessions,
        )) {
            Ok(()) => {
                self.last_sidebar_metadata_refresh = Instant::now();
                self.sidebar_metadata_pending = true;
            }
            Err(_) => {
                self.sidebar_metadata_tx = None;
                self.sidebar_metadata_rx = None;
                self.sidebar_metadata_pending = false;
            }
        }
    }

    fn sidebar_metadata_session_budget(&self, viewport: ViewportSnapshot) -> usize {
        let fullscreen_chrome = self.macos_non_native_fullscreen_active || viewport.fullscreen;
        let title_visible = self.config().window.custom_chrome_title_visible();
        let top_inset = if fullscreen_chrome && !title_visible {
            28.0
        } else {
            0.0
        };
        chrome::sidebar_metadata_session_budget(
            viewport.content_height,
            top_inset,
            title_visible,
            self.sidebar_metadata.usage_lines(),
        )
    }

    fn ensure_sidebar_metadata_worker(&mut self) {
        if self.sidebar_metadata_tx.is_some() && self.sidebar_metadata_rx.is_some() {
            return;
        }

        let (request_tx, request_rx) = mpsc::channel::<Vec<SidebarMetadataSession>>();
        let (result_tx, result_rx) = mpsc::channel::<SidebarMetadata>();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            while let Ok(sessions) = request_rx.recv() {
                let metadata = collect_sidebar_metadata(&sessions);
                if result_tx.send(metadata).is_err() {
                    break;
                }
                repaint();
            }
        });
        self.sidebar_metadata_tx = Some(request_tx);
        self.sidebar_metadata_rx = Some(result_rx);
    }

    fn open_new_mux_session_dialog(&mut self) {
        self.session_picker_dialog = None;
        self.new_mux_session_dialog = Some(NewMuxSessionDialog::open());
        self.input_focus = InputFocus::Picker;
    }

    fn toggle_session_picker_dialog(&mut self) {
        self.new_mux_session_dialog = None;
        if self.session_picker_dialog.is_some() {
            self.session_picker_dialog = None;
            self.input_focus = InputFocus::Terminal;
        } else {
            self.session_picker_dialog = Some(SessionPickerDialog::open());
            self.input_focus = InputFocus::Picker;
        }
    }

    fn direct_terminal_input_enabled(&self) -> bool {
        self.input_focus.terminal_owns_input()
            && self.new_mux_session_dialog.is_none()
            && self.session_picker_dialog.is_none()
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
        let modifier_remaps = match next.input.modifier_remaps() {
            Ok(remaps) => remaps,
            Err(error) => {
                self.config_state.reject(error.to_string());
                self.last_error = self.config_state.last_error().map(str::to_owned);
                return false;
            }
        };
        let keybinds = next.input.keybinds_for_backend(next.multiplexer.backend);
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

        if previous.colors != next.colors
            && let Err(error) = self
                .terminal
                .set_colors(next.colors.terminal_color_config())
        {
            self.config_state.reject(error.to_string());
            self.last_error = self.config_state.last_error().map(str::to_owned);
            return false;
        }
        if previous.font != next.font {
            effects.push(AppEffect::SetTerminalTextConfig(
                next.font.terminal_text_config(),
            ));
        }
        if previous.window.title != next.window.title {
            effects.push(AppEffect::SetWindowTitle(next.window.title.clone()));
        }
        if previous.window.fullscreen != next.window.fullscreen {
            apply_macos_non_native_fullscreen_presentation(&next.window);
            self.macos_non_native_fullscreen_active = next.window.non_native_fullscreen_enabled();
            effects.push(AppEffect::SetFullscreen(
                next.window.native_fullscreen_enabled(),
            ));
            if !macos_handles_non_native_fullscreen_frame(&next.window) {
                effects.push(AppEffect::SetMaximized(
                    next.window.non_native_fullscreen_enabled(),
                ));
            }
        }
        if previous.window.decorations_enabled() != next.window.decorations_enabled() {
            effects.push(AppEffect::SetDecorations(next.window.decorations_enabled()));
        }
        if previous.diagnostics != next.diagnostics {
            self.stability_trace = StabilityTrace::from_config(&next);
        }

        self.modifier_remaps = modifier_remaps;
        self.macos_option_as_alt = next.input.macos_option_as_alt.into();
        self.app_key_bindings = app_key_bindings;
        self.sidebar_key_bindings = sidebar_key_bindings;
        self.terminal
            .set_terminal_config(next.terminal_session_config());
        self.has_new_session_config_changes = new_session_only_config_changed(&previous, &next)
            || self.has_new_session_config_changes;
        self.config_state.accept(next);
        self.session_order = SessionOrderStore::for_config_path(&self.config().config_path);
        self.sync_session_order();
        self.last_error = if self.has_new_session_config_changes {
            Some("config reloaded; session/window creation changes apply next time".to_owned())
        } else {
            None
        };
        effects.push(AppEffect::RequestRepaint);
        true
    }

    fn hot_reload_config_if_changed(&mut self, effects: &mut Vec<AppEffect>) {
        if !self.config_hot_reload.changed(Instant::now()) {
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
        split_app_actions_for_bindings(&mut self.app_key_bindings, events)
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
        let (events, actions) = self.split_app_actions(events);
        let routed = route_events(self.input_focus, events);
        let sidebar_count = self.handle_sidebar_input(routed.ui_events);
        let events = if self.direct_terminal_input_enabled() {
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
        };
        let commands = terminal_input_commands_with_options(
            snapshot,
            &self.modifier_remaps,
            self.macos_option_as_alt,
        );
        let count = commands.len() + actions.len() + sidebar_count;

        for action in actions {
            self.apply_keybind_action(action, viewport, effects);
        }

        for command in commands {
            self.apply_terminal_input(command);
        }

        count
    }

    fn handle_direct_input(
        &mut self,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) -> usize {
        let inputs = std::mem::take(&mut self.pending_direct_input);
        let count = inputs.len();
        if !self.direct_terminal_input_enabled() {
            return count;
        }
        for input in inputs {
            let mut input = input.input();
            input.mods = self.modifier_remaps.apply(input.mods);
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
            if input.mods.command {
                continue;
            }
            self.apply_terminal_input(TerminalInputCommand::Key(input));
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
            KeybindAction::App(AppAction::NewWindow) => {
                if let Err(error) = spawn_new_window() {
                    self.last_error = Some(error.to_string());
                }
            }
            KeybindAction::App(AppAction::NewMuxSession) => {
                self.open_new_mux_session_dialog();
            }
            KeybindAction::App(AppAction::SessionPicker) => {
                self.toggle_session_picker_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::Close) => {
                effects.push(AppEffect::CloseWindow);
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
                        apply_macos_non_native_fullscreen_presentation(&self.config().window);
                    } else {
                        restore_macos_presentation();
                    }
                    effects.push(AppEffect::SetFullscreen(false));
                    if !macos_handles_non_native_fullscreen_frame(&self.config().window) {
                        effects.push(AppEffect::SetMaximized(next_maximized));
                    }
                }
            }
            KeybindAction::App(AppAction::ToggleSidebarFocus) => {
                self.session_picker_dialog = None;
                self.new_mux_session_dialog = None;
                if self.input_focus == InputFocus::Sidebar {
                    self.input_focus = InputFocus::Terminal;
                } else {
                    self.config_state.current_mut().chrome.sidebar = true;
                    self.input_focus = InputFocus::Sidebar;
                    self.sidebar_hovered_session = self
                        .mux
                        .selected_session()
                        .and_then(|selected| self.session_id_matching(selected))
                        .or_else(|| {
                            self.mux
                                .sessions()
                                .first()
                                .map(|session| session.id.clone())
                        });
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
            KeybindAction::Mux(action) => self.apply_mux_key_action(action),
            KeybindAction::Scroll(action) => self.apply_terminal_scroll_action(action),
            KeybindAction::Write(bytes) => {
                if let Err(error) = self.terminal.write_input(&bytes) {
                    self.last_error = Some(error.to_string());
                }
            }
            KeybindAction::Font(action) => self.apply_font_size_action(action, effects),
            KeybindAction::CopyToClipboard => {
                effects.push(AppEffect::RequestCopy);
            }
            KeybindAction::PasteFromClipboard => match read_clipboard_text() {
                Ok(Some(text)) => {
                    if let Err(error) = self.terminal.write_paste(&text) {
                        self.last_error = Some(error.to_string());
                    }
                }
                Ok(None) => {}
                Err(error) => self.last_error = Some(error.to_string()),
            },
        }
    }

    fn apply_mux_key_action(&mut self, action: MuxKeyAction) {
        if self.apply_session_navigation_action(action) {
            return;
        }
        if let MuxKeyAction::MoveSession(delta) = action {
            self.move_selected_session(delta);
            return;
        }
        let selected_session = self.mux.selected_session().unwrap_or("local").to_owned();
        let command = match action {
            MuxKeyAction::NewTab => MuxCommand::NewWindow {
                session_id: selected_session,
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
                delta,
            },
            MuxKeyAction::SplitPane => MuxCommand::SplitPane {
                session_id: selected_session,
            },
            MuxKeyAction::SelectPane(direction) => MuxCommand::SelectPane {
                session_id: selected_session,
                direction,
            },
            MuxKeyAction::NextPane => MuxCommand::SelectNextPane {
                session_id: selected_session,
            },
            MuxKeyAction::KillPane => MuxCommand::KillPane {
                session_id: selected_session,
            },
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
            MuxKeyAction::DitchSession => MuxCommand::DitchSession {
                session_id: selected_session,
            },
        };
        let mux_config = self.config().multiplexer.clone();
        self.mux
            .execute_command(&self.repaint, &mux_config, command);
    }

    fn ensure_sidebar_hovered_session(&mut self) {
        if self.sidebar_hovered_index().is_some() {
            return;
        }
        self.sidebar_hovered_session = self
            .mux
            .selected_session()
            .and_then(|selected| self.session_id_matching(selected))
            .or_else(|| {
                self.mux
                    .sessions()
                    .first()
                    .map(|session| session.id.clone())
            });
    }

    fn move_sidebar_hover(&mut self, delta: isize) {
        self.ensure_sidebar_hovered_session();
        let Some(current) = self.sidebar_hovered_index() else {
            return;
        };
        let sessions = self.mux.sessions();
        let next = (current as isize + delta).rem_euclid(sessions.len() as isize) as usize;
        self.sidebar_hovered_session = sessions.get(next).map(|session| session.id.clone());
    }

    fn activate_sidebar_hovered_session(&mut self) {
        self.ensure_sidebar_hovered_session();
        if let Some(session_id) = self.sidebar_hovered_session.clone() {
            self.activate_session_from_ui(&session_id);
        }
        self.input_focus = InputFocus::Terminal;
    }

    fn sidebar_hovered_index(&self) -> Option<usize> {
        let hovered = self.sidebar_hovered_session.as_deref()?;
        self.mux
            .sessions()
            .iter()
            .position(|session| session.id == hovered || session.name == hovered)
    }

    fn session_id_matching(&self, value: &str) -> Option<String> {
        self.mux
            .sessions()
            .iter()
            .find(|session| session.id == value || session.name == value)
            .map(|session| session.id.clone())
    }

    fn apply_session_navigation_action(&mut self, action: MuxKeyAction) -> bool {
        let target = match action {
            MuxKeyAction::SelectSession(index) => self
                .mux
                .sessions()
                .get(index.saturating_sub(1) as usize)
                .map(|session| session.id.clone()),
            MuxKeyAction::NextSession => self.relative_session(1),
            MuxKeyAction::PreviousSession => self.relative_session(-1),
            MuxKeyAction::LastSession => self.mux.previous_selected_session().map(str::to_owned),
            _ => None,
        };
        let Some(target) = target else {
            return false;
        };
        self.mux.activate_session(&target);
        true
    }

    fn relative_session(&self, delta: isize) -> Option<String> {
        let sessions = self.mux.sessions();
        if sessions.is_empty() {
            return None;
        }
        let selected = self.mux.selected_session();
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

    fn apply_terminal_scroll_action(&mut self, action: TerminalScrollAction) {
        let delta = match action {
            TerminalScrollAction::Top => -1_000_000,
            TerminalScrollAction::Bottom => 1_000_000,
            TerminalScrollAction::PageUp => -(self.terminal.grid_size().1 as isize),
            TerminalScrollAction::PageDown => self.terminal.grid_size().1 as isize,
            TerminalScrollAction::Lines(lines) => isize::from(lines),
        };
        if let Err(error) = self.terminal.scroll_viewport_delta(delta) {
            self.last_error = Some(error.to_string());
        }
    }

    fn apply_terminal_input(&mut self, command: TerminalInputCommand) {
        match command {
            TerminalInputCommand::Text(text) => {
                if let Err(error) = self.terminal.write_input(text.as_bytes()) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalInputCommand::Paste(text) => {
                if let Err(error) = self.terminal.write_paste(&text) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalInputCommand::Focus(focused) => {
                if let Err(error) = self.terminal.encode_focus(focused) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalInputCommand::Key(input) => {
                if let Err(error) = self.terminal.encode_key(input) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalInputCommand::Mouse(input) => {
                if let Err(error) = self.terminal.encode_mouse(input) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalInputCommand::MouseWheel {
                input,
                scroll_delta,
            } => {
                if let Err(error) = self.terminal.handle_mouse_wheel(input, scroll_delta) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WindowFullscreen;

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
    fn initial_sidebar_metadata_refresh_is_deferred() {
        let started_at = Instant::now();
        let last_refresh = initial_sidebar_metadata_refresh_mark(started_at);

        assert!(!sidebar_metadata_refresh_due(
            last_refresh,
            started_at,
            false
        ));
        assert!(sidebar_metadata_refresh_due(
            last_refresh,
            started_at + SIDEBAR_METADATA_INITIAL_DELAY,
            false
        ));
    }

    #[test]
    fn sidebar_metadata_refresh_waits_when_pending() {
        let now = Instant::now();
        let last_refresh = now - SIDEBAR_METADATA_REFRESH_INTERVAL;

        assert!(!sidebar_metadata_refresh_due(last_refresh, now, true));
    }

    #[test]
    fn initial_macos_app_icon_install_is_deferred() {
        let started_at = Instant::now();
        let next_attempt = initial_macos_app_icon_install_after(started_at);

        assert!(!macos_app_icon_install_due(false, next_attempt, started_at));
        assert!(macos_app_icon_install_due(
            false,
            next_attempt,
            started_at + MACOS_APP_ICON_INITIAL_DELAY
        ));
    }

    #[test]
    fn macos_app_icon_install_does_not_retry_when_installed() {
        let now = Instant::now();
        let next_attempt = now - MACOS_APP_ICON_INITIAL_DELAY;

        assert!(!macos_app_icon_install_due(true, next_attempt, now));
    }

    #[test]
    fn failed_macos_app_icon_install_retries_later() {
        let now = Instant::now();
        let next_attempt = next_macos_app_icon_retry(now);

        assert!(!macos_app_icon_install_due(false, next_attempt, now));
        assert!(macos_app_icon_install_due(
            false,
            next_attempt,
            now + MACOS_APP_ICON_RETRY_INTERVAL
        ));
    }

    #[test]
    fn new_mux_session_request_uses_configured_working_directory() {
        let mut config = BoottyConfig::default();
        config.session.working_directory = Some("/tmp/bootty-project".into());

        let request = new_mux_session_request_with_name(&config, "review-session");

        assert_eq!(request.session_id, "review-session");
        assert_eq!(request.cwd, "/tmp/bootty-project");
    }

    fn test_state() -> AppState {
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let config = BoottyConfig {
            config_path: std::env::temp_dir().join(format!("bootty-test-{unique}.toml")),
            ..BoottyConfig::default()
        };
        AppState::new(config, repaint, None, None).expect("state")
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
        state.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: "project".to_owned(),
                cwd: "/repo".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );

        state.activate_session_from_ui("local");
        state.activate_session_from_ui("project");
        state.apply_mux_key_action(MuxKeyAction::LastSession);
        assert_eq!(state.mux.selected_session(), Some("local"));

        state.apply_mux_key_action(MuxKeyAction::LastSession);
        assert_eq!(state.mux.selected_session(), Some("project"));
    }

    #[test]
    fn move_session_reorders_bootty_owned_session_order() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let alpha = format!("alpha-{unique}");
        let beta = format!("beta-{unique}");
        state.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: alpha.clone(),
                cwd: "/repo/a".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: beta.clone(),
                cwd: "/repo/b".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );

        state.apply_mux_key_action(MuxKeyAction::MoveSession(-1));

        assert_eq!(state.mux.selected_session(), Some(beta.as_str()));
        let ordered = state
            .mux
            .sessions()
            .iter()
            .map(|session| session.name.as_str())
            .collect::<Vec<_>>();
        let beta_index = ordered
            .iter()
            .position(|name| *name == beta)
            .expect("beta present");
        let alpha_index = ordered
            .iter()
            .position(|name| *name == alpha)
            .expect("alpha present");
        assert!(beta_index < alpha_index, "{ordered:?}");
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
