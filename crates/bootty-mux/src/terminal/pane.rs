use std::{
    collections::HashMap,
    env,
    ffi::OsStr,
    hash::{Hash, Hasher},
    path::Path,
    process::Command,
    sync::{Arc, mpsc},
    thread,
};

use anyhow::Result;
use bootty_surface::geometry::{CellMetrics, TerminalGeometry};
use bootty_terminal::terminal_frame::RenderFrame;
use derive_more::{Deref, DerefMut};

use bootty_config::config::{MultiplexerBackendConfig, MultiplexerConfig};
use bootty_runtime::{
    DrainStats, TerminalSession, TerminalSessionConfig, render_source::TerminalRenderSource,
};
use bootty_terminal::{
    terminal_engine::{
        TerminalColorConfig, TerminalCopyModeAction, TerminalCopyModeOutcome, TerminalCursorConfig,
        TerminalFeatureConfig, TerminalSearchDirection, TerminalSelectionEvent,
        TerminalSelectionFormat,
    },
    terminal_input_model::{KeyInput, MouseInput},
};

use crate::{config::selected_backend, controller::MuxScope, snapshot::MuxPaneAnchor};

use super::{rmux_native::RmuxNativeTerminal, startup::StartingNativeTerminal};

pub(super) const TMUX_CLIENT_FEATURES: &str =
    "256,RGB,clipboard,focus,hyperlinks,overline,strikethrough,sync,title";

struct RmuxWindowResizeRequest {
    window_id: String,
    cols: u16,
    rows: u16,
}

struct RmuxWindowResizeWorker {
    tx: mpsc::Sender<RmuxWindowResizeRequest>,
    result_rx: mpsc::Receiver<std::result::Result<(), String>>,
}

#[derive(Deref, DerefMut)]
pub struct BackendPaneTerminal {
    backend: MultiplexerBackendConfig,
    active_target: Option<ScopedMuxPaneTarget>,
    geometry: TerminalGeometry,
    terminal_config: TerminalSessionConfig,
    repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    native_terminals: HashMap<ScopedMuxPaneTarget, Box<dyn TerminalRuntime>>,
    /// The active native window's panes (focused + the parked siblings rendered alongside it). Empty
    /// for non-native backends, which render a single attach surface.
    native_window_targets: Vec<ScopedMuxPaneTarget>,
    native_window_spawn_geometry: Option<TerminalGeometry>,
    native_window_id: Option<String>,
    native_window_scope: Option<MuxScope>,
    last_rmux_window_size: Option<(String, u16, u16)>,
    rmux_window_resize_worker: Option<RmuxWindowResizeWorker>,
    /// Tmux sessions whose status bars are hidden while Bootty keeps an attached runtime.
    status_hidden_sessions: Vec<String>,
    /// Pane-local passthrough values to restore when Bootty releases its attached runtimes.
    passthrough_all_panes: HashMap<String, TmuxPanePassthroughOverride>,
    #[deref]
    #[deref_mut]
    terminal: Box<dyn TerminalRuntime>,
}
fn idle_terminal() -> Box<dyn TerminalRuntime> {
    Box::new(IdleRenderSource)
}

pub trait TerminalRuntime: TerminalRenderSource {
    fn drain_pty(&mut self) -> DrainStats;
    fn pending_pty_len(&self) -> usize;
    fn child_exited(&mut self) -> Result<bool>;
    fn tty_name(&self) -> Option<&str> {
        None
    }
    fn discard_pending_output(&mut self) -> Result<()> {
        Ok(())
    }
    fn force_resize(&mut self) -> Result<()> {
        Ok(())
    }
    fn format_selection(&mut self, _format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn current_working_directory(&mut self) -> Result<Option<String>> {
        Ok(None)
    }
    fn set_cursor_config(&mut self, _cursor: TerminalCursorConfig) -> Result<()> {
        Ok(())
    }
    fn set_feature_config(&mut self, _features: TerminalFeatureConfig) -> Result<()> {
        Ok(())
    }
    fn set_colors(&mut self, colors: TerminalColorConfig) -> Result<()>;
    fn write_input(&mut self, bytes: &[u8]) -> Result<()>;
    fn write_paste(&mut self, text: &str) -> Result<()>;
    fn encode_key(&mut self, input: KeyInput) -> Result<()>;
    fn encode_focus(&mut self, gained: bool) -> Result<()>;
    fn encode_mouse(&mut self, input: MouseInput) -> Result<()>;
    fn handle_mouse_wheel(&mut self, input: MouseInput, scroll_delta: isize) -> Result<()>;
}
struct IdleRenderSource;

impl TerminalRenderSource for IdleRenderSource {
    fn resize(&mut self, _geometry: TerminalGeometry) -> Result<()> {
        Ok(())
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        Ok(Arc::new(RenderFrame::default()))
    }
}

impl TerminalRuntime for IdleRenderSource {
    fn drain_pty(&mut self) -> DrainStats {
        DrainStats::default()
    }

    fn pending_pty_len(&self) -> usize {
        0
    }

    fn child_exited(&mut self) -> Result<bool> {
        Ok(false)
    }

    fn set_colors(&mut self, _colors: TerminalColorConfig) -> Result<()> {
        Ok(())
    }

    fn write_input(&mut self, _bytes: &[u8]) -> Result<()> {
        Ok(())
    }

    fn write_paste(&mut self, _text: &str) -> Result<()> {
        Ok(())
    }
    fn encode_key(&mut self, _input: KeyInput) -> Result<()> {
        Ok(())
    }

    fn encode_focus(&mut self, _gained: bool) -> Result<()> {
        Ok(())
    }

    fn encode_mouse(&mut self, _input: MouseInput) -> Result<()> {
        Ok(())
    }

    fn handle_mouse_wheel(&mut self, _input: MouseInput, _scroll_delta: isize) -> Result<()> {
        Ok(())
    }
}

struct TmuxPanePassthroughOverride {
    pane_id: String,
    previous: TmuxOptionValue,
}

struct TmuxOptionValue {
    value: String,
    local: bool,
}

impl TerminalRuntime for TerminalSession {
    fn drain_pty(&mut self) -> DrainStats {
        Self::drain_pty(self)
    }

    fn pending_pty_len(&self) -> usize {
        Self::pending_pty_len(self)
    }

    fn child_exited(&mut self) -> Result<bool> {
        Self::child_exited(self)
    }

    fn tty_name(&self) -> Option<&str> {
        Self::tty_name(self)
    }

    fn discard_pending_output(&mut self) -> Result<()> {
        Self::discard_pending_output(self)
    }

    fn format_selection(&mut self, format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
        Self::format_selection(self, format)
    }

    fn current_working_directory(&mut self) -> Result<Option<String>> {
        Ok(Self::current_working_directory(self))
    }

    fn set_cursor_config(&mut self, cursor: TerminalCursorConfig) -> Result<()> {
        Self::set_cursor_config(self, cursor)
    }

    fn set_feature_config(&mut self, features: TerminalFeatureConfig) -> Result<()> {
        Self::set_feature_config(self, features)
    }

    fn set_colors(&mut self, colors: TerminalColorConfig) -> Result<()> {
        Self::set_colors(self, colors)
    }

    fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        Self::write_input(self, bytes)
    }

    fn write_paste(&mut self, text: &str) -> Result<()> {
        Self::write_paste(self, text)
    }

    fn encode_key(&mut self, input: KeyInput) -> Result<()> {
        Self::encode_key(self, input)
    }

    fn encode_focus(&mut self, gained: bool) -> Result<()> {
        Self::encode_focus(self, gained)
    }

    fn encode_mouse(&mut self, input: MouseInput) -> Result<()> {
        Self::encode_mouse(self, input)
    }

    fn handle_mouse_wheel(&mut self, input: MouseInput, scroll_delta: isize) -> Result<()> {
        Self::handle_mouse_wheel(self, input, scroll_delta)
    }
}

impl BackendPaneTerminal {
    pub fn new(
        geometry: TerminalGeometry,
        config: &MultiplexerConfig,
        terminal_config: TerminalSessionConfig,
        repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    ) -> Self {
        Self::new_with_backend(
            geometry,
            selected_backend(config),
            terminal_config,
            repaint_wakeup,
        )
    }

    pub(super) fn new_with_backend(
        geometry: TerminalGeometry,
        backend: MultiplexerBackendConfig,
        terminal_config: TerminalSessionConfig,
        repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    ) -> Self {
        Self {
            backend,
            active_target: None,
            geometry,
            terminal_config,
            repaint_wakeup,
            native_terminals: HashMap::new(),
            native_window_targets: Vec::new(),
            native_window_spawn_geometry: None,
            native_window_id: None,
            native_window_scope: None,
            last_rmux_window_size: None,
            rmux_window_resize_worker: None,
            status_hidden_sessions: Vec::new(),
            passthrough_all_panes: HashMap::new(),
            terminal: idle_terminal(),
        }
    }

    pub fn sync_mux_anchor(
        &mut self,
        config: &MultiplexerConfig,
        anchor: Option<&MuxPaneAnchor>,
    ) -> Result<()> {
        self.sync_mux_anchor_in_scope(None, config, anchor)
    }

