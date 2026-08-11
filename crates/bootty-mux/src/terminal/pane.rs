use std::{
    collections::{BTreeMap, HashMap, HashSet},
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
    DrainStats, TerminalOutputObservation, TerminalRuntimeObservations, TerminalSession,
    TerminalSessionConfig, render_source::TerminalRenderSource,
};
use bootty_terminal::{
    terminal_engine::{
        TerminalColorConfig, TerminalCopyModeAction, TerminalCopyModeOutcome, TerminalCursorConfig,
        TerminalFeatureConfig, TerminalSearchDirection, TerminalSelectionEvent,
        TerminalSelectionFormat, TerminalSideEffect, TerminalSideEffectEvent,
    },
    terminal_input_model::{KeyInput, MouseInput},
};

use crate::{
    backend::MuxAllocatedResources,
    command::{MuxPaneLaunchPlan, MuxSessionLaunchPlan},
    config::{remote_transport, selected_backend},
    controller::MuxScope,
    native::{
        NativeBackend, NativePaneRuntimeIdentity, NativeTerminalLaunch, NativeTerminalProcess,
    },
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
    backend: MultiplexerBackendConfig,
    /// Set when this pane's multiplexer runs on another host: the attach client and the pane-local
    /// options bootty sets alongside it all have to reach that host's server rather than this one's.
    remote: Option<SshRemote>,
    active_target: Option<ScopedMuxPaneTarget>,
    geometry: TerminalGeometry,
    terminal_config: TerminalSessionConfig,
    repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    native_terminals: HashMap<ScopedMuxPaneTarget, Box<dyn TerminalRuntime>>,
    /// Materialized native terminal intent keyed by authoritative pane and terminal identity. It
    /// is installed before any runtime starts, so argv never crosses the shell-selection path.
    native_launches: HashMap<ScopedMuxPaneTarget, NativeTerminalLaunch>,
    /// The workspace's authoritative native mux state. It is set by the AppState binding that owns
    /// the same workspace identity as its controller backend.
    native_event_backend: Option<NativeBackend>,
    /// A runtime can emit only against its lease's exact occupant; a spawned replacement receives a
    /// new lease before it can be drained.
    native_runtime_identities: HashMap<ScopedMuxPaneTarget, NativePaneRuntimeIdentity>,
    /// Distinguishes local native PTYs from cached attach clients that reuse `native_terminals`.
    native_runtime_targets: HashSet<ScopedMuxPaneTarget>,
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
    Box::new(IdleRenderSource)
}

