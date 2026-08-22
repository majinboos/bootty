use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, UdpSocket},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use anyhow::Result;
use bootty_config::config::{AppearanceVariant, BoottyConfig, MultiplexerBackendConfig};
use bootty_mux::{
    RepaintHandle,
    command::MuxCommand,
    controller::{
        MuxCommandError, MuxCommandResult, MuxController, MuxScope, SpaceId,
        mux_session_refresh_interval,
    },
    membership::BackendMembership,
    provider::{
        MuxAppBackendPolicy, MuxBackendRegistry, MuxCommandDispatch, PaneTopology,
        PersistedSessionPolicy, SelectionPublicationPolicy, TerminalResidency,
    },
    snapshot::{MuxSession, MuxSessionTag},
    terminal::ActiveTerminal,
};
use bootty_runtime::terminal_session::DrainStats;
use bootty_terminal::terminal_engine::{TerminalLiveConfig, TerminalSideEffectEvent};

mod binding_panes;
mod binding_session_names;
mod binding_terminal_facts;
mod binding_windows;
mod mux_config;
mod remote_reconnect;
mod space_summary;
mod workspace_sessions;

use self::{
    binding_terminal_facts::BindingTerminalFacts, mux_config::realize_binding,
    remote_reconnect::BindingReconnect,
};

pub(crate) use binding_panes::mux_split_direction;
pub(crate) use binding_session_names::RenameSessionOutcome;
pub(crate) use binding_terminal_facts::{TerminalProgress, TerminalProgressState};
pub(crate) use binding_windows::terminal_cwd_for_mux_command;
pub use space_summary::SpaceSummary;

use crate::{
    layout::{PaneLayout, SplitDirection},
    renderer::TerminalWidget,
    terminal_config::terminal_session_config_with_side_effects,
};
use bootty_workspace::{
    BindingMembershipMutation, SessionMembership, SpaceMuxOverride, SpaceRemoteOverride,
    WorkspaceBinding, WorkspacePersistenceError, WorkspaceRepository, WorkspaceSpace,
};

