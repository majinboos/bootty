use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::Result;
use eframe::egui::{
    self, FontData, FontDefinitions, FontFamily, Pos2, Rect, TextureHandle, UiBuilder,
};

use crate::{
    app_actions::{
        AppAction, AppKeyBindings, FontSizeAction, KeybindAction, MuxKeyAction,
        TerminalScrollAction, builtin_app_action_for_direct_key, split_app_actions_for_bindings,
    },
    config::{BoottyConfig, ConfigState, WindowConfig, load_config_from_path},
    config_reload::{CONFIG_HOT_RELOAD_INTERVAL, ConfigHotReload, new_session_only_config_changed},
    diagnostics::{
        STATUS_METRICS_SAMPLE_INTERVAL, StabilityTrace, StabilityTraceSample, StatusMetrics,
        should_sample_status_metrics,
    },
    direct_input::{DirectKeyInput, ModifierSideState, suppress_egui_events_for_direct_input},
    geometry::TerminalSurface,
    input::{
        InputSnapshot, TerminalInputCommand, focus::InputFocus, router::route_events,
        terminal_input_commands_with_options,
    },
    modifier_remap::ModifierRemapSet,
    mux::{
        command::MuxCommand,
        config::selected_backend,
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
    renderer::TerminalWidget,
    scheduler::{RepaintScheduler, RepaintSignal},
    terminal::DrainStats,
    theme::{theme_from_config, theme_palette_from_config},
    ui::{
        chrome::{self, SidebarModel, StatusBarModel},
        new_session_picker::{NewMuxSessionDialog, NewSessionPickerEvent},
    },
};

const SIDEBAR_METADATA_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const SIDEBAR_METADATA_INITIAL_DELAY: Duration = Duration::from_secs(1);
const MACOS_APP_ICON_INITIAL_DELAY: Duration = Duration::from_secs(1);
const MACOS_APP_ICON_RETRY_INTERVAL: Duration = Duration::from_secs(5);

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
    modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
    modifier_sides: ModifierSideState,
    pending_direct_input: Vec<DirectKeyInput>,
    modifier_remaps: ModifierRemapSet,
    macos_option_as_alt: crate::terminal::MacosOptionAsAlt,
    stability_trace: Option<StabilityTrace>,
    config_hot_reload: ConfigHotReload,
    sidebar_metadata: SidebarMetadata,
    last_sidebar_metadata_refresh: Instant,
    sidebar_metadata_tx: Option<mpsc::Sender<Vec<SidebarMetadataSession>>>,
    sidebar_metadata_rx: Option<mpsc::Receiver<SidebarMetadata>>,
    sidebar_metadata_pending: bool,
    new_mux_session_dialog: Option<NewMuxSessionDialog>,
    app_icon_texture: Option<TextureHandle>,
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
        Self::new_inner(cc, config, None, None)
    }

    pub fn new_with_direct_input(
        cc: &eframe::CreationContext<'_>,
        config: BoottyConfig,
        direct_input_rx: mpsc::Receiver<DirectKeyInput>,
        modifier_side_rx: mpsc::Receiver<ModifierSideState>,
    ) -> Result<Self> {
        Self::new_inner(cc, config, Some(direct_input_rx), Some(modifier_side_rx))
    }

    fn new_inner(
        cc: &eframe::CreationContext<'_>,
        config: BoottyConfig,
        direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
        modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
    ) -> Result<Self> {
        configure_egui_fonts(&cc.egui_ctx, &config.font.family);
        let repaint_ctx = cc.egui_ctx.clone();
        let modifier_remaps = config.input.modifier_remaps()?;
        let macos_option_as_alt = config.input.macos_option_as_alt.into();
        let keybinds = config
            .input
            .keybinds_for_backend(config.multiplexer.backend);
        let app_key_bindings = AppKeyBindings::from_keybinds(&keybinds)?;
        let stability_trace = StabilityTrace::from_config(&config);
        let text_config = config.font.terminal_text_config();
        let session_config = config.terminal_session_config();
        let config_hot_reload = ConfigHotReload::new(&config.config_path);
        let _backend = selected_backend(&config.multiplexer);
        let macos_non_native_fullscreen_active = config.window.non_native_fullscreen_enabled();
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
            modifier_side_rx,
            modifier_sides: ModifierSideState::default(),
            pending_direct_input: Vec::new(),
            modifier_remaps,
            macos_option_as_alt,
            stability_trace,
            config_hot_reload,
            sidebar_metadata: SidebarMetadata::default(),
            last_sidebar_metadata_refresh: initial_sidebar_metadata_refresh_mark(Instant::now()),
            sidebar_metadata_tx: None,
            sidebar_metadata_rx: None,
            sidebar_metadata_pending: false,
            new_mux_session_dialog: None,
            app_icon_texture: None,
            macos_app_icon_installed: false,
            next_macos_app_icon_install: initial_macos_app_icon_install_after(Instant::now()),
            macos_non_native_fullscreen_active,
        })
    }
}

