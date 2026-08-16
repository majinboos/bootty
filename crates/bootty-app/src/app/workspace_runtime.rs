use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, UdpSocket},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use anyhow::Result;
use bootty_config::config::MultiplexerBackendConfig;
use bootty_terminal::terminal_engine::TerminalSideEffectEvent;

use super::{
    binding_terminal_facts::BindingTerminalFacts, mux_config::realize_binding,
    remote_reconnect::BindingReconnect, state::SpaceSummary,
    terminal_config::terminal_session_config_with_side_effects,
};

use crate::{
    config::{AppearanceVariant, BoottyConfig},
    layout::{PaneLayout, SplitDirection},
    mux::{
        RepaintHandle,
        command::MuxCommand,
        controller::{BindingMuxController, MuxScope, SpaceId, mux_session_refresh_interval},
        provider::{
            MuxAppBackendPolicy, MuxAppBackendRegistry, MuxCommandDispatch, PaneTopology,
            PersistedSessionPolicy, TerminalResidency,
        },
        snapshot::MuxSession,
        terminal::ActiveTerminal,
    },
    renderer::TerminalWidget,
    session_names::SessionNameStore,
    session_order::SessionOrderStore,
    terminal::DrainStats,
    workspace::{
        BackendMembership, BindingMembershipMutation, SpaceMuxOverride, SpaceRemoteOverride,
        WorkspaceBinding, WorkspacePersistenceError, WorkspaceRepository, WorkspaceSpace,
    },
};

/// The only terminal data that the host needs to interpret after a workspace frame.
///
/// The workspace drains every live terminal. It returns only the active drain statistics and
/// active terminal side effects. Inactive side effects are discarded because no host surface owns
/// them.
pub(super) struct WorkspaceDrainResult {
    pub(super) active_drain: DrainStats,
    pub(super) active_terminal_side_effects: Vec<TerminalSideEffectEvent>,
}

pub(super) struct WorkspaceFrameOutcome {
    pub(super) next_wake: Option<Duration>,
    pub(super) errors: Vec<String>,
}