pub trait TerminalRuntime: TerminalRenderSource {
    fn drain_pty(&mut self) -> DrainStats;
    fn pending_pty_len(&self) -> usize;
    fn child_exited(&mut self) -> Result<bool>;
    fn drain_runtime_observations(&mut self) -> TerminalRuntimeObservations {
        TerminalRuntimeObservations::default()
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

#[derive(Debug, PartialEq, Eq)]
struct TmuxOptionValue {
    value: String,
    local: bool,
}

impl TerminalRuntime for TerminalSession {
    fn drain_pty(&mut self) -> DrainStats {
        Self::drain_pty(self)
    }

    fn drain_runtime_observations(&mut self) -> TerminalRuntimeObservations {
        Self::drain_runtime_observations(self)
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
        backend: MultiplexerBackendConfig,
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
            native_launches: HashMap::new(),
            native_event_backend: None,
            native_runtime_identities: HashMap::new(),
            native_runtime_targets: HashSet::new(),
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

    /// Connect this terminal owner to the same native state selected by its mux controller.
    pub fn set_native_event_backend(&mut self, backend: NativeBackend) {
        self.native_event_backend = Some(backend);
        self.native_runtime_identities.clear();
    }

    /// Called only after a new local native PTY has been created. A cache swap keeps the prior
    /// lease; a true respawn replaces the opaque occupant exactly once.
    fn note_native_runtime_started(&mut self, target: &ScopedMuxPaneTarget) {
        self.native_runtime_targets.insert(target.clone());
        let Some(pane_id) = target.pane_id() else {
            return;
        };
        let Some(backend) = self.native_event_backend.clone() else {
            return;
        };
        let runtime = self.native_runtime_identities.get(target).map_or_else(
            || backend.start_pane_runtime(target.session_id(), pane_id),
            |previous| backend.restart_pane_runtime(previous),
        );
        if let Ok(runtime) = runtime {
            self.native_runtime_identities
                .insert(target.clone(), runtime);
        }
    }

    fn native_runtime_identity(
        &mut self,
        target: &ScopedMuxPaneTarget,
    ) -> Option<(NativeBackend, NativePaneRuntimeIdentity)> {
        if !self.native_runtime_targets.contains(target) {
            return None;
        }
        let backend = self.native_event_backend.clone()?;
        if let Some(runtime) = self.native_runtime_identities.get(target) {
            return Some((backend, runtime.clone()));
        }
        let pane_id = target.pane_id()?;
        let runtime = backend
            .start_pane_runtime(target.session_id(), pane_id)
            .ok()?;
        self.native_runtime_identities
            .insert(target.clone(), runtime.clone());
        Some((backend, runtime))
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

    /// Installs a validated native launch plan against the backend's authoritative allocation and
    /// snapshot terminal identities. Staging the complete mapping first prevents an invalid
    /// allocation from changing terminal startup behavior for any pane.
    pub fn register_native_session_launch(
        &mut self,
        scope: MuxScope,
        plan: &MuxSessionLaunchPlan,
        allocated: &MuxAllocatedResources,
        terminal_ids: &HashMap<String, HashMap<String, String>>,
    ) -> Result<()> {
        if allocated.session_id != plan.session_id {
            anyhow::bail!(
                "native allocation session {:?} does not match launch plan {:?}",
                allocated.session_id,
                plan.session_id
            );
        }
        if allocated.windows.len() != plan.windows.len() {
            anyhow::bail!(
                "native allocation has {} windows for a {}-window launch plan",
                allocated.windows.len(),
                plan.windows.len()
            );
        }

        let mut staged = HashMap::with_capacity(plan.pane_count());
        let staging_context = NativeLaunchStagingContext {
            scope,
            session_id: &plan.session_id,
            terminal_ids,
            session_environment: &plan.environment,
        };
        for (window_plan, allocated_window) in plan.windows.iter().zip(&allocated.windows) {
            let mut pane_ids = allocated_window.pane_ids.iter();
            stage_native_launch_layout(
                &mut staged,
                &staging_context,
                &allocated_window.window_id,
                &window_plan.layout,
                &mut pane_ids,
            )?;
            if pane_ids.next().is_some() {
                anyhow::bail!(
                    "native allocation window {:?} has more panes than its launch layout",
                    allocated_window.window_id
                );
            }
        }
        if staged.len() != plan.pane_count() {
            anyhow::bail!("native allocation does not contain every requested launch pane");
        }
        if staged.keys().any(|target| {
            self.native_launches.contains_key(target) || self.native_terminals.contains_key(target)
        }) {
            anyhow::bail!("native allocation reused an existing pane identity");
        }
        let targets: Vec<_> = staged.keys().cloned().collect();
        self.native_launches.extend(staged);
        self.prestart_native_launches(targets);
        Ok(())
    }

    /// Start every allocated pane now; selection later only moves one cached runtime into render.
    fn prestart_native_launches(&mut self, targets: Vec<ScopedMuxPaneTarget>) {
        self.native_terminals.reserve(targets.len());
        for target in targets {
            let runtime = self.spawn_native_runtime(&target);
            debug_assert!(self.native_terminals.insert(target, runtime).is_none());
        }
    }

    pub fn set_terminal_config(&mut self, terminal_config: TerminalSessionConfig) {
        self.terminal_config = terminal_config;
    }

    pub fn set_colors(&mut self, colors: TerminalColorConfig) -> Result<()> {
        self.terminal.set_colors(colors.clone())?;
        for terminal in self.native_terminals.values_mut() {
            terminal.set_colors(colors.clone())?;
        }
        self.terminal_config.colors = colors.clone();
        self.publish_native_option_for_live_runtimes("terminal.colors", format!("{colors:?}"));
        Ok(())
    }

    fn publish_native_option_for_live_runtimes(&mut self, name: &str, value: String) {
        let targets: Vec<_> = self.native_runtime_targets.iter().cloned().collect();
        for target in targets {
            if let Some((backend, runtime)) = self.native_runtime_identity(&target) {
                let _ = backend.observe_runtime_option(&runtime, name.to_owned(), value.clone());
            }
        }
    }

    fn publish_native_runtime_observations(
        &mut self,
        target: &ScopedMuxPaneTarget,
        observations: TerminalRuntimeObservations,
        cwd: Option<String>,
    ) {
        let Some((backend, runtime)) = self.native_runtime_identity(target) else {
            return;
        };
        if observations.side_effects_lagged
            && backend.publish_runtime_side_effect_lag(&runtime).is_err()
        {
            return;
        }
        for output in observations.output {
            let result = match output {
                TerminalOutputObservation::Bytes { sequence, bytes } => {
                    backend.publish_runtime_output(&runtime, sequence, bytes)
                }
                TerminalOutputObservation::Lag {
                    expected_sequence,
                    resume_sequence,
                    missed_events,
                } => backend.publish_runtime_output_lag(
                    &runtime,
                    expected_sequence,
                    resume_sequence,
                    missed_events,
                ),
            };
            if result.is_err() {
                return;
            }
        }
        for effect in observations.side_effects {
            let result = match effect {
                TerminalSideEffect::WindowTitle(title) => {
                    backend.observe_runtime_title(&runtime, title)
                }
                TerminalSideEffect::MouseShape(shape) => backend.observe_runtime_option(
                    &runtime,
                    "terminal.mouse_shape".to_owned(),
                    shape,
                ),
                TerminalSideEffect::KittyTextSizing(value) => backend.observe_runtime_option(
                    &runtime,
                    "terminal.kitty_text_sizing".to_owned(),
                    value,
                ),
                TerminalSideEffect::ConEmuControl(value) => backend.observe_runtime_option(
                    &runtime,
                    "terminal.conemu_control".to_owned(),
                    value,
                ),
                TerminalSideEffect::Iterm2Control(value) => backend.observe_runtime_option(
                    &runtime,
                    "terminal.iterm2_control".to_owned(),
                    value,
                ),
                TerminalSideEffect::Iterm2UserVarPorts(ports) => backend.observe_runtime_option(
                    &runtime,
                    "terminal.iterm2_user_var_ports".to_owned(),
                    ports
                        .into_iter()
                        .map(|port| port.to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                ),
                _ => Ok(()),
            };
            if result.is_err() {
                return;
            }
        }
        if let Some(cwd) = cwd {
            let _ = backend.observe_runtime_cwd(&runtime, cwd);
        }
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
                    return Ok(self.spawn_native_runtime(target));
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

    fn spawn_native_runtime(&mut self, target: &ScopedMuxPaneTarget) -> Box<dyn TerminalRuntime> {
        let mut config = self.terminal_config.clone();
        let initial_title = if let Some(launch) = self.native_launches.get(target) {
            configure_native_terminal_launch(&mut config, launch);
            launch.title().map(str::to_owned)
        } else {
            config.launch.working_directory = target.cwd().map(Path::new).map(Path::to_path_buf);
            None
        };
        config.side_effect_pane_id = target.side_effect_pane_id();
        config.capture_runtime_observations = true;
        let title_event = initial_title.and_then(|title| {
            config
                .side_effect_tx
                .clone()
                .map(|tx| (tx, config.side_effect_pane_id.clone(), title))
        });
        let terminal = StartingNativeTerminal::spawn(
            self.native_window_spawn_geometry.unwrap_or(self.geometry),
            config,
            Arc::clone(&self.repaint_wakeup),
        );
        if let Some((tx, source_pane_id, title)) = title_event {
            let _ = tx.send(TerminalSideEffectEvent::new(
                source_pane_id,
                TerminalSideEffect::WindowTitle(title),
            ));
        }
        self.note_native_runtime_started(target);
        Box::new(terminal)
    }

    /// Remove local PTYs and launch intent that an authoritative snapshot no longer names. The
    /// scope is part of the cache key: other Spaces can legitimately use identical pane ids.
    pub fn prune_scoped_native_panes<'a>(
        &mut self,
        scope: MuxScope,
        panes: impl IntoIterator<Item = &'a MuxPaneAnchor>,
    ) {
        self.prune_native_panes_in_scope(Some(scope), panes);
    }

    fn prune_native_panes_in_scope<'a>(
        &mut self,
        scope: Option<MuxScope>,
        panes: impl IntoIterator<Item = &'a MuxPaneAnchor>,
    ) {
        let live: HashSet<_> = panes
            .into_iter()
            .cloned()
            .map(|anchor| ScopedMuxPaneTarget::from_anchor(scope, anchor))
            .filter(|target| target.pane_id().is_some())
            .collect();
        let stale = self
            .native_targets_in_scope(scope)
            .into_iter()
            .filter(|target| !live.contains(target))
            .collect::<Vec<_>>();
        for target in stale {
            self.discard_native_target(&target);
        }
    }

    fn native_targets_in_scope(&self, scope: Option<MuxScope>) -> HashSet<ScopedMuxPaneTarget> {
        self.active_target
            .iter()
            .chain(self.native_window_targets.iter())
            .chain(self.native_terminals.keys())
            .chain(self.native_launches.keys())
            .chain(self.native_runtime_identities.keys())
            .chain(self.native_runtime_targets.iter())
            .filter(|target| target.scope == scope && target.pane_id().is_some())
            .cloned()
            .collect()
    }

    fn discard_native_target(&mut self, target: &ScopedMuxPaneTarget) {
        let was_active = self.active_target.as_ref() == Some(target);
        self.native_terminals.remove(target);
        self.native_launches.remove(target);
        self.native_runtime_targets.remove(target);
        self.native_runtime_identities.remove(target);
        self.native_window_targets
            .retain(|candidate| candidate != target);
        if was_active {
            self.active_target = None;
            self.terminal = idle_terminal();
            self.terminal_awaits_resize = false;
        }
        if self.native_window_scope == target.scope
            && !self
                .native_window_targets
                .iter()
                .any(|candidate| candidate.scope == target.scope)
        {
            self.native_window_id = None;
            self.native_window_scope = None;
            self.last_rmux_window_size = None;
        }
    }

    /// Reconcile the live native-layout runtimes against the active window's panes: make `focused`
    /// the deref/input runtime and keep every other pane alive in the parked map so it renders and
    /// drains alongside. Focus and tab changes keep shells alive; explicit closure and authoritative
    /// snapshot removal tear them down.
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

    /// Drop every local resource associated with a pane in the current scope, including its cached
    /// launch intent. Removing a runtime drops its PTY and stops its child.
    pub fn discard_pane(&mut self, pane_id: &str) {
        let scope = self
            .active_target
            .as_ref()
            .and_then(|target| target.scope)
            .or(self.native_window_scope);
        self.discard_pane_in_scope(scope, pane_id);
    }

    /// Drop every local resource associated with a pane in an explicit Space binding.
    pub fn discard_scoped_pane(&mut self, scope: MuxScope, pane_id: &str) {
        self.discard_pane_in_scope(Some(scope), pane_id);
    }

    fn discard_pane_in_scope(&mut self, scope: Option<MuxScope>, pane_id: &str) {
        let targets = self
            .native_targets_in_scope(scope)
            .into_iter()
            .filter(|target| target.pane_id() == Some(pane_id))
            .collect::<Vec<_>>();
        for target in targets {
            self.discard_native_target(&target);
        }
    }

    /// Drain the focused terminal and every cached runtime, including inactive scoped workspaces,
    /// so background PTYs cannot stall while another Space is selected.
    pub fn drain_native_window(&mut self) -> DrainStats {
        let active_target = self.active_target.clone();
        let stats = self.terminal.drain_pty();
        let active_observations = self.terminal.drain_runtime_observations();
        let active_cwd = self.terminal.current_working_directory().ok().flatten();
        if let Some(target) = active_target {
            self.publish_native_runtime_observations(&target, active_observations, active_cwd);
        }

        let targets: Vec<_> = self.native_terminals.keys().cloned().collect();
        for target in targets {
            let observes_native_runtime = self.native_runtime_targets.contains(&target);
            let (observations, cwd) = self
                .native_terminals
                .get_mut(&target)
                .map(|runtime| {
                    runtime.drain_pty();
                    if observes_native_runtime {
                        (
                            runtime.drain_runtime_observations(),
                            runtime.current_working_directory().ok().flatten(),
                        )
                    } else {
                        (TerminalRuntimeObservations::default(), None)
                    }
                })
                .unwrap_or_default();
            if observes_native_runtime {
                self.publish_native_runtime_observations(&target, observations, cwd);
            }
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
        self.publish_native_option_for_live_runtimes("terminal.cursor", format!("{cursor:?}"));
        Ok(())
    }

    pub fn set_feature_config(&mut self, features: TerminalFeatureConfig) -> Result<()> {
        self.terminal.set_feature_config(features)?;
        for terminal in self.native_terminals.values_mut() {
            terminal.set_feature_config(features)?;
        }
        self.publish_native_option_for_live_runtimes(
            "terminal.glyph_protocol",
            features.glyph_protocol.to_string(),
        );
        Ok(())
    }

    pub fn grid_size(&self) -> (u16, u16) {
        (self.geometry.cols, self.geometry.rows)
    }

    pub fn child_exited(&mut self) -> Result<bool> {
        self.terminal.child_exited()
    }

    // Drop the active pane's terminal (its PTY is killed on drop) and all of its native cache
    // entries, so the next sync_mux_anchor attaches the surviving pane instead of parking the
    // closed one.
    pub fn discard_active_pane(&mut self) {
        if let Some(target) = self.active_target.clone() {
            self.discard_native_target(&target);
        } else {
            self.terminal = idle_terminal();
            self.terminal_awaits_resize = false;
        }
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

struct NativeLaunchStagingContext<'a> {
    scope: MuxScope,
    session_id: &'a str,
    terminal_ids: &'a HashMap<String, HashMap<String, String>>,
    session_environment: &'a BTreeMap<String, String>,
}

fn stage_native_launch_layout(
    staged: &mut HashMap<ScopedMuxPaneTarget, NativeTerminalLaunch>,
    context: &NativeLaunchStagingContext<'_>,
    window_id: &str,
    layout: &MuxPaneLaunchPlan,
    pane_ids: &mut std::slice::Iter<'_, String>,
) -> Result<()> {
    match layout {
        MuxPaneLaunchPlan::Pane(pane) => {
            let pane_id = pane_ids
                .next()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("native allocation omitted a requested pane"))?;
            let terminal_id = context
                .terminal_ids
                .get(window_id)
                .and_then(|window_terminal_ids| window_terminal_ids.get(&pane_id))
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("native snapshot omitted terminal identity"))?;
            let cwd = pane.cwd.clone();
            let launch = NativeTerminalLaunch::from_mux_pane_launch_with_inherited_environment(
                pane,
                context.session_environment,
            )?;
            let target = ScopedMuxPaneTarget {
                scope: Some(context.scope),
                target: MuxPaneTarget::Pane {
                    session_id: context.session_id.to_owned(),
                    pane_id,
                    terminal_id: Some(terminal_id),
                    cwd: Some(cwd),
                },
            };
            if staged.insert(target, launch).is_some() {
                anyhow::bail!("native allocation reused a pane identity within one launch");
            }
            Ok(())
        }
        MuxPaneLaunchPlan::Split(split) => {
            stage_native_launch_layout(staged, context, window_id, &split.first, pane_ids)?;
            stage_native_launch_layout(staged, context, window_id, &split.second, pane_ids)
        }
    }
}

fn configure_native_terminal_launch(
    config: &mut TerminalSessionConfig,
    launch: &NativeTerminalLaunch,
) {
    config.launch.working_directory = Some(launch.cwd().to_path_buf());
    for (name, value) in launch.environment() {
        if let Some((_, existing)) = config
            .launch
            .env
            .iter_mut()
            .find(|(existing, _)| existing == name)
        {
            *existing = value.clone();
        } else {
            config.launch.env.push((name.clone(), value.clone()));
        }
    }
    config.launch.program = None;
    match launch.process() {
        NativeTerminalProcess::DefaultShell => {}
        NativeTerminalProcess::ShellCommand(command) => {
            config.launch.args = vec!["-c".to_owned(), command.clone()];
        }
        NativeTerminalProcess::Argv { program, arguments } => {
            config.launch.shell = None;
            config.launch.program = Some(program.clone());
            config.launch.args = arguments.clone();
        }
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
        terminal_id: Option<String>,
        cwd: Option<String>,
    },
}

impl PartialEq for MuxPaneTarget {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Session {
                    session_id: left, ..
                },
                Self::Session {
                    session_id: right, ..
                },
            ) => left == right,
            (
                Self::Pane {
                    session_id: left_session,
                    pane_id: left_pane,
                    terminal_id: left_terminal,
                    ..
                },
                Self::Pane {
                    session_id: right_session,
                    pane_id: right_pane,
                    terminal_id: right_terminal,
                    ..
                },
            ) => {
                left_session == right_session
                    && left_pane == right_pane
                    && left_terminal == right_terminal
            }
            _ => false,
        }
    }
}

impl Hash for MuxPaneTarget {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Session { session_id, .. } => {
                0_u8.hash(state);
                session_id.hash(state);
            }
            Self::Pane {
                session_id,
                pane_id,
                terminal_id,
                ..
            } => {
                1_u8.hash(state);
                session_id.hash(state);
                pane_id.hash(state);
                terminal_id.hash(state);
            }
        }
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