impl eframe::App for BoottyApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.drain_direct_input();
        suppress_egui_events_for_direct_input(&mut raw_input.events, &self.pending_direct_input);
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = Instant::now();
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
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            Ok(false) => {}
            Err(error) => self.last_error = Some(error.to_string()),
        }

        if let Some(error) = self
            .mux
            .refresh_sessions(ctx, &self.config_state.current().multiplexer)
        {
            self.last_error = Some(error);
        }
        if let Some(result) = self.mux.poll_command() {
            self.last_error = result.err();
        }
        if self.config_state.current().chrome.sidebar {
            self.refresh_sidebar_metadata(ctx);
        }
        if let Err(error) = self.terminal.sync_mux_anchor(
            &self.config_state.current().multiplexer,
            self.mux.selected_session_anchor(),
        ) {
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
        ctx.request_repaint_after(repaint.after.min(CONFIG_HOT_RELOAD_INTERVAL));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let palette = self.ui_theme().palette;
        egui::Frame::NONE.fill(palette.mantle).show(ui, |ui| {
            self.show_fixed_layout(ui);
        });
        self.show_new_mux_session_dialog(ui.ctx());
    }
}

fn configure_egui_fonts(ctx: &egui::Context, families: &[String]) {
    let db = bootty_render::font_database::system_font_database();
    let mut fonts = FontDefinitions::default();
    for family in families.iter().rev() {
        let query_families = [fontdb::Family::Name(family)];
        let query = fontdb::Query {
            families: &query_families,
            ..fontdb::Query::default()
        };
        let Some(id) = db.query(&query) else {
            continue;
        };
        let Some((bytes, index)) = db.with_face_data(id, |data, index| (data.to_vec(), index))
        else {
            continue;
        };
        let name = format!("bootty-ui-{family}");
        let mut font_data = FontData::from_owned(bytes);
        font_data.index = index;
        fonts
            .font_data
            .insert(name.clone(), std::sync::Arc::new(font_data));
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .insert(0, name.clone());
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, name);
    }

    ctx.set_fonts(fonts);
}
impl BoottyApp {
    fn config(&self) -> &BoottyConfig {
        self.config_state.current()
    }

    fn ui_theme(&self) -> bootty_ui::Theme {
        theme_from_config(self.config())
    }

