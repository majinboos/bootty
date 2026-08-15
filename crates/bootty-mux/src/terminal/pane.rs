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

use bootty_mux_model::{MuxBackendKind, MuxBindingConfig};
use bootty_runtime::{
    DrainStats, TerminalSession, TerminalSessionConfig, frame_source::TerminalFrameSource,
};
use bootty_terminal::{
    terminal_engine::{
        TerminalCopyModeAction, TerminalCopyModeOutcome, TerminalLiveConfig,
        TerminalSearchDirection, TerminalSelectionEvent, TerminalSelectionFormat,
    },
    terminal_input_model::{KeyInput, MouseInput},
};

use crate::{
    config::{remote_transport, selected_backend},
    controller::MuxScope,
    snapshot::MuxPaneAnchor,
    ssh::SshRemote,
};

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
    backend: MuxBackendKind,
    /// Set when this pane's multiplexer runs on another host: the attach client and the pane-local
    /// options bootty sets alongside it all have to reach that host's server rather than this one's.
    remote: Option<SshRemote>,
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
    /// Set when a runtime is swapped into the slot, cleared by the render resize that follows it.
    terminal_awaits_resize: bool,
    #[deref]
    #[deref_mut]
    terminal: Box<dyn TerminalRuntime>,
}
fn idle_terminal() -> Box<dyn TerminalRuntime> {
    Box::new(IdleTerminalRuntime)
}

pub trait TerminalRuntime: TerminalFrameSource {
    fn drain_pty(&mut self) -> DrainStats;
    fn pending_pty_len(&self) -> usize;
    fn child_exited(&mut self) -> Result<bool>;
    fn tty_name(&self) -> Option<&str>;
    fn discard_pending_output(&mut self) -> Result<()>;
    fn force_resize(&mut self) -> Result<()>;
    fn format_selection(&mut self, format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>>;
    fn current_working_directory(&mut self) -> Result<Option<String>>;
    /// Apply colors, cursor, and terminal features as one runtime update.
    ///
    /// A runtime that is not ready yet must retain the aggregate and apply it when it starts.
    fn apply_live_config(&mut self, config: TerminalLiveConfig) -> Result<()>;
    fn is_mouse_tracking(&mut self) -> Result<bool>;
    fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()>;
    fn enter_copy_mode(&mut self) -> Result<()>;
    fn copy_mode_active(&mut self) -> Result<bool>;
    fn handle_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome>;
    fn search_viewport(&mut self, query: &str, direction: TerminalSearchDirection) -> Result<bool>;
    fn begin_selection(&mut self, event: TerminalSelectionEvent) -> Result<()>;
    fn update_selection(&mut self, event: TerminalSelectionEvent) -> Result<()>;
    fn end_selection(&mut self, event: Option<TerminalSelectionEvent>) -> Result<()>;
    fn write_input(&mut self, bytes: &[u8]) -> Result<()>;
    fn write_paste(&mut self, text: &str) -> Result<()>;
    fn encode_key(&mut self, input: KeyInput) -> Result<()>;
    fn encode_focus(&mut self, gained: bool) -> Result<()>;
    fn encode_mouse(&mut self, input: MouseInput) -> Result<()>;
    fn handle_mouse_wheel(&mut self, input: MouseInput, scroll_delta: isize) -> Result<()>;
}
struct IdleTerminalRuntime;

impl TerminalFrameSource for IdleTerminalRuntime {
    fn set_display_scale(&mut self, _display_scale: f32) -> Result<()> {
        Ok(())
    }

    fn set_render_cell_metrics(&mut self, _cell: CellMetrics) -> Result<()> {
        Ok(())
    }

    fn resize(&mut self, _geometry: TerminalGeometry) -> Result<()> {
        Ok(())
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        Ok(Arc::new(RenderFrame::default()))
    }
}

impl TerminalRuntime for IdleTerminalRuntime {
    fn drain_pty(&mut self) -> DrainStats {
        DrainStats::default()
    }

    fn pending_pty_len(&self) -> usize {
        0
    }

    fn child_exited(&mut self) -> Result<bool> {
        Ok(false)
    }

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

    // Idle slots have no terminal state. The live update is intentionally a no-op.
    fn apply_live_config(&mut self, _config: TerminalLiveConfig) -> Result<()> {
        Ok(())
    }

    fn is_mouse_tracking(&mut self) -> Result<bool> {
        Ok(false)
    }

    fn scroll_viewport_delta(&mut self, _delta: isize) -> Result<()> {
        Ok(())
    }

    fn enter_copy_mode(&mut self) -> Result<()> {
        Ok(())
    }

    fn copy_mode_active(&mut self) -> Result<bool> {
        Ok(false)
    }