    pub fn sync_scoped_mux_anchor(
        &mut self,
        scope: MuxScope,
        config: &MultiplexerConfig,
        anchor: Option<&MuxPaneAnchor>,
    ) -> Result<()> {
        self.sync_mux_anchor_in_scope(Some(scope), config, anchor)
    }

    fn sync_mux_anchor_in_scope(
        &mut self,
        scope: Option<MuxScope>,
        config: &MultiplexerConfig,
        anchor: Option<&MuxPaneAnchor>,
    ) -> Result<()> {
        let backend = selected_backend(config);
        let target = anchor
            .cloned()
            .map(|anchor| ScopedMuxPaneTarget::from_anchor(scope, anchor));
        if self.backend == backend
            && scoped_target_matches_anchor(backend, scope, self.active_target.as_ref(), anchor)
        {
            // The tmux attach client follows pane/window changes server-side, so avoid
            // restarting it. Still update Bootty's tracked target so pane-local option
            // overrides follow the pane currently being rendered.
            self.active_target = target;
            self.sync_tmux_passthrough_override();
            self.sync_status_bar(config.hide_tmux_status);
            return Ok(());
        }

        self.park_native_layout_terminal();
        let terminal = self
            .start_terminal(backend, target.as_ref())
            .inspect_err(|_| {
                self.backend = backend;
                self.active_target = None;
                self.sync_tmux_passthrough_override();
                self.clear_terminal();
            })?;

        self.backend = backend;
        self.active_target = target;
        self.terminal = terminal;
        self.sync_tmux_passthrough_override();
        self.sync_status_bar(config.hide_tmux_status);
        Ok(())
    }

    fn sync_tmux_passthrough_override(&mut self) {
        let Some(pane_id) = passthrough_override_target(
            self.backend,
            self.active_target.as_ref().map(|target| &target.target),
        ) else {
            self.restore_tmux_passthrough_overrides();
            return;
        };
        if self.passthrough_all_panes.contains_key(pane_id) {
            return;
        }
        if let Ok(previous) = pane_allow_passthrough(pane_id)
            && set_pane_allow_passthrough(pane_id, "all").is_ok()
        {
            self.passthrough_all_panes.insert(
                pane_id.to_owned(),
                TmuxPanePassthroughOverride {
                    pane_id: pane_id.to_owned(),
                    previous,
                },
            );
        }
    }

    fn restore_tmux_passthrough_overrides(&mut self) {
        for (_, previous) in self.passthrough_all_panes.drain() {
            let _ = restore_pane_allow_passthrough(&previous);
        }
    }

    /// Keep status hidden for every tmux session with a live Bootty runtime. Restoring the previous
    /// session during a switch resizes its client and destroys the cached frame we are swapping to.
    fn sync_status_bar(&mut self, hide_enabled: bool) {
        let Some(session) = status_bar_hidden_target(
            hide_enabled,
            self.backend,
            self.active_target
                .as_ref()
                .map(ScopedMuxPaneTarget::session_id),
        ) else {
            self.restore_tmux_status_bars();
            return;
        };
        if self
            .status_hidden_sessions
            .iter()
            .any(|hidden| hidden == session)
        {
            return;
        }
        if set_session_status_hidden(session, true).is_ok() {
            self.status_hidden_sessions.push(session.to_owned());
        }
    }

    fn restore_tmux_status_bars(&mut self) {
        for session in self.status_hidden_sessions.drain(..) {
            let _ = set_session_status_hidden(&session, false);
        }
    }

    pub fn deactivate_backend_side_effects(&mut self) {
        self.restore_tmux_passthrough_overrides();
        self.restore_tmux_status_bars();
    }

    pub fn set_terminal_config(&mut self, terminal_config: TerminalSessionConfig) {
        self.terminal_config = terminal_config;
    }

    pub fn set_colors(&mut self, colors: TerminalColorConfig) -> Result<()> {
        self.terminal.set_colors(colors.clone())?;
        for terminal in self.native_terminals.values_mut() {
            terminal.set_colors(colors.clone())?;
        }
        self.terminal_config.colors = colors;
        Ok(())
    }

    pub fn current_working_directory(&mut self) -> Result<Option<String>> {
        self.terminal.current_working_directory()
    }

    fn start_terminal(
        &mut self,
        backend: MultiplexerBackendConfig,
        target: Option<&ScopedMuxPaneTarget>,
    ) -> Result<Box<dyn TerminalRuntime>> {
        let Some(target) = target else {
            return Ok(idle_terminal());
        };

        match backend {
            MultiplexerBackendConfig::Native | MultiplexerBackendConfig::Rmux => {
                // A native session whose tabs have all been closed resolves to a session-level target
                // with no pane; it has no shell to attach, so it renders as idle. Rmux session targets
                // can resolve to the active backend pane.
                if backend == MultiplexerBackendConfig::Native
                    && !matches!(&target.target, MuxPaneTarget::Pane { .. })
                {
                    return Ok(idle_terminal());
                }
                if let Some(terminal) = self.native_terminals.remove(target) {
                    return Ok(terminal);
                }
                if backend == MultiplexerBackendConfig::Native {
                    return self.spawn_native_runtime(target);
                }
                let mut config = self.terminal_config.clone();
                config.side_effect_pane_id = target.side_effect_pane_id();
                Ok(Box::new(RmuxNativeTerminal::new(
                    target.target.clone(),
                    self.native_window_spawn_geometry.unwrap_or(self.geometry),
                    config,
                    Arc::clone(&self.repaint_wakeup),
                )?))
            }
            MultiplexerBackendConfig::Tmux | MultiplexerBackendConfig::Zellij => {
                if backend == MultiplexerBackendConfig::Tmux
                    && let Some(terminal) = self.native_terminals.remove(target)
                {
                    return Ok(terminal);
                }
                let mut terminal_config = self.terminal_config.clone();
                terminal_config.side_effect_pane_id = target.side_effect_pane_id();
                let config = backend_attach_session_config(
                    terminal_config,
                    backend,
                    target.session_id(),
                    bootty_runtime::terminfo::vendored_terminfo_dir().is_some(),
                )?;
                Ok(Box::new(TerminalSession::new_with_config(
                    self.geometry,
                    config,
                    Arc::clone(&self.repaint_wakeup),
                )?))
            }
        }
    }

    fn spawn_native_runtime(
        &self,
        target: &ScopedMuxPaneTarget,
    ) -> Result<Box<dyn TerminalRuntime>> {
        let mut config = self.terminal_config.clone();
        config.launch.working_directory = target.cwd().map(Path::new).map(Path::to_path_buf);
        config.side_effect_pane_id = target.side_effect_pane_id();
        Ok(Box::new(StartingNativeTerminal::spawn(
            self.native_window_spawn_geometry.unwrap_or(self.geometry),
            config,
            Arc::clone(&self.repaint_wakeup),
        )))
    }

    /// Reconcile the live native-layout runtimes against the active window's panes: make `focused`
    /// the deref/input runtime and keep every other pane alive in the parked map so it renders and
    /// drains alongside. Panes are only torn down on explicit close, so switching focus or tabs
    /// never kills a shell.
    pub fn sync_native_window(
        &mut self,
        window_panes: &[MuxPaneAnchor],
        focused: Option<&MuxPaneAnchor>,
        window_id: Option<&str>,
        layout_backend: MultiplexerBackendConfig,
        hide_tmux_status: bool,
    ) -> Result<()> {
        self.sync_native_window_in_scope(
            None,
            window_panes,
            focused,
            window_id,
            layout_backend,
            hide_tmux_status,
        )
    }

    pub fn sync_scoped_native_window(
        &mut self,
        scope: MuxScope,
        window_panes: &[MuxPaneAnchor],
        focused: Option<&MuxPaneAnchor>,
        window_id: Option<&str>,
        layout_backend: MultiplexerBackendConfig,
        hide_tmux_status: bool,
    ) -> Result<()> {
        self.sync_native_window_in_scope(
            Some(scope),
            window_panes,
            focused,
            window_id,
            layout_backend,
            hide_tmux_status,
        )
    }

