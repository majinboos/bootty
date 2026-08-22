use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use anyhow::Result;
use bootty_command::CommandInvocation;
use bootty_config::config::MultiplexerBackendConfig;
use bootty_config::{
    config::{AppearanceMode, AppearanceVariant, BoottyConfig, ConfigDocument, ConfigResult},
    config_reload::CONFIG_HOT_RELOAD_INTERVAL,
};
use bootty_mux::{
    RepaintHandle,
    controller::{MuxController, MuxScope, SpaceId},
    provider::{MuxBackendRegistry, selected_backend},
    snapshot::{MuxSession, MuxWindow},
    terminal::{ActiveTerminal, TerminalRuntime, decode_scoped_pane_id},
};
use bootty_render::{
    geometry::{TerminalSurface, ViewTransform},
    terminal_text::TerminalTextConfig,
};
use bootty_runtime::{
    scheduler::{RepaintScheduler, RepaintSignal},
    terminal_session::DrainStats,
};
use bootty_terminal::terminal_engine::{
    TerminalSideEffect, TerminalSideEffectEvent, encode_iterm2_report_cell_size,
    encode_iterm2_report_variable, encode_osc52_response,
};
use bootty_terminal::terminal_input_model::MouseButton;
use bootty_winit::direct_input::{DirectKeyInput, ModifierSideState};
use eframe::egui::{self, Pos2, Rect};

mod dialogs;
mod ditch;
mod input;
mod keybinds;
mod mux_actions;
pub(crate) use mux_actions::ExactMuxAction;
mod recorded_chord;
mod spaces;

use crate::commands::{CommandRuntime, ExactMuxTarget};
use crate::config_runtime::AppConfigRuntime;
use crate::terminal_config::terminal_live_config;
use crate::terminal_interaction::{TerminalFocusIntent, TerminalInteractionRuntime};
use crate::ui::DialogRuntime;
use crate::workspace_runtime::TerminalProgress;
use crate::workspace_runtime::WorkspaceRuntime;
use keybinds::terminal_cursor_icon_for_mouse_shape;

use crate::{
    app_actions::AppKeyBindings,
    diagnostics::StabilityTraceSample,
    input::{WheelScrollState, focus::InputFocus},
    layout::Divider,
    platform::{
        apply_macos_non_native_fullscreen_presentation, read_clipboard_text,
        show_desktop_notification, write_clipboard_text,
    },
    renderer::RendererMetrics,
    theme::theme_from_config,
    ui::{
        session_navigation::{BindingSessionGroup, ScopedSessionTarget},
        terminal_find::{TerminalFindDialog, TerminalFindEvent},
    },
};
use bootty_mux::command::MuxCommand;
use bootty_workspace::WorkspacePersistenceError;

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
#[derive(Clone, Copy, Debug, Default)]
pub struct ViewportSnapshot {
    pub fullscreen: bool,
    pub maximized: bool,
    pub content_height: f32,
}