    fn handle_copy_mode_action(
        &mut self,
        _action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome> {
        Ok(TerminalCopyModeOutcome::default())
    }

    fn search_viewport(
        &mut self,
        _query: &str,
        _direction: TerminalSearchDirection,
    ) -> Result<bool> {
        Ok(false)
    }

    fn begin_selection(&mut self, _event: TerminalSelectionEvent) -> Result<()> {
        Ok(())
    }

    fn update_selection(&mut self, _event: TerminalSelectionEvent) -> Result<()> {
        Ok(())
    }

    fn end_selection(&mut self, _event: Option<TerminalSelectionEvent>) -> Result<()> {
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

#[derive(Debug, PartialEq, Eq)]
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

    fn force_resize(&mut self) -> Result<()> {
        Ok(())
    }

    fn format_selection(&mut self, format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
        Self::format_selection(self, format)
    }

    fn current_working_directory(&mut self) -> Result<Option<String>> {
        Ok(Self::current_working_directory(self))
    }

    fn apply_live_config(&mut self, config: TerminalLiveConfig) -> Result<()> {
        Self::apply_live_config(self, config)
    }

    fn is_mouse_tracking(&mut self) -> Result<bool> {
        Self::is_mouse_tracking(self)
    }

    fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        Self::scroll_viewport_delta(self, delta)
    }

    fn enter_copy_mode(&mut self) -> Result<()> {
        Self::enter_copy_mode(self)
    }

    fn copy_mode_active(&mut self) -> Result<bool> {
        Self::copy_mode_active(self)
    }

    fn handle_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome> {
        Self::handle_copy_mode_action(self, action)
    }

    fn search_viewport(&mut self, query: &str, direction: TerminalSearchDirection) -> Result<bool> {
        Self::search_viewport(self, query, direction)
    }

    fn begin_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        Self::begin_selection(self, event)
    }

    fn update_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        Self::update_selection(self, event)
    }

    fn end_selection(&mut self, event: Option<TerminalSelectionEvent>) -> Result<()> {
        Self::end_selection(self, event)
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
        config: &MuxBindingConfig,
        terminal_config: TerminalSessionConfig,
        repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    ) -> Self {
        let mut pane = Self::new_with_backend(
            geometry,
            selected_backend(config),
            terminal_config,
            repaint_wakeup,
        );
        pane.remote = remote_transport(config);
        pane
    }

    pub(super) fn new_with_backend(
        geometry: TerminalGeometry,
        backend: MuxBackendKind,
        terminal_config: TerminalSessionConfig,
        repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    ) -> Self {
        Self {
            backend,
            remote: None,
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
            terminal_awaits_resize: false,
            terminal: idle_terminal(),
        }
    }

    pub fn sync_mux_anchor(
        &mut self,
        config: &MuxBindingConfig,
        anchor: Option<&MuxPaneAnchor>,
    ) -> Result<()> {
        self.sync_mux_anchor_in_scope(None, config, anchor)
    }

    pub fn sync_scoped_mux_anchor(
        &mut self,
        scope: MuxScope,
        config: &MuxBindingConfig,
        anchor: Option<&MuxPaneAnchor>,
    ) -> Result<()> {
        self.sync_mux_anchor_in_scope(Some(scope), config, anchor)
    }