#[derive(Clone, Debug)]
pub(super) struct PendingGeneratedName {
    pub(super) cwd: String,
    /// The name asked of the backend, unique across the whole server.
    pub(super) name: String,
    /// What bootty calls it, which drops any uniqueness suffix `name` had to carry.
    pub(super) display_name: String,
    /// Whether the user chose the name instead of Bootty generating it.
    pub(super) explicit: bool,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(super) struct ScopedWindowId {
    pub(super) scope: MuxScope,
    pub(super) session_id: String,
    pub(super) window_id: String,
}

impl ScopedWindowId {
    pub(super) fn new(scope: MuxScope, session_id: String, window_id: String) -> Self {
        Self {
            scope,
            session_id,
            window_id,
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(super) struct ScopedPaneId {
    pub(super) window: ScopedWindowId,
    pub(super) pane_id: String,
}

pub(super) struct NativeTerminalOwner {
    pub(super) terminal: Box<ActiveTerminal>,
    pub(super) terminal_side_effect_tx: mpsc::Sender<TerminalSideEffectEvent>,
    pub(super) terminal_side_effect_rx: mpsc::Receiver<TerminalSideEffectEvent>,
}

impl NativeTerminalOwner {
    pub(super) fn new(
        config: &BoottyConfig,
        backends: Arc<MuxAppBackendRegistry>,
        variant: AppearanceVariant,
        repaint: RepaintHandle,
    ) -> Self {
        let (terminal_side_effect_tx, terminal_side_effect_rx) = mpsc::channel();
        let session_config =
            terminal_session_config_with_side_effects(config, variant, &terminal_side_effect_tx);
        Self {
            terminal: Box::new(ActiveTerminal::new(
                TerminalWidget::initial_geometry(),
                backends,
                &config.multiplexer,
                session_config,
                repaint,
            )),
            terminal_side_effect_tx,
            terminal_side_effect_rx,
        }
    }

    pub(super) fn replace_binding(binding: &mut BindingRuntime, replacement: Self) -> Self {
        Self {
            terminal: std::mem::replace(&mut binding.terminal, replacement.terminal),
            terminal_side_effect_tx: std::mem::replace(
                &mut binding.terminal_side_effect_tx,
                replacement.terminal_side_effect_tx,
            ),
            terminal_side_effect_rx: std::mem::replace(
                &mut binding.terminal_side_effect_rx,
                replacement.terminal_side_effect_rx,
            ),
        }
    }

    pub(super) fn swap_with_binding(&mut self, binding: &mut BindingRuntime) {
        std::mem::swap(&mut self.terminal, &mut binding.terminal);
        std::mem::swap(
            &mut self.terminal_side_effect_tx,
            &mut binding.terminal_side_effect_tx,
        );
        std::mem::swap(
            &mut self.terminal_side_effect_rx,
            &mut binding.terminal_side_effect_rx,
        );
    }

    pub(super) fn discard_side_effects(&mut self) {
        self.terminal_side_effect_rx.try_iter().for_each(drop);
    }

    pub(super) fn drain_inactive(&mut self) {
        self.terminal.drain_native_window();
        self.discard_side_effects();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersistedSessionRestoreDecision {
    Wait,
    Skip,
    Restore,
}

fn persisted_session_restore_decision(
    policy: PersistedSessionPolicy,
    refresh_applied: bool,
    daemon_has_sessions: bool,
) -> PersistedSessionRestoreDecision {
    match policy {
        PersistedSessionPolicy::Immediate => PersistedSessionRestoreDecision::Restore,
        PersistedSessionPolicy::AfterEmptyInitialSnapshot if !refresh_applied => {
            PersistedSessionRestoreDecision::Wait
        }
        PersistedSessionPolicy::AfterEmptyInitialSnapshot if daemon_has_sessions => {
            PersistedSessionRestoreDecision::Skip
        }
        PersistedSessionPolicy::AfterEmptyInitialSnapshot => {
            PersistedSessionRestoreDecision::Restore
        }
        PersistedSessionPolicy::Never => PersistedSessionRestoreDecision::Skip,
    }
}

pub(super) struct BindingRuntime {
    backends: Arc<MuxAppBackendRegistry>,
    pub(super) backend_policy: MuxAppBackendPolicy,
    pub(super) scope: MuxScope,
    pub(super) label: String,
    placement: SpaceMuxOverride,
    pub(super) reconnect: BindingReconnect,
    pub(super) multiplexer: crate::config::MultiplexerConfig,
    pub(super) terminal: Box<ActiveTerminal>,
    pub(super) mux: BindingMuxController,
    pub(super) session_order: SessionOrderStore,
    pub(super) session_names: SessionNameStore,
    pub(super) pending_generated_names: HashMap<String, PendingGeneratedName>,
    pub(super) membership_reconciliation_ready: bool,
    pub(super) membership_reconciliation_waiting_for_refresh: bool,
    pub(super) generated_names_signature: Option<u64>,
    pub(super) terminal_side_effect_tx: mpsc::Sender<TerminalSideEffectEvent>,
    pub(super) terminal_side_effect_rx: mpsc::Receiver<TerminalSideEffectEvent>,
    pub(super) pane_layouts: HashMap<ScopedWindowId, PaneLayout>,
    pub(super) pending_pane_split_directions: HashMap<ScopedWindowId, SplitDirection>,
    pub(super) terminal_facts: BindingTerminalFacts,
    pub(super) persisted_sessions_restored: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BindingStateCandidate {
    pub(super) scope: MuxScope,
    pub(super) session_order: SessionOrderStore,
    pub(super) session_names: SessionNameStore,
}

impl BindingRuntime {
    pub(super) fn new_with_binding_config(
        state: BindingStateCandidate,
        config: &BoottyConfig,
        backends: Arc<MuxAppBackendRegistry>,
        placement: SpaceMuxOverride,
        realized: super::mux_config::RealizedMuxBinding,
        variant: AppearanceVariant,
        repaint: RepaintHandle,
    ) -> Self {
        let BindingStateCandidate {
            scope,
            session_order,
            session_names,
        } = state;
        let remote_error = realized.availability_error.clone();
        let backend_policy = backends.app_policy(&realized.config);
        let mut binding_config = config.clone();
        binding_config.multiplexer = realized.config.clone();
        let NativeTerminalOwner {
            terminal,
            terminal_side_effect_tx,
            terminal_side_effect_rx,
        } = NativeTerminalOwner::new(&binding_config, Arc::clone(&backends), variant, repaint);
        // Bindings of one workspace share native sessions, separate workspaces cannot see each
        // other's, and reopening a window keeps its own. Native sessions live in this process rather
        // than in a server, so which state a binding reaches is a choice bootty has to make.
        let workspace = config.config_path.clone();
        let mux = BindingMuxController::new(scope, Arc::clone(&backends), Some(workspace));
        let mut binding = Self {
            backends,
            backend_policy,
            label: binding_label(scope, &realized.config),
            placement,
            reconnect: BindingReconnect::default(),
            multiplexer: realized.config,
            scope,
            terminal,
            terminal_side_effect_tx,
            terminal_side_effect_rx,
            mux,
            session_order,
            session_names,
            pending_generated_names: HashMap::new(),
            membership_reconciliation_ready: false,
            membership_reconciliation_waiting_for_refresh: false,
            generated_names_signature: None,
            pane_layouts: HashMap::new(),
            pending_pane_split_directions: HashMap::new(),
            terminal_facts: BindingTerminalFacts::default(),
            persisted_sessions_restored: false,
        };
        if let Some(error) = remote_error {
            binding.mux.set_configured_availability_error(Some(error));
        }
        binding
    }

    pub(super) fn from_workspace(
        workspace_binding: &WorkspaceBinding,
        config: &BoottyConfig,
        backends: Arc<MuxAppBackendRegistry>,
        variant: AppearanceVariant,
        repaint: RepaintHandle,
    ) -> Self {
        let placement = SpaceMuxOverride {
            backend: workspace_binding.backend_override(),
            remote: workspace_binding.remote_override().clone(),
        };
        let realized = realize_binding(config, placement.backend, &placement.remote);
        let mut binding = Self::new_with_binding_config(
            BindingStateCandidate {
                scope: workspace_binding.mux_scope(),
                session_order: workspace_binding.session_order().clone(),
                session_names: workspace_binding.session_names().clone(),
            },
            config,
            backends,
            placement,
            realized,
            variant,
            repaint.clone(),
        );
        binding.label = workspace_binding.name().to_owned();
        if workspace_binding.unavailable() && binding.mux.unavailable_reason().is_none() {
            binding.mux.set_configured_availability_error(Some(
                "binding unavailable; reconnect to restore it".to_owned(),
            ));
        }
        binding.restore_persisted_sessions(&repaint, false);
        if let Some(selection) = workspace_binding.selection() {
            binding.mux.restore_selection(
                selection.session_id().to_owned(),
                selection.window_id().map(str::to_owned),
            );
        }
        binding
    }

    pub(super) fn placement(&self) -> &SpaceMuxOverride {
        &self.placement
    }

    pub(super) fn rebuild(
        &mut self,
        config: &BoottyConfig,
        placement: SpaceMuxOverride,
        variant: AppearanceVariant,
        repaint: RepaintHandle,
    ) {
        let state = BindingStateCandidate {
            scope: self.scope,
            session_order: std::mem::take(&mut self.session_order),
            session_names: std::mem::take(&mut self.session_names),
        };
        let pending_generated_names = std::mem::take(&mut self.pending_generated_names);
        let label = self.label.clone();
        let realized = realize_binding(config, placement.backend, &placement.remote);
        let mut replacement = Self::new_with_binding_config(
            state,
            config,
            Arc::clone(&self.backends),
            placement,
            realized,
            variant,
            repaint.clone(),
        );
        replacement.label = label;
        replacement.pending_generated_names = pending_generated_names;
        replacement.restore_persisted_sessions(&repaint, false);
        *self = replacement;
    }

    pub(super) fn restore_persisted_sessions(&mut self, repaint: &RepaintHandle, applied: bool) {
        if self.mux.unavailable_reason().is_some() || self.persisted_sessions_restored {
            return;
        }
        let decision = persisted_session_restore_decision(
            self.backend_policy.persisted_sessions,
            applied,
            !self.mux.sessions().is_empty(),
        );
        match decision {
            PersistedSessionRestoreDecision::Wait => return,
            PersistedSessionRestoreDecision::Skip => {
                self.persisted_sessions_restored = true;
                return;
            }
            PersistedSessionRestoreDecision::Restore => {
                self.persisted_sessions_restored = true;
            }
        }

        // Flat-session fallback only; split-tree restoration remains out of scope.
        for (session_id, name, cwd) in self
            .session_names
            .persisted_sessions(&self.session_order.session_names())
        {
            self.mux.create_project_session(
                crate::mux::controller::NewMuxSessionRequest {
                    session_id: session_id.clone(),
                    cwd,
                },
                repaint,
                &self.multiplexer,
            );
            if name != session_id {
                self.mux
                    .rename_session(&session_id, name, repaint, &self.multiplexer);
            }
        }
        self.mux
            .apply_session_order(&self.session_order.session_names());
    }

    /// The names bootty shows for `sessions`, in the same order.
    ///
    /// A backend name has to be unique across a whole shared server, so bootty's own name for a
    /// session can differ from it: creating `agents/main` while another Space (or a hand-made tmux
    /// session) already holds that name asks the backend for `agents/main-2`, and that suffix is the
    /// backend's business, not the sidebar's. Sessions bootty has no name for keep the backend name,
    /// and so do two members that would otherwise show the same name — there the suffix is the only
    /// thing telling them apart.
    pub(super) fn session_display_names(&self, sessions: &[MuxSession]) -> Vec<String> {
        let mut counts = HashMap::<&str, usize>::new();
        let candidates = sessions
            .iter()
            .map(|session| {
                let display_name = self
                    .session_names
                    .display_name(&session.id)
                    .unwrap_or(session.name.as_str());
                *counts.entry(display_name).or_default() += 1;
                display_name
            })
            .collect::<Vec<_>>();
        sessions
            .iter()
            .zip(candidates)
            .map(|(session, display_name)| {
                if counts.get(display_name).copied().unwrap_or_default() > 1 {
                    session.name.clone()
                } else {
                    display_name.to_owned()
                }
            })
            .collect()
    }

    /// The same names keyed by session id, for the UI groups that carry sessions from several
    /// bindings at once.
    pub(super) fn session_display_name_map(
        &self,
        sessions: &[MuxSession],
    ) -> HashMap<String, String> {
        sessions
            .iter()
            .map(|session| session.id.clone())
            .zip(self.session_display_names(sessions))
            .collect()
    }

    fn reconcile_session_state(&self, candidate: &mut BindingStateCandidate) {
        let renames = self
            .mux
            .all_sessions()
            .iter()
            .filter_map(|session| {
                let previous = candidate.session_names.last_observed_name(&session.id)?;
                (previous != session.name).then(|| (previous.to_owned(), session.name.clone()))
            })
            .collect::<Vec<_>>();
        for (previous, current) in renames {
            candidate.session_order.rename_session(&previous, &current);
        }
        if self.multiplexer.remote_space_id.is_some() {
            for session in self.mux.all_sessions() {
                candidate.session_order.add_session(&session.name);
            }
        }
        candidate.session_order.sync_sessions(
            self.mux
                .all_sessions()
                .iter()
                .map(|session| session.name.as_str()),
        );
        for pending in self.pending_generated_names.values() {
            if self
                .mux
                .all_sessions()
                .iter()
                .any(|session| session.name == pending.name)
            {
                candidate.session_order.add_session(&pending.name);
            }
        }
    }

    fn publish_session_state(&mut self, candidate: BindingStateCandidate) {
        self.session_order = candidate.session_order;
        self.session_names = candidate.session_names;
        self.mux
            .apply_session_order(&self.session_order.session_names());
    }

    pub(super) fn discard_terminal_side_effects(&mut self) {
        self.terminal_side_effect_rx.try_iter().for_each(drop);
    }

    pub(super) fn membership_completion_is_immediate(&self) -> bool {
        self.backends.command_dispatch(&self.multiplexer) == MuxCommandDispatch::CallerThread
    }

    pub(super) fn window_id(&self, session_id: String, window_id: String) -> ScopedWindowId {
        ScopedWindowId::new(self.scope, session_id, window_id)
    }

    pub(super) fn pane_id(
        &self,
        window: ScopedWindowId,
        pane_id: impl Into<String>,
    ) -> ScopedPaneId {
        ScopedPaneId {
            window,
            pane_id: pane_id.into(),
        }
    }
}

pub(super) struct SpaceRuntime {
    pub(super) id: SpaceId,
    pub(super) name: String,
    pub(super) icon: String,
    pub(super) color: [u8; 3],
    pub(super) tint_sidebar: bool,
    pub(super) position: i64,
    pub(super) binding: BindingRuntime,
    pub(super) inactive_bindings: Vec<BindingRuntime>,
}

impl SpaceRuntime {
    pub(super) fn from_workspace(
        space: &WorkspaceSpace,
        config: &BoottyConfig,
        backends: Arc<MuxAppBackendRegistry>,
        variant: AppearanceVariant,
        repaint: RepaintHandle,
    ) -> Option<Self> {
        let mut bindings = space
            .bindings()
            .iter()
            .map(|workspace_binding| {
                BindingRuntime::from_workspace(
                    workspace_binding,
                    config,
                    Arc::clone(&backends),
                    variant,
                    repaint.clone(),
                )
            })
            .collect::<Vec<_>>();
        if bindings.is_empty() {
            return None;
        }
        Some(Self {
            id: space.id(),
            name: space.name().to_owned(),
            icon: space.icon().to_owned(),
            color: space.color(),
            tint_sidebar: space.tint_sidebar(),
            position: space.position(),
            binding: bindings.remove(0),
            inactive_bindings: bindings,
        })
    }

    pub(super) fn bindings(&self) -> impl Iterator<Item = &BindingRuntime> {
        std::iter::once(&self.binding).chain(self.inactive_bindings.iter())
    }

    pub(super) fn bindings_mut(&mut self) -> impl Iterator<Item = &mut BindingRuntime> {
        std::iter::once(&mut self.binding).chain(self.inactive_bindings.iter_mut())
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct SpaceTransition {
    pub(super) from: SpaceId,
    pub(super) to: SpaceId,
    pub(super) started: Instant,
}

impl SpaceTransition {
    pub(super) const DURATION: Duration = Duration::from_millis(180);

    pub(super) fn progress_at(self, now: Instant) -> f32 {
        (now.saturating_duration_since(self.started).as_secs_f32() / Self::DURATION.as_secs_f32())
            .clamp(0.0, 1.0)
    }
}

fn binding_label(scope: MuxScope, multiplexer: &crate::config::MultiplexerConfig) -> String {
    format!(
        "{} / Binding {}",
        multiplexer.backend,
        scope.binding_id().persistence_value()
    )
}

fn mux_refresh_repaint_after(topology: PaneTopology, window_focused: bool) -> Option<Duration> {
    (topology != PaneTopology::ProcessLocal).then(|| mux_session_refresh_interval(window_focused))
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
        if now < self.next_check {
            return false;
        }
        self.next_check = now + Self::INTERVAL;
        let signature = network_signature();
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

pub(super) struct WorkspaceRuntime {
    backends: Arc<MuxAppBackendRegistry>,
    repository: WorkspaceRepository,
    network_change_detector: NetworkChangeDetector,
    deferred_profile_binding_rebuilds: HashSet<MuxScope>,
    pub(super) active: SpaceRuntime,
    pub(super) inactive_spaces: Vec<SpaceRuntime>,
    pub(super) space_transition: Option<SpaceTransition>,
    pub(super) parked_native_terminal: Option<NativeTerminalOwner>,
}

impl WorkspaceRuntime {
    pub(super) fn open(
        config: &BoottyConfig,
        window_state_key: &str,
        backends: Arc<MuxAppBackendRegistry>,
        variant: AppearanceVariant,
        repaint: RepaintHandle,
    ) -> Result<Self> {
        let (mut repository, snapshot) = WorkspaceRepository::open(&config.config_path)?;
        let selected_space_id = snapshot.selected_space(window_state_key);
        let mut spaces = snapshot
            .spaces()
            .iter()
            .map(|space| {
                let mut runtime = SpaceRuntime::from_workspace(
                    space,
                    config,
                    Arc::clone(&backends),
                    variant,
                    repaint.clone(),
                )
                .ok_or_else(|| anyhow::anyhow!("persisted Space has no backend binding"))?;
                for binding in runtime.bindings_mut() {
                    if snapshot.has_pending_binding_operation(binding.scope) {
                        binding.membership_reconciliation_waiting_for_refresh = true;
                        binding.mux.refresh_on_next_frame();
                    }
                }
                Ok(runtime)
            })
            .collect::<Result<Vec<_>>>()?;
        let active_index = selected_space_id
            .and_then(|id| spaces.iter().position(|space| space.id == id))
            .unwrap_or(0);
        let active = spaces.remove(active_index);
        repository.set_selected_space(window_state_key, active.id)?;

        Ok(Self {
            backends,
            repository,
            network_change_detector: NetworkChangeDetector::new(Instant::now()),
            deferred_profile_binding_rebuilds: HashSet::new(),
            active,
            inactive_spaces: spaces,
            space_transition: None,
            parked_native_terminal: None,
        })
    }

    /// Drain every live terminal before the host interprets active terminal side effects.
    ///
    /// The host owns interpretation of active terminal side effects. The workspace owns all
    /// terminal traversal. Lifecycle work starts later in `advance_frame`.
    pub(super) fn drain(&mut self) -> WorkspaceDrainResult {
        let active_drain = self.active.binding.terminal.drain_native_window();
        for binding in &mut self.active.inactive_bindings {
            binding.terminal.drain_native_window();
            binding.discard_terminal_side_effects();
        }
        for space in &mut self.inactive_spaces {
            for binding in space.bindings_mut() {
                binding.terminal.drain_native_window();
                binding.discard_terminal_side_effects();
            }
        }
        if let Some(owner) = &mut self.parked_native_terminal {
            owner.drain_inactive();
        }

        let active_terminal_side_effects = self
            .active
            .binding
            .terminal_side_effect_rx
            .try_iter()
            .collect();
        WorkspaceDrainResult {
            active_drain,
            active_terminal_side_effects,
        }
    }

    /// Advance backend membership, persistence, naming, profile, and pane state for one frame.
    ///
    /// Each error is retained in order. The host applies them in order so the last error remains
    /// visible to the user.
    pub(super) fn advance_frame(
        &mut self,
        config: &BoottyConfig,
        variant: AppearanceVariant,
        repaint: &RepaintHandle,
        now: Instant,
        window_focused: bool,
    ) -> WorkspaceFrameOutcome {
        let mut errors = Vec::new();
        if self.has_degraded_remote() && self.network_change_detector.changed(now) {
            self.reset_remote_reconnects(now);
        }
        if self.active.binding.backend_policy.panes.topology == PaneTopology::ProcessLocal {
            let exited = self.active.binding.terminal.native_exited_panes();
            for pane_id in exited {
                self.active.binding.close_focused_pane(repaint, &pane_id);
            }
        } else {
            match self.active.binding.terminal.child_exited() {
                Ok(true) => {
                    if self.active.binding.handle_attach_client_exit(now) {
                        self.close_active_attach_pane(repaint);
                    }
                }
                Ok(false) => self.active.binding.note_attach_client_alive(now),
                Err(error) => errors.push(error.to_string()),
            }
            let _ = self.active.binding.reattach_wait(now);
        }
        for binding in self.bindings_mut() {
            binding.poll_membership_command();
        }

        let refresh = self.active.binding.mux.refresh_sessions(
            repaint,
            &self.active.binding.multiplexer.clone(),
            mux_session_refresh_interval(window_focused),
        );
        self.active
            .binding
            .restore_persisted_sessions(repaint, refresh.applied);
        if refresh.applied
            && self
                .active
                .binding
                .membership_reconciliation_waiting_for_refresh
        {
            self.active.binding.membership_reconciliation_ready = true;
        }
        self.active
            .binding
            .resolve_attach_exit_after_refresh(refresh.applied);

        let mut next_wake = mux_refresh_repaint_after(
            self.active.binding.backend_policy.panes.topology,
            window_focused,
        );
        for binding in &mut self.active.inactive_bindings {
            binding.restore_persisted_sessions(repaint, false);
        }
        for space in &mut self.inactive_spaces {
            for binding in space.bindings_mut() {
                binding.restore_persisted_sessions(repaint, false);
            }
        }

        if let Err(error) = self.reconcile_binding_membership_mutations() {
            errors.push(error.to_string());
        }
        let requested_profile_scopes = self.deferred_profile_binding_rebuilds.clone();
        if !requested_profile_scopes.is_empty()
            && let Err(error) = self.rebuild_profile_bindings(
                config,
                Some(&requested_profile_scopes),
                variant,
                repaint.clone(),
            )
        {
            errors.push(error.to_string());
        }
        if let Err(error) = self.reconcile_generated_session_names(repaint) {
            errors.push(error.to_string());
        }
        if let Err(error) = self.reconcile_binding_states() {
            errors.push(error.to_string());
        }

        if !self.active.binding.waiting_to_reattach()
            && let Err(error) = self.active.binding.sync_terminal_panes()
        {
            if self.active.binding.multiplexer.remote.is_some() {
                self.active
                    .binding
                    .handle_attach_start_failure(now, &error.to_string());
            } else {
                errors.push(error.to_string());
            }
        }

        let reattach_wake = self.active.binding.reattach_wait(now);
        next_wake = [next_wake, reattach_wake].into_iter().flatten().min();
        WorkspaceFrameOutcome { next_wake, errors }
    }

    fn close_active_attach_pane(&mut self, repaint: &RepaintHandle) {
        let session_id = self
            .active
            .binding
            .mux
            .selected_session()
            .unwrap_or("local")
            .to_owned();
        let config = self.active.binding.multiplexer.clone();
        self.active.binding.mux.execute_command(
            repaint,
            &config,
            MuxCommand::ClosePane {
                session_id,
                pane_id: None,
            },
        );
        self.active.binding.terminal.discard_active_pane();
    }

    pub(super) fn sync_active_terminal_panes(&mut self) -> Result<()> {
        self.active.binding.sync_terminal_panes()
    }

    pub(super) fn reconnect_space(&mut self, space_id: SpaceId, now: Instant) -> bool {
        let Some(space) = std::iter::once(&mut self.active)
            .chain(self.inactive_spaces.iter_mut())
            .find(|space| space.id == space_id)
        else {
            return false;
        };
        let mut restarted = false;
        for binding in space.bindings_mut() {
            restarted |= binding.restart_remote(now);
        }
        restarted
    }

    fn has_degraded_remote(&self) -> bool {
        self.active
            .bindings()
            .chain(
                self.inactive_spaces
                    .iter()
                    .flat_map(|space| space.bindings()),
            )
            .any(BindingRuntime::is_degraded_remote)
    }

    fn reset_remote_reconnects(&mut self, now: Instant) {
        for binding in self.active.bindings_mut().chain(
            self.inactive_spaces
                .iter_mut()
                .flat_map(|space| space.bindings_mut()),
        ) {
            if binding.is_degraded_remote() {
                binding.restart_remote(now);
            }
        }
    }

    pub(super) fn multiplexer_backend(&self) -> MultiplexerBackendConfig {
        self.active.binding.multiplexer.backend
    }

    pub(super) fn binding_count(&self) -> usize {
        self.active.inactive_bindings.len() + 1
    }

    pub(super) fn active_space_id(&self) -> SpaceId {
        self.active.id
    }

    fn space(&self, space_id: SpaceId) -> Option<&SpaceRuntime> {
        std::iter::once(&self.active)
            .chain(self.inactive_spaces.iter())
            .find(|space| space.id == space_id)
    }

    pub(super) fn selected_binding_scope(&self, space_id: SpaceId) -> Option<MuxScope> {
        self.space(space_id).map(|space| space.binding.scope)
    }

    pub(super) fn space_summaries(&self) -> Vec<SpaceSummary> {
        let mut spaces = vec![(
            self.active.position,
            SpaceSummary {
                id: self.active.id,
                name: self.active.name.clone(),
                icon: self.active.icon.clone(),
                color: self.active.color,
                tint_sidebar: self.active.tint_sidebar,
                active: true,
                error: self.active.binding.degraded_error(),
            },
        )];
        spaces.extend(self.inactive_spaces.iter().map(|space| {
            (
                space.position,
                SpaceSummary {
                    id: space.id,
                    name: space.name.clone(),
                    icon: space.icon.clone(),
                    color: space.color,
                    tint_sidebar: space.tint_sidebar,
                    active: false,
                    error: space.binding.degraded_error(),
                },
            )
        }));
        spaces.sort_by_key(|(position, _)| *position);
        spaces.into_iter().map(|(_, summary)| summary).collect()
    }

    pub(super) fn space_placement(&self, space_id: SpaceId) -> Option<SpaceMuxOverride> {
        let scope = self.selected_binding_scope(space_id)?;
        self.binding(scope).map(|binding| binding.placement.clone())
    }

    pub(super) fn binding_scopes(&self) -> impl Iterator<Item = MuxScope> + '_ {
        self.all_bindings().map(|binding| binding.scope)
    }

    pub(super) fn binding_placement(&self, scope: MuxScope) -> Option<&SpaceMuxOverride> {
        self.binding(scope).map(BindingRuntime::placement)
    }

    pub(super) fn transition(&self, now: Instant) -> Option<(SpaceId, SpaceId, f32)> {
        let transition = self.space_transition?;
        let progress = transition.progress_at(now);
        (progress < 1.0).then_some((transition.from, transition.to, progress))
    }

    pub(super) fn space_backend(&self, space_id: SpaceId) -> Option<MultiplexerBackendConfig> {
        self.space(space_id)
            .map(|space| space.binding.multiplexer.backend)
    }

    pub(super) fn binding_backend(&self, scope: MuxScope) -> Option<MultiplexerBackendConfig> {
        if scope == self.active.binding.scope {
            return Some(self.active.binding.multiplexer.backend);
        }
        self.active
            .inactive_bindings
            .iter()
            .find(|binding| binding.scope == scope)
            .map(|binding| binding.multiplexer.backend)
    }

    pub(super) fn activate_binding(
        &mut self,
        scope: MuxScope,
        config: &BoottyConfig,
        variant: AppearanceVariant,
        repaint: &RepaintHandle,
    ) -> bool {
        if scope == self.active.binding.scope {
            return false;
        }
        let Some(index) = self
            .active
            .inactive_bindings
            .iter()
            .position(|binding| binding.scope == scope)
        else {
            return false;
        };

        let mut target = self.active.inactive_bindings.remove(index);
        self.active.binding.discard_terminal_side_effects();
        target.discard_terminal_side_effects();
        if let Some(owner) = &mut self.parked_native_terminal {
            owner.discard_side_effects();
        }
        self.prepare_terminal_residency_transition(&mut target, config, variant, repaint);
        let current = std::mem::replace(&mut self.active.binding, target);
        self.active.inactive_bindings.insert(index, current);

        self.resume_active_binding(repaint);
        true
    }

    /// Bring the freshly activated binding's multiplexer back in step with its persisted state.
    fn resume_active_binding(&mut self, repaint: &RepaintHandle) {
        if self.active.binding.session_order.session_names().is_empty() {
            return;
        }
        self.active.binding.mux.refresh_on_next_frame();
        let refresh = self.active.binding.mux.refresh_sessions(
            repaint,
            &self.active.binding.multiplexer.clone(),
            mux_session_refresh_interval(true),
        );
        self.active
            .binding
            .mux
            .apply_session_order(&self.active.binding.session_order.session_names());
        if self.active.binding.backend_policy.persisted_sessions == PersistedSessionPolicy::Immediate
        {
            self.active.binding.persisted_sessions_restored = false;
            self.active
                .binding
                .restore_persisted_sessions(repaint, refresh.applied);
        }
    }

    pub(super) fn activate_space(
        &mut self,
        space_id: SpaceId,
        window_state_key: &str,
        config: &BoottyConfig,
        variant: AppearanceVariant,
        repaint: &RepaintHandle,
        now: Instant,
    ) -> Result<bool, WorkspacePersistenceError> {
        if space_id == self.active.id {
            return Ok(false);
        }
        let Some(index) = self
            .inactive_spaces
            .iter()
            .position(|space| space.id == space_id)
        else {
            return Ok(false);
        };

        self.persist_active_binding_restore_state()?;
        self.repository
            .set_selected_space(window_state_key, space_id)?;

        let mut target = self.inactive_spaces.remove(index);
        self.active.binding.discard_terminal_side_effects();
        for binding in &mut self.active.inactive_bindings {
            binding.discard_terminal_side_effects();
        }
        for binding in target.bindings_mut() {
            binding.discard_terminal_side_effects();
        }
        if let Some(owner) = &mut self.parked_native_terminal {
            owner.discard_side_effects();
        }
        self.prepare_terminal_residency_transition(&mut target.binding, config, variant, repaint);

        let current = std::mem::replace(&mut self.active, target);

        self.resume_active_binding(repaint);

        let previous_space_id = current.id;
        self.inactive_spaces.push(current);
        self.inactive_spaces.sort_by_key(|space| space.position);
        self.space_transition = Some(SpaceTransition {
            from: previous_space_id,
            to: self.active.id,
            started: now,
        });
        Ok(true)
    }

    fn prepare_terminal_residency_transition(
        &mut self,
        target: &mut BindingRuntime,
        config: &BoottyConfig,
        variant: AppearanceVariant,
        repaint: &RepaintHandle,
    ) {
        let active_is_shared = self.active.binding.backend_policy.terminal_residency
            == TerminalResidency::WorkspaceShared;
        let target_is_shared =
            target.backend_policy.terminal_residency == TerminalResidency::WorkspaceShared;

        match (active_is_shared, target_is_shared) {
            (true, true) => {
                std::mem::swap(&mut self.active.binding.terminal, &mut target.terminal);
                std::mem::swap(
                    &mut self.active.binding.terminal_side_effect_tx,
                    &mut target.terminal_side_effect_tx,
                );
                std::mem::swap(
                    &mut self.active.binding.terminal_side_effect_rx,
                    &mut target.terminal_side_effect_rx,
                );
            }
            (true, false) => {
                let mut binding_config = config.clone();
                binding_config.multiplexer = self.active.binding.multiplexer.clone();
                let replacement = NativeTerminalOwner::new(
                    &binding_config,
                    Arc::clone(&self.backends),
                    variant,
                    repaint.clone(),
                );
                let native_terminal =
                    NativeTerminalOwner::replace_binding(&mut self.active.binding, replacement);
                debug_assert!(self.parked_native_terminal.is_none());
                self.parked_native_terminal = Some(native_terminal);
            }
            (false, true) => {
                if let Some(mut native_terminal) = self.parked_native_terminal.take() {
                    native_terminal.swap_with_binding(target);
                }
            }
            (false, false) => {}
        }
    }

    pub(super) fn insert_space(
        &mut self,
        space: &WorkspaceSpace,
        config: &BoottyConfig,
        variant: AppearanceVariant,
        repaint: RepaintHandle,
    ) -> SpaceId {
        let runtime = SpaceRuntime::from_workspace(
            space,
            config,
            Arc::clone(&self.backends),
            variant,
            repaint,
        )
        .expect("a persisted space always has a binding");
        let id = runtime.id;
        self.inactive_spaces.push(runtime);
        self.inactive_spaces.sort_by_key(|space| space.position);
        id
    }

    pub(super) fn create_space(
        &mut self,
        name: &str,
        icon: &str,
        color: [u8; 3],
        tint_sidebar: bool,
        mux: SpaceMuxOverride,
        config: &BoottyConfig,
    ) -> Result<Option<WorkspaceSpace>, WorkspacePersistenceError> {
        self.repository
            .create_space(name, icon, color, tint_sidebar, mux, &config.multiplexer)
    }

    pub(super) fn delete_space(
        &mut self,
        space_id: SpaceId,
    ) -> Result<bool, WorkspacePersistenceError> {
        self.repository.delete_space(space_id)
    }

    pub(super) fn update_space(
        &mut self,
        scope: MuxScope,
        name: &str,
        icon: &str,
        color: [u8; 3],
        tint_sidebar: bool,
        mux: SpaceMuxOverride,
    ) -> Result<bool, WorkspacePersistenceError> {
        let placement_changed = self
            .binding(scope)
            .is_some_and(|binding| binding.placement != mux);
        if placement_changed
            && self
                .repository
                .pending_binding_membership_mutation(scope)?
                .is_some()
        {
            return Err(WorkspacePersistenceError::operation(
                "finish the pending binding membership recovery before changing its backend",
            ));
        }
        let updated = self.repository.update_space_and_binding(
            scope,
            name,
            icon,
            color,
            tint_sidebar,
            mux.clone(),
        )?;
        if updated {
            let space = if self.active.id == scope.space_id() {
                &mut self.active
            } else {
                self.inactive_spaces
                    .iter_mut()
                    .find(|space| space.id == scope.space_id())
                    .expect("the updated Space remains live")
            };
            space.name = name.trim().to_owned();
            space.icon = icon.trim().to_owned();
            space.color = color;
            space.tint_sidebar = tint_sidebar;
            if placement_changed {
                space
                    .bindings_mut()
                    .find(|binding| binding.scope == scope)
                    .expect("the updated binding remains live")
                    .placement = mux;
            }
        }
        Ok(updated)
    }

    pub(super) fn rebuild_binding(
        &mut self,
        scope: MuxScope,
        config: &BoottyConfig,
        variant: AppearanceVariant,
        repaint: RepaintHandle,
    ) {
        let placement = self
            .binding(scope)
            .expect("the binding remains live")
            .placement
            .clone();
        let binding = self.binding_mut(scope).expect("the binding remains live");
        binding.rebuild(config, placement, variant, repaint);
    }

    pub(super) fn rebuild_profile_bindings(
        &mut self,
        config: &BoottyConfig,
        requested_scopes: Option<&HashSet<MuxScope>>,
        variant: AppearanceVariant,
        repaint: RepaintHandle,
    ) -> Result<(), WorkspacePersistenceError> {
        let profile_scopes = self
            .binding_scopes()
            .filter(|scope| requested_scopes.is_none_or(|scopes| scopes.contains(scope)))
            .filter(|scope| {
                self.binding_placement(*scope).is_some_and(|placement| {
                    matches!(placement.remote, SpaceRemoteOverride::Profile(_))
                })
            })
            .collect::<Vec<_>>();
        let mut pending_scopes = HashSet::new();
        for scope in &profile_scopes {
            match self.binding_has_pending_membership_operation(*scope) {
                Ok(true) => {
                    pending_scopes.insert(*scope);
                }
                Ok(false) => {}
                Err(error) => {
                    self.deferred_profile_binding_rebuilds
                        .extend(profile_scopes.iter().copied());
                    return Err(error);
                }
            }
        }
        self.deferred_profile_binding_rebuilds
            .extend(pending_scopes.iter().copied());
        let mut rebuilt_scopes = Vec::new();
        for binding in self.bindings_mut() {
            if !profile_scopes.contains(&binding.scope) || pending_scopes.contains(&binding.scope) {
                continue;
            }
            let placement = binding.placement().clone();
            binding.rebuild(config, placement, variant, repaint.clone());
            rebuilt_scopes.push(binding.scope);
        }
        for scope in rebuilt_scopes {
            self.deferred_profile_binding_rebuilds.remove(&scope);
        }
        Ok(())
    }

    pub(super) fn persist_active_binding_restore_state(
        &mut self,
    ) -> Result<(), WorkspacePersistenceError> {
        let selected_session = self
            .active
            .binding
            .mux
            .selected_session()
            .map(str::to_owned);
        let selected_window = self.active.binding.mux.selected_window().map(str::to_owned);
        self.repository.set_binding_restore_state(
            self.active.binding.scope,
            self.active.binding.mux.last_error().is_some(),
            selected_session.as_deref(),
            selected_window.as_deref(),
        )?;
        Ok(())
    }

    pub(super) fn persist_binding_restore_selection(
        &mut self,
        scope: MuxScope,
        session_id: &str,
        window_id: Option<&str>,
    ) -> Result<(), WorkspacePersistenceError> {
        self.repository
            .set_binding_restore_state(scope, false, Some(session_id), window_id)
    }

    pub(super) fn binding_state_candidate(&self, scope: MuxScope) -> Option<BindingStateCandidate> {
        let binding = self.binding(scope)?;
        Some(BindingStateCandidate {
            scope,
            session_order: binding.session_order.clone(),
            session_names: binding.session_names.clone(),
        })
    }

    pub(super) fn active_binding_state_candidate(&self) -> BindingStateCandidate {
        self.binding_state_candidate(self.active.binding.scope)
            .expect("the active binding has committed workspace state")
    }

    pub(super) fn begin_active_binding_membership_mutation(
        &mut self,
        command: &MuxCommand,
        naming: Option<&PendingGeneratedName>,
    ) -> Result<Option<BindingMembershipMutation>, WorkspacePersistenceError> {
        let mutation = match command {
            MuxCommand::CreateProjectSession { session_id, cwd }
            | MuxCommand::CreateWorktreeSession { session_id, cwd } => {
                Some(BindingMembershipMutation::Create {
                    session_id: session_id.clone(),
                    session_name: session_id.clone(),
                    display_name: naming
                        .map(|naming| naming.display_name.clone())
                        .unwrap_or_else(|| session_id.clone()),
                    explicit: naming.is_none_or(|naming| naming.explicit),
                    cwd: Some(cwd.clone()),
                })
            }
            MuxCommand::RenameSession { session_id, name } => {
                let session = self
                    .active
                    .binding
                    .mux
                    .all_sessions()
                    .iter()
                    .find(|session| session.id == *session_id || session.name == *session_id);
                let old_name = session
                    .map(|session| session.name.clone())
                    .or_else(|| {
                        self.active
                            .binding
                            .session_names
                            .last_observed_name(session_id)
                            .map(str::to_owned)
                    })
                    .or_else(|| {
                        self.active
                            .binding
                            .session_order
                            .session_names()
                            .into_iter()
                            .find(|stored| stored == session_id)
                    })
                    .ok_or_else(|| {
                        WorkspacePersistenceError::operation(format!(
                            "rename session {session_id}: current binding membership is unavailable"
                        ))
                    })?;
                let cwd = self
                    .active
                    .binding
                    .session_names
                    .record(session_id)
                    .map(|record| record.cwd.clone())
                    .or_else(|| session.and_then(|session| session.anchor.cwd.clone()))
                    .or_else(|| naming.map(|naming| naming.cwd.clone()));
                Some(BindingMembershipMutation::Rename {
                    session_id: session_id.clone(),
                    old_name,
                    new_name: name.clone(),
                    display_name: naming
                        .map(|naming| naming.display_name.clone())
                        .unwrap_or_else(|| name.clone()),
                    explicit: naming.is_none_or(|naming| naming.explicit),
                    cwd,
                })
            }
            MuxCommand::DitchSession { session_id } => {
                let old_name = self
                    .active
                    .binding
                    .mux
                    .all_sessions()
                    .iter()
                    .find(|session| session.id == *session_id || session.name == *session_id)
                    .map(|session| session.name.clone())
                    .or_else(|| {
                        self.active
                            .binding
                            .session_names
                            .last_observed_name(session_id)
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| session_id.clone());
                Some(BindingMembershipMutation::Ditch {
                    session_id: session_id.clone(),
                    old_name,
                })
            }
            _ => None,
        };
        if let Some(mutation) = &mutation {
            self.repository
                .begin_binding_membership_mutation(self.active.binding.scope, mutation)?;
            self.active.binding.membership_reconciliation_ready = false;
            self.active
                .binding
                .membership_reconciliation_waiting_for_refresh = false;
        }
        Ok(mutation)
    }

    pub(super) fn commit_active_binding_membership_mutation(
        &mut self,
        mutation: &BindingMembershipMutation,
    ) -> Result<(), WorkspacePersistenceError> {
        let mut candidate = self.active_binding_state_candidate();
        self.repository.commit_binding_membership_mutation(
            candidate.scope,
            mutation,
            &mut candidate.session_order,
            &mut candidate.session_names,
        )?;
        self.binding_mut(candidate.scope)
            .expect("the committed binding remains live")
            .publish_session_state(candidate);
        Ok(())
    }

    pub(super) fn defer_active_binding_membership_reconciliation(&mut self) {
        self.active
            .binding
            .membership_reconciliation_waiting_for_refresh = true;
        self.active.binding.mux.refresh_on_next_frame();
    }

    pub(super) fn reconcile_binding_membership_mutations(
        &mut self,
    ) -> Result<(), WorkspacePersistenceError> {
        let observations = self
            .all_bindings()
            .filter(|binding| binding.membership_reconciliation_ready)
            .map(|binding| {
                (
                    binding.scope,
                    binding
                        .mux
                        .all_sessions()
                        .iter()
                        .map(|session| BackendMembership {
                            id: session.id.clone(),
                            name: session.name.clone(),
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        for (scope, memberships) in observations {
            let mut candidate = self
                .binding_state_candidate(scope)
                .expect("each observed binding remains live");
            let resolution = self.repository.reconcile_binding_membership_mutation(
                scope,
                &memberships,
                &mut candidate.session_order,
                &mut candidate.session_names,
            )?;
            if resolution {
                self.binding_mut(scope)
                    .expect("the reconciled binding remains live")
                    .publish_session_state(candidate);
            }
            let binding = self
                .binding_mut(scope)
                .expect("the checked binding remains live");
            binding.membership_reconciliation_ready = false;
            binding.membership_reconciliation_waiting_for_refresh = false;
        }
        Ok(())
    }

    pub(super) fn binding_has_pending_membership_operation(
        &mut self,
        scope: MuxScope,
    ) -> Result<bool, WorkspacePersistenceError> {
        self.repository
            .pending_binding_membership_mutation(scope)
            .map(|pending| pending.is_some())
    }

    pub(super) fn active_reconciled_binding_state_candidate(&self) -> BindingStateCandidate {
        let mut candidate = self.active_binding_state_candidate();
        self.active.binding.reconcile_session_state(&mut candidate);
        candidate
    }

    pub(super) fn reconcile_binding_states(&mut self) -> Result<(), WorkspacePersistenceError> {
        let candidates = self
            .all_bindings()
            .map(|binding| {
                let mut candidate = BindingStateCandidate {
                    scope: binding.scope,
                    session_order: binding.session_order.clone(),
                    session_names: binding.session_names.clone(),
                };
                binding.reconcile_session_state(&mut candidate);
                candidate
            })
            .collect::<Vec<_>>();
        self.commit_binding_state_candidates(candidates)
    }

    pub(super) fn commit_binding_state_candidate(
        &mut self,
        candidate: BindingStateCandidate,
    ) -> Result<(), WorkspacePersistenceError> {
        self.commit_binding_state_candidates(vec![candidate])
    }

    fn commit_binding_state_candidates(
        &mut self,
        candidates: Vec<BindingStateCandidate>,
    ) -> Result<(), WorkspacePersistenceError> {
        let changed = candidates
            .iter()
            .filter_map(|candidate| {
                let binding = self
                    .binding(candidate.scope)
                    .expect("a candidate binding remains live");
                (binding.session_order != candidate.session_order
                    || binding.session_names != candidate.session_names)
                    .then_some((
                        candidate.scope,
                        candidate.session_order.clone(),
                        candidate.session_names.clone(),
                    ))
            })
            .collect::<Vec<_>>();
        self.repository.commit_binding_states(&changed)?;
        for candidate in candidates {
            self.binding_mut(candidate.scope)
                .expect("a committed binding remains live")
                .publish_session_state(candidate);
        }
        Ok(())
    }

    pub(super) fn all_bindings(&self) -> impl Iterator<Item = &BindingRuntime> {
        self.active
            .bindings()
            .chain(self.inactive_spaces.iter().flat_map(SpaceRuntime::bindings))
    }

    pub(super) fn bindings_mut(&mut self) -> impl Iterator<Item = &mut BindingRuntime> {
        std::iter::once(&mut self.active.binding)
            .chain(self.active.inactive_bindings.iter_mut())
            .chain(
                self.inactive_spaces
                    .iter_mut()
                    .flat_map(SpaceRuntime::bindings_mut),
            )
    }

    fn binding_mut(&mut self, scope: MuxScope) -> Option<&mut BindingRuntime> {
        if self.active.binding.scope == scope {
            return Some(&mut self.active.binding);
        }
        if let Some(binding) = self
            .active
            .inactive_bindings
            .iter_mut()
            .find(|binding| binding.scope == scope)
        {
            return Some(binding);
        }
        self.inactive_spaces
            .iter_mut()
            .flat_map(SpaceRuntime::bindings_mut)
            .find(|binding| binding.scope == scope)
    }

    pub(super) fn binding(&self, scope: MuxScope) -> Option<&BindingRuntime> {
        self.all_bindings().find(|binding| binding.scope == scope)
    }
}