    fn sync_native_window_in_scope(
        &mut self,
        scope: Option<MuxScope>,
        window_panes: &[MuxPaneAnchor],
        focused: Option<&MuxPaneAnchor>,
        window_id: Option<&str>,
        layout_backend: MultiplexerBackendConfig,
        hide_tmux_status: bool,
    ) -> Result<()> {
        debug_assert!(matches!(
            layout_backend,
            MultiplexerBackendConfig::Native | MultiplexerBackendConfig::Rmux
        ));
        self.backend = layout_backend;
        let targets: Vec<ScopedMuxPaneTarget> = window_panes
            .iter()
            .cloned()
            .map(|anchor| ScopedMuxPaneTarget::from_anchor(scope, anchor))
            .filter(|target| matches!(&target.target, MuxPaneTarget::Pane { .. }))
            .collect();
        let focused_target = focused
            .cloned()
            .map(|anchor| ScopedMuxPaneTarget::from_anchor(scope, anchor))
            .filter(|target| matches!(&target.target, MuxPaneTarget::Pane { .. }))
            .or_else(|| targets.first().cloned());

        if self.active_target.as_ref() != focused_target.as_ref() {
            self.park_native_layout_terminal();
            let terminal = self
                .start_terminal(layout_backend, focused_target.as_ref())
                .inspect_err(|_| {
                    self.active_target = None;
                    self.clear_terminal();
                })?;
            self.active_target = focused_target;
            self.terminal = terminal;
        }

        for target in &targets {
            if self.active_target.as_ref() == Some(target) {
                continue;
            }
            if !self.native_terminals.contains_key(target) {
                let runtime = self.start_terminal(layout_backend, Some(target))?;
                self.native_terminals.insert(target.clone(), runtime);
            }
        }
        let window_id = window_id.map(str::to_owned);
        if self.native_window_scope != scope || self.native_window_id != window_id {
            self.native_window_scope = scope;
            self.native_window_id = window_id;
            self.last_rmux_window_size = None;
        }
        self.native_window_targets = targets;
        self.sync_status_bar(hide_tmux_status);
        Ok(())
    }

    pub fn resize_native_layout_window(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.native_window_spawn_geometry = Some(TerminalGeometry {
            cols,
            rows,
            cell_width: self.geometry.cell_width,
            cell_height: self.geometry.cell_height,
        });
        self.drain_rmux_window_resize_results()?;
        if self.backend != MultiplexerBackendConfig::Rmux {
            return Ok(());
        }
        let Some(window_id) = self.native_window_id.clone() else {
            return Ok(());
        };
        let requested = (window_id.clone(), cols, rows);
        if self.last_rmux_window_size.as_ref() == Some(&requested) {
            return Ok(());
        }
        self.ensure_rmux_window_resize_worker();
        let Some(worker) = &self.rmux_window_resize_worker else {
            anyhow::bail!("rmux window resize worker did not start");
        };
        worker
            .tx
            .send(RmuxWindowResizeRequest {
                window_id,
                cols,
                rows,
            })
            .map_err(|_| anyhow::anyhow!("rmux window resize worker stopped"))?;
        self.last_rmux_window_size = Some(requested);
        Ok(())
    }

    fn ensure_rmux_window_resize_worker(&mut self) {
        if self.rmux_window_resize_worker.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel::<RmuxWindowResizeRequest>();
        let (result_tx, result_rx) = mpsc::channel::<std::result::Result<(), String>>();
        let repaint = Arc::clone(&self.repaint_wakeup);
        thread::spawn(move || {
            while let Ok(mut request) = rx.recv() {
                while let Ok(next) = rx.try_recv() {
                    request = next;
                }
                let result = crate::rmux::resize_bootty_rmux_window(
                    &request.window_id,
                    request.cols,
                    request.rows,
                )
                .map_err(|error| error.to_string());
                let _ = result_tx.send(result);
                repaint();
            }
        });
        self.rmux_window_resize_worker = Some(RmuxWindowResizeWorker { tx, result_rx });
    }

    fn drain_rmux_window_resize_results(&mut self) -> Result<()> {
        let mut completed = false;
        let mut error = None;
        if let Some(worker) = &self.rmux_window_resize_worker {
            while let Ok(result) = worker.result_rx.try_recv() {
                match result {
                    Ok(()) => completed = true,
                    Err(result_error) => error = Some(result_error),
                }
            }
        }
        if let Some(error) = error {
            self.last_rmux_window_size = None;
            anyhow::bail!(error);
        }
        if completed {
            self.force_native_layout_pane_resizes()?;
        }
        Ok(())
    }

    fn force_native_layout_pane_resizes(&mut self) -> Result<()> {
        self.terminal.force_resize()?;
        let targets = self.native_window_targets.clone();
        for target in targets {
            if self.active_target.as_ref() == Some(&target) {
                continue;
            }
            if let Some(runtime) = self.native_terminals.get_mut(&target) {
                runtime.force_resize()?;
            }
        }
        Ok(())
    }

    /// A non-focused window pane's render source, for painting it into its own sub-rect. The focused
    /// pane is rendered through `BackendPaneTerminal` itself (which keeps `geometry` in sync).
    pub fn render_source_for_pane(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut (dyn TerminalRuntime + '_)> {
        if self
            .active_target
            .as_ref()
            .map(ScopedMuxPaneTarget::input_selector)
            == Some(pane_id)
        {
            return None;
        }
        let target = self
            .native_window_targets
            .iter()
            .find(|target| target.input_selector() == pane_id)?
            .clone();
        let terminal = self.native_terminals.get_mut(&target)?;
        Some(&mut **terminal)
    }

    /// The requested pane's runtime, including the focused/input pane.
    pub fn focused_render_source(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut (dyn TerminalRuntime + '_)> {
        if self
            .active_target
            .as_ref()
            .map(ScopedMuxPaneTarget::input_selector)
            == Some(pane_id)
        {
            return Some(&mut *self.terminal);
        }
        self.render_source_for_pane(pane_id)
    }

    /// The focused pane's id (the deref/input runtime), if any.
    pub fn focused_pane_id(&self) -> Option<&str> {
        self.active_target
            .as_ref()
            .map(ScopedMuxPaneTarget::input_selector)
    }

    pub fn active_mux_scope(&self) -> Option<MuxScope> {
        self.active_target.as_ref().and_then(|target| target.scope)
    }

    /// Pane ids in the active window whose shell has exited (focused or background), so the layout
    /// can close them. Checked across every live pane, not just the focused one.
    pub fn native_exited_panes(&mut self) -> Vec<String> {
        let mut exited = Vec::new();
        if matches!(self.terminal.child_exited(), Ok(true))
            && let Some(id) = self.focused_pane_id()
        {
            exited.push(id.to_owned());
        }
        let targets = self.native_window_targets.clone();
        for target in &targets {
            if self.active_target.as_ref() == Some(target) {
                continue;
            }
            if let Some(runtime) = self.native_terminals.get_mut(target)
                && matches!(runtime.child_exited(), Ok(true))
            {
                exited.push(target.input_selector().to_owned());
            }
        }
        exited
    }

    /// Drop a pane's runtime (killing its PTY) whether it is the focused runtime or a parked sibling.
    pub fn discard_pane(&mut self, pane_id: &str) {
        if self.focused_pane_id() == Some(pane_id) {
            self.discard_active_pane();
            return;
        }
        if let Some(target) = self
            .native_window_targets
            .iter()
            .find(|target| target.input_selector() == pane_id)
            .cloned()
        {
            self.native_terminals.remove(&target);
        }
    }

    /// Drain the focused terminal and every cached runtime, including inactive scoped workspaces,
    /// so background PTYs cannot stall while another Space is selected.
    pub fn drain_native_window(&mut self) -> DrainStats {
        let stats = self.terminal.drain_pty();
        for runtime in self.native_terminals.values_mut() {
            runtime.drain_pty();
        }
        stats
    }

    pub fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        self.terminal.scroll_viewport_delta(delta)
    }

    pub fn enter_copy_mode(&mut self) -> Result<()> {
        self.terminal.enter_copy_mode()
    }

    pub fn copy_mode_active(&mut self) -> Result<bool> {
        self.terminal.copy_mode_active()
    }

    pub fn handle_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome> {
        self.terminal.handle_copy_mode_action(action)
    }

    pub fn set_cursor_config(&mut self, cursor: TerminalCursorConfig) -> Result<()> {
        self.terminal.set_cursor_config(cursor)?;
        for terminal in self.native_terminals.values_mut() {
            terminal.set_cursor_config(cursor)?;
        }
        Ok(())
    }

    pub fn set_feature_config(&mut self, features: TerminalFeatureConfig) -> Result<()> {
        self.terminal.set_feature_config(features)?;
        for terminal in self.native_terminals.values_mut() {
            terminal.set_feature_config(features)?;
        }
        Ok(())
    }

    pub fn grid_size(&self) -> (u16, u16) {
        (self.geometry.cols, self.geometry.rows)
    }

    pub fn child_exited(&mut self) -> Result<bool> {
        self.terminal.child_exited()
    }

    // Drop the active pane's terminal (its PTY is killed on drop) and forget its target, so the next
    // sync_mux_anchor attaches the surviving pane instead of parking the closed one.
    pub fn discard_active_pane(&mut self) {
        self.terminal = idle_terminal();
        self.active_target = None;
    }

    fn clear_terminal(&mut self) {
        self.terminal = idle_terminal();
    }

    fn park_native_layout_terminal(&mut self) {
        if !matches!(
            self.backend,
            MultiplexerBackendConfig::Native
                | MultiplexerBackendConfig::Rmux
                | MultiplexerBackendConfig::Tmux
        ) {
            return;
        }
        let Some(target) = self.active_target.clone() else {
            return;
        };
        let terminal = std::mem::replace(&mut self.terminal, idle_terminal());
        self.native_terminals.insert(target, terminal);
    }
}

impl Drop for BackendPaneTerminal {
    fn drop(&mut self) {
        // Best-effort cleanup: a hard kill skips this, and a later attach reapplies overrides.
        self.deactivate_backend_side_effects();
    }
}

impl TerminalRenderSource for BackendPaneTerminal {
    fn set_display_scale(&mut self, display_scale: f32) -> Result<()> {
        self.terminal.set_display_scale(display_scale)
    }