    fn sync_mux_anchor_in_scope(
        &mut self,
        scope: Option<MuxScope>,
        config: &MuxBindingConfig,
        anchor: Option<&MuxPaneAnchor>,
    ) -> Result<()> {
        let backend = selected_backend(config);
        let remote = remote_transport(config);
        let target = anchor
            .cloned()
            .map(|anchor| ScopedMuxPaneTarget::from_anchor(scope, anchor));
        // A different host is a different server, so its sessions need their own attach client even
        // when the backend and the target name are unchanged.
        if self.backend == backend
            && self.remote == remote
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
        // Restore the outgoing host's pane options before the incoming one's are read, so a switch
        // between hosts cannot leave an override behind on the server bootty is leaving.
        if self.remote != remote {
            self.deactivate_backend_side_effects();
        }
        self.remote = remote;
        let phase = bootty_runtime::latency::start();
        let terminal = self
            .start_terminal(backend, target.as_ref())
            .inspect_err(|_| {
                self.backend = backend;
                self.active_target = None;
                self.sync_tmux_passthrough_override();
                self.clear_terminal();
            })?;
        bootty_runtime::latency::trace_slow("attach.start_terminal", phase, 2.0);

        self.backend = backend;
        self.active_target = target;
        let phase = bootty_runtime::latency::start();
        self.set_active_terminal(terminal);
        bootty_runtime::latency::trace_slow("attach.set_active_terminal", phase, 2.0);
        let phase = bootty_runtime::latency::start();
        self.sync_tmux_passthrough_override();
        bootty_runtime::latency::trace_slow("attach.passthrough_override", phase, 2.0);
        let phase = bootty_runtime::latency::start();
        self.sync_status_bar(config.hide_tmux_status);
        bootty_runtime::latency::trace_slow("attach.status_bar", phase, 2.0);
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
        if let Ok(previous) = take_pane_allow_passthrough(self.remote.as_ref(), pane_id) {
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
        let remote = self.remote.clone();
        for (_, previous) in self.passthrough_all_panes.drain() {
            let _ = restore_pane_allow_passthrough(remote.as_ref(), &previous);
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
        if set_session_status_hidden(self.remote.as_ref(), session, true).is_ok() {
            self.status_hidden_sessions.push(session.to_owned());
        }
    }

    fn restore_tmux_status_bars(&mut self) {
        let remote = self.remote.clone();
        for session in self.status_hidden_sessions.drain(..) {
            let _ = set_session_status_hidden(remote.as_ref(), &session, false);
        }
    }

    pub fn deactivate_backend_side_effects(&mut self) {
        self.restore_tmux_passthrough_overrides();
        self.restore_tmux_status_bars();
    }

    pub fn set_terminal_config(&mut self, terminal_config: TerminalSessionConfig) {
        self.terminal_config = terminal_config;
    }

    pub fn apply_live_config(&mut self, config: TerminalLiveConfig) -> Result<()> {
        self.terminal_config.colors = config.colors.clone();
        self.terminal_config.cursor = config.cursor;
        self.terminal_config.features = config.features;

        // Try every runtime. One dead native pane must not block healthy focused or parked panes.
        let mut failures = Vec::new();
        if let Err(error) = self.terminal.apply_live_config(config.clone()) {
            failures.push(format!("active terminal: {error}"));
        }
        for (target, terminal) in &mut self.native_terminals {
            if let Err(error) = terminal.apply_live_config(config.clone()) {
                failures.push(format!("cached terminal {target:?}: {error}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(
                "failed to apply live terminal config: {}",
                failures.join("; ")
            );
        }
    }

    pub fn current_working_directory(&mut self) -> Result<Option<String>> {
        self.terminal.current_working_directory()
    }

    fn start_terminal(
        &mut self,
        backend: MuxBackendKind,
        target: Option<&ScopedMuxPaneTarget>,
    ) -> Result<Box<dyn TerminalRuntime>> {
        let Some(target) = target else {
            return Ok(idle_terminal());
        };

        match backend {
            MuxBackendKind::Native | MuxBackendKind::Rmux => {
                // A native session whose tabs have all been closed resolves to a session-level target
                // with no pane; it has no shell to attach, so it renders as idle. Rmux session targets
                // can resolve to the active backend pane.
                if backend == MuxBackendKind::Native
                    && !matches!(&target.target, MuxPaneTarget::Pane { .. })
                {
                    return Ok(idle_terminal());
                }
                if let Some(terminal) = self.native_terminals.remove(target) {
                    return Ok(terminal);
                }
                if backend == MuxBackendKind::Native {
                    return self.spawn_native_runtime(target);
                }
                let mut config = self.terminal_config.clone();
                config.side_effect_pane_id = target.side_effect_pane_id();
                Ok(Box::new(RmuxNativeTerminal::new(
                    target.target.clone(),
                    self.remote.as_ref(),
                    self.native_window_spawn_geometry.unwrap_or(self.geometry),
                    config,
                    Arc::clone(&self.repaint_wakeup),
                )?))
            }
            MuxBackendKind::Tmux | MuxBackendKind::Zellij => {
                if backend == MuxBackendKind::Tmux
                    && let Some(terminal) = self.native_terminals.remove(target)
                {
                    return Ok(terminal);
                }
                let mut terminal_config = self.terminal_config.clone();
                terminal_config.side_effect_pane_id = target.side_effect_pane_id();
                let config = backend_attach_session_config(
                    terminal_config,
                    backend,
                    self.remote.as_ref(),
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

    /// Swap in the runtime the pane slot renders and takes input through. The next render resize is
    /// forwarded even when the slot's geometry is unchanged: the incoming runtime holds whatever
    /// geometry it was parked at, and only the renderer knows the rect this pane now occupies.
    fn set_active_terminal(&mut self, terminal: Box<dyn TerminalRuntime>) {
        self.terminal = terminal;
        self.terminal_awaits_resize = true;
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
        layout_backend: MuxBackendKind,
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
        layout_backend: MuxBackendKind,
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
        layout_backend: MuxBackendKind,
        hide_tmux_status: bool,
    ) -> Result<()> {
        debug_assert!(matches!(
            layout_backend,
            MuxBackendKind::Native | MuxBackendKind::Rmux
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
            self.set_active_terminal(terminal);
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
        if self.backend != MuxBackendKind::Rmux {
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

    /// A non-focused window pane's runtime, for painting it into its own sub-rect. The focused pane
    /// is rendered through `BackendPaneTerminal` itself (which keeps `geometry` in sync).
    pub fn terminal_runtime_for_pane(
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
    pub fn focused_terminal_runtime(
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
        self.terminal_runtime_for_pane(pane_id)
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
            MuxBackendKind::Native | MuxBackendKind::Rmux | MuxBackendKind::Tmux
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

impl TerminalFrameSource for BackendPaneTerminal {
    fn set_display_scale(&mut self, display_scale: f32) -> Result<()> {
        self.terminal.set_display_scale(display_scale)
    }

    fn set_render_cell_metrics(&mut self, cell: CellMetrics) -> Result<()> {
        self.terminal.set_render_cell_metrics(cell)
    }

    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        if self.geometry == geometry {
            // A runtime that just landed in the slot is still at the geometry it was parked at, and
            // this is the first call that knows the rect it now occupies. Runtimes drop a resize
            // they already applied, so an unchanged one still never reaches the PTY.
            if !std::mem::take(&mut self.terminal_awaits_resize) {
                return Ok(());
            }
            return self.terminal.resize(geometry);
        }
        self.terminal_awaits_resize = false;
        self.geometry = geometry;
        if self.backend == MuxBackendKind::Tmux {
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
}

impl TerminalRuntime for BackendPaneTerminal {
    fn drain_pty(&mut self) -> DrainStats {
        self.drain_native_window()
    }

    fn pending_pty_len(&self) -> usize {
        self.terminal.pending_pty_len()
    }

    fn child_exited(&mut self) -> Result<bool> {
        Self::child_exited(self)
    }

    fn tty_name(&self) -> Option<&str> {
        self.terminal.tty_name()
    }

    fn discard_pending_output(&mut self) -> Result<()> {
        self.terminal.discard_pending_output()
    }

    fn force_resize(&mut self) -> Result<()> {
        self.terminal.force_resize()
    }

    fn format_selection(&mut self, format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
        self.terminal.format_selection(format)
    }

    fn current_working_directory(&mut self) -> Result<Option<String>> {
        Self::current_working_directory(self)
    }

    fn apply_live_config(&mut self, config: TerminalLiveConfig) -> Result<()> {
        Self::apply_live_config(self, config)
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

    fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        self.terminal.write_input(bytes)
    }

    fn write_paste(&mut self, text: &str) -> Result<()> {
        self.terminal.write_paste(text)
    }

    fn encode_key(&mut self, input: KeyInput) -> Result<()> {
        self.terminal.encode_key(input)
    }

    fn encode_focus(&mut self, gained: bool) -> Result<()> {
        self.terminal.encode_focus(gained)
    }

    fn encode_mouse(&mut self, input: MouseInput) -> Result<()> {
        self.terminal.encode_mouse(input)
    }

    fn handle_mouse_wheel(&mut self, input: MouseInput, scroll_delta: isize) -> Result<()> {
        self.terminal.handle_mouse_wheel(input, scroll_delta)
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
    backend: MuxBackendKind,
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
    backend: MuxBackendKind,
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
            if matches!(backend, MuxBackendKind::Tmux | MuxBackendKind::Zellij) {
                return true;
            }
            let anchor_selector = anchor.pane_id.as_deref().unwrap_or(&anchor.session_id);
            target.input_selector() == anchor_selector
        }
        _ => false,
    }
}

pub(super) fn backend_attach_launch(
    backend: MuxBackendKind,
    session: &str,
) -> (String, Vec<String>) {
    let session = session.to_owned();
    match backend {
        // -T declares outer-terminal features tmux cannot learn from the
        // forced xterm-256color terminfo; "clipboard" enables OSC 52 and
        // "sync" wraps redraws in DEC 2026 to avoid blank layout flashes.
        MuxBackendKind::Tmux => (
            "tmux".to_owned(),
            vec![
                "-T".to_owned(),
                TMUX_CLIENT_FEATURES.to_owned(),
                "attach-session".to_owned(),
                "-t".to_owned(),
                session,
            ],
        ),
        MuxBackendKind::Rmux => unreachable!("rmux is rendered natively via rmux-sdk"),
        MuxBackendKind::Native => {
            unreachable!("native panes are rendered directly by Bootty")
        }
        MuxBackendKind::Zellij => (
            "zellij".to_owned(),
            vec!["attach".to_owned(), "--create".to_owned(), session],
        ),
    }
}

fn backend_attach_env_remove(backend: MuxBackendKind) -> Vec<String> {
    match backend {
        MuxBackendKind::Tmux => vec!["TMUX".to_owned()],
        MuxBackendKind::Rmux => unreachable!("rmux is rendered natively via rmux-sdk"),
        MuxBackendKind::Native => {
            unreachable!("native panes are rendered directly by Bootty")
        }
        MuxBackendKind::Zellij => vec!["ZELLIJ".to_owned()],
    }
}

fn backend_attach_session_config(
    config: TerminalSessionConfig,
    backend: MuxBackendKind,
    remote: Option<&SshRemote>,
    attach_session: &str,
    bootty_terminfo_available: bool,
) -> Result<TerminalSessionConfig> {
    backend_attach_session_config_with_path(
        config,
        backend,
        remote,
        attach_session,
        bootty_terminfo_available,
        env::var_os("PATH").as_deref(),
    )
}

fn backend_attach_session_config_with_path(
    mut config: TerminalSessionConfig,
    backend: MuxBackendKind,
    remote: Option<&SshRemote>,
    attach_session: &str,
    bootty_terminfo_available: bool,
    path: Option<&OsStr>,
) -> Result<TerminalSessionConfig> {
    let (program, args) = backend_attach_launch(backend, attach_session);
    // A remote pane runs the same attach client, in the SSH session that carries its PTY.
    let (program, args) = match remote {
        Some(remote) => remote.proxy_tty_command(&program, &args),
        None => Ok((program, args)),
    }?;
    config.launch.shell = Some(resolve_launch_program_with_path(&program, path)?);
    config.launch.args = args;
    config.launch.env_remove = backend_attach_env_remove(backend);
    // The attach client hard-fails on a TERM it cannot resolve. xterm-bootty
    // only resolves through Bootty's vendored terminfo; anything else falls
    // back to the universally installed xterm-256color, with required
    // features pinned via the -T attach flag either way.
    //
    // That vendored terminfo is on this machine. SSH forwards the name of the
    // terminal and nothing else, so a remote client looks xterm-bootty up in the
    // other host's terminfo and refuses to start: "missing or unsuitable
    // terminal". A remote attach therefore always takes the fallback.
    let terminfo_reaches_the_client = bootty_terminfo_available && remote.is_none();
    if config.launch.term != bootty_runtime::terminfo::XTERM_BOOTTY || !terminfo_reaches_the_client
    {
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
    backend: MuxBackendKind,
    target: Option<&MuxPaneTarget>,
) -> Option<&str> {
    if backend != MuxBackendKind::Tmux {
        return None;
    }
    target.map(|target| match target {
        MuxPaneTarget::Pane { pane_id, .. } => pane_id.as_str(),
        MuxPaneTarget::Session { session_id, .. } => session_id.as_str(),
    })
}

/// Read the pane's current `allow-passthrough` and switch it to `all`, returning the value to put
/// back on drop.
///
/// tmux runs a `;`-separated sequence in one process, so this is a single fork on the
/// session-switch path instead of three. Both reads always run: asking for the global costs
/// nothing extra once the process exists, and it saves a second fork when the pane has no local
/// value. A pane-local value prints its own line first, so two lines means local and one means the
/// pane was inheriting the global.
fn take_pane_allow_passthrough(
    remote: Option<&SshRemote>,
    pane_id: &str,
) -> Result<TmuxOptionValue> {
    let stdout = run_tmux(
        remote,
        &[
            "show-options",
            "-p",
            "-t",
            pane_id,
            "allow-passthrough",
            ";",
            "show-options",
            "-g",
            "allow-passthrough",
            ";",
            "set-option",
            "-p",
            "-t",
            pane_id,
            "allow-passthrough",
            "all",
        ],
        "allow-passthrough read-and-set",
    )?;
    parse_allow_passthrough(&stdout)
        .ok_or_else(|| anyhow::anyhow!("tmux reported no allow-passthrough value"))
}

/// Run one tmux command against the server the pane's runtime is attached to: this machine's, or
/// the remote binding's over SSH. Pane-local options only mean anything on the server that owns the
/// pane, so every one of these has to follow the attach client to its host.
fn run_tmux(remote: Option<&SshRemote>, args: &[&str], what: &str) -> Result<String> {
    let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    let (program, args) = match remote {
        Some(remote) => remote.command("tmux", &args),
        None => ("tmux".to_owned(), args),
    };
    let output = Command::new(resolve_launch_program(&program)?)
        .args(&args)
        .env_remove("TMUX")
        .env_remove("ZELLIJ")
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "tmux {what} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Pick the effective `allow-passthrough` out of the paired `show-options` output.
///
/// tmux prints the pane-local line first and the global second, and omits the local line when the
/// pane has none. So two lines means the pane owns a value worth restoring, and one means it was
/// inheriting and should be unset again on drop.
fn parse_allow_passthrough(stdout: &str) -> Option<TmuxOptionValue> {
    let values: Vec<&str> = stdout
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .collect();
    Some(TmuxOptionValue {
        value: (*values.first()?).to_owned(),
        local: values.len() > 1,
    })
}

fn set_pane_allow_passthrough(
    remote: Option<&SshRemote>,
    pane_id: &str,
    value: &str,
) -> Result<()> {
    run_tmux(
        remote,
        &[
            "set-option",
            "-p",
            "-t",
            pane_id,
            "allow-passthrough",
            value,
        ],
        "set-option allow-passthrough",
    )
    .map(|_| ())
}

fn restore_pane_allow_passthrough(
    remote: Option<&SshRemote>,
    previous: &TmuxPanePassthroughOverride,
) -> Result<()> {
    if previous.previous.local {
        return set_pane_allow_passthrough(remote, &previous.pane_id, &previous.previous.value);
    }
    unset_pane_allow_passthrough(remote, &previous.pane_id)
}

fn unset_pane_allow_passthrough(remote: Option<&SshRemote>, pane_id: &str) -> Result<()> {
    run_tmux(
        remote,
        &["set-option", "-u", "-p", "-t", pane_id, "allow-passthrough"],
        "unset-option allow-passthrough",
    )
    .map(|_| ())
}

/// The session whose tmux status bar should be hidden: only with the feature on,
/// the tmux backend, and a session attached. Native/rmux/zellij are never
/// touched, so this can only ever issue a `set-option` against a tmux server.
fn status_bar_hidden_target(
    hide_enabled: bool,
    backend: MuxBackendKind,
    session_id: Option<&str>,
) -> Option<&str> {
    if hide_enabled && backend == MuxBackendKind::Tmux {
        session_id
    } else {
        None
    }
}

/// Toggle a single tmux session's `status` option on the default-socket server
/// bootty attached. Hiding sets it off for that session alone; restoring unsets
/// the session override so it falls back to the global default. Never sets a
/// global option, so it cannot affect any other session.
fn set_session_status_hidden(
    remote: Option<&SshRemote>,
    session_id: &str,
    hidden: bool,
) -> Result<()> {
    let args: &[&str] = if hidden {
        &["set-option", "-t", session_id, "status", "off"]
    } else {
        &["set-option", "-u", "-t", session_id, "status"]
    };
    run_tmux(remote, args, "set-option status").map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    use std::sync::Mutex;

    use bootty_terminal::terminal_engine::{
        TerminalColorConfig, TerminalCursorConfig, TerminalFeatureConfig,
    };
    use bootty_terminal::terminal_frame::RenderFrame;
    use tempfile::TempDir;

    /// Both fixtures are verbatim output from `tmux show-options -p ... ; show-options -g ...`.
    /// Reading the pair in the wrong order restores the wrong value on drop: the pane either keeps
    /// `all` forever, or loses a setting its owner chose.
    #[test]
    fn paired_show_options_tells_a_pane_local_value_from_an_inherited_one() {
        // Pane with no value of its own: tmux omits the local line and prints only the global.
        assert_eq!(
            parse_allow_passthrough("allow-passthrough off\n"),
            Some(TmuxOptionValue {
                value: "off".to_owned(),
                local: false,
            })
        );
        // Pane carrying its own value: the local line comes first, the global follows.
        assert_eq!(
            parse_allow_passthrough("allow-passthrough all\nallow-passthrough off\n"),
            Some(TmuxOptionValue {
                value: "all".to_owned(),
                local: true,
            })
        );
        // A server that answered with nothing leaves no value to restore.
        assert_eq!(parse_allow_passthrough(""), None);
    }

    #[test]
    fn status_bar_hidden_only_targets_tmux_when_enabled() {
        // Enabled, tmux backend, attached session: that session is the target.
        assert_eq!(
            status_bar_hidden_target(true, MuxBackendKind::Tmux, Some("$1")),
            Some("$1")
        );
        // Disabled: never hide, even on tmux.
        assert_eq!(
            status_bar_hidden_target(false, MuxBackendKind::Tmux, Some("$1")),
            None
        );
        // Safety contract: a non-tmux backend is never touched, so bootty can
        // never run `set-option` against native/rmux/zellij sessions.
        assert_eq!(
            status_bar_hidden_target(true, MuxBackendKind::Native, Some("$1")),
            None
        );
        // No attached session means nothing to toggle.
        assert_eq!(
            status_bar_hidden_target(true, MuxBackendKind::Tmux, None),
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
            MuxBackendKind::Tmux,
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
            MuxBackendKind::Native,
            terminal_config(),
            Arc::new(|| {}),
        );

        terminal
            .sync_native_window(&[], None, Some("@1"), MuxBackendKind::Rmux, false)
            .unwrap();

        assert_eq!(terminal.backend, MuxBackendKind::Rmux);
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
            pane_pid: None,
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
            MuxBackendKind::Rmux,
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
                MuxBackendKind::Rmux,
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
                MuxBackendKind::Rmux,
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
    #[ignore = "requires an isolated RMUX_TMPDIR"]
    fn rmux_live_window_resize_worker_is_non_blocking_and_reaches_server() -> Result<()> {
        std::env::var_os("RMUX_TMPDIR").context("set isolated RMUX_TMPDIR")?;
        crate::start_embedded_rmux_daemon_for_tests()?;
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
            MuxBackendKind::Rmux,
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

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let expected = format!("{session} {window_id} 117x40");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let response = runtime.block_on(crate::rmux::rmux_request(
                rmux_proto::Request::ListWindows(Box::new(rmux_proto::ListWindowsRequest {
                    target: rmux_sdk::SessionName::new(&session)?,
                    format: Some(
                        "#{session_name} #{window_id} #{window_width}x#{window_height}".to_owned(),
                    ),
                    filter: None,
                    sort_order: None,
                    reversed: false,
                })),
            ))?;
            let rmux_proto::Response::ListWindows(response) = response else {
                anyhow::bail!("rmux returned an unexpected list-windows response");
            };
            let last_output = String::from_utf8_lossy(&response.output.stdout).into_owned();
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
        Ok(())
    }

    #[test]
    #[ignore = "requires an isolated RMUX_TMPDIR"]
    fn rmux_live_native_window_attach_and_switch_stay_interactive() -> Result<()> {
        std::env::var_os("RMUX_TMPDIR").context("set isolated RMUX_TMPDIR")?;
        crate::start_embedded_rmux_daemon_for_tests()?;
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
            MuxBackendKind::Rmux,
            terminal_config(),
            Arc::new(|| {}),
        );

        let attach_start = std::time::Instant::now();
        terminal.sync_native_window(
            &first_window.panes,
            Some(&first_focused),
            Some(&first_window.id),
            MuxBackendKind::Rmux,
            false,
        )?;
        let attach_elapsed = attach_start.elapsed();

        let switch_start = std::time::Instant::now();
        terminal.sync_native_window(
            &second_window.panes,
            Some(&second_focused),
            Some(&second_window.id),
            MuxBackendKind::Rmux,
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
            passthrough_override_target(MuxBackendKind::Tmux, Some(&target)),
            Some("%3")
        );
        assert_eq!(
            passthrough_override_target(MuxBackendKind::Native, Some(&target)),
            None
        );
        assert_eq!(
            passthrough_override_target(MuxBackendKind::Tmux, None),
            None
        );
        assert_eq!(
            passthrough_override_target(
                MuxBackendKind::Tmux,
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
        live_config_calls: Arc<Mutex<usize>>,
        fail_live_config: bool,
    }

    macro_rules! noop_terminal_interactions {
        () => {
            fn tty_name(&self) -> Option<&str> {
                None
            }

            fn discard_pending_output(&mut self) -> Result<()> {
                Ok(())
            }

            fn format_selection(
                &mut self,
                _format: TerminalSelectionFormat,
            ) -> Result<Option<Vec<u8>>> {
                Ok(None)
            }

            fn current_working_directory(&mut self) -> Result<Option<String>> {
                Ok(None)
            }

            fn is_mouse_tracking(&mut self) -> Result<bool> {
                Ok(false)
            }

            fn scroll_viewport_delta(&mut self, _delta: isize) -> Result<()> {
                Ok(())
            }

            fn enter_copy_mode(&mut self) -> Result<()> {
                Ok(())
            }

            fn copy_mode_active(&mut self) -> Result<bool> {
                Ok(false)
            }

            fn handle_copy_mode_action(
                &mut self,
                _action: TerminalCopyModeAction,
            ) -> Result<TerminalCopyModeOutcome> {
                Ok(TerminalCopyModeOutcome::default())
            }

            fn search_viewport(
                &mut self,
                _query: &str,
                _direction: TerminalSearchDirection,
            ) -> Result<bool> {
                Ok(false)
            }

            fn begin_selection(&mut self, _event: TerminalSelectionEvent) -> Result<()> {
                Ok(())
            }

            fn update_selection(&mut self, _event: TerminalSelectionEvent) -> Result<()> {
                Ok(())
            }

            fn end_selection(&mut self, _event: Option<TerminalSelectionEvent>) -> Result<()> {
                Ok(())
            }
        };
    }

    impl TerminalFrameSource for ColorRecordingRuntime {
        fn set_display_scale(&mut self, _display_scale: f32) -> Result<()> {
            Ok(())
        }

        fn set_render_cell_metrics(&mut self, _cell: CellMetrics) -> Result<()> {
            Ok(())
        }

        fn resize(&mut self, _geometry: TerminalGeometry) -> Result<()> {
            Ok(())
        }

        fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
            Ok(Arc::new(RenderFrame::default()))
        }
    }

    impl TerminalRuntime for ColorRecordingRuntime {
        noop_terminal_interactions!();

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
            Ok(())
        }

        fn apply_live_config(&mut self, config: TerminalLiveConfig) -> Result<()> {
            *self.live_config_calls.lock().unwrap() += 1;
            self.colors.lock().unwrap().push((
                config.colors.background.r,
                config.colors.background.g,
                config.colors.background.b,
            ));
            if self.fail_live_config {
                anyhow::bail!("test live config failure");
            }
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

    impl TerminalFrameSource for ResizeRecordingRuntime {
        fn set_display_scale(&mut self, _display_scale: f32) -> Result<()> {
            Ok(())
        }

        fn set_render_cell_metrics(&mut self, _cell: CellMetrics) -> Result<()> {
            Ok(())
        }

        fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
            self.resize_calls.lock().unwrap().push(geometry);
            Ok(())
        }

        fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
            Ok(Arc::new(RenderFrame::default()))
        }
    }

    impl TerminalRuntime for ResizeRecordingRuntime {
        noop_terminal_interactions!();

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

        fn apply_live_config(&mut self, _config: TerminalLiveConfig) -> Result<()> {
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
            MuxBackendKind::Rmux,
            terminal_config(),
            Arc::new(|| {}),
        );
        terminal.active_target = Some(target.clone());
        terminal.terminal = Box::new(ColorRecordingRuntime {
            colors: Arc::new(Mutex::new(Vec::new())),
            live_config_calls: Arc::new(Mutex::new(0)),
            fail_live_config: false,
        });

        terminal.park_native_layout_terminal();

        assert!(terminal.native_terminals.contains_key(&target));
    }

    /// Focusing a parked pane whose rect matches the outgoing pane's leaves the slot geometry
    /// unchanged, so the render resize is swallowed — while the runtime that just swapped in is
    /// still at the geometry it was parked at. The first resize after a swap has to get through.
    #[test]
    fn first_render_resize_after_a_swap_reaches_the_new_runtime() {
        let geometry = TerminalGeometry {
            cols: 120,
            rows: 40,
            cell_width: 10,
            cell_height: 20,
        };
        let swapped_in_resizes = Arc::new(Mutex::new(Vec::new()));
        let mut terminal = BackendPaneTerminal::new_with_backend(
            geometry,
            MuxBackendKind::Rmux,
            terminal_config(),
            Arc::new(|| {}),
        );

        terminal.set_active_terminal(Box::new(ResizeRecordingRuntime {
            resize_calls: Arc::clone(&swapped_in_resizes),
        }));
        TerminalFrameSource::resize(&mut terminal, geometry).unwrap();
        // Steady state: later frames at the same geometry stay off the PTY.
        TerminalFrameSource::resize(&mut terminal, geometry).unwrap();

        assert_eq!(swapped_in_resizes.lock().unwrap().as_slice(), &[geometry]);
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
            MuxBackendKind::Tmux,
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
        TerminalFrameSource::resize(&mut terminal, resized).unwrap();

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
            MuxBackendKind::Tmux,
            terminal_config(),
            Arc::new(|| {}),
        );
        terminal.active_target = Some(target.clone());
        terminal.terminal = Box::new(ResizeRecordingRuntime {
            resize_calls: Arc::clone(&resize_calls),
        });

        terminal.park_native_layout_terminal();
        let mut restored = terminal
            .start_terminal(MuxBackendKind::Tmux, Some(&target))
            .unwrap();
        let resized = TerminalGeometry {
            cols: 121,
            ..geometry
        };
        TerminalFrameSource::resize(restored.as_mut(), resized).unwrap();

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
            MuxBackendKind::Rmux,
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
            .start_terminal(MuxBackendKind::Rmux, Some(&target))
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
            MuxBackendKind::Tmux,
            terminal_config(),
            Arc::new(|| {}),
        );
        terminal.terminal = Box::new(ResizeRecordingRuntime {
            resize_calls: Arc::clone(&resize_calls),
        });

        TerminalFrameSource::resize(&mut terminal, geometry).unwrap();

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
            MuxBackendKind::Rmux,
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
    fn apply_live_config_updates_focused_and_parked_native_panes() {
        let geometry = TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        };
        let mut terminal = BackendPaneTerminal::new_with_backend(
            geometry,
            MuxBackendKind::Native,
            terminal_config(),
            Arc::new(|| {}),
        );
        let active_colors = Arc::new(Mutex::new(Vec::new()));
        terminal.terminal = Box::new(ColorRecordingRuntime {
            colors: Arc::clone(&active_colors),
            live_config_calls: Arc::new(Mutex::new(0)),
            fail_live_config: false,
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
                live_config_calls: Arc::new(Mutex::new(0)),
                fail_live_config: false,
            }),
        );

        terminal
            .apply_live_config(TerminalLiveConfig {
                colors: color_config((1, 2, 3)),
                cursor: TerminalCursorConfig::default(),
                features: TerminalFeatureConfig::default(),
            })
            .unwrap();

        assert_eq!(*active_colors.lock().unwrap(), vec![(1, 2, 3)]);
        assert_eq!(*parked_colors.lock().unwrap(), vec![(1, 2, 3)]);
        assert_eq!(terminal.terminal_config.colors.background.r, 1);
        assert_eq!(terminal.terminal_config.colors.background.g, 2);
        assert_eq!(terminal.terminal_config.colors.background.b, 3);
    }

    #[test]
    fn apply_live_config_attempts_all_runtimes_after_one_failure() {
        let geometry = TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        };
        let mut terminal = BackendPaneTerminal::new_with_backend(
            geometry,
            MuxBackendKind::Native,
            terminal_config(),
            Arc::new(|| {}),
        );
        let active_calls = Arc::new(Mutex::new(0));
        terminal.terminal = Box::new(ColorRecordingRuntime {
            colors: Arc::new(Mutex::new(Vec::new())),
            live_config_calls: Arc::clone(&active_calls),
            fail_live_config: true,
        });
        let parked_calls = Arc::new(Mutex::new(0));
        terminal.native_terminals.insert(
            MuxPaneTarget::Pane {
                session_id: "agents".to_owned(),
                pane_id: "%4".to_owned(),
                cwd: None,
            }
            .into(),
            Box::new(ColorRecordingRuntime {
                colors: Arc::new(Mutex::new(Vec::new())),
                live_config_calls: Arc::clone(&parked_calls),
                fail_live_config: false,
            }),
        );

        let result = terminal.apply_live_config(TerminalLiveConfig::default());

        assert!(result.is_err());
        assert_eq!(*active_calls.lock().unwrap(), 1);
        assert_eq!(*parked_calls.lock().unwrap(), 1);
    }

    #[test]
    fn attach_target_uses_session_and_pane_identity_not_process_metadata() {
        let before = MuxPaneAnchor {
            session_id: "agents".to_owned(),
            pane_id: Some("%3".to_owned()),
            pane_pid: None,
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
            pane_pid: None,
            cwd: Some("/repo/subdir".to_owned()),
            process: Some("zsh".to_owned()),
        };

        assert!(target_matches_anchor(
            MuxBackendKind::Rmux,
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
            pane_pid: None,
            cwd: None,
            process: None,
        };
        let other_pane = MuxPaneAnchor {
            pane_id: Some("%4".to_owned()),
            pane_pid: None,
            ..session_anchor.clone()
        };

        for backend in [MuxBackendKind::Rmux, MuxBackendKind::Native] {
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
            pane_pid: None,
            cwd: None,
            process: None,
        };
        let other_session = MuxPaneAnchor {
            session_id: "dotfiles".to_owned(),
            ..split_changed_active_pane.clone()
        };

        for backend in [MuxBackendKind::Tmux, MuxBackendKind::Zellij] {
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
            backend_attach_launch(MuxBackendKind::Tmux, "agents"),
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
            backend_attach_launch(MuxBackendKind::Zellij, "agents"),
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
            backend_attach_env_remove(MuxBackendKind::Tmux),
            vec!["TMUX".to_owned()]
        );
        assert_eq!(
            backend_attach_env_remove(MuxBackendKind::Zellij),
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
            MuxBackendKind::Tmux,
            None,
            "agents",
            true,
            Some(path.path().as_os_str()),
        )
        .expect("attach config");
        assert_eq!(with_terminfo.launch.term, "xterm-bootty");

        let without_terminfo = backend_attach_session_config_with_path(
            config,
            MuxBackendKind::Tmux,
            None,
            "agents",
            false,
            Some(path.path().as_os_str()),
        )
        .expect("attach config");
        assert_eq!(without_terminfo.launch.term, "xterm-256color");
    }

    /// A remote pane is the same attach client, run on the other host. Launching tmux locally here
    /// would attach to whatever session this machine happens to have under that name — or fail on a
    /// machine without tmux at all, which is the whole reason the binding is remote.
    #[test]
    fn a_remote_binding_attaches_over_ssh_instead_of_running_tmux_here() {
        let config = TerminalSessionConfig {
            launch: bootty_runtime::SessionLaunchConfig {
                term: bootty_runtime::terminfo::XTERM_BOOTTY.to_owned(),
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
        let remote = SshRemote::new(bootty_mux_model::SshTarget {
            host: "devbox".to_owned(),
            user: None,
            port: None,
            program: "ssh".to_owned(),
            args: Vec::new(),
        });

        let path = fake_backend_path("ssh");
        let attach = backend_attach_session_config_with_path(
            config,
            MuxBackendKind::Tmux,
            Some(&remote),
            "agents",
            true,
            Some(path.path().as_os_str()),
        )
        .expect("attach config");

        assert_eq!(
            Path::new(attach.launch.shell.as_deref().expect("attach program")).file_name(),
            Some(OsStr::new("ssh"))
        );
        assert!(attach.launch.args.contains(&"-t".to_owned()));
        assert_eq!(
            crate::ssh::decode_proxy_command_line(
                attach.launch.args.last().expect("remote command")
            )
            .expect("decode remote command"),
            (
                "tmux".to_owned(),
                vec![
                    "-T".to_owned(),
                    TMUX_CLIENT_FEATURES.to_owned(),
                    "attach-session".to_owned(),
                    "-t".to_owned(),
                    "agents".to_owned(),
                ],
                true,
            )
        );
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
            MuxBackendKind::Tmux,
            None,
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
        let (program, args) = backend_attach_launch(MuxBackendKind::Tmux, "agents");
        config.launch.shell = Some(program);
        config.launch.args = args;
        config.launch.env_remove = backend_attach_env_remove(MuxBackendKind::Tmux);
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
