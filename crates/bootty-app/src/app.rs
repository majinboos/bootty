use std::{sync::mpsc, time::Instant};

use anyhow::Result;
use eframe::egui::{self, Pos2, Rect, UiBuilder};

use crate::{
    app_actions::{
        AppAction, AppKeyBindings, FontSizeAction, KeybindAction, MuxKeyAction,
        TerminalScrollAction, builtin_app_action_for_direct_key, split_app_actions_for_bindings,
    },
    config::{BoottyConfig, ConfigState, load_config_from_path},
    config_reload::{ConfigHotReload, new_session_only_config_changed},
    diagnostics::{
        STATUS_METRICS_SAMPLE_INTERVAL, StabilityTrace, StabilityTraceSample, StatusMetrics,
        should_sample_status_metrics,
    },
    direct_input::{DirectKeyInput, suppress_egui_events_for_direct_input},
    geometry::TerminalSurface,
    input::{
        InputSnapshot, TerminalInputCommand, focus::InputFocus, router::route_events,
        terminal_input_commands_with_modifier_remaps,
    },
    modifier_remap::ModifierRemapSet,
    mux::{
        command::MuxCommand, config::selected_backend, controller::MuxController,
        terminal::ActiveTerminal,
    },
    platform::{
        apply_macos_non_native_fullscreen_presentation, macos_handles_non_native_fullscreen_frame,
        read_clipboard_text, restore_macos_presentation, spawn_new_window,
    },
    renderer::TerminalWidget,
    scheduler::{RepaintScheduler, RepaintSignal},
    terminal::DrainStats,
    theme::theme_from_config,
    ui::{
        chrome::{self, SidebarModel, StatusBarModel},
        new_session_picker::{NewMuxSessionDialog, NewSessionPickerEvent},
    },
};

pub struct BoottyApp {
    terminal: ActiveTerminal,
    terminal_widget: TerminalWidget,
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
    has_new_session_config_changes: bool,
    mux: MuxController,
    direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
    pending_direct_input: Vec<DirectKeyInput>,
    modifier_remaps: ModifierRemapSet,
    stability_trace: Option<StabilityTrace>,
    config_hot_reload: ConfigHotReload,
    new_mux_session_dialog: Option<NewMuxSessionDialog>,
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

impl BoottyApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Result<Self> {
        Self::new_with_config(cc, BoottyConfig::default())
    }

    pub fn new_with_config(cc: &eframe::CreationContext<'_>, config: BoottyConfig) -> Result<Self> {
        Self::new_inner(cc, config, None)
    }

    pub fn new_with_direct_input(
        cc: &eframe::CreationContext<'_>,
        config: BoottyConfig,
        direct_input_rx: mpsc::Receiver<DirectKeyInput>,
    ) -> Result<Self> {
        Self::new_inner(cc, config, Some(direct_input_rx))
    }

    fn new_inner(
        cc: &eframe::CreationContext<'_>,
        config: BoottyConfig,
        direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
    ) -> Result<Self> {
        let repaint_ctx = cc.egui_ctx.clone();
        let modifier_remaps = config.input.modifier_remaps()?;
        let keybinds = config
            .input
            .keybinds_for_backend(config.multiplexer.backend);
        let app_key_bindings = AppKeyBindings::from_keybinds(&keybinds)?;
        let stability_trace = StabilityTrace::from_config(&config);
        let text_config = config.font.terminal_text_config();
        let session_config = config.terminal_session_config();
        let config_hot_reload = ConfigHotReload::new(&config.config_path);
        let _backend = selected_backend(&config.multiplexer);
        apply_macos_non_native_fullscreen_presentation(&config.window);

        Ok(Self {
            terminal: ActiveTerminal::new(
                TerminalWidget::initial_geometry(),
                &config.multiplexer,
                session_config,
                std::sync::Arc::new(move || repaint_ctx.request_repaint()),
            ),
            terminal_widget: TerminalWidget::new(
                cc.wgpu_render_state
                    .as_ref()
                    .map(|render_state| render_state.target_format),
            )
            .with_text_config(text_config),
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
            has_new_session_config_changes: false,
            mux: MuxController::new(),
            direct_input_rx,
            pending_direct_input: Vec::new(),
            modifier_remaps,
            stability_trace,
            config_hot_reload,
            new_mux_session_dialog: None,
        })
    }
}