/// Window and screen facts the chrome layout needs, sampled once per frame outside the paint
/// pass. Each field is measured in points, in screen space; the view adds its own origin.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowChromeFacts {
    /// Native or non-native fullscreen: chrome drops its borders and reserves the notch band.
    pub fullscreen: bool,
    /// The active screen has a notch and we are fullscreen, so the band has to be cleared.
    pub notched: bool,
    /// Measured height of the macOS notch band, or 0 when unreadable.
    pub notch_band: f32,
    /// Horizontal span the notch occupies, sampled only while tabs-in-notch is on.
    pub notch_span: Option<(f32, f32)>,
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
    /// Held while the status-bar caffeinate toggle is on. The OS assertion is owner state: the
    /// chrome view only reports and toggles it.
    keep_awake: Option<keepawake::KeepAwake>,
    wheel_scroll_state: WheelScrollState,
    terminal_cursor_icon: egui::CursorIcon,
    mouse_pointer_hidden_while_typing: bool,
    last_mouse_hover_pos: Option<Pos2>,
    dialogs: DialogRuntime,
    sidebar_hovered_session: Option<ScopedSessionTarget>,
    theme_picker_restore_config: Option<BoottyConfig>,
    macos_non_native_fullscreen_active: bool,
    macos_non_native_fullscreen_pending_apply: bool,
    window_chrome: WindowChromeFacts,
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
        .or_else(bootty_config::config::default_working_directory)
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
        backends: Arc<MuxBackendRegistry>,
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
        backends: Arc<MuxBackendRegistry>,
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
            keep_awake: None,
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
            window_chrome: WindowChromeFacts::default(),
        })
    }
    pub fn config(&self) -> &BoottyConfig {
        self.config_runtime.current()
    }

    /// Identifies the accepted config/document pair, so a view can tell whether the copy it
    /// already holds is current.
    pub fn config_revision(&self) -> u64 {
        self.config_runtime.revision()
    }

    /// Built-in settings plus whatever the loaded extensions declared.
    pub(crate) fn settings_schema(
        &self,
    ) -> std::sync::Arc<bootty_config::settings_schema::SettingsSchema> {
        self.config_runtime.settings_schema()
    }

    /// Rebuild the settings schema when the extension host's declarations change.
    pub(crate) fn sync_settings_schema(
        &mut self,
        declarations: &[bootty_config::settings_schema::ExtensionSetting],
        revision: u64,
    ) {
        self.config_runtime
            .sync_settings_schema(declarations, revision);
    }

    /// The accepted extension settings, for publication to the modules that declared them.
    pub(crate) fn extension_settings(
        &self,
    ) -> std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, bootty_config::config::ExtensionSettingValue>,
    > {
        self.config().extensions.clone()
    }

    pub(crate) fn config_document(&self) -> ConfigDocument {
        self.config_runtime.document().clone()
    }

    pub(crate) fn commit_settings_document(
        &mut self,
        document: ConfigDocument,
    ) -> Result<(ConfigDocument, Option<String>, Vec<AppEffect>)> {
        let backend = self.workspace.active.binding.multiplexer.backend;
        let (change, document, outcome) = self.config_runtime.commit_document(
            document,
            backend,
            self.active_appearance_variant,
        )?;
        let warning = outcome.durability_warning().map(str::to_owned);
        let mut effects = Vec::new();
        self.apply_accepted_config(change, &mut effects);
        if let Some(warning) = &warning {
            self.last_error = Some(match self.last_error.take() {
                Some(existing) => format!("{existing}; {warning}"),
                None => warning.clone(),
            });
        }
        Ok((document, warning, effects))
    }

    fn mutate_config_document(
        &mut self,
        mutate: impl FnOnce(&mut ConfigDocument) -> ConfigResult<()>,
        effects: &mut Vec<AppEffect>,
    ) {
        let mut document = self.config_runtime.document().clone();
        if let Err(error) = mutate(&mut document) {
            self.last_error = Some(error.to_string());
            return;
        }
        match self.commit_settings_document(document) {
            Ok((_, _, accepted_effects)) => effects.extend(accepted_effects),
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    /// Apply a dragged sidebar width to the live config without touching disk, so the layout
    /// tracks the pointer each frame. [`Self::persist_sidebar_width`] writes the final value.
    pub fn set_sidebar_width_live(&mut self, width: f32) {
        self.config_runtime.set_sidebar_width(width);
    }

    /// Persist the sidebar width to `config.toml` on drag release. The live value already matches,
    /// so the hot-reload baseline is refreshed to skip the redundant reload the write would trigger.
    pub fn persist_sidebar_width(&mut self, width: f32, effects: &mut Vec<AppEffect>) {
        self.mutate_config_document(
            |document| document.set_f32(&["chrome", "sidebar-width"], width),
            effects,
        );
    }
    fn persist_appearance_mode(&mut self, mode: AppearanceMode, effects: &mut Vec<AppEffect>) {
        let token = match mode {
            AppearanceMode::System => "system",
            AppearanceMode::Light => "light",
            AppearanceMode::Dark => "dark",
        };
        self.mutate_config_document(
            |document| document.set_str(&["appearance", "mode"], token),
            effects,
        );
    }
    fn persist_active_theme(&mut self, theme: &str, effects: &mut Vec<AppEffect>) {
        let branch = match self.active_appearance_variant {
            AppearanceVariant::Light => "light",
            AppearanceVariant::Dark => "dark",
        };
        self.mutate_config_document(
            |document| document.set_str(&["appearance", branch, "theme"], theme),
            effects,
        );
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
        let warnings = self
            .workspace
            .publish_terminal_config(&config, variant, Some(&live_config));
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
    pub(super) fn active_multiplexer(&self) -> &bootty_config::config::MultiplexerConfig {
        &self.workspace.active.binding.multiplexer
    }
    pub fn multiplexer_backend(&self) -> bootty_config::config::MultiplexerBackendConfig {
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
    pub fn macos_non_native_fullscreen_active(&self) -> bool {
        self.macos_non_native_fullscreen_active
    }

    pub fn window_chrome_facts(&self) -> WindowChromeFacts {
        self.window_chrome
    }

    /// Read this frame's window/screen facts and re-assert the window chrome AppKit resets across
    /// fullscreen transitions. Runs once per frame in the update phase, never from paint.
    fn sample_window_chrome(&mut self, viewport: ViewportSnapshot) {
        let fullscreen = self.macos_non_native_fullscreen_active || viewport.fullscreen;
        if fullscreen {
            bootty_winit::window::macos_disable_titlebar_separator();
        }
        // Drop the window shadow in fullscreen; its rim otherwise reads as a border around the
        // screen-filling window. Re-asserted every frame because macOS resets it.
        bootty_winit::window::macos_set_window_shadow(!fullscreen);
        // Detect the notch by display name (stable across fullscreen/menu-bar state) rather than
        // the safe-area inset, which zeroes out when the menu bar is hidden in non-native
        // fullscreen.
        let notched = fullscreen && bootty_winit::window::macos_active_screen_is_notched();
        // Measured every frame, notched or not: the reader keeps a sticky cache that a transient
        // zero must not clear, and it has to be warm on the first fullscreen frame.
        let notch_band = bootty_winit::window::macos_active_screen_notch_height();
        let notch_span = (notched && self.config().window.fullscreen_tabs_in_notch)
            .then(bootty_winit::window::macos_active_screen_notch_span)
            .flatten();
        self.window_chrome = WindowChromeFacts {
            fullscreen,
            notched,
            notch_band,
            notch_span,
        };
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

    pub fn keep_awake_active(&self) -> bool {
        self.keep_awake.is_some()
    }

    /// Take or release the display/idle sleep assertion behind the caffeinate status item.
    pub fn toggle_keep_awake(&mut self) {
        if self.keep_awake.take().is_some() {
            return;
        }
        match keepawake::Builder::default()
            .display(true)
            .idle(true)
            .reason("Bootty status-bar toggle")
            .app_name("Bootty")
            .app_reverse_domain("dev.bootty")
            .create()
        {
            Ok(guard) => self.keep_awake = Some(guard),
            Err(error) => self.record_render_error(error),
        }
    }

    /// Replace the frame's chrome-handle rects with the ones chrome just painted, so the next
    /// input pass suppresses selection over live handles only. A frame that paints no chrome (the
    /// settings surface) leaves the previous set in place.
    pub fn set_chrome_handles(&mut self, rects: Vec<egui::Rect>) {
        self.chrome_handle_rects = rects;
    }

    /// Add a handle painted after chrome, during the terminal pass (the pane dividers).
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

    fn publish_backend_transition(&mut self, bindings: AppKeyBindings) {
        self.config_runtime.publish_backend_keybindings(bindings);
        self.terminal_surface = None;
        self.last_pane_area = None;
    }

    fn sync_terminal_panes_or_record_error(&mut self) {
        if let Err(error) = self.sync_terminal_panes() {
            self.last_error = Some(error.to_string());
        }
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
        self.sync_terminal_panes_or_record_error();
    }
    pub fn record_pane_area(&mut self, area: Rect) {
        self.last_pane_area = Some(area);
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
            self.publish_backend_transition(app_key_bindings);
        }
        if let Err(error) =
            self.workspace
                .activate_target(target.scope, &target.session_id, None, &self.repaint)
        {
            self.last_error = Some(error.to_string());
            return false;
        }
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
    pub(crate) fn apply_exact_mux_action(
        &mut self,
        action: ExactMuxAction,
        target: ExactMuxTarget,
    ) -> bool {
        let Some(command) = self.plan_exact_mux_action(action, &target) else {
            return false;
        };
        match command {
            MuxCommand::ActivateWindow {
                session_id,
                window_id,
            } => {
                if let Err(error) = self.workspace.activate_target(
                    target.scope(),
                    &session_id,
                    Some(&window_id),
                    &self.repaint,
                ) {
                    self.last_error = Some(error.to_string());
                    return false;
                }
                self.sync_native_layout_terminal_now();
            }
            MuxCommand::ClosePane {
                session_id,
                pane_id: Some(pane_id),
            } => {
                let (ExactMuxTarget::Window(_, _, window_id)
                | ExactMuxTarget::Pane(_, _, window_id, _)) = target
                else {
                    return false;
                };
                let binding = &mut self.workspace.active.binding;
                let window = binding.window_id(session_id.clone(), window_id);
                let target_is_current = binding.current_window_id() == window;
                let config = binding.multiplexer.clone();
                binding
                    .mux
                    .close_pane(&session_id, Some(&pane_id), &self.repaint, &config);
                binding.terminal.discard_pane(&pane_id);
                if binding.uses_native_terminal_layout() {
                    binding.remove_pane_from_layout(&window, &pane_id, target_is_current);
                }
            }
            command => self.execute_mux_command(command),
        }
        true
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
    fn create_project_session_for_cwd(&mut self, cwd: String) {
        let command = self.workspace.project_session_command(&cwd);
        self.execute_mux_command(command);
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
        let result = self.workspace.move_active_session(session_id, delta);
        self.apply_workspace_change(result)
    }
    pub fn reorder_session_before(&mut self, source: &str, target: Option<&str>) -> bool {
        let result = self.workspace.reorder_active_session_before(source, target);
        self.apply_workspace_change(result)
    }
    fn apply_workspace_change(&mut self, result: Result<bool, WorkspacePersistenceError>) -> bool {
        match result {
            Ok(changed) => changed,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }
    pub fn detach_scoped_session_from_space(&mut self, target: &ScopedSessionTarget) -> bool {
        let result = self
            .workspace
            .detach_session_from_space(target.scope, &target.session_id);
        let changed = self.apply_workspace_change(result);
        if changed {
            (self.repaint)();
        }
        changed
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
        self.apply_terminal_outcome(outcome.last_error, outcome.focus_intent);
    }
    fn apply_terminal_outcome(
        &mut self,
        last_error: Option<String>,
        focus_intent: TerminalFocusIntent,
    ) {
        if let Some(error) = last_error {
            self.last_error = Some(error);
        }
        self.apply_terminal_focus_intent(focus_intent);
    }
    fn apply_terminal_focus_intent(&mut self, intent: TerminalFocusIntent) {
        match intent {
            TerminalFocusIntent::None => {}
            TerminalFocusIntent::Terminal => self.input_focus = InputFocus::Terminal,
            TerminalFocusIntent::Find => self.input_focus = InputFocus::Picker,
        }
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
        // Sampled last: this frame's keybinds may have toggled fullscreen and its config reload may
        // have changed the tabs-in-notch gate, and the chrome paint that follows must see both.
        self.sample_window_chrome(viewport);
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
        self.apply_accepted_config(change, effects);
        true
    }

    fn apply_accepted_config(
        &mut self,
        change: crate::config_runtime::AcceptedConfigChange,
        effects: &mut Vec<AppEffect>,
    ) {
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
        warnings.extend(self.workspace.publish_terminal_config(
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
}