    fn refresh_sidebar_metadata(&mut self, ctx: &egui::Context) {
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

        self.ensure_sidebar_metadata_worker(ctx);
        let Some(tx) = &self.sidebar_metadata_tx else {
            return;
        };
        let max_sessions = self.sidebar_metadata_session_budget(ctx);
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

    fn sidebar_metadata_session_budget(&self, ctx: &egui::Context) -> usize {
        let height = ctx.input(|input| input.content_rect().height());
        let fullscreen_chrome = self.macos_non_native_fullscreen_active
            || ctx.input(|input| input.viewport().fullscreen.unwrap_or(false));
        let title_visible = self.config().window.custom_chrome_title_visible();
        let top_inset = if fullscreen_chrome && !title_visible {
            28.0
        } else {
            0.0
        };
        chrome::sidebar_metadata_session_budget(
            height,
            top_inset,
            title_visible,
            self.sidebar_metadata.usage_lines(),
        )
    }

    fn ensure_sidebar_metadata_worker(&mut self, ctx: &egui::Context) {
        if self.sidebar_metadata_tx.is_some() && self.sidebar_metadata_rx.is_some() {
            return;
        }

        let (request_tx, request_rx) = mpsc::channel::<Vec<SidebarMetadataSession>>();
        let (result_tx, result_rx) = mpsc::channel::<SidebarMetadata>();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            while let Ok(sessions) = request_rx.recv() {
                let metadata = collect_sidebar_metadata(&sessions);
                if result_tx.send(metadata).is_err() {
                    break;
                }
                repaint.request_repaint();
            }
        });
        self.sidebar_metadata_tx = Some(request_tx);
        self.sidebar_metadata_rx = Some(result_rx);
    }

    fn show_fixed_layout(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let palette = theme_palette_from_config(self.config());
        let chrome = &self.config().chrome;
        let sidebar = chrome.sidebar;
        let status_bar = chrome.status_bar;
        let configured_sidebar_width = chrome.sidebar_width;
        let status_height_config = chrome.status_height;
        let chrome_gap = chrome.gap;
        let fullscreen_chrome = self.macos_non_native_fullscreen_active
            || ui
                .ctx()
                .input(|input| input.viewport().fullscreen.unwrap_or(false));
        let sidebar_width = if sidebar {
            configured_sidebar_width
        } else {
            0.0
        };
        let gap = if sidebar && sidebar_width > 0.0 && !fullscreen_chrome {
            chrome_gap
        } else {
            0.0
        };
        let status_height = if status_bar {
            status_height_config
        } else {
            0.0
        };
        let show_window_tabs = matches!(
            self.config().multiplexer.backend,
            crate::config::MultiplexerBackendConfig::Rmux
                | crate::config::MultiplexerBackendConfig::Native
        ) && !self.mux.selected_session_windows().is_empty();
        let window_tabs_height = if show_window_tabs { 34.0 } else { 0.0 };
        let sidebar_rect = if sidebar {
            Rect::from_min_size(
                rect.min,
                egui::vec2(sidebar_width.min(rect.width()), rect.height()),
            )
        } else {
            Rect::from_min_size(rect.min, egui::vec2(0.0, rect.height()))
        };
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

        if sidebar {
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(sidebar_rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
                |ui| {
                    let title_visible = self.config().window.custom_chrome_title_visible();
                    let reserve_titlebar_buttons =
                        self.config().window.reserves_macos_titlebar_button_area();
                    let top_inset = if fullscreen_chrome && !title_visible {
                        28.0
                    } else {
                        0.0
                    };
                    let title_icon = title_visible.then(|| {
                        chrome::load_app_icon_texture(ui.ctx(), &mut self.app_icon_texture)
                    });
                    if let Some(session_id) = chrome::show_sidebar(
                        ui,
                        palette,
                        sidebar_rect.height(),
                        SidebarModel {
                            sessions: self.mux.sessions(),
                            selected_session: self.mux.selected_session(),
                            metadata: &self.sidebar_metadata,
                            title_visible,
                            reserve_titlebar_buttons,
                            title_icon: title_icon.as_ref(),
                            top_inset,
                            border_visible: !fullscreen_chrome,
                            separator_visible: !fullscreen_chrome,
                        },
                    ) {
                        let mux_config = self.config().multiplexer.clone();
                        self.mux
                            .activate_session(&session_id, ui.ctx(), &mux_config);
                    }
                },
            );
        }

        if status_bar {
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(status_rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                |ui| {
                    chrome::show_status_bar(
                        ui,
                        palette,
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
                        palette,
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
            self.macos_non_native_fullscreen_active = next.window.non_native_fullscreen_enabled();
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
        self.macos_option_as_alt = next.input.macos_option_as_alt.into();
        self.app_key_bindings = app_key_bindings;
        self.terminal
            .set_terminal_config(next.terminal_session_config());
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
            let (events, actions) = self.split_app_actions(input.events.clone());
            let routed = route_events(self.input_focus, events);
            let events = if self.new_mux_session_dialog.is_some() {
                Vec::new()
            } else {
                routed.terminal_events
            };
            (
                InputSnapshot {
                    events,
                    modifiers: input.modifiers,
                    modifier_sides: self.modifier_sides,
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
        let commands = terminal_input_commands_with_options(
            snapshot,
            &self.modifier_remaps,
            self.macos_option_as_alt,
        );
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
        if let Some(rx) = &self.modifier_side_rx {
            let mut latest = self.modifier_sides;
            let mut latest_alt = None;
            for side_state in rx.try_iter() {
                if side_state.left_alt || side_state.right_alt {
                    latest_alt = Some(side_state);
                }
                latest = side_state;
            }
            self.modifier_sides = latest_alt.unwrap_or(latest);
        }
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
                if should_toggle_native_fullscreen(&self.config().window) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
                } else {
                    let viewport_maximized =
                        ctx.input(|input| input.viewport().maximized.unwrap_or(false));
                    let next_maximized = next_non_native_fullscreen_state(
                        macos_handles_non_native_fullscreen_frame(&self.config().window),
                        self.macos_non_native_fullscreen_active,
                        viewport_maximized,
                    );
                    self.macos_non_native_fullscreen_active = next_maximized;
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
            KeybindAction::App(AppAction::ToggleSidebarFocus) => {
                if self.input_focus == InputFocus::Sidebar {
                    self.input_focus = InputFocus::Terminal;
                } else {
                    self.config_state.current_mut().chrome.sidebar = true;
                    self.input_focus = InputFocus::Sidebar;
                }
                ctx.request_repaint();
            }
            KeybindAction::App(AppAction::ToggleSidebarVisibility) => {
                let chrome = &mut self.config_state.current_mut().chrome;
                chrome.sidebar = !chrome.sidebar;
                if !chrome.sidebar {
                    self.input_focus = InputFocus::Terminal;
                }
                ctx.request_repaint();
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
        if self.apply_session_navigation_action(ctx, action) {
            return;
        }
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

    fn apply_session_navigation_action(
        &mut self,
        ctx: &egui::Context,
        action: MuxKeyAction,
    ) -> bool {
        let target = match action {
            MuxKeyAction::SelectSession(index) => self
                .mux
                .sessions()
                .get(index.saturating_sub(1) as usize)
                .map(|session| session.id.clone()),
            MuxKeyAction::NextSession => self.relative_session(1),
            MuxKeyAction::PreviousSession => self.relative_session(-1),
            _ => None,
        };
        let Some(target) = target else {
            return false;
        };
        let mux_config = self.config().multiplexer.clone();
        self.mux.activate_session(&target, ctx, &mux_config);
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
}