impl eframe::App for BoottyApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.drain_direct_input();
        suppress_egui_events_for_direct_input(&mut raw_input.events, &self.pending_direct_input);
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.last_drain = self.terminal.drain_pty();
        match self.terminal.child_exited() {
            Ok(true) => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            Ok(false) => {}
            Err(error) => self.last_error = Some(error.to_string()),
        }

        let mux_config = self.config().multiplexer.clone();
        if let Some(error) = self.mux.refresh_sessions(ctx, &mux_config) {
            self.last_error = Some(error);
        }
        if let Some(result) = self.mux.poll_command() {
            self.last_error = result.err();
        }
        if let Err(error) = self
            .terminal
            .sync_mux_anchor(&mux_config, self.mux.selected_session_anchor().cloned())
        {
            self.last_error = Some(error.to_string());
        }
        self.hot_reload_config_if_changed(ctx);
        let input_commands = self.handle_direct_input(ctx) + self.handle_egui_input(ctx);
        self.last_frame_dt_ms = ctx.input(|input| input.stable_dt) * 1000.0;

        let metrics = self.terminal_widget.metrics();
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
                text_runs: metrics.text_runs,
                last_error: self.last_error.as_deref(),
            });
        }
        if should_sample_status_metrics(self.last_status_metrics_sample.elapsed()) {
            self.status_metrics = StatusMetrics {
                drain: self.last_drain,
                renderer: metrics,
                cols,
                rows,
            };
            self.last_status_metrics_sample = Instant::now();
        }
        let repaint = self.repaint_scheduler.recommend(RepaintSignal {
            drained_bytes: self.last_drain.bytes,
            drain_elapsed_us: self.last_drain.elapsed_us,
            pending_bytes: pending_pty_bytes,
            dirty_rows: metrics.dirty_rows,
            cursor_blinking: metrics.cursor_blinking,
            input_commands,
        });
        ctx.request_repaint_after(repaint.after);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let palette = self.ui_theme().palette;
        egui::Frame::NONE.fill(palette.mantle).show(ui, |ui| {
            self.show_fixed_layout(ui);
        });
        self.show_new_mux_session_dialog(ui.ctx());
    }
}

impl BoottyApp {
    fn config(&self) -> &BoottyConfig {
        self.config_state.current()
    }

    fn ui_theme(&self) -> bootty_ui::Theme {
        theme_from_config(self.config())
    }

