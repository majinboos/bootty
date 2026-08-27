use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::Arc,
};

use anyhow::Result;
use bootty_surface::geometry::{CellMetrics, TerminalGeometry};
use bootty_terminal::terminal_frame::RenderFrame;
use derive_more::{Deref, DerefMut};

use bootty_mux_model::{MuxBackendKind, MuxBindingConfig, SshTarget};
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
    controller::SpaceId,
    provider::{MuxBackendRegistry, PaneBehavior, PaneTopology},
    snapshot::MuxPaneAnchor,
};

pub struct PaneStartRequest<'a> {
    pub target: &'a ScopedMuxPaneTarget,
    pub geometry: TerminalGeometry,
    pub spawn_geometry: TerminalGeometry,
    pub terminal_config: &'a TerminalSessionConfig,
    pub repaint_wakeup: &'a Arc<dyn Fn() + Send + Sync + 'static>,
}

pub struct PaneLayoutResizeRequest<'a> {
    pub window_id: Option<&'a str>,
    pub cols: u16,
    pub rows: u16,
    pub repaint_wakeup: &'a Arc<dyn Fn() + Send + Sync + 'static>,
}

pub trait BackendPanePolicy: Send {
    fn remote_target(&self) -> Option<&SshTarget>;
    fn start_terminal(
        &mut self,
        request: PaneStartRequest<'_>,
    ) -> Result<Option<Box<dyn TerminalRuntime>>>;
    fn sync_target(&mut self, target: Option<&ScopedMuxPaneTarget>, hide_tmux_status: bool);
    fn set_layout_window(&mut self, window_id: Option<&str>);
    fn resize_layout_window(&mut self, request: PaneLayoutResizeRequest<'_>) -> Result<bool>;
    fn deactivate(&mut self);
}

#[derive(Deref, DerefMut)]
pub struct BackendPaneTerminal {
    registry: Arc<MuxBackendRegistry>,
    policy_kind: MuxBackendKind,
    policy: Box<dyn BackendPanePolicy>,
    behavior: PaneBehavior,
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
    native_window_scope: Option<SpaceId>,
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
        registry: Arc<MuxBackendRegistry>,
        config: &MuxBindingConfig,
        terminal_config: TerminalSessionConfig,
        repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    ) -> Self {
        let kind = registry.selected_kind(config);
        let policy = registry.build_pane_policy(config);
        let behavior = registry.app_policy(config).panes;
        Self {
            registry,
            policy_kind: kind,
            policy,
            behavior,
            active_target: None,
            geometry,
            terminal_config,
            repaint_wakeup,
            native_terminals: HashMap::new(),
            native_window_targets: Vec::new(),
            native_window_spawn_geometry: None,
            native_window_id: None,
            native_window_scope: None,
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
        scope: SpaceId,
        config: &MuxBindingConfig,
        anchor: Option<&MuxPaneAnchor>,
    ) -> Result<()> {
        self.sync_mux_anchor_in_scope(Some(scope), config, anchor)
    }

    fn sync_mux_anchor_in_scope(
        &mut self,
        scope: Option<SpaceId>,
        config: &MuxBindingConfig,
        anchor: Option<&MuxPaneAnchor>,
    ) -> Result<()> {
        let next_policy = self.registry.build_pane_policy(config);
        let next_kind = self.registry.selected_kind(config);
        let next_behavior = self.registry.app_policy(config).panes;
        let backend_changed = self.policy_kind != next_kind
            || self.policy.remote_target() != next_policy.remote_target();
        if backend_changed {
            self.policy.deactivate();
            self.policy_kind = next_kind;
            self.policy = next_policy;
            self.behavior = next_behavior;
            self.active_target = None;
            self.native_terminals.clear();
            self.terminal = idle_terminal();
        }
        let target = anchor
            .cloned()
            .map(|anchor| ScopedMuxPaneTarget::from_anchor(scope, anchor));
        if scoped_target_matches_anchor(
            self.behavior.topology,
            scope,
            self.active_target.as_ref(),
            anchor,
        ) {
            self.active_target = target;
            self.policy
                .sync_target(self.active_target.as_ref(), config.hide_tmux_status);
            return Ok(());
        }

        self.park_cached_terminal();
        let phase = bootty_runtime::latency::start();
        let terminal = self.start_terminal(target.as_ref()).inspect_err(|_| {
            self.active_target = None;
            self.policy.sync_target(None, config.hide_tmux_status);
            self.terminal = idle_terminal();
        })?;
        bootty_runtime::latency::trace_slow("attach.start_terminal", phase, 2.0);

        self.active_target = target;
        let phase = bootty_runtime::latency::start();
        self.set_active_terminal(terminal);
        bootty_runtime::latency::trace_slow("attach.set_active_terminal", phase, 2.0);
        let phase = bootty_runtime::latency::start();
        self.policy
            .sync_target(self.active_target.as_ref(), config.hide_tmux_status);
        bootty_runtime::latency::trace_slow("attach.backend_policy", phase, 2.0);
        Ok(())
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
        let mut failed_cached_targets = Vec::new();
        for (target, terminal) in &mut self.native_terminals {
            if terminal.apply_live_config(config.clone()).is_err() {
                // A cached runtime cannot recover from a failed command: its pane or worker is
                // gone. Retire it so the next backend reconciliation can create a fresh runtime.
                failed_cached_targets.push(target.clone());
            }
        }
        for target in failed_cached_targets {
            self.native_terminals.remove(&target);
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
        target: Option<&ScopedMuxPaneTarget>,
    ) -> Result<Box<dyn TerminalRuntime>> {
        let Some(target) = target else {
            return Ok(idle_terminal());
        };

        if self.behavior.cache_terminals
            && let Some(terminal) = self.native_terminals.remove(target)
        {
            return Ok(terminal);
        }

        let request = PaneStartRequest {
            target,
            geometry: self.geometry,
            spawn_geometry: self.native_window_spawn_geometry.unwrap_or(self.geometry),
            terminal_config: &self.terminal_config,
            repaint_wakeup: &self.repaint_wakeup,
        };
        Ok(self
            .policy
            .start_terminal(request)?
            .unwrap_or_else(idle_terminal))
    }

    /// Swap in the runtime the pane slot renders and takes input through. The next render resize is
    /// forwarded even when the slot's geometry is unchanged: the incoming runtime holds whatever
    /// geometry it was parked at, and only the renderer knows the rect this pane now occupies.
    fn set_active_terminal(&mut self, terminal: Box<dyn TerminalRuntime>) {
        self.terminal = terminal;
        self.terminal_awaits_resize = true;
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
        scope: SpaceId,
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
        scope: Option<SpaceId>,
        window_panes: &[MuxPaneAnchor],
        focused: Option<&MuxPaneAnchor>,
        window_id: Option<&str>,
        layout_backend: MuxBackendKind,
        hide_tmux_status: bool,
    ) -> Result<()> {
        debug_assert_eq!(self.policy_kind, layout_backend);
        debug_assert!(matches!(
            self.behavior.topology,
            PaneTopology::ProcessLocal | PaneTopology::BackendReconciled
        ));
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
            self.park_cached_terminal();
            let terminal = self
                .start_terminal(focused_target.as_ref())
                .inspect_err(|_| {
                    self.active_target = None;
                    self.terminal = idle_terminal();
                })?;
            self.active_target = focused_target;
            self.set_active_terminal(terminal);
        }

        for target in &targets {
            if self.active_target.as_ref() == Some(target) {
                continue;
            }
            if !self.native_terminals.contains_key(target) {
                let runtime = self.start_terminal(Some(target))?;
                self.native_terminals.insert(target.clone(), runtime);
            }
        }
        let window_id = window_id.map(str::to_owned);
        if self.native_window_scope != scope || self.native_window_id != window_id {
            self.native_window_scope = scope;
            self.native_window_id = window_id;
            self.policy
                .set_layout_window(self.native_window_id.as_deref());
        }
        self.native_window_targets = targets;
        self.policy
            .sync_target(self.active_target.as_ref(), hide_tmux_status);
        Ok(())
    }

    pub fn resize_native_layout_window(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.native_window_spawn_geometry = Some(TerminalGeometry {
            cols,
            rows,
            cell_width: self.geometry.cell_width,
            cell_height: self.geometry.cell_height,
        });
        let completed = self.policy.resize_layout_window(PaneLayoutResizeRequest {
            window_id: self.native_window_id.as_deref(),
            cols,
            rows,
            repaint_wakeup: &self.repaint_wakeup,
        })?;
        if completed {
            self.force_native_layout_pane_resizes()?;
        }
        Ok(())
    }

    fn force_native_layout_pane_resizes(&mut self) -> Result<()> {
        self.terminal.force_resize()?;
        let mut failed_cached_targets = Vec::new();
        for target in &self.native_window_targets {
            if self.active_target.as_ref() == Some(target) {
                continue;
            }
            if let Some(runtime) = self.native_terminals.get_mut(target)
                && runtime.force_resize().is_err()
            {
                failed_cached_targets.push(target.clone());
            }
        }
        for target in failed_cached_targets {
            self.native_terminals.remove(&target);
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
            .find(|target| target.input_selector() == pane_id)?;
        let terminal = self.native_terminals.get_mut(target)?;
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

    /// Pane ids in the active window whose shell has exited (focused or background), so the layout
    /// can close them. Checked across every live pane, not just the focused one.
    pub fn native_exited_panes(&mut self) -> Vec<String> {
        let mut exited = Vec::new();
        if matches!(self.terminal.child_exited(), Ok(true))
            && let Some(id) = self.focused_pane_id()
        {
            exited.push(id.to_owned());
        }
        for target in &self.native_window_targets {
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
        {
            self.native_terminals.remove(target);
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

    fn park_cached_terminal(&mut self) {
        if !self.behavior.cache_terminals {
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
        self.policy.deactivate();
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
        if self.behavior.resize_cached_terminals {
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
pub enum MuxPaneTarget {
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
    pub fn session_id(&self) -> &str {
        match self {
            Self::Session { session_id, .. } | Self::Pane { session_id, .. } => session_id,
        }
    }

    pub fn input_selector(&self) -> &str {
        match self {
            Self::Pane { pane_id, .. } => pane_id,
            target => target.session_id(),
        }
    }

    pub fn pane_id(&self) -> Option<&str> {
        match self {
            Self::Pane { pane_id, .. } => Some(pane_id),
            Self::Session { .. } => None,
        }
    }

    pub fn cwd(&self) -> Option<&str> {
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
pub struct ScopedMuxPaneTarget {
    scope: Option<SpaceId>,
    target: MuxPaneTarget,
}

impl ScopedMuxPaneTarget {
    fn from_anchor(scope: Option<SpaceId>, anchor: MuxPaneAnchor) -> Self {
        Self {
            scope,
            target: MuxPaneTarget::from(anchor),
        }
    }

    pub fn session_id(&self) -> &str {
        self.target.session_id()
    }

    pub fn mux_target(&self) -> &MuxPaneTarget {
        &self.target
    }

    pub fn input_selector(&self) -> &str {
        self.target.input_selector()
    }

    pub fn pane_id(&self) -> Option<&str> {
        self.target.pane_id()
    }

    pub fn cwd(&self) -> Option<&str> {
        self.target.cwd()
    }

    pub fn side_effect_pane_id(&self) -> Option<String> {
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

pub fn encode_scoped_pane_id(scope: SpaceId, pane_id: &str) -> String {
    format!(
        "{SCOPED_PANE_PREFIX}{}:{pane_id}",
        scope.persistence_value()
    )
}

pub fn decode_scoped_pane_id(value: &str) -> Option<(SpaceId, String)> {
    let mut parts = value.strip_prefix(SCOPED_PANE_PREFIX)?.splitn(2, ':');
    let space_id = parts.next()?.parse().ok()?;
    let pane_id = parts.next()?.to_owned();
    Some((SpaceId::from_persistence(space_id), pane_id))
}

fn scoped_target_matches_anchor(
    topology: PaneTopology,
    scope: Option<SpaceId>,
    target: Option<&ScopedMuxPaneTarget>,
    anchor: Option<&MuxPaneAnchor>,
) -> bool {
    if target.is_some_and(|target| target.scope != scope) {
        return false;
    }
    target_matches_anchor(topology, target.map(|target| &target.target), anchor)
}

fn target_matches_anchor(
    topology: PaneTopology,
    target: Option<&MuxPaneTarget>,
    anchor: Option<&MuxPaneAnchor>,
) -> bool {
    match (target, anchor) {
        (None, None) => true,
        (Some(target), Some(anchor)) if target.session_id() == anchor.session_id => {
            // Attached multiplexer clients follow pane and window changes
            // server-side; restarting them on an active-pane
            // change blanks the whole surface for nothing.
            let anchor_selector = anchor.pane_id.as_deref().unwrap_or(&anchor.session_id);
            topology == PaneTopology::Attach || target.input_selector() == anchor_selector
        }
        _ => false,
    }
}