macro_rules! swap_terminal_owner {
    ($left:expr, $right:expr) => {{
        std::mem::swap(&mut $left.terminal, &mut $right.terminal);
        std::mem::swap(
            &mut $left.terminal_side_effect_tx,
            &mut $right.terminal_side_effect_tx,
        );
        std::mem::swap(
            &mut $left.terminal_side_effect_rx,
            &mut $right.terminal_side_effect_rx,
        );
    }};
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SpaceUpdateOutcome {
    pub(super) changed: bool,
    pub(super) active_placement_changed: bool,
}

#[derive(Clone, Debug)]
pub(super) struct PendingGeneratedName {
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

/// Identity for per-pane terminal facts. Pane ids are unique within a binding, so the enclosing
/// window is not part of the key: a pane that moves between windows keeps its recorded facts, and
/// neither a read nor a write has to search the topology for its window.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(super) struct ScopedPaneId {
    pub(super) scope: MuxScope,
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
        backends: Arc<MuxBackendRegistry>,
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

    pub(super) fn replace_binding(binding: &mut BindingRuntime, mut replacement: Self) -> Self {
        replacement.swap_with_binding(binding);
        replacement
    }

    pub(super) fn swap_with_binding(&mut self, binding: &mut BindingRuntime) {
        swap_terminal_owner!(self, binding);
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
    backends: Arc<MuxBackendRegistry>,
    pub(super) backend_policy: MuxAppBackendPolicy,
    pub(super) scope: MuxScope,
    pub(super) label: String,
    placement: SpaceMuxOverride,
    reconnect: BindingReconnect,
    pub(super) multiplexer: bootty_config::config::MultiplexerConfig,
    /// The Space id stamped onto every session this binding creates. A remote binding uses the
    /// id the far side knows its Space by, since that is what its daemon filters on.
    pub(super) space_tag: String,
    pub(super) terminal: Box<ActiveTerminal>,
    pub(super) mux: MuxController,
    pub(super) sessions: SessionMembership,
    pub(super) pending_generated_names: HashMap<String, PendingGeneratedName>,
    pub(super) membership_reconciliation_ready: bool,
    pub(super) membership_reconciliation_waiting_for_refresh: bool,
    pub(super) generated_names_signature: Option<u64>,
    pub(super) terminal_side_effect_tx: mpsc::Sender<TerminalSideEffectEvent>,
    pub(super) terminal_side_effect_rx: mpsc::Receiver<TerminalSideEffectEvent>,
    pub(super) pane_layouts: HashMap<ScopedWindowId, PaneLayout>,
    pub(super) pending_pane_split_directions: HashMap<ScopedWindowId, SplitDirection>,
    terminal_facts: BindingTerminalFacts,
    pub(super) persisted_sessions_restored: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BindingStateCandidate {
    pub(super) scope: MuxScope,
    pub(super) sessions: SessionMembership,
}

impl BindingRuntime {
    fn new_with_binding_config(
        state: BindingStateCandidate,
        config: &BoottyConfig,
        backends: Arc<MuxBackendRegistry>,
        placement: SpaceMuxOverride,
        realized: mux_config::RealizedMuxBinding,
        variant: AppearanceVariant,
        repaint: RepaintHandle,
    ) -> Self {
        let BindingStateCandidate { scope, sessions } = state;
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
        let mux = MuxController::new(scope, Arc::clone(&backends), Some(workspace));
        let mut binding = Self {
            backends,
            backend_policy,
            label: binding_label(scope, &realized.config),
            placement,
            reconnect: BindingReconnect::default(),
            space_tag: realized.space_tag,
            multiplexer: realized.config,
            scope,
            terminal,
            terminal_side_effect_tx,
            terminal_side_effect_rx,
            mux,
            sessions,
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
        backends: Arc<MuxBackendRegistry>,
        space_tag: String,
        variant: AppearanceVariant,
        repaint: RepaintHandle,
    ) -> Self {
        let placement = SpaceMuxOverride {
            backend: workspace_binding.backend_override(),
            remote: workspace_binding.remote_override().clone(),
        };
        let realized = realize_binding(config, placement.backend, &placement.remote, space_tag);
        let mut binding = Self::new_with_binding_config(
            BindingStateCandidate {
                scope: workspace_binding.mux_scope(),
                sessions: workspace_binding.sessions().clone(),
            },
            config,
            backends,
            placement,
            realized,
            variant,
            repaint.clone(),
        );
        binding.label = workspace_binding.name().to_owned();
        // Last session ended with this binding erroring. Say so, but as a runtime error, not a
        // configured one: a configured error stops `refresh_sessions` from even trying, so a
        // binding that was merely unreachable once could never refresh, never succeed, and never
        // clear the flag. A runtime error clears itself the moment a refresh works.
        if workspace_binding.unavailable() && binding.mux.unavailable_reason().is_none() {
            binding.mux.set_availability_error(Some(
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
            sessions: std::mem::take(&mut self.sessions),
        };
        let pending_generated_names = std::mem::take(&mut self.pending_generated_names);
        let label = self.label.clone();
        let realized = realize_binding(
            config,
            placement.backend,
            &placement.remote,
            self.space_tag.clone(),
        );
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
        //
        // Each session comes back under the identity it had. A backend that does not persist gets
        // a new session either way, but as far as the workspace is concerned it is the same one,
        // so its name, its place in the Space, and its order all survive the restart.
        for session in self.sessions.sessions().to_vec() {
            self.mux.create_project_session(
                bootty_mux::controller::NewMuxSessionRequest {
                    session_id: session.backend_name.clone(),
                    cwd: session.cwd.clone(),
                    tag: MuxSessionTag {
                        identity: Some(session.identity.clone()),
                        space: (!self.space_tag.is_empty()).then(|| self.space_tag.clone()),
                    },
                },
                repaint,
                &self.multiplexer,
            );
        }
        self.mux.apply_session_order(&self.sessions.backend_names());
    }

    /// The stamp for a session this binding is about to create. A fresh identity every time.
    pub(super) fn new_session_tag(&self) -> MuxSessionTag {
        MuxSessionTag {
            identity: Some(bootty_mux::snapshot::new_session_identity()),
            space: (!self.space_tag.is_empty()).then(|| self.space_tag.clone()),
        }
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
                let display_name = session
                    .tag
                    .identity
                    .as_deref()
                    .and_then(|identity| self.sessions.get(identity))
                    .map_or(session.name.as_str(), |claimed| claimed.label());
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

    /// Bring this Space's claims in line with what the backend reports.
    ///
    /// Membership is read rather than maintained: each session says which Space holds it. Returns
    /// the stamps to write back, for claimed sessions that arrived untagged after a server restart.
    fn reconcile_session_state(&self, candidate: &mut BindingStateCandidate) -> Vec<MuxCommand> {
        let backend = self.mux.all_sessions();
        let remote = self.multiplexer.remote.is_some();

        for session in backend {
            let Some(identity) = session.tag.identity.as_deref() else {
                continue;
            };
            if session.tag.space.as_deref() != Some(self.space_tag.as_str()) {
                continue;
            }
            if let Some(claimed) = candidate.sessions.get(identity) {
                // A rename from anywhere lands here and nowhere else. The claim does not move.
                // A name bootty did not ask for is one the user chose somewhere else, so bootty
                // adopts it and stops regenerating a name over the top of it.
                if claimed.backend_name != session.name
                    && !self
                        .pending_generated_names
                        .values()
                        .any(|pending| pending.name == session.name)
                {
                    candidate
                        .sessions
                        .set_display_name(identity, &session.name, true);
                }
                candidate
                    .sessions
                    .observe_backend_name(identity, &session.name);
            } else {
                candidate
                    .sessions
                    .claim(bootty_workspace::WorkspaceSession {
                        identity: identity.to_owned(),
                        backend_name: session.name.clone(),
                        display_name: String::new(),
                        explicit: false,
                        cwd: session
                            .anchor
                            .cwd
                            .as_deref()
                            .map(|cwd| binding_session_names::session_cwd(cwd, remote))
                            .unwrap_or_default(),
                    });
            }
            if let Some(cwd) = session.anchor.cwd.as_deref() {
                candidate
                    .sessions
                    .set_cwd(identity, &binding_session_names::session_cwd(cwd, remote));
            }
        }

        let carried = backend
            .iter()
            .filter_map(|session| session.tag.identity.as_deref())
            .collect::<HashSet<_>>();
        let mut restamps = Vec::new();
        for claimed in candidate.sessions.sessions() {
            if carried.contains(claimed.identity.as_str()) {
                continue;
            }
            // The name is only ever consulted here, and only to re-find a session whose tag the
            // multiplexer lost. It is a hint for recovery, never a key.
            let Some(session) = backend.iter().find(|session| {
                session.tag.identity.is_none() && session.name == claimed.backend_name
            }) else {
                continue;
            };
            restamps.push(MuxCommand::StampSession {
                session_id: session.id.clone(),
                tag: MuxSessionTag {
                    identity: Some(claimed.identity.clone()),
                    space: Some(self.space_tag.clone()),
                },
            });
        }

        // A claim survives while its re-stamp is still in flight; the next pass sees it carried.
        let alive = carried
            .into_iter()
            .map(str::to_owned)
            .chain(restamps.iter().filter_map(|command| match command {
                MuxCommand::StampSession { tag, .. } => tag.identity.clone(),
                _ => None,
            }))
            .collect::<HashSet<_>>();
        candidate
            .sessions
            .retain_alive(&alive.iter().map(String::as_str).collect());
        restamps
    }

    fn publish_session_state(&mut self, candidate: BindingStateCandidate) {
        self.sessions = candidate.sessions;
        self.mux.apply_session_order(&self.sessions.backend_names());
    }

    pub(super) fn discard_terminal_side_effects(&mut self) {
        self.terminal_side_effect_rx.try_iter().for_each(drop);
    }

    pub(super) fn membership_completion_is_immediate(&self) -> bool {
        self.backends.command_dispatch(&self.multiplexer) == MuxCommandDispatch::CallerThread
    }

    fn refresh_waiting_membership(&mut self, repaint: &RepaintHandle, window_focused: bool) {
        if !self.membership_reconciliation_waiting_for_refresh {
            return;
        }
        let refresh = self.mux.refresh_sessions(
            repaint,
            &self.multiplexer.clone(),
            mux_session_refresh_interval(window_focused),
        );
        if refresh.applied {
            self.membership_reconciliation_ready = true;
        }
    }

    pub(super) fn window_id(&self, session_id: String, window_id: String) -> ScopedWindowId {
        ScopedWindowId::new(self.scope, session_id, window_id)
    }

    pub(super) fn pane_id(&self, pane_id: impl Into<String>) -> ScopedPaneId {
        ScopedPaneId {
            scope: self.scope,
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
        backends: Arc<MuxBackendRegistry>,
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
                    space.remote_id().to_owned(),
                    variant,
                    repaint.clone(),
                )
            })
            .collect::<Vec<_>>()
            .into_iter();
        let binding = bindings.next()?;
        Some(Self {
            id: space.id(),
            name: space.name().to_owned(),
            icon: space.icon().to_owned(),
            color: space.color(),
            tint_sidebar: space.tint_sidebar(),
            position: space.position(),
            binding,
            inactive_bindings: bindings.collect(),
        })
    }

    pub(super) fn bindings(&self) -> impl Iterator<Item = &BindingRuntime> {
        std::iter::once(&self.binding).chain(self.inactive_bindings.iter())
    }

    fn bindings_mut(&mut self) -> impl Iterator<Item = &mut BindingRuntime> {
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

fn binding_label(
    scope: MuxScope,
    multiplexer: &bootty_config::config::MultiplexerConfig,
) -> String {
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
    backends: Arc<MuxBackendRegistry>,
    repository: WorkspaceRepository,
    repaint: RepaintHandle,
    network_change_detector: NetworkChangeDetector,
    deferred_profile_binding_rebuilds: HashSet<MuxScope>,
    pub(super) active: SpaceRuntime,
    pub(super) inactive_spaces: Vec<SpaceRuntime>,
    space_transition: Option<SpaceTransition>,
    parked_native_terminal: Option<NativeTerminalOwner>,
}

impl WorkspaceRuntime {
    pub(super) fn spaces(&self) -> impl Iterator<Item = &SpaceRuntime> {
        std::iter::once(&self.active).chain(self.inactive_spaces.iter())
    }

    fn spaces_mut(&mut self) -> impl Iterator<Item = &mut SpaceRuntime> {
        std::iter::once(&mut self.active).chain(self.inactive_spaces.iter_mut())
    }

    pub(super) fn open(
        config: &BoottyConfig,
        window_state_key: &str,
        backends: Arc<MuxBackendRegistry>,
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
            repaint,
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
        for binding in self.bindings_mut().skip(1) {
            binding.terminal.drain_native_window();
            binding.discard_terminal_side_effects();
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
    pub(super) fn publish_terminal_config(
        &mut self,
        config: &BoottyConfig,
        variant: AppearanceVariant,
        live_config: Option<&TerminalLiveConfig>,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        if let Some(owner) = &mut self.parked_native_terminal {
            let mut owner_config = config.clone();
            owner_config.multiplexer.backend = MultiplexerBackendConfig::Native;
            let session_config = terminal_session_config_with_side_effects(
                &owner_config,
                variant,
                &owner.terminal_side_effect_tx,
            );
            owner.terminal.set_terminal_config(session_config);
            if let Some(live_config) = live_config
                && let Err(error) = owner.terminal.apply_live_config(live_config.clone())
            {
                warnings.push(format!(
                    "terminal config publication failed for parked native terminal: {error}"
                ));
            }
        }
        for binding in self.bindings_mut() {
            let mut binding_config = config.clone();
            binding_config.multiplexer = binding.multiplexer.clone();
            let session_config = terminal_session_config_with_side_effects(
                &binding_config,
                variant,
                &binding.terminal_side_effect_tx,
            );
            binding.terminal.set_terminal_config(session_config);
            if let Some(live_config) = live_config
                && let Err(error) = binding.terminal.apply_live_config(live_config.clone())
            {
                warnings.push(format!(
                    "terminal config publication failed for {:?}: {error}",
                    binding.scope
                ));
            }
        }
        warnings
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
        for binding in self.bindings_mut().skip(1) {
            binding.refresh_waiting_membership(repaint, window_focused);
            binding.restore_persisted_sessions(repaint, false);
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
        if let Err(error) = self.reconcile_binding_states(repaint) {
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
        let Some(space) = self.spaces_mut().find(|space| space.id == space_id) else {
            return false;
        };
        let mut restarted = false;
        for binding in space.bindings_mut() {
            restarted |= binding.restart_remote(now);
        }
        restarted
    }

    fn has_degraded_remote(&self) -> bool {
        self.all_bindings().any(BindingRuntime::is_degraded_remote)
    }

    fn reset_remote_reconnects(&mut self, now: Instant) {
        for binding in self.bindings_mut() {
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
        self.spaces().find(|space| space.id == space_id)
    }

    fn space_mut(&mut self, space_id: SpaceId) -> Option<&mut SpaceRuntime> {
        self.spaces_mut().find(|space| space.id == space_id)
    }

    pub(super) fn space_summaries(&self) -> Vec<SpaceSummary> {
        let mut spaces = self
            .spaces()
            .map(|space| {
                (
                    space.position,
                    SpaceSummary {
                        id: space.id,
                        name: space.name.clone(),
                        icon: space.icon.clone(),
                        color: space.color,
                        tint_sidebar: space.tint_sidebar,
                        active: space.id == self.active.id,
                        error: space.binding.degraded_error(),
                    },
                )
            })
            .collect::<Vec<_>>();
        spaces.sort_by_key(|(position, _)| *position);
        spaces.into_iter().map(|(_, summary)| summary).collect()
    }

    pub(super) fn space_placement(&self, space_id: SpaceId) -> Option<SpaceMuxOverride> {
        self.space(space_id)
            .map(|space| space.binding.placement.clone())
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
        self.active
            .bindings()
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
        if self.active.binding.sessions.is_empty() {
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
            .apply_session_order(&self.active.binding.sessions.backend_names());
        if self.active.binding.backend_policy.persisted_sessions
            == PersistedSessionPolicy::Immediate
        {
            self.active.binding.persisted_sessions_restored = false;
            self.active
                .binding
                .restore_persisted_sessions(repaint, refresh.applied);
        }
    }

    pub(super) fn activate_target(
        &mut self,
        scope: MuxScope,
        session_id: &str,
        window_id: Option<&str>,
        repaint: &RepaintHandle,
    ) -> Result<()> {
        debug_assert_eq!(self.active.binding.scope, scope);
        if self.active.binding.backend_policy.selection_publication
            == SelectionPublicationPolicy::PersistBeforePublish
        {
            self.repository
                .set_binding_restore_state(scope, false, Some(session_id), window_id)?;
        }
        let config = self.active.binding.multiplexer.clone();
        match window_id {
            Some(window_id) => self
                .active
                .binding
                .mux
                .activate_window(session_id, window_id, repaint, &config),
            None => self.active.binding.mux.activate_session(session_id),
        }
        Ok(())
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
                swap_terminal_owner!(self.active.binding, target);
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

    #[allow(clippy::too_many_arguments)]
    pub(super) fn create_space(
        &mut self,
        name: &str,
        icon: &str,
        color: [u8; 3],
        tint_sidebar: bool,
        mux: SpaceMuxOverride,
        config: &BoottyConfig,
        variant: AppearanceVariant,
    ) -> Result<Option<SpaceId>, WorkspacePersistenceError> {
        let Some(space) = self.repository.create_space(
            name,
            icon,
            color,
            tint_sidebar,
            mux,
            config.multiplexer.hide_tmux_status,
        )?
        else {
            return Ok(None);
        };
        let runtime = SpaceRuntime::from_workspace(
            &space,
            config,
            Arc::clone(&self.backends),
            variant,
            self.repaint.clone(),
        )
        .expect("a persisted space always has a binding");
        let id = runtime.id;
        self.inactive_spaces.push(runtime);
        self.inactive_spaces.sort_by_key(|space| space.position);
        Ok(Some(id))
    }

    pub(super) fn delete_space(
        &mut self,
        space_id: SpaceId,
    ) -> Result<bool, WorkspacePersistenceError> {
        // Any journal rows go with the Space. Its sessions keep running and stop being claimed,
        // which is what the sidebar shows as unassigned.
        let deleted = self.repository.delete_space(space_id)?;
        if deleted {
            self.inactive_spaces.retain(|space| space.id != space_id);
        }
        Ok(deleted)
    }

    pub(super) fn update_space(
        &mut self,
        summary: &SpaceSummary,
        mux: SpaceMuxOverride,
        config: &BoottyConfig,
        variant: AppearanceVariant,
    ) -> Result<SpaceUpdateOutcome, WorkspacePersistenceError> {
        let Some(scope) = self.space(summary.id).map(|space| space.binding.scope) else {
            return Ok(SpaceUpdateOutcome {
                changed: false,
                active_placement_changed: false,
            });
        };
        let placement_changed = self
            .binding(scope)
            .is_some_and(|binding| binding.placement != mux);
        let updated = self.repository.update_space_and_binding(
            scope,
            &summary.name,
            &summary.icon,
            summary.color,
            summary.tint_sidebar,
            mux.clone(),
        )?;
        if updated {
            let active_placement_changed = self.active.id == scope.space_id() && placement_changed;
            let repaint = self.repaint.clone();
            let space = self
                .space_mut(scope.space_id())
                .expect("the updated Space remains live");
            space.name = summary.name.trim().to_owned();
            space.icon = summary.icon.trim().to_owned();
            space.color = summary.color;
            space.tint_sidebar = summary.tint_sidebar;
            if placement_changed {
                let binding = space
                    .bindings_mut()
                    .find(|binding| binding.scope == scope)
                    .expect("the updated binding remains live");
                binding.rebuild(config, mux, variant, repaint);
            }
            return Ok(SpaceUpdateOutcome {
                changed: true,
                active_placement_changed,
            });
        }
        Ok(SpaceUpdateOutcome {
            changed: updated,
            active_placement_changed: false,
        })
    }

    pub(super) fn rebuild_profile_bindings(
        &mut self,
        config: &BoottyConfig,
        requested_scopes: Option<&HashSet<MuxScope>>,
        variant: AppearanceVariant,
        repaint: RepaintHandle,
    ) -> Result<(), WorkspacePersistenceError> {
        let profile_scopes = self
            .all_bindings()
            .map(|binding| (binding.scope, binding.placement()))
            .filter(|(scope, _)| requested_scopes.is_none_or(|scopes| scopes.contains(scope)))
            .filter(|(_, placement)| matches!(placement.remote, SpaceRemoteOverride::Profile(_)))
            .map(|(scope, _)| scope)
            .collect::<Vec<_>>();
        let mut pending_scopes = HashSet::new();
        for scope in &profile_scopes {
            match self.repository.pending_binding_membership_mutations(*scope) {
                Ok(pending) if !pending.is_empty() => {
                    pending_scopes.insert(*scope);
                }
                Ok(_) => {}
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

    pub(super) fn binding_state_candidate(&self, scope: MuxScope) -> Option<BindingStateCandidate> {
        let binding = self.binding(scope)?;
        Some(BindingStateCandidate {
            scope,
            sessions: binding.sessions.clone(),
        })
    }

    pub(super) fn active_binding_state_candidate(&self) -> BindingStateCandidate {
        self.binding_state_candidate(self.active.binding.scope)
            .expect("the active binding has committed workspace state")
    }

    /// The identity a backend session carries, or `None` when no Space claims it.
    pub(super) fn session_identity(&self, scope: MuxScope, session_id: &str) -> Option<String> {
        self.binding(scope)?
            .mux
            .backend_session_by_id_or_name(session_id)?
            .tag
            .identity
            .clone()
    }

    fn active_session_identity(&self, session_id: &str) -> Option<String> {
        self.session_identity(self.active.binding.scope, session_id)
    }

    pub(super) fn move_active_session(
        &mut self,
        session_id: &str,
        delta: i32,
    ) -> Result<bool, WorkspacePersistenceError> {
        let Some(identity) = self.active_session_identity(session_id) else {
            return Ok(false);
        };
        let mut candidate = self.active_binding_state_candidate();
        if !candidate.sessions.move_by(&identity, delta) {
            return Ok(false);
        }
        self.commit_binding_state_candidate(candidate)
            .map(|()| true)
    }

    pub(super) fn reorder_active_session_before(
        &mut self,
        source: &str,
        before: Option<&str>,
    ) -> Result<bool, WorkspacePersistenceError> {
        let Some(source) = self.active_session_identity(source) else {
            return Ok(false);
        };
        let before = match before {
            Some(before) => match self.active_session_identity(before) {
                Some(identity) => Some(identity),
                None => return Ok(false),
            },
            None => None,
        };
        let mut candidate = self.active_binding_state_candidate();
        if !candidate.sessions.move_before(&source, before.as_deref()) {
            return Ok(false);
        }
        self.commit_binding_state_candidate(candidate)
            .map(|()| true)
    }

    /// Bring a session this Space does not hold into it, minting an identity if it has none.
    pub(super) fn adopt_session_into_binding(
        &mut self,
        scope: MuxScope,
        session_id: &str,
        repaint: &RepaintHandle,
    ) -> Result<bool, WorkspacePersistenceError> {
        let Some(binding) = self.binding(scope) else {
            return Ok(false);
        };
        let Some(session) = binding.mux.backend_session_by_id_or_name(session_id) else {
            return Ok(false);
        };
        let identity = session
            .tag
            .identity
            .clone()
            .unwrap_or_else(bootty_mux::snapshot::new_session_identity);
        let claimed = bootty_workspace::WorkspaceSession {
            identity: identity.clone(),
            backend_name: session.name.clone(),
            display_name: String::new(),
            explicit: false,
            cwd: session.anchor.cwd.clone().unwrap_or_default(),
        };
        let backend_session_id = session.id.clone();
        let space_tag = binding.space_tag.clone();

        let mut candidate = self
            .binding_state_candidate(scope)
            .expect("a live binding has committed workspace state");
        if !candidate.sessions.claim(claimed) {
            return Ok(false);
        }
        self.commit_binding_state_candidate(candidate)?;

        let binding = self
            .binding_mut(scope)
            .expect("a committed binding remains live");
        let config = binding.multiplexer.clone();
        binding.mux.execute_command(
            repaint,
            &config,
            MuxCommand::StampSession {
                session_id: backend_session_id,
                tag: MuxSessionTag {
                    identity: Some(identity),
                    space: (!space_tag.is_empty()).then_some(space_tag),
                },
            },
        );
        Ok(true)
    }

    /// Whether `session_id` can move from `from` into `to`.
    ///
    /// Only within one multiplexer: a session cannot change servers, so a local Space and a remote
    /// one are never reachable from each other.
    pub(super) fn session_move_is_possible(&self, from: MuxScope, to: SpaceId) -> bool {
        let Some(source) = self.binding(from) else {
            return false;
        };
        self.space(to).is_some_and(|space| {
            space.binding.scope != from
                && space.binding.multiplexer.backend == source.multiplexer.backend
                && space.binding.multiplexer.remote == source.multiplexer.remote
        })
    }

    /// Hand a session over to another Space on the same multiplexer.
    ///
    /// The session itself is untouched -- only which Space claims it changes, in both bootty's
    /// record and the tag the multiplexer holds.
    pub(super) fn move_session_to_space(
        &mut self,
        from: MuxScope,
        session_id: &str,
        to: SpaceId,
        repaint: &RepaintHandle,
    ) -> Result<bool, WorkspacePersistenceError> {
        if !self.session_move_is_possible(from, to) {
            return Ok(false);
        }
        let Some(identity) = self.session_identity(from, session_id) else {
            return Ok(false);
        };
        let backend_session_id = self
            .binding(from)
            .and_then(|binding| binding.mux.backend_session_by_id_or_name(session_id))
            .map(|session| session.id.clone());
        let Some(target) = self.space(to).map(|space| space.binding.scope) else {
            return Ok(false);
        };
        let space_tag = self
            .binding(target)
            .map(|binding| binding.space_tag.clone())
            .unwrap_or_default();

        let mut source_state = self
            .binding_state_candidate(from)
            .expect("a live binding has committed workspace state");
        let Some(claimed) = source_state.sessions.release(&identity) else {
            return Ok(false);
        };
        let mut target_state = self
            .binding_state_candidate(target)
            .expect("a live binding has committed workspace state");
        target_state.sessions.claim(claimed);
        self.commit_binding_state_candidates(vec![source_state, target_state])?;

        if let Some(backend_session_id) = backend_session_id
            && let Some(binding) = self.binding_mut(from)
        {
            let config = binding.multiplexer.clone();
            binding.mux.execute_command(
                repaint,
                &config,
                MuxCommand::StampSession {
                    session_id: backend_session_id,
                    tag: MuxSessionTag {
                        identity: Some(identity),
                        space: (!space_tag.is_empty()).then_some(space_tag),
                    },
                },
            );
        }
        Ok(true)
    }

    /// Let go of a session, leaving it running and claimed by nobody.
    pub(super) fn detach_session_from_space(
        &mut self,
        scope: MuxScope,
        session_id: &str,
        repaint: &RepaintHandle,
    ) -> Result<bool, WorkspacePersistenceError> {
        let Some(identity) = self.session_identity(scope, session_id) else {
            return Ok(false);
        };
        let backend_session_id = self
            .binding(scope)
            .and_then(|binding| binding.mux.backend_session_by_id_or_name(session_id))
            .map(|session| session.id.clone());
        let mut candidate = self
            .binding_state_candidate(scope)
            .expect("a live binding has committed workspace state");
        if candidate.sessions.release(&identity).is_none() {
            return Ok(false);
        }
        self.commit_binding_state_candidate(candidate)?;

        // The identity stays on the session so a Space can take it back without minting a new one;
        // only the Space claim is cleared.
        if let Some(backend_session_id) = backend_session_id
            && let Some(binding) = self.binding_mut(scope)
        {
            let config = binding.multiplexer.clone();
            binding.mux.execute_command(
                repaint,
                &config,
                MuxCommand::StampSession {
                    session_id: backend_session_id,
                    tag: MuxSessionTag {
                        identity: Some(identity),
                        space: None,
                    },
                },
            );
        }
        Ok(true)
    }

    /// Journal what bootty is about to ask the backend for, keyed on the session's identity, so
    /// an ambiguous answer is recoverable without guessing from names.
    pub(super) fn begin_active_binding_membership_mutation(
        &mut self,
        command: &MuxCommand,
        naming: Option<&PendingGeneratedName>,
    ) -> Result<Option<BindingMembershipMutation>, WorkspacePersistenceError> {
        let display_name = |fallback: &str| {
            naming
                .map(|naming| naming.display_name.clone())
                .unwrap_or_else(|| fallback.to_owned())
        };
        let mutation = match command {
            MuxCommand::CreateProjectSession {
                session_id,
                cwd,
                tag,
            }
            | MuxCommand::CreateWorktreeSession {
                session_id,
                cwd,
                tag,
            } => tag
                .identity
                .clone()
                .map(|identity| BindingMembershipMutation::Create {
                    identity,
                    session_name: session_id.clone(),
                    display_name: display_name(session_id),
                    explicit: naming.is_none_or(|naming| naming.explicit),
                    cwd: cwd.clone(),
                }),
            MuxCommand::RenameSession { session_id, name } => {
                let identity = self.active_session_identity(session_id).ok_or_else(|| {
                    WorkspacePersistenceError::operation(format!(
                        "rename session {session_id}: this Space does not hold it"
                    ))
                })?;
                let old_name = self
                    .active
                    .binding
                    .sessions
                    .get(&identity)
                    .map(|claimed| claimed.backend_name.clone())
                    .unwrap_or_else(|| session_id.clone());
                Some(BindingMembershipMutation::Rename {
                    identity,
                    old_name,
                    new_name: name.clone(),
                    display_name: display_name(name),
                    explicit: naming.is_none_or(|naming| naming.explicit),
                })
            }
            MuxCommand::DitchSession { session_id } => self
                .active_session_identity(session_id)
                .map(|identity| BindingMembershipMutation::Ditch {
                    old_name: self
                        .active
                        .binding
                        .sessions
                        .get(&identity)
                        .map_or_else(|| session_id.clone(), |c| c.backend_name.clone()),
                    identity,
                }),
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

    pub(super) fn complete_binding_membership_command(
        &mut self,
        scope: MuxScope,
        membership: Option<&BindingMembershipMutation>,
        result: &MuxCommandResult,
    ) -> Result<(), WorkspacePersistenceError> {
        let Some(membership) = membership else {
            return Ok(());
        };
        let committable = result.as_ref().is_ok_and(|completion| {
            self.binding(scope)
                .is_some_and(|binding| completion.matches_config(&binding.multiplexer))
        });
        if !committable {
            self.defer_binding_membership_reconciliation(scope);
            return Ok(());
        }
        let Some(mut candidate) = self.binding_state_candidate(scope) else {
            return Ok(());
        };
        if let Err(error) = self.repository.commit_binding_membership_mutation(
            candidate.scope,
            membership,
            &mut candidate.sessions,
        ) {
            self.defer_binding_membership_reconciliation(scope);
            return Err(error);
        }
        if let Some(binding) = self.binding_mut(candidate.scope) {
            binding.publish_session_state(candidate);
        }
        Ok(())
    }

    pub(super) fn defer_binding_membership_reconciliation(&mut self, scope: MuxScope) {
        if let Some(binding) = self.binding_mut(scope) {
            binding.membership_reconciliation_waiting_for_refresh = true;
            binding.mux.refresh_on_next_frame();
        }
    }

    pub(super) fn complete_authoritative_command(
        &mut self,
        scope: MuxScope,
        result: MuxCommandResult,
    ) -> (MuxCommandResult, Option<String>) {
        let completion = {
            let Some(binding) = self.binding_mut(scope) else {
                return (Err(MuxCommandError::Stale), None);
            };
            let config = binding.multiplexer.clone();
            binding.mux.complete_authoritative_command(result, &config)
        };
        let sync_error = if completion.is_ok()
            && self.active.binding.scope == scope
            && self.active.binding.uses_native_terminal_layout()
        {
            self.sync_active_terminal_panes()
                .err()
                .map(|error| error.to_string())
        } else {
            None
        };
        (completion, sync_error)
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
                            identity: session.tag.identity.clone(),
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        for (scope, memberships) in observations {
            let mut candidate = self
                .binding_state_candidate(scope)
                .expect("each observed binding remains live");
            let resolution = self.repository.reconcile_binding_membership_mutations(
                scope,
                &memberships,
                &mut candidate.sessions,
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

    pub(super) fn active_reconciled_binding_state_candidate(&self) -> BindingStateCandidate {
        let mut candidate = self.active_binding_state_candidate();
        // The re-stamps this pass would ask for are dropped: the caller is committing a naming
        // change, and `reconcile_binding_states` issues them on the next frame anyway.
        self.active.binding.reconcile_session_state(&mut candidate);
        candidate
    }

    pub(super) fn reconcile_binding_states(
        &mut self,
        repaint: &RepaintHandle,
    ) -> Result<(), WorkspacePersistenceError> {
        let mut candidates = Vec::new();
        let mut restamps = Vec::new();
        for binding in self.all_bindings() {
            let mut candidate = BindingStateCandidate {
                scope: binding.scope,
                sessions: binding.sessions.clone(),
            };
            for command in binding.reconcile_session_state(&mut candidate) {
                restamps.push((binding.scope, command));
            }
            candidates.push(candidate);
        }
        self.commit_binding_state_candidates(candidates)?;
        // After the commit: a stamp that fails is retried by the next reconcile, whereas a claim
        // dropped before the stamp landed would have to be rediscovered by name all over again.
        for (scope, command) in restamps {
            let Some(binding) = self.binding_mut(scope) else {
                continue;
            };
            let config = binding.multiplexer.clone();
            binding.mux.execute_command(repaint, &config, command);
        }
        Ok(())
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
                (binding.sessions != candidate.sessions)
                    .then_some((candidate.scope, candidate.sessions.clone()))
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
        self.spaces().flat_map(SpaceRuntime::bindings)
    }

    fn bindings_mut(&mut self) -> impl Iterator<Item = &mut BindingRuntime> {
        self.spaces_mut().flat_map(SpaceRuntime::bindings_mut)
    }

    fn binding_mut(&mut self, scope: MuxScope) -> Option<&mut BindingRuntime> {
        self.bindings_mut().find(|binding| binding.scope == scope)
    }

    pub(super) fn binding(&self, scope: MuxScope) -> Option<&BindingRuntime> {
        self.all_bindings().find(|binding| binding.scope == scope)
    }
}
