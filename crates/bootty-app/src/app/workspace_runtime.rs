use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use anyhow::Result;
use bootty_config::config::MultiplexerBackendConfig;
use bootty_terminal::terminal_engine::TerminalSideEffectEvent;

use super::state::{SpaceSummary, TerminalProgress};

use crate::{
    config::{AppearanceVariant, BoottyConfig},
    layout::{PaneLayout, SplitDirection},
    mux::{
        RepaintHandle,
        command::MuxCommand,
        config::selected_backend,
        controller::{BindingMuxController, MuxScope, SpaceId},
        snapshot::MuxSession,
        terminal::ActiveTerminal,
    },
    renderer::TerminalWidget,
    session_names::SessionNameStore,
    session_order::SessionOrderStore,
    terminal::TerminalSessionConfig,
    workspace::{
        BackendSessionMembership, BindingMembershipMutation, SpaceMuxOverride, SpaceRemoteOverride,
        WorkspacePersistenceError, WorkspaceRepository, WorkspaceSpace,
    },
};

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
        variant: AppearanceVariant,
        repaint: RepaintHandle,
    ) -> Self {
        let (terminal_side_effect_tx, terminal_side_effect_rx) = mpsc::channel();
        let session_config =
            terminal_session_config_with_side_effects(config, variant, &terminal_side_effect_tx);
        Self {
            terminal: Box::new(ActiveTerminal::new(
                TerminalWidget::initial_geometry(),
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
    backend: MultiplexerBackendConfig,
    refresh_completed: bool,
    daemon_has_sessions: bool,
) -> PersistedSessionRestoreDecision {
    match backend {
        MultiplexerBackendConfig::Native => PersistedSessionRestoreDecision::Restore,
        MultiplexerBackendConfig::Rmux if !refresh_completed => {
            PersistedSessionRestoreDecision::Wait
        }
        MultiplexerBackendConfig::Rmux if daemon_has_sessions => {
            PersistedSessionRestoreDecision::Skip
        }
        MultiplexerBackendConfig::Rmux => PersistedSessionRestoreDecision::Restore,
        MultiplexerBackendConfig::Tmux | MultiplexerBackendConfig::Zellij => {
            PersistedSessionRestoreDecision::Skip
        }
    }
}

pub(super) struct BindingRuntime {
    pub(super) scope: MuxScope,
    pub(super) label: String,
    pub(super) backend_override: Option<MultiplexerBackendConfig>,
    pub(super) remote_override: SpaceRemoteOverride,
    /// Set while this binding's remote attach client is gone and bootty is waiting to start
    /// another. Per binding, not per window: one space's outage is not another's, and a reconnect
    /// pending here must not discard the pane of whichever space is active when it comes due.
    pub(super) reattach: Option<RemoteReattach>,
    /// When this binding's current remote attach client was asked for, so an outage that keeps
    /// ending clients can be told from one connection that lasted and then dropped much later.
    pub(super) remote_attach_started: Option<Instant>,
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
    pub(super) custom_tab_names: HashSet<ScopedWindowId>,
    pub(super) terminal_tab_titles: HashMap<ScopedWindowId, String>,
    pub(super) terminal_progress: HashMap<ScopedPaneId, TerminalProgress>,
    pub(super) unscoped_terminal_progress: Option<TerminalProgress>,
    pub(super) terminal_ports: HashMap<ScopedPaneId, Vec<u16>>,
    pub(super) unscoped_terminal_ports: Vec<u16>,
    pub(super) persisted_sessions_restored: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BindingStateCandidate {
    pub(super) scope: MuxScope,
    pub(super) session_order: SessionOrderStore,
    pub(super) session_names: SessionNameStore,
}

impl BindingRuntime {
    pub(super) fn new_with_backend_override(
        state: BindingStateCandidate,
        config: &BoottyConfig,
        backend_override: Option<MultiplexerBackendConfig>,
        remote_override: SpaceRemoteOverride,
        variant: AppearanceVariant,
        repaint: RepaintHandle,
    ) -> Self {
        let BindingStateCandidate {
            scope,
            session_order,
            session_names,
        } = state;
        let mut config = config.clone();
        let backend_override = match &remote_override {
            SpaceRemoteOverride::Profile(remote) => Some(remote.backend),
            _ => backend_override,
        };
        if let Some(backend) = backend_override {
            config.multiplexer.backend = backend;
        }
        config.multiplexer.remote_space_id = None;
        let remote_error = match &remote_override {
            SpaceRemoteOverride::Inherit => None,
            SpaceRemoteOverride::Local => {
                config.multiplexer.remote = None;
                None
            }
            SpaceRemoteOverride::Profile(remote) => {
                config.multiplexer.remote_space_id = Some(remote.remote_space_id.clone());
                if let Some(profile) = config.ssh_profiles.get(&remote.profile_id) {
                    config.multiplexer.remote = Some(profile.to_remote());
                    None
                } else {
                    config.multiplexer.remote = None;
                    Some(format!(
                        "SSH profile '{}' is unavailable",
                        remote.profile_id
                    ))
                }
            }
            SpaceRemoteOverride::Inline(remote) => {
                config.multiplexer.remote = Some(remote.clone());
                None
            }
        };
        if !config.multiplexer.backend.supports_remote() {
            config.multiplexer.remote = None;
        }
        let NativeTerminalOwner {
            terminal,
            terminal_side_effect_tx,
            terminal_side_effect_rx,
        } = NativeTerminalOwner::new(&config, variant, repaint);
        let mut mux = BindingMuxController::new(scope);
        // Bindings of one workspace share native sessions, separate workspaces cannot see each
        // other's, and reopening a window keeps its own. Native sessions live in this process rather
        // than in a server, so which state a binding reaches is a choice bootty has to make.
        let workspace = config.config_path.clone();
        let unavailable = remote_error.clone();
        mux.set_backend_factory(Arc::new(move |multiplexer| {
            if let Some(message) = &unavailable {
                return bootty_mux::config::unavailable_backend(message.clone());
            }
            bootty_mux::config::build_backend_for_workspace(multiplexer, Some(&workspace))
        }));
        let mut binding = Self {
            label: binding_label(scope, &config.multiplexer),
            backend_override,
            remote_override,
            reattach: None,
            remote_attach_started: None,
            multiplexer: config.multiplexer.clone(),
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
            custom_tab_names: HashSet::new(),
            terminal_tab_titles: HashMap::new(),
            terminal_progress: HashMap::new(),
            terminal_ports: HashMap::new(),
            unscoped_terminal_ports: Vec::new(),
            unscoped_terminal_progress: None,
            persisted_sessions_restored: false,
        };
        if let Some(error) = remote_error {
            binding.mux.set_availability_error(Some(error));
        }
        binding
    }

    pub(super) fn restore_persisted_sessions(&mut self, repaint: &RepaintHandle) {
        if self.persisted_sessions_restored {
            return;
        }
        let decision = persisted_session_restore_decision(
            selected_backend(&self.multiplexer),
            self.mux.take_refresh_completed(),
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

    pub(super) fn resolve_empty_remote_after_attach_exit(
        &mut self,
        refresh_completed: bool,
    ) -> bool {
        if !refresh_completed || self.reattach.is_none() || !self.mux.sessions().is_empty() {
            return false;
        }
        self.reattach = None;
        self.remote_attach_started = None;
        self.mux.set_availability_error(None);
        true
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

    pub(super) fn degraded_error(&self) -> Option<String> {
        self.mux.last_error().map(str::to_owned).or_else(|| {
            self.reattach
                .map(|reattach| format!("reconnecting (attempt {})", reattach.attempts))
        })
    }
}

pub(super) fn binding_runtime_for_multiplexer(
    config: &BoottyConfig,
    state: BindingStateCandidate,
    label: String,
    backend_override: Option<MultiplexerBackendConfig>,
    remote_override: SpaceRemoteOverride,
    variant: AppearanceVariant,
    repaint: RepaintHandle,
) -> BindingRuntime {
    let mut binding = BindingRuntime::new_with_backend_override(
        state,
        config,
        backend_override,
        remote_override,
        variant,
        repaint.clone(),
    );
    binding.label = label;
    binding.restore_persisted_sessions(&repaint);
    binding
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
        variant: AppearanceVariant,
        repaint: RepaintHandle,
    ) -> Option<Self> {
        let mut bindings = space
            .bindings()
            .iter()
            .map(|workspace_binding| {
                let mut runtime = binding_runtime_for_multiplexer(
                    config,
                    BindingStateCandidate {
                        scope: workspace_binding.mux_scope(),
                        session_order: workspace_binding.session_order().clone(),
                        session_names: workspace_binding.session_names().clone(),
                    },
                    workspace_binding.name().to_owned(),
                    workspace_binding.backend_override(),
                    workspace_binding.remote_override().clone(),
                    variant,
                    repaint.clone(),
                );
                if workspace_binding.unavailable() {
                    runtime.mux.set_availability_error(Some(
                        "binding unavailable; reconnect to restore it".to_owned(),
                    ));
                }
                if let Some(selection) = workspace_binding.selection() {
                    runtime.mux.restore_selection(
                        selection.session_id().to_owned(),
                        selection.window_id().map(str::to_owned),
                    );
                }
                runtime
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

/// A remote binding's attach client is gone and bootty is waiting to start the next one.
///
/// The sessions themselves live on the other host and outlive the connection, so a lost link is
/// reconnected to rather than treated as the pane ending. Attempts back off, because the same loss
/// that ends one client usually ends the next few too, and each attempt is a fresh SSH handshake.
#[derive(Clone, Copy, Debug)]
pub(super) struct RemoteReattach {
    pub(super) retry_at: Instant,
    pub(super) attempts: u32,
    /// Set once the waiting is over and a new attach client has been asked for.
    pub(super) started: bool,
}

impl RemoteReattach {
    pub(super) const FIRST_DELAY: Duration = Duration::from_millis(500);
    pub(super) const MAX_DELAY: Duration = Duration::from_secs(30);
    /// How long an attach client has to survive before its connection counts as established. A
    /// client that dies sooner is the same outage continuing, so the backoff keeps growing.
    pub(super) const STABLE_AFTER: Duration = Duration::from_secs(5);

    pub(super) fn after_failure(
        previous: Option<Self>,
        attached_for: Option<Duration>,
        now: Instant,
    ) -> Self {
        let established = attached_for.is_some_and(|elapsed| elapsed >= Self::STABLE_AFTER);
        let attempts = match previous {
            Some(previous) if !established => previous.attempts.saturating_add(1),
            _ => 1,
        };
        Self {
            retry_at: now + Self::delay(attempts),
            attempts,
            started: false,
        }
    }

    pub(super) fn due(self, now: Instant) -> bool {
        !self.started && now >= self.retry_at
    }

    pub(super) fn delay(attempts: u32) -> Duration {
        Self::FIRST_DELAY
            .saturating_mul(1u32 << attempts.saturating_sub(1).min(8))
            .min(Self::MAX_DELAY)
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

pub(super) struct WorkspaceRuntime {
    repository: WorkspaceRepository,
    pub(super) active: SpaceRuntime,
    pub(super) inactive_spaces: Vec<SpaceRuntime>,
    pub(super) space_transition: Option<SpaceTransition>,
    pub(super) parked_native_terminal: Option<NativeTerminalOwner>,
}

impl WorkspaceRuntime {
    pub(super) fn open(
        config: &BoottyConfig,
        window_state_key: &str,
        variant: AppearanceVariant,
        repaint: RepaintHandle,
    ) -> Result<Self> {
        let (mut repository, snapshot) = WorkspaceRepository::open(&config.config_path)?;
        let selected_space_id = snapshot.selected_space(window_state_key);
        let mut spaces = snapshot
            .spaces()
            .iter()
            .map(|space| {
                let mut runtime =
                    SpaceRuntime::from_workspace(space, config, variant, repaint.clone())
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
            repository,
            active,
            inactive_spaces: spaces,
            space_transition: None,
            parked_native_terminal: None,
        })
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

    pub(super) fn selected_binding_scope(&self, space_id: SpaceId) -> Option<MuxScope> {
        if space_id == self.active.id {
            return Some(self.active.binding.scope);
        }
        self.inactive_spaces
            .iter()
            .find(|space| space.id == space_id)
            .map(|space| space.binding.scope)
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

    pub(super) fn backend_override(
        &self,
        space_id: SpaceId,
    ) -> Option<Option<MultiplexerBackendConfig>> {
        if space_id == self.active.id {
            return Some(self.active.binding.backend_override);
        }
        self.inactive_spaces
            .iter()
            .find(|space| space.id == space_id)
            .map(|space| space.binding.backend_override)
    }

    pub(super) fn remote_override(&self, space_id: SpaceId) -> Option<SpaceRemoteOverride> {
        if space_id == self.active.id {
            return Some(self.active.binding.remote_override.clone());
        }
        self.inactive_spaces
            .iter()
            .find(|space| space.id == space_id)
            .map(|space| space.binding.remote_override.clone())
    }

    pub(super) fn transition(&self, now: Instant) -> Option<(SpaceId, SpaceId, f32)> {
        let transition = self.space_transition?;
        let progress = transition.progress_at(now);
        (progress < 1.0).then_some((transition.from, transition.to, progress))
    }

    pub(super) fn space_backend(&self, space_id: SpaceId) -> Option<MultiplexerBackendConfig> {
        if space_id == self.active.id {
            return Some(self.active.binding.multiplexer.backend);
        }
        self.inactive_spaces
            .iter()
            .find(|space| space.id == space_id)
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
        self.prepare_native_terminal_transition(&mut target, config, variant, repaint);
        let current = std::mem::replace(&mut self.active.binding, target);
        self.active.inactive_bindings.insert(index, current);

        if !self.active.binding.session_order.session_names().is_empty() {
            self.active.binding.mux.refresh_on_next_frame();
            let active_config = self.active.binding.multiplexer.clone();
            let _ = self
                .active
                .binding
                .mux
                .refresh_sessions(repaint, &active_config);
            self.active
                .binding
                .mux
                .apply_session_order(&self.active.binding.session_order.session_names());
            if selected_backend(&active_config) == MultiplexerBackendConfig::Native {
                self.active.binding.persisted_sessions_restored = false;
                self.active.binding.restore_persisted_sessions(repaint);
            }
        }
        true
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
        self.prepare_native_terminal_transition(&mut target.binding, config, variant, repaint);

        let current = std::mem::replace(&mut self.active, target);

        if !self.active.binding.session_order.session_names().is_empty() {
            self.active.binding.mux.refresh_on_next_frame();
            let active_config = self.active.binding.multiplexer.clone();
            let _ = self
                .active
                .binding
                .mux
                .refresh_sessions(repaint, &active_config);
            self.active
                .binding
                .mux
                .apply_session_order(&self.active.binding.session_order.session_names());
            if selected_backend(&active_config) == MultiplexerBackendConfig::Native {
                self.active.binding.persisted_sessions_restored = false;
                self.active.binding.restore_persisted_sessions(repaint);
            }
        }

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

    fn prepare_native_terminal_transition(
        &mut self,
        target: &mut BindingRuntime,
        config: &BoottyConfig,
        variant: AppearanceVariant,
        repaint: &RepaintHandle,
    ) {
        let active_is_native =
            selected_backend(&self.active.binding.multiplexer) == MultiplexerBackendConfig::Native;
        let target_is_native =
            selected_backend(&target.multiplexer) == MultiplexerBackendConfig::Native;

        match (active_is_native, target_is_native) {
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
                let replacement =
                    NativeTerminalOwner::new(&binding_config, variant, repaint.clone());
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
        let runtime = SpaceRuntime::from_workspace(space, config, variant, repaint)
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
        let placement_changed = self.binding(scope).is_some_and(|binding| {
            binding.backend_override != mux.backend || binding.remote_override != mux.remote
        });
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
        self.repository
            .update_space_and_binding(scope, name, icon, color, tint_sidebar, mux)
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
                        .map(|session| BackendSessionMembership {
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
            if resolution.is_some() {
                self.binding_mut(scope)
                    .expect("the reconciled binding remains live")
                    .publish_session_state(candidate);
            }
            self.binding_mut(scope)
                .expect("the checked binding remains live")
                .membership_reconciliation_ready = false;
            self.binding_mut(scope)
                .expect("the checked binding remains live")
                .membership_reconciliation_waiting_for_refresh = false;
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
        let mut candidates = self
            .all_bindings()
            .map(|binding| {
                self.binding_state_candidate(binding.scope)
                    .expect("each live binding has committed workspace state")
            })
            .collect::<Vec<_>>();
        for candidate in &mut candidates {
            self.binding(candidate.scope)
                .expect("candidate binding remains live")
                .reconcile_session_state(candidate);
        }
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

    fn all_bindings(&self) -> impl Iterator<Item = &BindingRuntime> {
        self.active
            .bindings()
            .chain(self.inactive_spaces.iter().flat_map(SpaceRuntime::bindings))
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

pub(super) fn terminal_session_config_with_side_effects(
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