    fn show_fixed_layout(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let chrome = self.config().chrome.clone();
        let sidebar_width = if chrome.sidebar {
            chrome.sidebar_width
        } else {
            0.0
        };
        let gap = if chrome.sidebar && sidebar_width > 0.0 {
            chrome.gap
        } else {
            0.0
        };
        let status_height = if chrome.status_bar {
            chrome.status_height
        } else {
            0.0
        };
        let show_window_tabs = matches!(
            self.config().multiplexer.backend,
            crate::config::MultiplexerBackendConfig::Rmux
                | crate::config::MultiplexerBackendConfig::Native
        ) && !self.mux.selected_session_windows().is_empty();
        let window_tabs_height = if show_window_tabs { 34.0 } else { 0.0 };
        let sidebar_rect = chrome::sidebar_rect(rect, &chrome);
        let right_rect = Rect::from_min_max(
            Pos2::new((sidebar_rect.max.x + gap).min(rect.max.x), rect.min.y),
            rect.max,
        );
        let status_rect = Rect::from_min_size(
            right_rect.min,
            egui::vec2(right_rect.width(), status_height.min(right_rect.height())),
        );
        let window_tabs_rect = Rect::from_min_max(
            Pos2::new(right_rect.min.x, status_rect.max.y),
            Pos2::new(
                right_rect.max.x,
                (status_rect.max.y + window_tabs_height).min(right_rect.max.y),
            ),
        );
        let terminal_rect = Rect::from_min_max(
            Pos2::new(right_rect.min.x, window_tabs_rect.max.y),
            right_rect.max,
        );

        if chrome.sidebar {
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(sidebar_rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
                |ui| {
                    if let Some(session_id) = chrome::show_sidebar(
                        ui,
                        self.ui_theme().palette,
                        sidebar_rect.height(),
                        SidebarModel {
                            sessions: self.mux.sessions(),
                            selected_session: self.mux.selected_session(),
                        },
                    ) {
                        let mux_config = self.config().multiplexer.clone();
                        self.mux
                            .activate_session(&session_id, ui.ctx(), &mux_config);
                    }
                },
            );
        }

        if chrome.status_bar {
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(status_rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                |ui| {
                    chrome::show_status_bar(
                        ui,
                        self.ui_theme().palette,
                        StatusBarModel {
                            backend: selected_backend(&self.config().multiplexer),
                            selected_session_name: chrome::selected_session_name(
                                self.mux.sessions(),
                                self.mux.selected_session(),
                            ),
                            metrics: self.status_metrics,
                            last_error: self.last_error.as_deref(),
                        },
                    );
                },
            );
        }

        if show_window_tabs {
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(window_tabs_rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                |ui| {
                    if let Some(window_id) = chrome::show_window_tabs(
                        ui,
                        self.ui_theme().palette,
                        chrome::WindowTabsModel {
                            windows: self.mux.selected_session_windows(),
                            selected_window: self.mux.selected_window(),
                        },
                    ) && let Some(session_id) = self.mux.selected_session().map(str::to_owned)
                    {
                        let mux_config = self.config().multiplexer.clone();
                        self.mux
                            .activate_window(&session_id, &window_id, ui.ctx(), &mux_config);
                    }
                },
            );
        }

        ui.scope_builder(
            UiBuilder::new()
                .max_rect(terminal_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
            |ui| match self.terminal_widget.show(ui, &mut self.terminal) {
                Ok(surface) => {
                    self.terminal_surface = Some(surface);
                }
                Err(error) => self.last_error = Some(error.to_string()),
            },
        );
    }

    fn open_new_mux_session_dialog(&mut self) {
        self.new_mux_session_dialog = Some(NewMuxSessionDialog::open());
    }

    fn show_new_mux_session_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.new_mux_session_dialog.take() else {
            return;
        };
        match dialog.show(ctx, self.ui_theme()) {
            NewSessionPickerEvent::None => {
                self.new_mux_session_dialog = Some(dialog);
            }
            NewSessionPickerEvent::Close => {}
            NewSessionPickerEvent::NewWorktreeUnavailable => {
                self.last_error = Some("new worktree creation is not wired yet".to_owned());
                self.new_mux_session_dialog = Some(dialog);
            }
            NewSessionPickerEvent::CreateSession(request) => {
                let mux_config = self.config().multiplexer.clone();
                self.mux.create_project_session(request, ctx, &mux_config);
            }
        }
    }