    fn set_render_cell_metrics(&mut self, cell: CellMetrics) -> Result<()> {
        self.terminal.set_render_cell_metrics(cell)
    }

    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        if self.geometry == geometry {
            return Ok(());
        }
        self.geometry = geometry;
        if self.backend == MultiplexerBackendConfig::Tmux {
            // Parked sessions keep their `attach-session` client alive, and that client's size is
            // the session's size. Skipping them leaves the window at the old size when we switch
            // back, and clamps live windows under `window-size smallest`.
            for terminal in self.native_terminals.values_mut() {
                terminal.resize(geometry)?;
            }
        }
        self.terminal.resize(geometry)
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        self.terminal.extract_frame()
    }

    fn is_mouse_tracking(&mut self) -> Result<bool> {
        self.terminal.is_mouse_tracking()
    }

    fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        self.terminal.scroll_viewport_delta(delta)
    }

    fn enter_copy_mode(&mut self) -> Result<()> {
        self.terminal.enter_copy_mode()
    }

    fn copy_mode_active(&mut self) -> Result<bool> {
        self.terminal.copy_mode_active()
    }

    fn handle_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome> {
        self.terminal.handle_copy_mode_action(action)
    }

    fn search_viewport(&mut self, query: &str, direction: TerminalSearchDirection) -> Result<bool> {
        self.terminal.search_viewport(query, direction)
    }

    fn begin_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        self.terminal.begin_selection(event)
    }

    fn update_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        self.terminal.update_selection(event)
    }

    fn end_selection(&mut self, event: Option<TerminalSelectionEvent>) -> Result<()> {
        self.terminal.end_selection(event)
    }
}

#[derive(Clone, Debug, Eq)]
pub(super) enum MuxPaneTarget {
    Session {
        session_id: String,
        cwd: Option<String>,
    },
    Pane {
        session_id: String,
        pane_id: String,
        cwd: Option<String>,
    },
}

impl PartialEq for MuxPaneTarget {
    fn eq(&self, other: &Self) -> bool {
        self.session_id() == other.session_id() && self.input_selector() == other.input_selector()
    }
}

impl Hash for MuxPaneTarget {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.session_id().hash(state);
        self.input_selector().hash(state);
    }
}

impl MuxPaneTarget {
    pub(super) fn session_id(&self) -> &str {
        match self {
            Self::Session { session_id, .. } | Self::Pane { session_id, .. } => session_id,
        }
    }

    pub(super) fn input_selector(&self) -> &str {
        match self {
            Self::Pane { pane_id, .. } => pane_id,
            target => target.session_id(),
        }
    }

    fn pane_id(&self) -> Option<&str> {
        match self {
            Self::Pane { pane_id, .. } => Some(pane_id),
            Self::Session { .. } => None,
        }
    }

    fn cwd(&self) -> Option<&str> {
        match self {
            Self::Session { cwd, .. } | Self::Pane { cwd, .. } => cwd.as_deref(),
        }
    }
}