    fn terminal_id(&self) -> Option<&str> {
        match self {
            Self::Pane { terminal_id, .. } => terminal_id.as_deref(),
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
                terminal_id: anchor.terminal_id,
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
            if let Some(terminal_id) = target.terminal_id()
                && anchor.terminal_id.as_deref() != Some(terminal_id)
            {
                return false;
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
    backend: MultiplexerBackendConfig,
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
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use crate::{
        backend::{MuxBackend, MuxEventPayload, MuxEventTopic},
        command::MuxCommand,
    };
    use bootty_terminal::terminal_engine::TerminalColorConfig;
    use bootty_terminal::terminal_frame::RenderFrame;
    use tempfile::TempDir;

    struct ObservationRuntime {
        observations: TerminalRuntimeObservations,
        drops: Option<Arc<AtomicUsize>>,
    }

    impl Drop for ObservationRuntime {
        fn drop(&mut self) {
            if let Some(drops) = &self.drops {
                drops.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    impl TerminalRenderSource for ObservationRuntime {
        fn resize(&mut self, _geometry: TerminalGeometry) -> Result<()> {
            Ok(())
        }

        fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
            Ok(Arc::new(RenderFrame::default()))
        }
    }

    impl TerminalRuntime for ObservationRuntime {
        fn drain_pty(&mut self) -> DrainStats {
            DrainStats::default()
        }

        fn pending_pty_len(&self) -> usize {
            0
        }

        fn child_exited(&mut self) -> Result<bool> {
            Ok(false)
        }

        fn drain_runtime_observations(&mut self) -> TerminalRuntimeObservations {
            std::mem::take(&mut self.observations)
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
            terminal_id: Some("%1".to_owned()),
            pane_pid: None,
            occupant_id: None,
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
        Ok(())
    }
    #[test]
    fn passthrough_override_targets_only_tmux_targets() {
        let target = MuxPaneTarget::Pane {
            session_id: "$1".to_owned(),
            pane_id: "%3".to_owned(),
            terminal_id: Some("t3".to_owned()),
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
            capture_runtime_observations: false,
            benchmark_trace: None,
        }
    }

    #[test]
    fn native_window_drain_publishes_runtime_output_and_state_with_one_live_lease() -> Result<()> {
        let workspace = TempDir::new()?;
        let mut backend = NativeBackend::for_workspace(workspace.path());
        backend.execute(MuxCommand::CreateProjectSession {
            session_id: "native-drain".to_owned(),
            cwd: "/repo".to_owned(),
        })?;
        let anchor = backend
            .snapshot()?
            .sessions
            .into_iter()
            .next()
            .expect("created native session")
            .anchor;
        let scope = MuxScope::new(
            crate::controller::SpaceId::from_persistence(81),
            crate::controller::BindingId::from_persistence(82),
        );
        let target = ScopedMuxPaneTarget::from_anchor(Some(scope), anchor);
        let mut terminal = BackendPaneTerminal::new_with_backend(
            TerminalGeometry {
                cols: 80,
                rows: 24,
                cell_width: 10,
                cell_height: 20,
            },
            MultiplexerBackendConfig::Native,
            terminal_config(),
            Arc::new(|| {}),
        );
        terminal.set_native_event_backend(backend.clone());
        terminal.active_target = Some(target.clone());
        terminal.terminal = Box::new(ObservationRuntime {
            observations: TerminalRuntimeObservations {
                output: vec![TerminalOutputObservation::Bytes {
                    sequence: 11,
                    bytes: b"runtime output".to_vec(),
                }],
                side_effects: vec![TerminalSideEffect::WindowTitle("runtime title".to_owned())],
                side_effects_lagged: true,
            },
            drops: None,
        });
        terminal.note_native_runtime_started(&target);
        let _ = backend.drain_events(scope, 8);

        terminal.drain_native_window();
        let events = backend.drain_events(scope, 8);
        assert!(
            events
                .iter()
                .any(|event| event.topic == MuxEventTopic::SnapshotRebased),
            "side-effect lag must request an authoritative rebase"
        );
        let output = events
            .iter()
            .find(|event| event.topic == MuxEventTopic::TerminalOutput)
            .expect("drained runtime output");
        let state = events
            .iter()
            .find(|event| event.topic == MuxEventTopic::PaneStateChanged)
            .expect("state after runtime title");
        let output_target = output.target.as_ref().expect("output target");
        assert_eq!(output_target.session_id.as_deref(), Some("native-drain"));
        assert_eq!(output_target.window_id.as_deref(), Some("tab-1"));
        assert_eq!(output_target.pane_id.as_deref(), Some("pane-1"));
        assert_eq!(output_target.terminal_id.as_deref(), Some("terminal-1"));
        assert!(output_target.occupant.is_some());
        assert_eq!(state.target.as_ref(), Some(output_target));
        assert!(matches!(
            (&output.cursor, &output.payload),
            (Some(cursor), MuxEventPayload::Output { bytes })
                if cursor.sequence == 11 && bytes == b"runtime output"
        ));
        Ok(())
    }

    #[test]
    fn discard_pane_drops_cached_runtime_and_every_launch_and_lease_entry() -> Result<()> {
        let workspace = TempDir::new()?;
        let mut backend = NativeBackend::for_workspace(workspace.path());
        backend.execute(MuxCommand::CreateProjectSession {
            session_id: "discard-runtime".to_owned(),
            cwd: "/repo".to_owned(),
        })?;
        let anchor = backend
            .snapshot()?
            .sessions
            .into_iter()
            .next()
            .expect("created native session")
            .anchor;
        let scope = MuxScope::new(
            crate::controller::SpaceId::from_persistence(91),
            crate::controller::BindingId::from_persistence(92),
        );
        let target = ScopedMuxPaneTarget::from_anchor(Some(scope), anchor);
        let pane_id = target.pane_id().expect("native pane target").to_owned();
        let drops = Arc::new(AtomicUsize::new(0));
        let launch = NativeTerminalLaunch::from_mux_pane_launch_with_inherited_environment(
            &crate::command::MuxPaneLaunch {
                cwd: "/repo".to_owned(),
                command: None,
                argv: None,
                environment: std::collections::BTreeMap::new(),
                title: None,
            },
            &std::collections::BTreeMap::new(),
        )?;
        let mut terminal = BackendPaneTerminal::new_with_backend(
            TerminalGeometry {
                cols: 80,
                rows: 24,
                cell_width: 10,
                cell_height: 20,
            },
            MultiplexerBackendConfig::Native,
            terminal_config(),
            Arc::new(|| {}),
        );
        terminal.set_native_event_backend(backend);
        terminal.native_launches.insert(target.clone(), launch);
        terminal.native_terminals.insert(
            target.clone(),
            Box::new(ObservationRuntime {
                observations: TerminalRuntimeObservations::default(),
                drops: Some(Arc::clone(&drops)),
            }),
        );
        terminal.note_native_runtime_started(&target);
        assert!(terminal.native_runtime_identities.contains_key(&target));
        terminal.native_window_scope = Some(scope);

        terminal.discard_pane(&pane_id);

        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert!(!terminal.native_terminals.contains_key(&target));
        assert!(!terminal.native_launches.contains_key(&target));
        assert!(!terminal.native_runtime_targets.contains(&target));
        assert!(!terminal.native_runtime_identities.contains_key(&target));
        Ok(())
    }

    #[test]
    fn authoritative_snapshot_drops_runtimes_from_a_ditched_native_session() -> Result<()> {
        let workspace = TempDir::new()?;
        let mut backend = NativeBackend::for_workspace(workspace.path());
        backend.execute(MuxCommand::CreateProjectSession {
            session_id: "ditch-runtime".to_owned(),
            cwd: "/repo".to_owned(),
        })?;
        backend.execute(MuxCommand::SplitPane {
            session_id: "ditch-runtime".to_owned(),
            pane_id: None,
            direction: crate::command::MuxSplitDirection::Right,
        })?;
        let panes = backend
            .snapshot()?
            .sessions
            .into_iter()
            .next()
            .expect("created native session")
            .windows
            .into_iter()
            .next()
            .expect("created native window")
            .panes;
        let scope = MuxScope::new(
            crate::controller::SpaceId::from_persistence(93),
            crate::controller::BindingId::from_persistence(94),
        );
        let active = ScopedMuxPaneTarget::from_anchor(Some(scope), panes[0].clone());
        let parked = ScopedMuxPaneTarget::from_anchor(Some(scope), panes[1].clone());
        let active_drops = Arc::new(AtomicUsize::new(0));
        let parked_drops = Arc::new(AtomicUsize::new(0));
        let launch = NativeTerminalLaunch::from_mux_pane_launch_with_inherited_environment(
            &crate::command::MuxPaneLaunch {
                cwd: "/repo".to_owned(),
                command: None,
                argv: None,
                environment: std::collections::BTreeMap::new(),
                title: None,
            },
            &std::collections::BTreeMap::new(),
        )?;
        let mut terminal = BackendPaneTerminal::new_with_backend(
            TerminalGeometry {
                cols: 80,
                rows: 24,
                cell_width: 10,
                cell_height: 20,
            },
            MultiplexerBackendConfig::Native,
            terminal_config(),
            Arc::new(|| {}),
        );
        terminal.set_native_event_backend(backend.clone());
        terminal.active_target = Some(active.clone());
        terminal.terminal = Box::new(ObservationRuntime {
            observations: TerminalRuntimeObservations::default(),
            drops: Some(Arc::clone(&active_drops)),
        });
        terminal.native_terminals.insert(
            parked.clone(),
            Box::new(ObservationRuntime {
                observations: TerminalRuntimeObservations::default(),
                drops: Some(Arc::clone(&parked_drops)),
            }),
        );
        terminal
            .native_launches
            .insert(active.clone(), launch.clone());
        terminal.native_launches.insert(parked.clone(), launch);
        terminal.note_native_runtime_started(&active);
        terminal.note_native_runtime_started(&parked);
        terminal.native_window_targets = vec![active.clone(), parked.clone()];
        terminal.native_window_scope = Some(scope);
        terminal.native_window_id = Some("tab-1".to_owned());

        backend.execute(MuxCommand::DitchSession {
            session_id: "ditch-runtime".to_owned(),
        })?;
        let snapshot = backend.snapshot()?;
        terminal.prune_scoped_native_panes(
            scope,
            snapshot
                .sessions
                .iter()
                .flat_map(|session| session.windows.iter())
                .flat_map(|window| window.panes.iter()),
        );

        assert_eq!(active_drops.load(Ordering::Relaxed), 1);
        assert_eq!(parked_drops.load(Ordering::Relaxed), 1);
        assert!(terminal.active_target.is_none());
        assert!(terminal.native_terminals.is_empty());
        assert!(terminal.native_launches.is_empty());
        assert!(terminal.native_runtime_targets.is_empty());
        assert!(terminal.native_runtime_identities.is_empty());
        assert!(terminal.native_window_targets.is_empty());
        assert_eq!(terminal.native_window_id, None);
        Ok(())
    }

    fn fake_backend_path(program: &str) -> TempDir {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join(program), "").unwrap();
        temp
    }

    #[test]
    fn native_argv_handoff_preserves_relative_program_arguments_environment_and_cwd() {
        let pane = crate::command::MuxPaneLaunch {
            cwd: "/tmp/native launch cwd".to_owned(),
            command: None,
            argv: Some(vec![
                "printf".to_owned(),
                "first spaced argument".to_owned(),
                "second spaced argument".to_owned(),
            ]),
            environment: std::collections::BTreeMap::from([
                ("FROM_PANE".to_owned(), "pane value".to_owned()),
                ("OVERRIDE".to_owned(), "pane value".to_owned()),
            ]),
            title: None,
        };
        let inherited = std::collections::BTreeMap::from([
            ("FROM_SESSION".to_owned(), "session value".to_owned()),
            ("OVERRIDE".to_owned(), "session value".to_owned()),
        ]);
        let launch = NativeTerminalLaunch::from_mux_pane_launch_with_inherited_environment(
            &pane, &inherited,
        )
        .unwrap();
        let mut config = terminal_config();
        config.launch.shell = Some("/bin/sh".to_owned());

        configure_native_terminal_launch(&mut config, &launch);

        assert_eq!(config.launch.program.as_deref(), Some("printf"));
        assert_eq!(config.launch.shell, None);
        assert_eq!(
            config.launch.args,
            vec![
                "first spaced argument".to_owned(),
                "second spaced argument".to_owned(),
            ]
        );
        assert_eq!(
            config.launch.working_directory.as_deref(),
            Some(Path::new("/tmp/native launch cwd"))
        );
        assert!(
            config
                .launch
                .env
                .contains(&("FROM_SESSION".to_owned(), "session value".to_owned()))
        );
        assert!(
            config
                .launch
                .env
                .contains(&("FROM_PANE".to_owned(), "pane value".to_owned()))
        );
        assert_eq!(
            config
                .launch
                .env
                .iter()
                .find(|(name, _)| name == "OVERRIDE")
                .map(|(_, value)| value.as_str()),
            Some("pane value")
        );
        assert_eq!(
            config
                .launch
                .env
                .iter()
                .filter(|(name, _)| name == "OVERRIDE")
                .count(),
            1
        );
    }

    #[test]
    fn native_handoff_keeps_default_shell_and_absolute_argv_distinct() {
        let default_launch = NativeTerminalLaunch::from_mux_pane_launch_with_inherited_environment(
            &crate::command::MuxPaneLaunch {
                cwd: "/tmp/default".to_owned(),
                command: None,
                argv: None,
                environment: std::collections::BTreeMap::new(),
                title: None,
            },
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        let mut default_config = terminal_config();
        default_config.launch.program = Some("stale-direct-program".to_owned());
        default_config.launch.shell = Some("/configured/shell".to_owned());
        default_config.launch.args = vec!["--login".to_owned()];

        configure_native_terminal_launch(&mut default_config, &default_launch);

        assert_eq!(default_config.launch.program, None);
        assert_eq!(
            default_config.launch.shell.as_deref(),
            Some("/configured/shell")
        );
        assert_eq!(default_config.launch.args, vec!["--login".to_owned()]);

        let absolute_argv_launch =
            NativeTerminalLaunch::from_mux_pane_launch_with_inherited_environment(
                &crate::command::MuxPaneLaunch {
                    cwd: "/tmp/absolute".to_owned(),
                    command: None,
                    argv: Some(vec![
                        "/usr/bin/printf".to_owned(),
                        "absolute argv argument".to_owned(),
                    ]),
                    environment: std::collections::BTreeMap::new(),
                    title: None,
                },
                &std::collections::BTreeMap::new(),
            )
            .unwrap();
        let mut absolute_config = terminal_config();
        absolute_config.launch.shell = Some("/configured/shell".to_owned());

        configure_native_terminal_launch(&mut absolute_config, &absolute_argv_launch);

        assert_eq!(
            absolute_config.launch.program.as_deref(),
            Some("/usr/bin/printf")
        );
        assert_eq!(absolute_config.launch.shell, None);
        assert_eq!(
            absolute_config.launch.args,
            vec!["absolute argv argument".to_owned()]
        );
    }

    #[test]
    fn native_launch_registration_prestarts_all_windows_before_selection() -> Result<()> {
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
        let scope = MuxScope::new(
            crate::controller::SpaceId::from_persistence(1),
            crate::controller::BindingId::from_persistence(1),
        );
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let pane = || crate::command::MuxPaneLaunch {
            cwd: cwd.clone(),
            command: Some("exit".to_owned()),
            argv: None,
            environment: std::collections::BTreeMap::new(),
            title: None,
        };
        let plan = crate::command::MuxSessionLaunchPlan {
            session_id: "launch-all-windows".to_owned(),
            focus: true,
            default_cwd: cwd.clone(),
            environment: std::collections::BTreeMap::new(),
            windows: vec![
                crate::command::MuxWindowLaunchPlan {
                    name: Some("first".to_owned()),
                    focus: true,
                    layout: crate::command::MuxPaneLaunchPlan::Pane(pane()),
                },
                crate::command::MuxWindowLaunchPlan {
                    name: Some("second".to_owned()),
                    focus: false,
                    layout: crate::command::MuxPaneLaunchPlan::Pane(pane()),
                },
            ],
            focused_window: 0,
        };
        let allocated = MuxAllocatedResources {
            session_id: plan.session_id.clone(),
            windows: vec![
                crate::backend::MuxAllocatedWindow {
                    window_id: "tab-1".to_owned(),
                    pane_ids: vec!["pane-1".to_owned()],
                },
                crate::backend::MuxAllocatedWindow {
                    window_id: "tab-2".to_owned(),
                    pane_ids: vec!["pane-2".to_owned()],
                },
            ],
        };

        let terminal_ids = HashMap::from([
            (
                "tab-1".to_owned(),
                HashMap::from([("pane-1".to_owned(), "terminal-1".to_owned())]),
            ),
            (
                "tab-2".to_owned(),
                HashMap::from([("pane-2".to_owned(), "terminal-2".to_owned())]),
            ),
        ]);
        terminal.register_native_session_launch(scope, &plan, &allocated, &terminal_ids)?;

        assert_eq!(terminal.native_launches.len(), 2);
        assert_eq!(terminal.native_terminals.len(), 2);
        assert!(terminal.active_target.is_none());
        assert!(terminal.native_window_targets.is_empty());

        let selected = MuxPaneAnchor {
            session_id: plan.session_id.clone(),
            pane_id: Some("pane-1".to_owned()),
            terminal_id: Some("terminal-1".to_owned()),
            pane_pid: None,
            cwd: Some(cwd.clone()),
            process: None,
            occupant_id: None,
        };
        let background = ScopedMuxPaneTarget::from_anchor(
            Some(scope),
            MuxPaneAnchor {
                session_id: plan.session_id,
                pane_id: Some("pane-2".to_owned()),
                terminal_id: Some("terminal-2".to_owned()),
                pane_pid: None,
                cwd: Some(cwd),
                process: None,
                occupant_id: None,
            },
        );

        assert!(
            terminal.native_terminals.contains_key(&background),
            "prestarted native runtime keeps the snapshot terminal identity"
        );
        terminal.sync_scoped_native_window(
            scope,
            std::slice::from_ref(&selected),
            Some(&selected),
            Some("tab-1"),
            MultiplexerBackendConfig::Native,
            false,
        )?;

        assert_eq!(terminal.focused_pane_id(), Some("pane-1"));
        assert!(terminal.native_terminals.contains_key(&background));
        assert_eq!(
            terminal
                .native_window_targets
                .iter()
                .map(ScopedMuxPaneTarget::input_selector)
                .collect::<Vec<_>>(),
            vec!["pane-1"]
        );
        Ok(())
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
            terminal_id: Some("t4".to_owned()),
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
            MultiplexerBackendConfig::Rmux,
            terminal_config(),
            Arc::new(|| {}),
        );

        terminal.set_active_terminal(Box::new(ResizeRecordingRuntime {
            resize_calls: Arc::clone(&swapped_in_resizes),
        }));
        TerminalRenderSource::resize(&mut terminal, geometry).unwrap();
        // Steady state: later frames at the same geometry stay off the PTY.
        TerminalRenderSource::resize(&mut terminal, geometry).unwrap();

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
            terminal_id: Some("t7".to_owned()),
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
            terminal_id: Some("t7".to_owned()),
            cwd: None,
        }
        .into();
        let parked_target: ScopedMuxPaneTarget = MuxPaneTarget::Pane {
            session_id: "agents".to_owned(),
            pane_id: "%8".to_owned(),
            terminal_id: Some("t8".to_owned()),
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
                terminal_id: Some("t4".to_owned()),
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
            terminal_id: Some("t3".to_owned()),
            pane_pid: None,
            occupant_id: None,
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
    fn render_target_distinguishes_terminal_identity_from_pane_identity() {
        let pane = MuxPaneAnchor {
            session_id: "agents".to_owned(),
            pane_id: Some("%p".to_owned()),
            terminal_id: Some("t1".to_owned()),
            pane_pid: None,
            occupant_id: None,
            cwd: None,
            process: None,
        };
        let replacement = MuxPaneAnchor {
            terminal_id: Some("t2".to_owned()),
            ..pane.clone()
        };

        assert_ne!(MuxPaneTarget::from(pane), MuxPaneTarget::from(replacement));
    }
    #[test]
    fn target_match_uses_exact_pane_and_terminal_without_cloning_metadata() {
        let target = MuxPaneTarget::Pane {
            session_id: "agents".to_owned(),
            pane_id: "%3".to_owned(),
            terminal_id: Some("t3".to_owned()),
            cwd: Some("/repo".to_owned()),
        };
        let anchor = MuxPaneAnchor {
            session_id: "agents".to_owned(),
            pane_id: Some("%3".to_owned()),
            terminal_id: Some("t3".to_owned()),
            pane_pid: None,
            occupant_id: None,
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
            terminal_id: Some("t3".to_owned()),
            cwd: None,
        };
        let session_anchor = MuxPaneAnchor {
            session_id: "agents".to_owned(),
            pane_id: None,
            terminal_id: None,
            pane_pid: None,
            occupant_id: None,
            cwd: None,
            process: None,
        };
        let other_pane = MuxPaneAnchor {
            pane_id: Some("%4".to_owned()),
            terminal_id: Some("t4".to_owned()),
            pane_pid: None,
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
            terminal_id: Some("t3".to_owned()),
            cwd: None,
        };
        let split_changed_active_pane = MuxPaneAnchor {
            session_id: "agents".to_owned(),
            pane_id: Some("%4".to_owned()),
            terminal_id: Some("t4".to_owned()),
            pane_pid: None,
            occupant_id: None,
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
            capture_runtime_observations: false,
            benchmark_trace: None,
        };

        let path = fake_backend_path("tmux");
        let with_terminfo = backend_attach_session_config_with_path(
            config.clone(),
            MultiplexerBackendConfig::Tmux,
            None,
            "agents",
            true,
            Some(path.path().as_os_str()),
        )
        .expect("attach config");
        assert_eq!(with_terminfo.launch.term, "xterm-bootty");

        let without_terminfo = backend_attach_session_config_with_path(
            config,
            MultiplexerBackendConfig::Tmux,
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
            capture_runtime_observations: false,
            benchmark_trace: None,
        };
        let remote = SshRemote::new(bootty_config::config::SshRemoteConfig {
            host: "devbox".to_owned(),
            user: None,
            port: None,
            program: "ssh".to_owned(),
            args: Vec::new(),
        });

        let path = fake_backend_path("ssh");
        let attach = backend_attach_session_config_with_path(
            config,
            MultiplexerBackendConfig::Tmux,
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
            capture_runtime_observations: false,
            benchmark_trace: None,
        };

        let path = fake_backend_path("tmux");
        let attach = backend_attach_session_config_with_path(
            config,
            MultiplexerBackendConfig::Tmux,
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
            capture_runtime_observations: false,
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