    fn reload_config(&mut self, ctx: &egui::Context) -> bool {
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
            self.terminal_widget
                .set_text_config(next.font.terminal_text_config());
        }
        if previous.window.title != next.window.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(next.window.title.clone()));
        }
        if previous.window.fullscreen != next.window.fullscreen {
            apply_macos_non_native_fullscreen_presentation(&next.window);
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(
                next.window.native_fullscreen_enabled(),
            ));
            if !macos_handles_non_native_fullscreen_frame(&next.window) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(
                    next.window.non_native_fullscreen_enabled(),
                ));
            }
        }
        if previous.window.decorations_enabled() != next.window.decorations_enabled() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(
                next.window.decorations_enabled(),
            ));
        }
        if previous.diagnostics != next.diagnostics {
            self.stability_trace = StabilityTrace::from_config(&next);
        }

        self.modifier_remaps = modifier_remaps;
        self.app_key_bindings = app_key_bindings;
        self.has_new_session_config_changes = new_session_only_config_changed(&previous, &next)
            || self.has_new_session_config_changes;
        self.config_state.accept(next);
        self.last_error = if self.has_new_session_config_changes {
            Some("config reloaded; session/window creation changes apply next time".to_owned())
        } else {
            None
        };
        ctx.request_repaint();
        true
    }

    fn hot_reload_config_if_changed(&mut self, ctx: &egui::Context) {
        if !self.config_hot_reload.changed(Instant::now()) {
            return;
        }
        let path = self.config().config_path.clone();
        if self.reload_config(ctx) {
            self.config_hot_reload.refresh_after_reload(&path);
        }
    }

    fn split_app_actions(
        &mut self,
        events: Vec<egui::Event>,
    ) -> (Vec<egui::Event>, Vec<KeybindAction>) {
        split_app_actions_for_bindings(&mut self.app_key_bindings, events)
    }
    fn handle_egui_input(&mut self, ctx: &egui::Context) -> usize {
        let (snapshot, actions) = ctx.input(|input| {
            let routed = route_events(self.input_focus, input.events.clone());
            let (events, actions) = self.split_app_actions(routed.terminal_events);
            let events = if self.new_mux_session_dialog.is_some() {
                Vec::new()
            } else {
                events
            };
            (
                InputSnapshot {
                    events,
                    modifiers: input.modifiers,
                    hover_pos: input.pointer.hover_pos(),
                    pressed_mouse_button: crate::input::pressed_mouse_button_from_egui(
                        &input.pointer,
                    ),
                    surface: self.terminal_surface,
                    mouse_exclusion: self
                        .terminal_surface
                        .map(crate::renderer::scrollbar_hit_rect),
                },
                actions,
            )
        });
        let commands =
            terminal_input_commands_with_modifier_remaps(snapshot, &self.modifier_remaps);
        let count = commands.len() + actions.len();

        for action in actions {
            self.apply_keybind_action(ctx, action);
        }

        for command in commands {
            self.apply_terminal_input(command);
        }

        count
    }

    fn drain_direct_input(&mut self) {
        let Some(rx) = &self.direct_input_rx else {
            return;
        };
        self.pending_direct_input.extend(rx.try_iter());
    }

    fn handle_direct_input(&mut self, ctx: &egui::Context) -> usize {
        let inputs = std::mem::take(&mut self.pending_direct_input);
        let count = inputs.len();
        for input in inputs {
            let mut input = input.input();
            input.mods = self.modifier_remaps.apply(input.mods);
            if self.new_mux_session_dialog.is_some() {
                continue;
            }
            if let Some(action) = self.app_key_bindings.action_for_input(input) {
                self.apply_keybind_action(ctx, action);
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

    fn apply_keybind_action(&mut self, ctx: &egui::Context, action: KeybindAction) {
        match action {
            KeybindAction::App(AppAction::ReloadConfig) => {
                if self.reload_config(ctx) {
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
            KeybindAction::App(AppAction::Close) => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            KeybindAction::App(AppAction::ToggleFullscreen) => {
                let fullscreen = ctx.input(|input| input.viewport().fullscreen.unwrap_or(false));
                if self.config().window.native_fullscreen_enabled() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
                } else {
                    let maximized = ctx.input(|input| input.viewport().maximized.unwrap_or(false));
                    let next_maximized = !maximized;
                    if next_maximized {
                        apply_macos_non_native_fullscreen_presentation(&self.config().window);
                    } else {
                        restore_macos_presentation();
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                    if !macos_handles_non_native_fullscreen_frame(&self.config().window) {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(next_maximized));
                    }
                }
            }
            KeybindAction::Mux(action) => self.apply_mux_key_action(ctx, action),
            KeybindAction::Scroll(action) => self.apply_terminal_scroll_action(action),
            KeybindAction::Write(bytes) => {
                if let Err(error) = self.terminal.write_input(&bytes) {
                    self.last_error = Some(error.to_string());
                }
            }
            KeybindAction::Font(action) => self.apply_font_size_action(action),
            KeybindAction::CopyToClipboard => {
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestCopy);
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

    fn apply_mux_key_action(&mut self, ctx: &egui::Context, action: MuxKeyAction) {
        let selected_session = self.mux.selected_session().unwrap_or("local").to_owned();
        let command = match action {
            MuxKeyAction::NextSession => MuxCommand::ActivateNextSession,
            MuxKeyAction::PreviousSession => MuxCommand::ActivatePreviousSession,
            MuxKeyAction::LastSession => MuxCommand::ActivateLastSession,
            MuxKeyAction::SelectSession(index) => MuxCommand::ActivateSessionIndex { index },
            MuxKeyAction::MoveSession(delta) => MuxCommand::MoveSession { delta },
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
            MuxKeyAction::DitchSession => MuxCommand::DitchSession {
                session_id: selected_session,
            },
        };
        let mux_config = self.config().multiplexer.clone();
        self.mux.execute_command(ctx, &mux_config, command);
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

    fn apply_font_size_action(&mut self, action: FontSizeAction) {
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
        self.terminal_widget
            .set_text_config(self.config().font.terminal_text_config());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_mux_session_request_uses_configured_working_directory() {
        let mut config = BoottyConfig::default();
        config.session.working_directory = Some("/tmp/bootty-project".into());

        let request = new_mux_session_request_with_name(&config, "review-session");

        assert_eq!(request.session_id, "review-session");
        assert_eq!(request.cwd, "/tmp/bootty-project");
    }
}