impl From<MuxPaneAnchor> for MuxPaneTarget {
    fn from(anchor: MuxPaneAnchor) -> Self {
        match anchor.pane_id {
            Some(pane_id) => Self::Pane {
                session_id: anchor.session_id,
                pane_id,
                cwd: anchor.cwd,
            },
            None => Self::Session {
                session_id: anchor.session_id,
                cwd: anchor.cwd,
            },
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ScopedMuxPaneTarget {
    scope: Option<MuxScope>,
    target: MuxPaneTarget,
}

impl ScopedMuxPaneTarget {
    fn from_anchor(scope: Option<MuxScope>, anchor: MuxPaneAnchor) -> Self {
        Self {
            scope,
            target: MuxPaneTarget::from(anchor),
        }
    }

    fn session_id(&self) -> &str {
        self.target.session_id()
    }

    fn input_selector(&self) -> &str {
        self.target.input_selector()
    }

    fn pane_id(&self) -> Option<&str> {
        self.target.pane_id()
    }

    fn cwd(&self) -> Option<&str> {
        self.target.cwd()
    }

    fn side_effect_pane_id(&self) -> Option<String> {
        let pane_id = self.pane_id()?;
        Some(match self.scope {
            Some(scope) => encode_scoped_pane_id(scope, pane_id),
            None => pane_id.to_owned(),
        })
    }
}

impl From<MuxPaneTarget> for ScopedMuxPaneTarget {
    fn from(target: MuxPaneTarget) -> Self {
        Self {
            scope: None,
            target,
        }
    }
}

const SCOPED_PANE_PREFIX: &str = "bootty-scope:";

pub fn encode_scoped_pane_id(scope: MuxScope, pane_id: &str) -> String {
    format!(
        "{SCOPED_PANE_PREFIX}{}:{}:{pane_id}",
        scope.space_id().persistence_value(),
        scope.binding_id().persistence_value()
    )
}

pub fn decode_scoped_pane_id(value: &str) -> Option<(MuxScope, String)> {
    let mut parts = value.strip_prefix(SCOPED_PANE_PREFIX)?.splitn(3, ':');
    let space_id = parts.next()?.parse().ok()?;
    let binding_id = parts.next()?.parse().ok()?;
    let pane_id = parts.next()?.to_owned();
    Some((
        MuxScope::new(
            crate::controller::SpaceId::from_persistence(space_id),
            crate::controller::BindingId::from_persistence(binding_id),
        ),
        pane_id,
    ))
}

fn scoped_target_matches_anchor(
    backend: MultiplexerBackendConfig,
    scope: Option<MuxScope>,
    target: Option<&ScopedMuxPaneTarget>,
    anchor: Option<&MuxPaneAnchor>,
) -> bool {
    match target {
        Some(target) if target.scope != scope => false,
        Some(target) => target_matches_anchor(backend, Some(&target.target), anchor),
        None => target_matches_anchor(backend, None, anchor),
    }
}

fn target_matches_anchor(
    backend: MultiplexerBackendConfig,
    target: Option<&MuxPaneTarget>,
    anchor: Option<&MuxPaneAnchor>,
) -> bool {
    match (target, anchor) {
        (None, None) => true,
        (Some(target), Some(anchor)) => {
            if target.session_id() != anchor.session_id {
                return false;
            }
            // Attached clients (tmux/zellij attach PTYs) follow pane and
            // window changes server-side; restarting them on an active-pane
            // change blanks the whole surface for nothing.
            if matches!(
                backend,
                MultiplexerBackendConfig::Tmux | MultiplexerBackendConfig::Zellij
            ) {
                return true;
            }
            let anchor_selector = anchor.pane_id.as_deref().unwrap_or(&anchor.session_id);
            target.input_selector() == anchor_selector
        }
        _ => false,
    }
}

pub(super) fn backend_attach_launch(
    backend: MultiplexerBackendConfig,
    session: &str,
) -> (String, Vec<String>) {
    let session = session.to_owned();
    match backend {
        // -T declares outer-terminal features tmux cannot learn from the
        // forced xterm-256color terminfo; "clipboard" enables OSC 52 and
        // "sync" wraps redraws in DEC 2026 to avoid blank layout flashes.
        MultiplexerBackendConfig::Tmux => (
            "tmux".to_owned(),
            vec![
                "-T".to_owned(),
                TMUX_CLIENT_FEATURES.to_owned(),
                "attach-session".to_owned(),
                "-t".to_owned(),
                session,
            ],
        ),
        MultiplexerBackendConfig::Rmux => unreachable!("rmux is rendered natively via rmux-sdk"),
        MultiplexerBackendConfig::Native => {
            unreachable!("native panes are rendered directly by Bootty")
        }
        MultiplexerBackendConfig::Zellij => (
            "zellij".to_owned(),
            vec!["attach".to_owned(), "--create".to_owned(), session],
        ),
    }
}

fn backend_attach_env_remove(backend: MultiplexerBackendConfig) -> Vec<String> {
    match backend {
        MultiplexerBackendConfig::Tmux => vec!["TMUX".to_owned()],
        MultiplexerBackendConfig::Rmux => unreachable!("rmux is rendered natively via rmux-sdk"),
        MultiplexerBackendConfig::Native => {
            unreachable!("native panes are rendered directly by Bootty")
        }
        MultiplexerBackendConfig::Zellij => vec!["ZELLIJ".to_owned()],
    }
}

fn backend_attach_session_config(
    config: TerminalSessionConfig,
    backend: MultiplexerBackendConfig,
    attach_session: &str,
    bootty_terminfo_available: bool,
) -> Result<TerminalSessionConfig> {
    backend_attach_session_config_with_path(
        config,
        backend,
        attach_session,
        bootty_terminfo_available,
        env::var_os("PATH").as_deref(),
    )
}

fn backend_attach_session_config_with_path(
    mut config: TerminalSessionConfig,
    backend: MultiplexerBackendConfig,
    attach_session: &str,
    bootty_terminfo_available: bool,
    path: Option<&OsStr>,
) -> Result<TerminalSessionConfig> {
    let (program, args) = backend_attach_launch(backend, attach_session);
    config.launch.shell = Some(resolve_launch_program_with_path(&program, path)?);
    config.launch.args = args;
    config.launch.env_remove = backend_attach_env_remove(backend);
    // The attach client hard-fails on a TERM it cannot resolve. xterm-bootty
    // only resolves through Bootty's vendored terminfo; anything else falls
    // back to the universally installed xterm-256color, with required
    // features pinned via the -T attach flag either way.
    if config.launch.term != bootty_runtime::terminfo::XTERM_BOOTTY || !bootty_terminfo_available {
        config.launch.term = "xterm-256color".to_owned();
    }
    Ok(config)
}

fn resolve_launch_program(program: &str) -> Result<String> {
    resolve_launch_program_with_path(program, env::var_os("PATH").as_deref())
}

fn resolve_launch_program_with_path(program: &str, path: Option<&OsStr>) -> Result<String> {
    if Path::new(program).is_absolute() {
        return Ok(program.to_owned());
    }
    if let Some(found) = path
        .into_iter()
        .flat_map(env::split_paths)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
    {
        return Ok(found.to_string_lossy().into_owned());
    }
    anyhow::bail!("backend attach program {program:?} not found in PATH")
}

fn passthrough_override_target(
    backend: MultiplexerBackendConfig,
    target: Option<&MuxPaneTarget>,
) -> Option<&str> {
    if backend != MultiplexerBackendConfig::Tmux {
        return None;
    }
    target.map(|target| match target {
        MuxPaneTarget::Pane { pane_id, .. } => pane_id.as_str(),
        MuxPaneTarget::Session { session_id, .. } => session_id.as_str(),
    })
}

fn pane_allow_passthrough(pane_id: &str) -> Result<TmuxOptionValue> {
    if let Some(value) =
        tmux_option_value(&["show-options", "-p", "-t", pane_id, "allow-passthrough"])?
    {
        return Ok(TmuxOptionValue { value, local: true });
    }
    let value = tmux_option_value(&["show-options", "-g", "allow-passthrough"])?
        .ok_or_else(|| anyhow::anyhow!("tmux global allow-passthrough option had no value"))?;
    Ok(TmuxOptionValue {
        value,
        local: false,
    })
}

fn tmux_option_value(args: &[&str]) -> Result<Option<String>> {
    let program = resolve_launch_program("tmux")?;
    let output = Command::new(program)
        .args(args)
        .env_remove("TMUX")
        .env_remove("ZELLIJ")
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "tmux option command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut fields = stdout.split_whitespace();
    if fields.next().is_none() {
        return Ok(None);
    }
    Ok(fields.next().map(str::to_owned))
}

fn set_pane_allow_passthrough(pane_id: &str, value: &str) -> Result<()> {
    let program = resolve_launch_program("tmux")?;
    let output = Command::new(program)
        .args([
            "set-option",
            "-p",
            "-t",
            pane_id,
            "allow-passthrough",
            value,
        ])
        .env_remove("TMUX")
        .env_remove("ZELLIJ")
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "tmux set-option allow-passthrough failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn restore_pane_allow_passthrough(previous: &TmuxPanePassthroughOverride) -> Result<()> {
    if previous.previous.local {
        return set_pane_allow_passthrough(&previous.pane_id, &previous.previous.value);
    }
    unset_pane_allow_passthrough(&previous.pane_id)
}

fn unset_pane_allow_passthrough(pane_id: &str) -> Result<()> {
    let program = resolve_launch_program("tmux")?;
    let output = Command::new(program)
        .args(["set-option", "-u", "-p", "-t", pane_id, "allow-passthrough"])
        .env_remove("TMUX")
        .env_remove("ZELLIJ")
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "tmux unset-option allow-passthrough failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// The session whose tmux status bar should be hidden: only with the feature on,
/// the tmux backend, and a session attached. Native/rmux/zellij are never
/// touched, so this can only ever issue a `set-option` against a tmux server.
fn status_bar_hidden_target(
    hide_enabled: bool,
    backend: MultiplexerBackendConfig,
    session_id: Option<&str>,
) -> Option<&str> {
    if hide_enabled && backend == MultiplexerBackendConfig::Tmux {
        session_id
    } else {
        None
    }
}

/// Toggle a single tmux session's `status` option on the default-socket server
/// bootty attached. Hiding sets it off for that session alone; restoring unsets
/// the session override so it falls back to the global default. Never sets a
/// global option, so it cannot affect any other session.
fn set_session_status_hidden(session_id: &str, hidden: bool) -> Result<()> {
    let program = resolve_launch_program("tmux")?;
    let mut command = Command::new(program);
    if hidden {
        command.args(["set-option", "-t", session_id, "status", "off"]);
    } else {
        command.args(["set-option", "-u", "-t", session_id, "status"]);
    }
    command.env_remove("TMUX").env_remove("ZELLIJ");
    let output = command.output()?;
    if !output.status.success() {
        anyhow::bail!(
            "tmux set-option status failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    use std::sync::Mutex;

    use bootty_terminal::terminal_engine::TerminalColorConfig;
    use bootty_terminal::terminal_frame::RenderFrame;
    use tempfile::TempDir;

    #[test]
    fn status_bar_hidden_only_targets_tmux_when_enabled() {
        // Enabled, tmux backend, attached session: that session is the target.
        assert_eq!(
            status_bar_hidden_target(true, MultiplexerBackendConfig::Tmux, Some("$1")),
            Some("$1")
        );
        // Disabled: never hide, even on tmux.
        assert_eq!(
            status_bar_hidden_target(false, MultiplexerBackendConfig::Tmux, Some("$1")),
            None
        );
        // Safety contract: a non-tmux backend is never touched, so bootty can
        // never run `set-option` against native/rmux/zellij sessions.
        assert_eq!(
            status_bar_hidden_target(true, MultiplexerBackendConfig::Native, Some("$1")),
            None
        );
        // No attached session means nothing to toggle.
        assert_eq!(
            status_bar_hidden_target(true, MultiplexerBackendConfig::Tmux, None),
            None
        );
    }

    #[test]
    fn deactivating_terminal_clears_tmux_status_override() {
        let mut terminal = BackendPaneTerminal::new_with_backend(
            TerminalGeometry {
                cols: 80,
                rows: 24,
                cell_width: 10,
                cell_height: 20,
            },
            MultiplexerBackendConfig::Tmux,
            terminal_config(),
            Arc::new(|| {}),
        );
        terminal
            .status_hidden_sessions
            .push("missing-test-session".to_owned());

        terminal.deactivate_backend_side_effects();

        assert!(terminal.status_hidden_sessions.is_empty());
    }

    #[test]
    fn native_layout_sync_preserves_rmux_backend() {
        let geometry = TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        };
        let mut terminal = BackendPaneTerminal::new_with_backend(
            geometry,
            MultiplexerBackendConfig::Native,
            terminal_config(),
            Arc::new(|| {}),
        );

        terminal
            .sync_native_window(&[], None, Some("@1"), MultiplexerBackendConfig::Rmux, false)
            .unwrap();

        assert_eq!(terminal.backend, MultiplexerBackendConfig::Rmux);
    }

    #[test]
    fn scoped_native_cache_distinguishes_colliding_session_and_pane_ids() {
        let geometry = TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        };
        let anchor = MuxPaneAnchor {
            session_id: "$1".to_owned(),
            pane_id: Some("%1".to_owned()),
            cwd: None,
            process: None,
        };
        let first_scope = MuxScope::new(
            crate::controller::SpaceId::from_persistence(1),
            crate::controller::BindingId::from_persistence(1),
        );
        let second_scope = MuxScope::new(
            crate::controller::SpaceId::from_persistence(2),
            crate::controller::BindingId::from_persistence(1),
        );
        let first_calls = Arc::new(Mutex::new(Vec::new()));
        let second_calls = Arc::new(Mutex::new(Vec::new()));
        let mut terminal = BackendPaneTerminal::new_with_backend(
            geometry,
            MultiplexerBackendConfig::Rmux,
            terminal_config(),
            Arc::new(|| {}),
        );
        terminal.native_terminals.insert(
            ScopedMuxPaneTarget::from_anchor(Some(first_scope), anchor.clone()),
            Box::new(ResizeRecordingRuntime {
                resize_calls: Arc::clone(&first_calls),
            }),
        );
        terminal.native_terminals.insert(
            ScopedMuxPaneTarget::from_anchor(Some(second_scope), anchor.clone()),
            Box::new(ResizeRecordingRuntime {
                resize_calls: Arc::clone(&second_calls),
            }),
        );

        terminal
            .sync_scoped_native_window(
                first_scope,
                std::slice::from_ref(&anchor),
                Some(&anchor),
                Some("@1"),
                MultiplexerBackendConfig::Rmux,
                false,
            )
            .unwrap();
        terminal.terminal.force_resize().unwrap();
        assert_eq!(terminal.active_mux_scope(), Some(first_scope));
        assert_eq!(first_calls.lock().unwrap().len(), 1);
        assert!(second_calls.lock().unwrap().is_empty());

        terminal
            .sync_scoped_native_window(
                second_scope,
                std::slice::from_ref(&anchor),
                Some(&anchor),
                Some("@1"),
                MultiplexerBackendConfig::Rmux,
                false,
            )
            .unwrap();
        terminal.terminal.force_resize().unwrap();
        assert_eq!(terminal.active_mux_scope(), Some(second_scope));
        assert_eq!(first_calls.lock().unwrap().len(), 1);
        assert_eq!(second_calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn starting_native_terminal_buffers_input_until_spawn_completes() -> Result<()> {
        let mut config = terminal_config();
        config.launch.shell = Some("/bin/cat".to_owned());
        let mut terminal = StartingNativeTerminal::spawn(
            TerminalGeometry {
                cols: 80,
                rows: 24,
                cell_width: 10,
                cell_height: 20,
            },
            config,
            Arc::new(|| {}),
        );

        terminal.write_input(b"bootty-queued-input\n")?;

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            terminal.drain_pty();
            let frame = terminal.extract_frame()?;
            let text = frame.text.iter().collect::<String>();
            if text.contains("bootty-queued-input") {
                break;
            }
            if std::time::Instant::now() >= deadline {
                anyhow::bail!("queued native input was not replayed; frame text: {text:?}");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires an rmux binary; set RMUX_TMPDIR to isolate the daemon"]
    fn rmux_live_window_resize_worker_is_non_blocking_and_reaches_server() -> Result<()> {
        std::env::var_os("RMUX_TMPDIR")
            .context("set RMUX_TMPDIR to an empty temporary directory before running this test")?;
        use crate::rmux::{RmuxSessionClient, SdkRmuxClient};

        let client = SdkRmuxClient::new();
        let session = format!("bootty-worker-{}", std::process::id());
        let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
        client.ensure_session(&session, &cwd)?;
        let snapshot = client.snapshot()?;
        let window_id = snapshot
            .sessions
            .iter()
            .find(|candidate| candidate.id == session)
            .and_then(|session| session.windows.first())
            .map(|window| window.id.clone())
            .context("worker resize window should exist")?;
        let mut terminal = BackendPaneTerminal::new_with_backend(
            TerminalGeometry {
                cols: 80,
                rows: 24,
                cell_width: 10,
                cell_height: 20,
            },
            MultiplexerBackendConfig::Rmux,
            terminal_config(),
            Arc::new(|| {}),
        );
        terminal.native_window_id = Some(window_id.clone());

        let start = std::time::Instant::now();
        terminal.resize_native_layout_window(117, 40)?;
        assert!(
            start.elapsed() < std::time::Duration::from_millis(50),
            "enqueueing rmux resize should not block the render path: {:?}",
            start.elapsed()
        );

        let expected = format!("{session} {window_id} 117x40");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let output = std::process::Command::new("rmux")
                .args([
                    "list-windows",
                    "-a",
                    "-F",
                    "#{session_name} #{window_id} #{window_width}x#{window_height}",
                ])
                .output()?;
            let last_output = String::from_utf8_lossy(&output.stdout).into_owned();
            if last_output.lines().any(|line| line == expected) {
                break;
            }
            if std::time::Instant::now() >= deadline {
                anyhow::bail!("expected {expected:?} in rmux windows:\n{last_output}");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        terminal.resize_native_layout_window(117, 40)?;
        client.kill_session(&session)?;
        let _ = std::process::Command::new("rmux")
            .arg("kill-server")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        Ok(())
    }

    #[test]
    #[ignore = "requires an rmux binary; set RMUX_TMPDIR to isolate the daemon"]
    fn rmux_live_native_window_attach_and_switch_stay_interactive() -> Result<()> {
        std::env::var_os("RMUX_TMPDIR")
            .context("set RMUX_TMPDIR to an empty temporary directory before running this test")?;
        use crate::rmux::{RmuxSessionClient, SdkRmuxClient};

        let client = SdkRmuxClient::new();
        let session = format!("bootty-attach-perf-{}", std::process::id());
        let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
        client.ensure_session(&session, &cwd)?;
        client.new_window(&session, Some(&cwd))?;
        client.new_window(&session, Some(&cwd))?;
        let snapshot = client.snapshot()?;
        let session_snapshot = snapshot
            .sessions
            .iter()
            .find(|candidate| candidate.id == session)
            .context("attach perf session should exist")?;
        let first_window = session_snapshot
            .windows
            .first()
            .context("attach perf first window should exist")?;
        let second_window = session_snapshot
            .windows
            .get(1)
            .context("attach perf second window should exist")?;
        let first_focused = first_window.anchor.clone();
        let second_focused = second_window.anchor.clone();
        let mut terminal = BackendPaneTerminal::new_with_backend(
            TerminalGeometry {
                cols: 100,
                rows: 30,
                cell_width: 10,
                cell_height: 20,
            },
            MultiplexerBackendConfig::Rmux,
            terminal_config(),
            Arc::new(|| {}),
        );

        let attach_start = std::time::Instant::now();
        terminal.sync_native_window(
            &first_window.panes,
            Some(&first_focused),
            Some(&first_window.id),
            MultiplexerBackendConfig::Rmux,
            false,
        )?;
        let attach_elapsed = attach_start.elapsed();

        let switch_start = std::time::Instant::now();
        terminal.sync_native_window(
            &second_window.panes,
            Some(&second_focused),
            Some(&second_window.id),
            MultiplexerBackendConfig::Rmux,
            false,
        )?;
        let switch_elapsed = switch_start.elapsed();

        eprintln!("rmux attach perf probe: attach={attach_elapsed:?} switch={switch_elapsed:?}");
        assert!(
            attach_elapsed < std::time::Duration::from_millis(100),
            "rmux initial native-window attach should not block UI: {attach_elapsed:?}"
        );
        assert!(
            switch_elapsed < std::time::Duration::from_millis(100),
            "rmux native-window switch should not block UI: {switch_elapsed:?}"
        );

        client.kill_session(&session)?;
        let _ = std::process::Command::new("rmux")
            .arg("kill-server")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        Ok(())
    }
    #[test]
    fn passthrough_override_targets_only_tmux_targets() {
        let target = MuxPaneTarget::Pane {
            session_id: "$1".to_owned(),
            pane_id: "%3".to_owned(),
            cwd: None,
        };

        assert_eq!(
            passthrough_override_target(MultiplexerBackendConfig::Tmux, Some(&target)),
            Some("%3")
        );
        assert_eq!(
            passthrough_override_target(MultiplexerBackendConfig::Native, Some(&target)),
            None
        );
        assert_eq!(
            passthrough_override_target(MultiplexerBackendConfig::Tmux, None),
            None
        );
        assert_eq!(
            passthrough_override_target(
                MultiplexerBackendConfig::Tmux,
                Some(&MuxPaneTarget::Session {
                    session_id: "$1".to_owned(),
                    cwd: None,
                })
            ),
            Some("$1")
        );
    }

    fn terminal_config() -> TerminalSessionConfig {
        TerminalSessionConfig {
            launch: Default::default(),
            colors: TerminalColorConfig::default(),
            cursor: TerminalCursorConfig::default(),
            features: TerminalFeatureConfig::default(),
            max_scrollback: 0,
            macos_option_as_alt: Default::default(),
            side_effect_tx: None,
            side_effect_pane_id: None,
            benchmark_trace: None,
        }
    }

    fn fake_backend_path(program: &str) -> TempDir {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join(program), "").unwrap();
        temp
    }

    struct ColorRecordingRuntime {
        colors: Arc<Mutex<Vec<(u8, u8, u8)>>>,
    }

    impl TerminalRenderSource for ColorRecordingRuntime {
        fn resize(&mut self, _geometry: TerminalGeometry) -> Result<()> {
            Ok(())
        }

        fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
            Ok(Arc::new(RenderFrame::default()))
        }
    }

    impl TerminalRuntime for ColorRecordingRuntime {
        fn drain_pty(&mut self) -> DrainStats {
            DrainStats::default()
        }

        fn pending_pty_len(&self) -> usize {
            0
        }

        fn child_exited(&mut self) -> Result<bool> {
            Ok(false)
        }

        fn set_colors(&mut self, colors: TerminalColorConfig) -> Result<()> {
            self.colors.lock().unwrap().push((
                colors.background.r,
                colors.background.g,
                colors.background.b,
            ));
            Ok(())
        }

        fn write_input(&mut self, _bytes: &[u8]) -> Result<()> {
            Ok(())
        }

        fn write_paste(&mut self, _text: &str) -> Result<()> {
            Ok(())
        }

        fn encode_key(&mut self, _input: KeyInput) -> Result<()> {
            Ok(())
        }

        fn encode_focus(&mut self, _gained: bool) -> Result<()> {
            Ok(())
        }

        fn encode_mouse(&mut self, _input: MouseInput) -> Result<()> {
            Ok(())
        }

        fn handle_mouse_wheel(&mut self, _input: MouseInput, _scroll_delta: isize) -> Result<()> {
            Ok(())
        }
    }

    struct ResizeRecordingRuntime {
        resize_calls: Arc<Mutex<Vec<TerminalGeometry>>>,
    }

    impl TerminalRenderSource for ResizeRecordingRuntime {
        fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
            self.resize_calls.lock().unwrap().push(geometry);
            Ok(())
        }

        fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
            Ok(Arc::new(RenderFrame::default()))
        }
    }

    impl TerminalRuntime for ResizeRecordingRuntime {
        fn drain_pty(&mut self) -> DrainStats {
            DrainStats::default()
        }

        fn pending_pty_len(&self) -> usize {
            0
        }

        fn child_exited(&mut self) -> Result<bool> {
            Ok(false)
        }

        fn force_resize(&mut self) -> Result<()> {
            self.resize_calls.lock().unwrap().push(TerminalGeometry {
                cols: 1,
                rows: 1,
                cell_width: 1,
                cell_height: 1,
            });
            Ok(())
        }

        fn set_colors(&mut self, _colors: TerminalColorConfig) -> Result<()> {
            Ok(())
        }

        fn write_input(&mut self, _bytes: &[u8]) -> Result<()> {
            Ok(())
        }

        fn write_paste(&mut self, _text: &str) -> Result<()> {
            Ok(())
        }

        fn encode_key(&mut self, _input: KeyInput) -> Result<()> {
            Ok(())
        }

        fn encode_focus(&mut self, _gained: bool) -> Result<()> {
            Ok(())
        }

        fn encode_mouse(&mut self, _input: MouseInput) -> Result<()> {
            Ok(())
        }

        fn handle_mouse_wheel(&mut self, _input: MouseInput, _scroll_delta: isize) -> Result<()> {
            Ok(())
        }
    }

    fn color_config(background: (u8, u8, u8)) -> TerminalColorConfig {
        let mut colors = TerminalColorConfig::default();
        colors.background.r = background.0;
        colors.background.g = background.1;
        colors.background.b = background.2;
        colors
    }

    #[test]
    fn rmux_native_layout_focus_switch_parks_active_runtime() {
        let geometry = TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        };
        let target: ScopedMuxPaneTarget = MuxPaneTarget::Pane {
            session_id: "agents".to_owned(),
            pane_id: "%4".to_owned(),
            cwd: None,
        }
        .into();
        let mut terminal = BackendPaneTerminal::new_with_backend(
            geometry,
            MultiplexerBackendConfig::Rmux,
            terminal_config(),
            Arc::new(|| {}),
        );
        terminal.active_target = Some(target.clone());
        terminal.terminal = Box::new(ColorRecordingRuntime {
            colors: Arc::new(Mutex::new(Vec::new())),
        });

        terminal.park_native_layout_terminal();

        assert!(terminal.native_terminals.contains_key(&target));
    }

    #[test]
    fn tmux_resize_reaches_parked_background_sessions() {
        let geometry = TerminalGeometry {
            cols: 120,
            rows: 40,
            cell_width: 10,
            cell_height: 20,
        };
        let background: ScopedMuxPaneTarget = MuxPaneTarget::Session {
            session_id: "$7".to_owned(),
            cwd: None,
        }
        .into();
        let background_resizes = Arc::new(Mutex::new(Vec::new()));
        let mut terminal = BackendPaneTerminal::new_with_backend(
            geometry,
            MultiplexerBackendConfig::Tmux,
            terminal_config(),
            Arc::new(|| {}),
        );
        terminal.native_terminals.insert(
            background,
            Box::new(ResizeRecordingRuntime {
                resize_calls: Arc::clone(&background_resizes),
            }),
        );

        let resized = TerminalGeometry {
            cols: 90,
            rows: 30,
            ..geometry
        };
        TerminalRenderSource::resize(&mut terminal, resized).unwrap();

        assert_eq!(background_resizes.lock().unwrap().as_slice(), &[resized]);
    }

    #[test]
    fn tmux_session_switch_parks_and_restores_its_runtime() {
        let geometry = TerminalGeometry {
            cols: 120,
            rows: 40,
            cell_width: 10,
            cell_height: 20,
        };
        let target: ScopedMuxPaneTarget = MuxPaneTarget::Session {
            session_id: "$4".to_owned(),
            cwd: None,
        }
        .into();
        let resize_calls = Arc::new(Mutex::new(Vec::new()));
        let mut terminal = BackendPaneTerminal::new_with_backend(
            geometry,
            MultiplexerBackendConfig::Tmux,
            terminal_config(),
            Arc::new(|| {}),
        );
        terminal.active_target = Some(target.clone());
        terminal.terminal = Box::new(ResizeRecordingRuntime {
            resize_calls: Arc::clone(&resize_calls),
        });

        terminal.park_native_layout_terminal();
        let mut restored = terminal
            .start_terminal(MultiplexerBackendConfig::Tmux, Some(&target))
            .unwrap();
        let resized = TerminalGeometry {
            cols: 121,
            ..geometry
        };
        TerminalRenderSource::resize(restored.as_mut(), resized).unwrap();

        assert_eq!(resize_calls.lock().unwrap().as_slice(), &[resized]);
        assert!(!terminal.native_terminals.contains_key(&target));
    }

    #[test]
    fn restoring_parked_native_runtime_keeps_its_pane_geometry_until_render_resize() {
        let previous_focused_geometry = TerminalGeometry {
            cols: 120,
            rows: 40,
            cell_width: 10,
            cell_height: 20,
        };
        let target: ScopedMuxPaneTarget = MuxPaneTarget::Pane {
            session_id: "agents".to_owned(),
            pane_id: "%7".to_owned(),
            cwd: None,
        }
        .into();
        let resize_calls = Arc::new(Mutex::new(Vec::new()));
        let mut terminal = BackendPaneTerminal::new_with_backend(
            previous_focused_geometry,
            MultiplexerBackendConfig::Rmux,
            terminal_config(),
            Arc::new(|| {}),
        );
        terminal.native_terminals.insert(
            target.clone(),
            Box::new(ResizeRecordingRuntime {
                resize_calls: Arc::clone(&resize_calls),
            }),
        );

        let _restored = terminal
            .start_terminal(MultiplexerBackendConfig::Rmux, Some(&target))
            .unwrap();

        assert!(resize_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn unchanged_render_geometry_does_not_resize_the_pty() {
        let geometry = TerminalGeometry {
            cols: 120,
            rows: 40,
            cell_width: 10,
            cell_height: 20,
        };
        let resize_calls = Arc::new(Mutex::new(Vec::new()));
        let mut terminal = BackendPaneTerminal::new_with_backend(
            geometry,
            MultiplexerBackendConfig::Tmux,
            terminal_config(),
            Arc::new(|| {}),
        );
        terminal.terminal = Box::new(ResizeRecordingRuntime {
            resize_calls: Arc::clone(&resize_calls),
        });

        TerminalRenderSource::resize(&mut terminal, geometry).unwrap();

        assert!(resize_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn completed_rmux_window_resize_forces_active_and_parked_pane_resizes() {
        let geometry = TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        };
        let active_target: ScopedMuxPaneTarget = MuxPaneTarget::Pane {
            session_id: "agents".to_owned(),
            pane_id: "%7".to_owned(),
            cwd: None,
        }
        .into();
        let parked_target: ScopedMuxPaneTarget = MuxPaneTarget::Pane {
            session_id: "agents".to_owned(),
            pane_id: "%8".to_owned(),
            cwd: None,
        }
        .into();
        let active_calls = Arc::new(Mutex::new(Vec::new()));
        let parked_calls = Arc::new(Mutex::new(Vec::new()));
        let (tx, _rx) = mpsc::channel::<RmuxWindowResizeRequest>();
        let (result_tx, result_rx) = mpsc::channel::<std::result::Result<(), String>>();
        result_tx.send(Ok(())).unwrap();
        let mut terminal = BackendPaneTerminal::new_with_backend(
            geometry,
            MultiplexerBackendConfig::Rmux,
            terminal_config(),
            Arc::new(|| {}),
        );
        terminal.native_window_id = Some("@1".to_owned());
        terminal.last_rmux_window_size = Some(("@1".to_owned(), 117, 40));
        terminal.rmux_window_resize_worker = Some(RmuxWindowResizeWorker { tx, result_rx });
        terminal.active_target = Some(active_target.clone());
        terminal.native_window_targets = vec![active_target, parked_target.clone()];
        terminal.terminal = Box::new(ResizeRecordingRuntime {
            resize_calls: Arc::clone(&active_calls),
        });
        terminal.native_terminals.insert(
            parked_target,
            Box::new(ResizeRecordingRuntime {
                resize_calls: Arc::clone(&parked_calls),
            }),
        );

        terminal.resize_native_layout_window(117, 40).unwrap();

        assert_eq!(active_calls.lock().unwrap().len(), 1);
        assert_eq!(parked_calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn set_colors_updates_focused_and_parked_native_panes() {
        let geometry = TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        };
        let mut terminal = BackendPaneTerminal::new_with_backend(
            geometry,
            MultiplexerBackendConfig::Native,
            terminal_config(),
            Arc::new(|| {}),
        );
        let active_colors = Arc::new(Mutex::new(Vec::new()));
        terminal.terminal = Box::new(ColorRecordingRuntime {
            colors: Arc::clone(&active_colors),
        });
        let parked_colors = Arc::new(Mutex::new(Vec::new()));
        terminal.native_terminals.insert(
            MuxPaneTarget::Pane {
                session_id: "agents".to_owned(),
                pane_id: "%4".to_owned(),
                cwd: None,
            }
            .into(),
            Box::new(ColorRecordingRuntime {
                colors: Arc::clone(&parked_colors),
            }),
        );

        terminal.set_colors(color_config((1, 2, 3))).unwrap();

        assert_eq!(*active_colors.lock().unwrap(), vec![(1, 2, 3)]);
        assert_eq!(*parked_colors.lock().unwrap(), vec![(1, 2, 3)]);
        assert_eq!(terminal.terminal_config.colors.background.r, 1);
        assert_eq!(terminal.terminal_config.colors.background.g, 2);
        assert_eq!(terminal.terminal_config.colors.background.b, 3);
    }

    #[test]
    fn attach_target_uses_session_and_pane_identity_not_process_metadata() {
        let before = MuxPaneAnchor {
            session_id: "agents".to_owned(),
            pane_id: Some("%3".to_owned()),
            cwd: Some("/repo".to_owned()),
            process: Some("nvim".to_owned()),
        };
        let after = MuxPaneAnchor {
            process: Some("zsh".to_owned()),
            cwd: Some("/repo/subdir".to_owned()),
            ..before.clone()
        };

        assert_eq!(MuxPaneTarget::from(before), MuxPaneTarget::from(after));
    }

    #[test]
    fn target_match_uses_session_and_pane_without_cloning_metadata() {
        let target = MuxPaneTarget::Pane {
            session_id: "agents".to_owned(),
            pane_id: "%3".to_owned(),
            cwd: Some("/repo".to_owned()),
        };
        let anchor = MuxPaneAnchor {
            session_id: "agents".to_owned(),
            pane_id: Some("%3".to_owned()),
            cwd: Some("/repo/subdir".to_owned()),
            process: Some("zsh".to_owned()),
        };

        assert!(target_matches_anchor(
            MultiplexerBackendConfig::Rmux,
            Some(&target),
            Some(&anchor)
        ));
    }

    #[test]
    fn pane_rendering_backends_restart_on_missing_and_changed_panes() {
        let target = MuxPaneTarget::Pane {
            session_id: "agents".to_owned(),
            pane_id: "%3".to_owned(),
            cwd: None,
        };
        let session_anchor = MuxPaneAnchor {
            session_id: "agents".to_owned(),
            pane_id: None,
            cwd: None,
            process: None,
        };
        let other_pane = MuxPaneAnchor {
            pane_id: Some("%4".to_owned()),
            ..session_anchor.clone()
        };

        for backend in [
            MultiplexerBackendConfig::Rmux,
            MultiplexerBackendConfig::Native,
        ] {
            assert!(!target_matches_anchor(
                backend,
                Some(&target),
                Some(&session_anchor)
            ));
            assert!(!target_matches_anchor(
                backend,
                Some(&target),
                Some(&other_pane)
            ));
            assert!(target_matches_anchor(backend, None, None));
        }
    }

    #[test]
    fn attached_client_backends_follow_pane_changes_without_restart() {
        let target = MuxPaneTarget::Pane {
            session_id: "agents".to_owned(),
            pane_id: "%3".to_owned(),
            cwd: None,
        };
        let split_changed_active_pane = MuxPaneAnchor {
            session_id: "agents".to_owned(),
            pane_id: Some("%4".to_owned()),
            cwd: None,
            process: None,
        };
        let other_session = MuxPaneAnchor {
            session_id: "dotfiles".to_owned(),
            ..split_changed_active_pane.clone()
        };

        for backend in [
            MultiplexerBackendConfig::Tmux,
            MultiplexerBackendConfig::Zellij,
        ] {
            assert!(target_matches_anchor(
                backend,
                Some(&target),
                Some(&split_changed_active_pane)
            ));
            assert!(!target_matches_anchor(
                backend,
                Some(&target),
                Some(&other_session)
            ));
            assert!(!target_matches_anchor(backend, Some(&target), None));
        }
    }

    #[test]
    fn backend_owned_ui_launches_normal_backend_attach() {
        assert_eq!(
            backend_attach_launch(MultiplexerBackendConfig::Tmux, "agents"),
            (
                "tmux".to_owned(),
                vec![
                    "-T".to_owned(),
                    "256,RGB,clipboard,focus,hyperlinks,overline,strikethrough,sync,title"
                        .to_owned(),
                    "attach-session".to_owned(),
                    "-t".to_owned(),
                    "agents".to_owned()
                ]
            )
        );
        assert_eq!(
            backend_attach_launch(MultiplexerBackendConfig::Zellij, "agents"),
            (
                "zellij".to_owned(),
                vec![
                    "attach".to_owned(),
                    "--create".to_owned(),
                    "agents".to_owned()
                ]
            )
        );
    }

    #[test]
    fn backend_owned_ui_removes_nested_backend_environment() {
        assert_eq!(
            backend_attach_env_remove(MultiplexerBackendConfig::Tmux),
            vec!["TMUX".to_owned()]
        );
        assert_eq!(
            backend_attach_env_remove(MultiplexerBackendConfig::Zellij),
            vec!["ZELLIJ".to_owned()]
        );
    }

    #[test]
    fn attach_keeps_bootty_term_only_when_vendored_terminfo_resolves() {
        let config = TerminalSessionConfig {
            launch: bootty_runtime::SessionLaunchConfig {
                term: "xterm-bootty".to_owned(),
                ..Default::default()
            },
            colors: TerminalColorConfig::default(),
            cursor: TerminalCursorConfig::default(),
            features: TerminalFeatureConfig::default(),
            max_scrollback: 0,
            macos_option_as_alt: Default::default(),
            side_effect_tx: None,
            side_effect_pane_id: None,
            benchmark_trace: None,
        };

        let path = fake_backend_path("tmux");
        let with_terminfo = backend_attach_session_config_with_path(
            config.clone(),
            MultiplexerBackendConfig::Tmux,
            "agents",
            true,
            Some(path.path().as_os_str()),
        )
        .expect("attach config");
        assert_eq!(with_terminfo.launch.term, "xterm-bootty");

        let without_terminfo = backend_attach_session_config_with_path(
            config,
            MultiplexerBackendConfig::Tmux,
            "agents",
            false,
            Some(path.path().as_os_str()),
        )
        .expect("attach config");
        assert_eq!(without_terminfo.launch.term, "xterm-256color");
    }

    #[test]
    fn attach_downgrades_unresolvable_custom_term_to_tmux_compatible() {
        let config = TerminalSessionConfig {
            launch: bootty_runtime::SessionLaunchConfig {
                term: "st-256color".to_owned(),
                ..Default::default()
            },
            colors: TerminalColorConfig::default(),
            cursor: TerminalCursorConfig::default(),
            features: TerminalFeatureConfig::default(),
            max_scrollback: 0,
            macos_option_as_alt: Default::default(),
            side_effect_tx: None,
            side_effect_pane_id: None,
            benchmark_trace: None,
        };

        let path = fake_backend_path("tmux");
        let attach = backend_attach_session_config_with_path(
            config,
            MultiplexerBackendConfig::Tmux,
            "agents",
            true,
            Some(path.path().as_os_str()),
        )
        .expect("attach config");
        assert_eq!(attach.launch.term, "xterm-256color");
    }

    #[test]
    fn backend_owned_ui_uses_tmux_compatible_term() {
        let mut config = TerminalSessionConfig {
            launch: bootty_runtime::SessionLaunchConfig {
                term: "xterm-bootty".to_owned(),
                ..Default::default()
            },
            colors: TerminalColorConfig::default(),
            cursor: TerminalCursorConfig::default(),
            features: TerminalFeatureConfig::default(),
            max_scrollback: 0,
            macos_option_as_alt: Default::default(),
            side_effect_tx: None,
            side_effect_pane_id: None,
            benchmark_trace: None,
        };
        let (program, args) = backend_attach_launch(MultiplexerBackendConfig::Tmux, "agents");
        config.launch.shell = Some(program);
        config.launch.args = args;
        config.launch.env_remove = backend_attach_env_remove(MultiplexerBackendConfig::Tmux);
        config.launch.term = "xterm-256color".to_owned();

        assert_eq!(config.launch.term, "xterm-256color");
        assert_eq!(config.launch.env_remove, vec!["TMUX".to_owned()]);
    }

    #[test]
    fn backend_attach_program_is_resolved_to_absolute_path() {
        let temp = TempDir::new().unwrap();
        let program = temp.path().join("tmux");
        std::fs::write(&program, "").unwrap();

        let resolved = resolve_launch_program_with_path("tmux", Some(temp.path().as_os_str()))
            .expect("program should resolve from supplied PATH");

        assert_eq!(resolved, program.to_string_lossy());
    }
}
