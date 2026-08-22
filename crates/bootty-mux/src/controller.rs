use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use bootty_mux_model::{MuxBackendKind, MuxBindingConfig};
use serde::{Deserialize, Serialize};

use crate::{
    backend::MuxBackend,
    capability::{BindingOperation, BindingOperationOutcome},
    command::MuxCommand,
    provider::{MuxBackendRegistry, MuxCommandDispatch},
    snapshot::{MuxSession, MuxSnapshot, selection_after_refresh, session_matches},
};

pub type RepaintHandle = Arc<dyn Fn() + Send + Sync + 'static>;

/// How often a focused window polls the backend for session structure. Nothing pushes these
/// changes to us: a session created from a shell, or a pane whose foreground command changed, only
/// shows up on the next poll, so the cadence is what makes the sidebar feel live. It also sets the
/// floor on how often an otherwise idle window repaints, and the session facts a row shows are
/// themselves refreshed every 500ms, so polling faster than that only bought frames.
pub const MUX_SESSION_REFRESH_INTERVAL: Duration = Duration::from_millis(500);
/// The same poll behind an unfocused window. Every poll spawns a backend client process and forces
/// a frame, and nobody is reading the sidebar, so it drops to a cadence that still notices sessions
/// coming and going without paying 4 processes a second to watch them.
pub const MUX_SESSION_REFRESH_INTERVAL_UNFOCUSED: Duration = Duration::from_secs(2);
static NEXT_BINDING_GENERATION: AtomicU64 = AtomicU64::new(1);

fn next_binding_generation() -> u64 {
    NEXT_BINDING_GENERATION.fetch_add(1, Ordering::Relaxed)
}

/// The session-poll cadence a window with this focus state should use.
pub fn mux_session_refresh_interval(focused: bool) -> Duration {
    if focused {
        MUX_SESSION_REFRESH_INTERVAL
    } else {
        MUX_SESSION_REFRESH_INTERVAL_UNFOCUSED
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewMuxSessionRequest {
    pub session_id: String,
    pub cwd: String,
}

type SessionRefreshSnapshot = std::result::Result<(MuxBackendKind, MuxSnapshot), String>;
type SessionRefreshResult = (u64, SessionRefreshSnapshot);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MuxSessionRefreshOutcome {
    pub applied: bool,
    pub error: Option<String>,
}

struct SessionRefreshRequest {
    generation: u64,
    config: MuxBindingConfig,
}

#[derive(Clone, Debug, Default)]
pub struct CommandCancellation(Arc<AtomicU8>);

impl CommandCancellation {
    const PENDING: u8 = 0;
    const STARTED: u8 = 1;
    const CANCELLED: u8 = 2;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) -> bool {
        self.0
            .compare_exchange(
                Self::PENDING,
                Self::CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire) == Self::CANCELLED
    }

    pub fn try_start(&self) -> bool {
        self.0
            .compare_exchange(
                Self::PENDING,
                Self::STARTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MuxCommandError {
    Cancelled,
    DeadlineExceeded,
    Unsupported,
    Unavailable,
    Stale,
    Failed(String),
}

impl std::fmt::Display for MuxCommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("command was cancelled"),
            Self::DeadlineExceeded => formatter.write_str("command deadline expired"),
            Self::Unsupported => formatter.write_str("mux operation is unsupported"),
            Self::Unavailable => formatter.write_str("mux operation is unavailable"),
            Self::Stale => formatter.write_str("mux operation capability is stale"),
            Self::Failed(message) => formatter.write_str(message),
        }
    }
}

pub type MuxCommandResult = std::result::Result<MuxCommandCompletion, MuxCommandError>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MuxCommandCompletion {
    pub selected_session: Option<String>,
    pub selected_window: Option<String>,
    snapshot: Option<(MuxBindingConfig, MuxSnapshot)>,
}

impl MuxCommandCompletion {
    pub fn matches_config(&self, config: &MuxBindingConfig) -> bool {
        self.snapshot
            .as_ref()
            .is_none_or(|(completed_config, _)| completed_config == config)
    }

    fn requested(selected_session: Option<String>, selected_window: Option<String>) -> Self {
        Self {
            selected_session,
            selected_window,
            snapshot: None,
        }
    }

    fn from_snapshot(config: MuxBindingConfig, snapshot: MuxSnapshot) -> Self {
        let selected_session = snapshot.active_session_id.clone().or_else(|| {
            snapshot
                .sessions
                .iter()
                .find(|session| session.active)
                .map(|session| session.id.clone())
        });
        let selected_window = selected_session.as_deref().and_then(|selected| {
            snapshot
                .sessions
                .iter()
                .find(|session| session.id == selected || session.name == selected)
                .and_then(|session| session.active_window_id.clone())
        });
        Self {
            selected_session,
            selected_window,
            snapshot: Some((config, snapshot)),
        }
    }
}
#[derive(Default)]
struct CommandConfigState {
    config: Option<MuxBindingConfig>,
    generation: u64,
}

struct MuxCommandJob {
    scope: Option<MuxScope>,
    config: MuxBindingConfig,
    command: MuxCommand,
    completion: MuxCommandCompletion,
    response: Option<mpsc::Sender<MuxCommandResult>>,
    deadline: Option<Instant>,
    cancellation: Option<CommandCancellation>,
    config_generation: u64,
}

fn execute_backend_command(
    registry: &MuxBackendRegistry,
    backend: &mut dyn MuxBackend,
    config: &MuxBindingConfig,
    scope: Option<MuxScope>,
    command: MuxCommand,
) -> Result<(), MuxCommandError> {
    let Some(scope) = scope else {
        return backend
            .execute(command)
            .map_err(|error| MuxCommandError::Failed(error.to_string()));
    };
    match backend.execute_checked(scope, command) {
        BindingOperationOutcome::Supported(result) => {
            result.map_err(|error| MuxCommandError::Failed(error.to_string()))
        }
        BindingOperationOutcome::Unsupported => Err(MuxCommandError::Unsupported),
        BindingOperationOutcome::Unavailable => Err(MuxCommandError::Unavailable),
        BindingOperationOutcome::Stale => Err(MuxCommandError::Stale),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActiveWindow {
    session_id: String,
    window_id: String,
}

fn selected_window_after_refresh(
    selected_session: Option<&str>,
    current: Option<String>,
    previous_active: Option<&ActiveWindow>,
    snapshot: &MuxSnapshot,
) -> Option<String> {
    let selected_session = selected_session?;
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == selected_session || session.name == selected_session)?;
    let active = session.active_window_id.as_deref();
    let previous_active = previous_active
        .filter(|previous| previous.session_id == session.id)
        .map(|previous| previous.window_id.as_str());
    // Follow an external switch: when tmux's active window moved since the last
    // snapshot, the highlight tracks it (e.g. windows changed from inside tmux).
    // Otherwise keep the current selection, stable across refreshes and during an
    // optimistic local switch that the snapshot hasn't caught up to yet.
    if previous_active.is_some() && active.is_some() && active != previous_active {
        return active.map(str::to_owned);
    }
    current
        .filter(|window_id| session.windows.iter().any(|window| &window.id == window_id))
        .or_else(|| session.active_window_id.clone())
}

fn active_window_of(
    sessions: &[MuxSession],
    selected_session: Option<&str>,
) -> Option<ActiveWindow> {
    let selected_session = selected_session?;
    let session = sessions
        .iter()
        .find(|session| session.id == selected_session || session.name == selected_session)?;
    Some(ActiveWindow {
        session_id: session.id.clone(),
        window_id: session.active_window_id.clone()?,
    })
}

fn selected_window_for_session<'a>(
    session: &'a MuxSession,
    selected_window: Option<&str>,
) -> Option<&'a crate::snapshot::MuxWindow> {
    selected_window
        .and_then(|id| session.windows.iter().find(|window| window.id == id))
        .or_else(|| {
            session
                .active_window_id
                .as_deref()
                .and_then(|id| session.windows.iter().find(|window| window.id == id))
        })
        .or_else(|| session.windows.first())
}

fn optimistic_window_after_command(
    sessions: &[MuxSession],
    selected_window: Option<&str>,
    command: &MuxCommand,
) -> Option<String> {
    let (session_id, step) = match command {
        MuxCommand::ActivateNextWindow { session_id } => (session_id.as_str(), 1_i32),
        MuxCommand::ActivatePreviousWindow { session_id } => (session_id.as_str(), -1_i32),
        MuxCommand::ActivateWindowIndex { session_id, index } => {
            let session = sessions
                .iter()
                .find(|session| session.id == *session_id || session.name == *session_id)?;
            return session
                .windows
                .iter()
                .find(|window| window.index == *index)
                .map(|window| window.id.clone());
        }
        MuxCommand::MoveWindow {
            session_id,
            window_id,
            ..
        } => {
            let session = sessions
                .iter()
                .find(|session| session.id == *session_id || session.name == *session_id)?;
            let current_id = window_id
                .as_deref()
                .or(selected_window)
                .or(session.active_window_id.as_deref())?;
            return session
                .windows
                .iter()
                .any(|window| window.id == current_id)
                .then(|| current_id.to_owned());
        }
        MuxCommand::MoveWindowPreservingSelection {
            session_id,
            selected_window_id,
            ..
        } => {
            let session = sessions
                .iter()
                .find(|session| session.id == *session_id || session.name == *session_id)?;
            return session
                .windows
                .iter()
                .any(|window| window.id == *selected_window_id)
                .then(|| selected_window_id.clone());
        }
        _ => return None,
    };
    let session = sessions
        .iter()
        .find(|session| session.id == session_id || session.name == session_id)?;
    if session.windows.is_empty() {
        return None;
    }
    let current_id = selected_window.or(session.active_window_id.as_deref());
    let current = current_id
        .and_then(|id| session.windows.iter().position(|window| window.id == id))
        .unwrap_or(0);
    let next = (current as i32 + step).rem_euclid(session.windows.len() as i32) as usize;
    Some(session.windows[next].id.clone())
}
fn command_session_id(command: &MuxCommand) -> &str {
    match command {
        MuxCommand::ActivateWindow { session_id, .. }
        | MuxCommand::NewWindow { session_id, .. }
        | MuxCommand::RenameWindow { session_id, .. }
        | MuxCommand::ActivateNextWindow { session_id }
        | MuxCommand::ActivatePreviousWindow { session_id }
        | MuxCommand::ActivateLastWindow { session_id }
        | MuxCommand::ActivateWindowIndex { session_id, .. }
        | MuxCommand::MoveWindow { session_id, .. }
        | MuxCommand::MoveWindowPreservingSelection { session_id, .. }
        | MuxCommand::SplitPane { session_id, .. }
        | MuxCommand::SelectPane { session_id, .. }
        | MuxCommand::SelectNextPane { session_id, .. }
        | MuxCommand::SelectPreviousPane { session_id, .. }
        | MuxCommand::KillPane { session_id, .. }
        | MuxCommand::ClosePane { session_id, .. }
        | MuxCommand::TogglePaneZoom { session_id, .. }
        | MuxCommand::CreateProjectSession { session_id, .. }
        | MuxCommand::CreateWorktreeSession { session_id, .. }
        | MuxCommand::RenameSession { session_id, .. }
        | MuxCommand::DitchSession { session_id } => session_id,
    }
}

fn stable_session_order(
    previous: &[MuxSession],
    mut refreshed: Vec<MuxSession>,
) -> Vec<MuxSession> {
    let mut ordered = Vec::with_capacity(refreshed.len());
    for old in previous {
        if let Some(index) = refreshed
            .iter()
            .position(|session| session.id == old.id || session.name == old.name)
        {
            ordered.push(refreshed.remove(index));
        }
    }
    ordered.extend(refreshed);
    ordered
}

fn order_sessions_by_names(sessions: &[MuxSession], ordered_names: &[String]) -> Vec<MuxSession> {
    let mut remaining = sessions.to_vec();
    let mut ordered = Vec::with_capacity(remaining.len());
    for name in ordered_names {
        if let Some(index) = remaining.iter().position(|session| &session.name == name) {
            ordered.push(remaining.remove(index));
        }
    }
    ordered
}

#[derive(Clone, Copy, Debug, Deserialize, Hash, PartialEq, Eq, Serialize)]
pub struct SpaceId(i64);

impl SpaceId {
    pub fn from_persistence(value: i64) -> Self {
        Self(value)
    }

    pub fn persistence_value(self) -> i64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Hash, PartialEq, Eq, Serialize)]
pub struct BindingId(i64);

impl BindingId {
    pub fn from_persistence(value: i64) -> Self {
        Self(value)
    }

    pub fn persistence_value(self) -> i64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Hash, PartialEq, Eq, Serialize)]
pub struct MuxScope {
    space_id: SpaceId,
    binding_id: BindingId,
}

impl MuxScope {
    pub fn new(space_id: SpaceId, binding_id: BindingId) -> Self {
        Self {
            space_id,
            binding_id,
        }
    }

    pub fn space_id(self) -> SpaceId {
        self.space_id
    }

    pub fn binding_id(self) -> BindingId {
        self.binding_id
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum MuxResourceKey {
    Session(String),
    Window(String, String),
    Pane(String, String, String),
}

impl MuxResourceKey {
    fn generation_in(
        &self,
        generations: &BTreeMap<MuxResourceKey, u64>,
        observed: &BTreeMap<MuxResourceKey, String>,
    ) -> Option<u64> {
        observed
            .contains_key(self)
            .then(|| generations.get(self).copied())
            .flatten()
    }
}

pub struct BindingMuxController {
    controller: MuxController,
    last_error: Option<String>,
    availability_error: Option<String>,
    refresh_completed: bool,
    refresh_failed: bool,
    binding_generation: u64,
    resource_generations: BTreeMap<MuxResourceKey, u64>,
    observed_resources: BTreeMap<MuxResourceKey, String>,
    observed_backend: Option<MuxBackendKind>,
}

#[derive(Clone, Debug)]
enum BindingAvailabilityError {
    Configured(String),
    Runtime(String),
}

impl BindingMuxController {
    pub fn new(scope: MuxScope) -> Self {
        Self {
            controller: MuxController::with_scope(scope),
            last_error: None,
            availability_error: None,
            refresh_completed: false,
            refresh_failed: false,
            binding_generation: next_binding_generation(),
            resource_generations: BTreeMap::new(),
            observed_resources: BTreeMap::new(),
            observed_backend: None,
        }
    }
}

    fn new_unscoped() -> Self {
        Self {
            controller: MuxController::new(),
            last_error: None,
            availability_error: None,
            refresh_completed: false,
            refresh_failed: false,
            binding_generation: next_binding_generation(),
            resource_generations: BTreeMap::new(),
            observed_resources: BTreeMap::new(),
            observed_backend: None,
        }
    }
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn set_error(&mut self, error: Option<String>) {
        self.last_error = error;
    }

    pub fn set_availability_error(&mut self, error: Option<String>) {
        self.availability_error.clone_from(&error);
        self.last_error = error;
    }

    pub fn unavailable_reason(&self) -> Option<&str> {
        self.availability_error.as_deref()
    }

    pub fn take_refresh_completed(&mut self) -> bool {
        std::mem::take(&mut self.refresh_completed)
    }

    pub fn binding_generation(&self) -> u64 {
        self.binding_generation
    }

    pub fn operation_outcome(
        &self,
        config: &MuxBindingConfig,
        operation: BindingOperation,
    ) -> BindingOperationOutcome<()> {
        let Some(scope) = self.controller.scope else {
            return BindingOperationOutcome::Supported(());
        };
        if self.availability_error.is_some() {
            return BindingOperationOutcome::Unavailable;
        }
        if self
            .controller
            .build_backend(config)
            .capabilities(scope)
            .supports(operation)
        {
            BindingOperationOutcome::Supported(())
        } else {
            BindingOperationOutcome::Unsupported
        }
    }

    pub fn session_generation(&self, session_id: &str) -> Option<u64> {
        MuxResourceKey::Session(session_id.to_owned())
            .generation_in(&self.resource_generations, &self.observed_resources)
    }

    pub fn window_generation(&self, session_id: &str, window_id: &str) -> Option<u64> {
        MuxResourceKey::Window(session_id.to_owned(), window_id.to_owned())
            .generation_in(&self.resource_generations, &self.observed_resources)
    }

    pub fn pane_generation(&self, session_id: &str, window_id: &str, pane_id: &str) -> Option<u64> {
        MuxResourceKey::Pane(
            session_id.to_owned(),
            window_id.to_owned(),
            pane_id.to_owned(),
        )
        .generation_in(&self.resource_generations, &self.observed_resources)
    }

    pub fn terminal_generation(
        &self,
        session_id: &str,
        window_id: &str,
        pane_id: &str,
    ) -> Option<u64> {
        self.pane_generation(session_id, window_id, pane_id)
    }

    fn record_resource_snapshot(&mut self) {
        let mut current = BTreeMap::new();
        for session in self.controller.sessions() {
            current.insert(MuxResourceKey::Session(session.id.clone()), String::new());
            for window in &session.windows {
                current.insert(
                    MuxResourceKey::Window(session.id.clone(), window.id.clone()),
                    String::new(),
                );
                for pane in std::iter::once(&window.anchor).chain(&window.panes) {
                    let Some(pane_id) = &pane.pane_id else {
                        continue;
                    };
                    current.insert(
                        MuxResourceKey::Pane(
                            session.id.clone(),
                            window.id.clone(),
                            pane_id.clone(),
                        ),
                        format!("{:?}:{:?}", pane.pane_pid, pane.process),
                    );
                }
            }
        }
        for (key, fingerprint) in &current {
            let reappeared = !self.observed_resources.contains_key(key);
            let occupant_changed = self
                .observed_resources
                .get(key)
                .is_some_and(|previous| previous != fingerprint);
            match self.resource_generations.get_mut(key) {
                Some(generation) if reappeared || occupant_changed => {
                    *generation = generation.saturating_add(1);
                }
                Some(_) => {}
                None => {
                    self.resource_generations.insert(key.clone(), 1);
                }
            }
        }
        self.observed_resources = current;
    }

    pub fn create_project_session(
        &mut self,
        request: NewMuxSessionRequest,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
    ) {
        self.controller
            .create_project_session(request, repaint, config);
        self.record_resource_snapshot();
    }

    pub fn execute_command(
        &mut self,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
        command: MuxCommand,
    ) {
        self.controller.execute_command(repaint, config, command);
        self.record_resource_snapshot();
    }

    pub fn refresh_sessions(
        &mut self,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
    ) -> Option<String> {
        let recovering = self.refresh_failed;
        let error = self.controller.refresh_sessions(repaint, config);
        if let Some(error) = &error {
            self.last_error = Some(error.clone());
            self.refresh_failed = true;
            self.availability_error = Some(error.clone());
        } else if self.controller.take_refresh_completed() {
            let backend = self.controller.current_backend;
            let backend_changed = self
                .observed_backend
                .is_some_and(|observed| Some(observed) != backend);
            if recovering || backend_changed {
                self.binding_generation = self.binding_generation.saturating_add(1);
                self.observed_resources.clear();
            }
            self.observed_backend = backend;
            self.last_error = None;
            self.availability_error = None;
            self.refresh_failed = false;
            self.refresh_completed = true;
            self.record_resource_snapshot();
        }
        error
    }

    pub fn poll_command(&mut self) -> Option<Result<(), String>> {
        let result = self.controller.poll_command();
        if let Some(result) = &result {
            self.last_error = result.as_ref().err().cloned();
            if result.is_ok() {
                self.record_resource_snapshot();
            }
        }
        result
    }

    pub fn complete_authoritative_command(
        &mut self,
        result: MuxCommandResult,
        config: &MuxBindingConfig,
    ) -> MuxCommandResult {
        let result = self
            .controller
            .complete_authoritative_command(result, Some(config));
        self.last_error = result.as_ref().err().map(ToString::to_string);
        if result.is_ok() {
            self.record_resource_snapshot();
        }
        result
    }
}

pub struct MuxController {
    last_error: Option<String>,
    availability_error: Option<BindingAvailabilityError>,
    refresh_failed: bool,
    binding_generation: u64,
    resource_generations: BTreeMap<MuxResourceKey, u64>,
    observed_resources: BTreeMap<MuxResourceKey, String>,
    observed_backend: Option<MuxBackendKind>,
    scope: Option<MuxScope>,
    sessions: Vec<MuxSession>,
    all_sessions: Vec<MuxSession>,
    backend_session_names: Vec<String>,
    selected_session: Option<String>,
    /// A session this binding just asked the backend to create and still expects to see. Selection
    /// falls back to whatever the backend calls active whenever the current one is missing, so
    /// without this the session being created loses focus in the frames before it shows up.
    expected_session: Option<String>,
    previous_selected_session: Option<String>,
    selected_window: Option<String>,
    /// The selected session's active window from the previous snapshot, used to detect window
    /// switches made outside bootty so the highlight follows them.
    last_active_window: Option<ActiveWindow>,
    current_backend: Option<MuxBackendKind>,
    last_session_refresh: Option<Instant>,
    session_refresh_generation: u64,
    session_refresh_tx: Option<mpsc::Sender<SessionRefreshRequest>>,
    session_refresh_rx: Option<mpsc::Receiver<SessionRefreshResult>>,
    session_refresh_pending: bool,
    mux_command_tx: Option<mpsc::Sender<MuxCommandJob>>,
    mux_command_rx: Option<mpsc::Receiver<MuxCommandResult>>,
    backend_factory: Option<BackendFactory>,
    command_config: Arc<Mutex<CommandConfigState>>,
}

impl MuxController {
    pub fn new(
        scope: MuxScope,
        registry: Arc<MuxBackendRegistry>,
        workspace: Option<PathBuf>,
    ) -> Self {
        Self {
            last_error: None,
            availability_error: None,
            refresh_failed: false,
            binding_generation: next_binding_generation(),
            resource_generations: BTreeMap::new(),
            observed_resources: BTreeMap::new(),
            observed_backend: None,
            scope: Some(scope),
            sessions: Vec::new(),
            all_sessions: Vec::new(),
            backend_session_names: Vec::new(),
            selected_session: None,
            expected_session: None,
            previous_selected_session: None,
            selected_window: None,
            last_active_window: None,
            current_backend: None,
            last_session_refresh: None,
            session_refresh_generation: 0,
            session_refresh_tx: None,
            session_refresh_rx: None,
            session_refresh_pending: false,
            mux_command_tx: None,
            mux_command_rx: None,
            registry,
            workspace,
            command_config: Arc::new(Mutex::new(CommandConfigState::default())),
        }
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    fn build_backend(&self, config: &MuxBindingConfig) -> Box<dyn MuxBackend> {
        build_backend_with(self.backend_factory.as_ref(), config)
    }

    fn observe_command_config(&mut self, config: &MuxBindingConfig) -> u64 {
        let mut state = self
            .command_config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.config.as_ref() != Some(config) {
            state.config = Some(config.clone());
            state.generation = state.generation.wrapping_add(1);
        }
        state.generation
    }

    pub fn refresh_on_next_frame(&mut self) {
        self.current_backend = None;
        self.last_session_refresh = None;
    }

    pub fn sessions(&self) -> &[MuxSession] {
        &self.sessions
    }

    pub fn all_sessions(&self) -> &[MuxSession] {
        if self.all_sessions.is_empty() {
            &self.sessions
        } else {
            &self.all_sessions
        }
    }

    pub fn session_by_id_or_name(&self, key: &str) -> Option<&MuxSession> {
        self.sessions()
            .iter()
            .find(|session| session_matches(session, key))
    }

    pub fn backend_session_by_id_or_name(&self, key: &str) -> Option<&MuxSession> {
        self.all_sessions()
            .iter()
            .find(|session| session_matches(session, key))
    }

    pub fn backend_session_names(&self) -> &[String] {
        &self.backend_session_names
    }

    pub fn selected_session(&self) -> Option<&str> {
        self.selected_session.as_deref()
    }

    pub fn restore_selection(&mut self, session_id: String, window_id: Option<String>) {
        self.selected_session = Some(session_id);
        self.selected_window = window_id;
    }

    pub fn previous_selected_session(&self) -> Option<&str> {
        let selected = self.previous_selected_session.as_deref()?;
        self.sessions
            .iter()
            .find(|session| session.id == selected || session.name == selected)
            .map(|session| session.id.as_str())
    }

    pub fn selected_session_anchor(&self) -> Option<&crate::snapshot::MuxPaneAnchor> {
        let selected = self.selected_session.as_deref()?;
        let session = self
            .sessions
            .iter()
            .find(|session| session.id == selected || session.name == selected)?;
        if let Some(window) = selected_window_for_session(session, self.selected_window.as_deref())
        {
            return Some(&window.anchor);
        }
        Some(&session.anchor)
    }

    pub fn selected_session_windows(&self) -> &[crate::snapshot::MuxWindow] {
        let Some(selected) = self.selected_session.as_deref() else {
            return &[];
        };
        self.sessions
            .iter()
            .find(|session| session.id == selected || session.name == selected)
            .map(|session| session.windows.as_slice())
            .unwrap_or_default()
    }

    /// Panes of the selected window (the active window of the selected session unless a specific
    /// window is selected). Native renders these as a split layout; other backends report a single
    /// attach anchor.
    pub fn selected_window_panes(&self) -> &[crate::snapshot::MuxPaneAnchor] {
        let Some(selected) = self.selected_session.as_deref() else {
            return &[];
        };
        let Some(session) = self
            .sessions
            .iter()
            .find(|session| session.id == selected || session.name == selected)
        else {
            return &[];
        };
        selected_window_for_session(session, self.selected_window.as_deref())
            .map(|window| window.panes.as_slice())
            .unwrap_or_default()
    }

    pub fn selected_window_layout(&self) -> Option<&crate::snapshot::MuxPaneLayout> {
        let selected = self.selected_session.as_deref()?;
        let session = self
            .sessions
            .iter()
            .find(|session| session.id == selected || session.name == selected)?;
        selected_window_for_session(session, self.selected_window.as_deref())
            .and_then(|window| window.layout.as_ref())
    }

    pub fn apply_session_order(&mut self, ordered_names: &[String]) {
        self.sessions = order_sessions_by_names(self.all_sessions(), ordered_names);
        if self.sessions.is_empty() {
            return;
        }
        // A session this binding is still waiting on keeps the selection: it belongs to the order
        // already, it is just missing from the backend list the order was applied to.
        if self.selected_session == self.expected_session {
            return;
        }
        if self.selected_session.as_deref().is_none_or(|selected| {
            !self
                .sessions
                .iter()
                .any(|session| session_matches(session, selected))
        }) {
            self.set_selected_session(self.sessions.first().map(|session| session.id.clone()));
            self.selected_window = None;
        }
    }

    pub fn selected_window(&self) -> Option<&str> {
        self.selected_window.as_deref()
    }

    pub fn refresh_sessions(
        &mut self,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
    ) -> Option<String> {
        self.observe_command_config(config);
        while let Some((generation, result)) = self.poll_session_refresh() {
            if generation != self.session_refresh_generation {
                continue;
            }
            match result {
                Ok((backend, snapshot)) => {
                    outcome.applied |= self.apply_refreshed_snapshot(backend, snapshot);
                }
                Err(error) => {
                    outcome.error = Some(error);
                    return outcome;
                }
            }
        }

        if self
            .last_session_refresh
            .is_some_and(|last| last.elapsed() < interval)
        {
            return outcome;
        }

        if backend == MuxBackendKind::Native {
            return self.refresh_native_sessions(config);
        }

        if self.session_refresh_pending {
            return outcome;
        }

        self.ensure_session_refresh_worker(repaint);
        let Some(tx) = &self.session_refresh_tx else {
            outcome.error = Some("mux session refresh worker did not start".to_owned());
            return outcome;
        };
        self.session_refresh_generation = self.session_refresh_generation.wrapping_add(1);
        let request = SessionRefreshRequest {
            generation: self.session_refresh_generation,
            config: config.clone(),
        };
        match tx.send(request) {
            Ok(()) => {
                self.last_session_refresh = Some(Instant::now());
                self.session_refresh_pending = true;
                outcome
            }
            Err(_) => {
                self.session_refresh_tx = None;
                self.session_refresh_rx = None;
                self.session_refresh_pending = false;
                outcome.error = Some("mux session refresh worker stopped".to_owned());
                outcome
            }
        }
    }

    fn refresh_native_sessions(&mut self, config: &MuxBindingConfig) -> Option<String> {
        match self.build_backend(config).snapshot() {
            Ok(snapshot) => {
                self.refresh_completed |=
                    self.apply_refreshed_snapshot(MuxBackendKind::Native, snapshot);
                self.last_session_refresh = Some(Instant::now());
                MuxSessionRefreshOutcome {
                    applied,
                    error: None,
                }
            }
            Err(error) => MuxSessionRefreshOutcome {
                applied: false,
                error: Some(error.to_string()),
            },
        }
    }

    pub fn poll_command(&mut self) -> Option<Result<(), String>> {
        let mut completed = false;
        let mut first_error = None;
        loop {
            let result = match self.mux_command_rx.as_ref().map(|rx| rx.try_recv()) {
                Some(Ok(result)) => result,
                Some(Err(mpsc::TryRecvError::Empty)) => break,
                None => return None,
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.mux_command_tx = None;
                    self.mux_command_rx = None;
                    let result = Some(Err("mux command worker stopped".to_owned()));
                    self.last_error = result
                        .as_ref()
                        .and_then(|result| result.as_ref().err().cloned());
                    return result;
                }
            };
            completed = true;
            if let Err(error) = self.complete_authoritative_command(result, None)
                && first_error.is_none()
            {
                first_error = Some(error.to_string());
            }
        }

        let result = completed.then(|| first_error.map_or(Ok(()), Err));
        if let Some(result) = &result {
            self.last_error = result.as_ref().err().cloned();
            if result.is_ok() {
                self.record_resource_snapshot();
            }
        }
        result
    }

    pub fn complete_authoritative_command(
        &mut self,
        result: MuxCommandResult,
        config: &MuxBindingConfig,
    ) -> MuxCommandResult {
        let result = self.complete_authoritative_command_inner(result, Some(config));
        self.last_error = result.as_ref().err().map(ToString::to_string);
        if result.is_ok() {
            self.record_resource_snapshot();
        }
        result
    }

    fn complete_authoritative_command_inner(
        &mut self,
        result: MuxCommandResult,
        active_config: Option<&MuxBindingConfig>,
    ) -> MuxCommandResult {
        match result {
            Ok(completion) => {
                if let Some((config, snapshot)) = &completion.snapshot {
                    if active_config.is_some_and(|active| active != config) {
                        return Err(MuxCommandError::Stale);
                    }
                    self.apply_snapshot(
                        self.registry.selected_kind(config),
                        snapshot.clone(),
                        completion.selected_session.clone(),
                        completion.selected_window.clone(),
                    );
                } else {
                    match (&completion.selected_session, &completion.selected_window) {
                        (Some(session), Some(window)) => {
                            self.set_selected_session(Some(session.clone()));
                            self.selected_window = Some(window.clone());
                        }
                        (Some(session), None) => self.activate_session(session),
                        (None, Some(window)) => self.selected_window = Some(window.clone()),
                        (None, None) => {}
                    }
                }
                self.last_session_refresh = None;
                self.session_refresh_generation = self.session_refresh_generation.wrapping_add(1);
                self.session_refresh_pending = false;
                Ok(completion)
            }
            Err(error) => {
                self.expected_session = None;
                Err(error)
            }
        }
    }

    fn complete_authoritative_command(
        &mut self,
        result: MuxCommandResult,
        active_config: Option<&MuxBindingConfig>,
    ) -> MuxCommandResult {
        match result {
            Ok(completion) => {
                if let Some((config, snapshot)) = &completion.snapshot {
                    if active_config.is_some_and(|active| active != config) {
                        return Err(MuxCommandError::Stale);
                    }
                    self.apply_snapshot(
                        selected_backend(config),
                        snapshot.clone(),
                        completion.selected_session.clone(),
                        completion.selected_window.clone(),
                    );
                } else {
                    match (&completion.selected_session, &completion.selected_window) {
                        (Some(session), Some(window)) => {
                            self.set_selected_session(Some(session.clone()));
                            self.selected_window = Some(window.clone());
                        }
                        (Some(session), None) => self.activate_session(session),
                        (None, Some(window)) => self.selected_window = Some(window.clone()),
                        (None, None) => {}
                    }
                }
                self.last_session_refresh = None;
                self.session_refresh_generation = self.session_refresh_generation.wrapping_add(1);
                self.session_refresh_pending = false;
                Ok(completion)
            }
            Err(error) => {
                self.expected_session = None;
                Err(error)
            }
        }
    }

    fn set_selected_session(&mut self, session_id: Option<String>) {
        if self.selected_session == session_id {
            return;
        }
        if let Some(current) = self.selected_session.take() {
            self.previous_selected_session = Some(current);
        }
        self.selected_session = session_id;
    }

    /// The selection to keep once `sessions` is the whole truth: the expected session survives even
    /// while the backend has yet to report it, and anything else falls back as usual.
    fn selection_within(
        &self,
        preferred: Option<String>,
        sessions: &[MuxSession],
    ) -> Option<String> {
        if let Some(preferred) = preferred.as_deref()
            && self.expected_session.as_deref() == Some(preferred)
        {
            return Some(preferred.to_owned());
        }
        selection_after_refresh(preferred, sessions)
    }

    /// The backend id behind the current selection. Selection resolves by name or id, and only the
    /// id survives a rename, so commands that rename carry the id.
    fn selected_session_id(&self) -> Option<String> {
        let selected = self.selected_session.as_deref()?;
        Some(
            self.sessions
                .iter()
                .chain(self.all_sessions.iter())
                .find(|session| session_matches(session, selected))
                .map_or_else(|| selected.to_owned(), |session| session.id.clone()),
        )
    }

    pub fn activate_session(&mut self, session_id: &str) {
        if self
            .expected_session
            .as_deref()
            .is_some_and(|expected| expected != session_id)
        {
            self.expected_session = None;
        }
        self.set_selected_session(Some(session_id.to_owned()));
        self.selected_window = None;
    }

    pub fn activate_window(
        &mut self,
        session_id: &str,
        window_id: &str,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
    ) {
        self.set_selected_session(Some(session_id.to_owned()));
        self.selected_window = Some(window_id.to_owned());
        let command = MuxCommand::ActivateWindow {
            session_id: session_id.to_owned(),
            window_id: window_id.to_owned(),
        };
        if self
            .execute_inline_command(
                config,
                command.clone(),
                Some(session_id.to_owned()),
                Some(window_id.to_owned()),
            )
            .is_ok()
        {
            repaint();
            return;
        }
        self.enqueue_command(
            repaint,
            config,
            command,
            MuxCommandCompletion::requested(
                Some(session_id.to_owned()),
                Some(window_id.to_owned()),
            ),
            None,
            None,
        );
    }
    pub fn rename_window(
        &mut self,
        session_id: &str,
        window_id: &str,
        name: String,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
    ) {
        let command = MuxCommand::RenameWindow {
            session_id: session_id.to_owned(),
            window_id: window_id.to_owned(),
            name,
        };
        self.execute_preserving_selection(repaint, config, command);
    }

    pub fn rename_session(
        &mut self,
        session_id: &str,
        name: String,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
    ) {
        // Names change here; ids do not. Pin the selection to its id first so it still resolves once
        // the session answers to the new name, whichever backend applies the rename.
        self.selected_session = self.selected_session_id();
        let command = MuxCommand::RenameSession {
            session_id: session_id.to_owned(),
            name,
        };
        self.execute_preserving_selection(repaint, config, command);
    }

    pub fn ditch_session(
        &mut self,
        session_id: &str,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
    ) {
        self.execute_preserving_selection(
            repaint,
            config,
            MuxCommand::DitchSession {
                session_id: session_id.to_owned(),
            },
        );
    }

    pub fn close_pane(
        &mut self,
        session_id: &str,
        pane_id: Option<&str>,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
    ) {
        self.execute_preserving_selection(
            repaint,
            config,
            MuxCommand::ClosePane {
                session_id: session_id.to_owned(),
                pane_id: pane_id.map(str::to_owned),
            },
        );
    }

    fn execute_preserving_selection(
        &mut self,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
        command: MuxCommand,
    ) {
        if self
            .execute_inline_command(
                config,
                command.clone(),
                self.selected_session.clone(),
                self.selected_window.clone(),
            )
            .is_ok()
        {
            repaint();
            return;
        }
        self.enqueue_command(
            repaint,
            config,
            command,
            MuxCommandCompletion::requested(None, None),
            None,
            None,
        );
    }

    pub fn create_project_session(
        &mut self,
        request: NewMuxSessionRequest,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
    ) {
        if self.availability_error.is_some() {
            return;
        }
        let command = MuxCommand::CreateProjectSession {
            session_id: request.session_id.clone(),
            cwd: request.cwd,
        };
        self.expected_session = Some(request.session_id.clone());
        if self
            .execute_inline_command(
                config,
                command.clone(),
                Some(request.session_id.clone()),
                None,
            )
            .is_ok()
        {
            repaint();
            self.record_resource_snapshot();
            return;
        }
        self.activate_session(&request.session_id);
        self.enqueue_command(
            repaint,
            config,
            command,
            MuxCommandCompletion::requested(Some(request.session_id), None),
            None,
            None,
        );
        self.record_resource_snapshot();
    }

    fn poll_session_refresh(&mut self) -> Option<SessionRefreshResult> {
        let result = match self.session_refresh_rx.as_ref()?.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some((
                self.session_refresh_generation,
                Err("mux session refresh worker stopped".to_owned()),
            )),
        };
        if matches!(result, Some((generation, _)) if generation == self.session_refresh_generation)
        {
            self.session_refresh_pending = false;
        }
        result
    }

    fn ensure_session_refresh_worker(&mut self, repaint: &RepaintHandle) {
        if self.session_refresh_tx.is_some() && self.session_refresh_rx.is_some() {
            return;
        }

        let (request_tx, request_rx) = mpsc::channel::<SessionRefreshRequest>();
        let (result_tx, result_rx) = mpsc::channel::<SessionRefreshResult>();
        let repaint = repaint.clone();
        let registry = Arc::clone(&self.registry);
        let workspace = self.workspace.clone();
        thread::spawn(move || {
            while let Ok(request) = request_rx.recv() {
                let backend_kind = registry.selected_kind(&request.config);
                let result = registry
                    .build_backend(&request.config, workspace.as_deref())
                    .snapshot()
                    .map(|snapshot| (backend_kind, snapshot))
                    .map_err(|error| error.to_string());
                if result_tx.send((request.generation, result)).is_err() {
                    break;
                }
                repaint();
            }
        });
        self.session_refresh_tx = Some(request_tx);
        self.session_refresh_rx = Some(result_rx);
    }

    pub fn execute_command(
        &mut self,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
        command: MuxCommand,
    ) {
        let (selected_session, preferred_window) = self.command_completion(&command);
        if self
            .execute_inline_command(
                config,
                command.clone(),
                selected_session.clone(),
                preferred_window.clone(),
            )
            .is_ok()
        {
            repaint();
            self.record_resource_snapshot();
            return;
        }
        let selected_window = self.apply_optimistic_command_selection(&command);
        self.enqueue_command(
            repaint,
            config,
            command,
            MuxCommandCompletion::requested(selected_session, selected_window),
            None,
            None,
        );
        self.record_resource_snapshot();
    }

    pub fn execute_command_authoritatively(
        &mut self,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
        command: MuxCommand,
        deadline: Instant,
        cancellation: CommandCancellation,
    ) -> mpsc::Receiver<MuxCommandResult> {
        let (response_tx, response_rx) = mpsc::channel();
        let (selected_session, selected_window) = self.command_completion(&command);
        let completion = MuxCommandCompletion::requested(selected_session, selected_window);
        if cancellation.is_cancelled() {
            let _ = response_tx.send(Err(MuxCommandError::Cancelled));
            return response_rx;
        }
        if Instant::now() >= deadline {
            cancellation.cancel();
            let _ = response_tx.send(Err(MuxCommandError::DeadlineExceeded));
            return response_rx;
        }
        if selected_backend(config) == MuxBackendKind::Native && !cancellation.try_start() {
            let _ = response_tx.send(Err(MuxCommandError::Cancelled));
            return response_rx;
        }
        if selected_backend(config) == MuxBackendKind::Native {
            let result = self
                .execute_native_command(
                    config,
                    command,
                    completion.selected_session.clone(),
                    completion.selected_window.clone(),
                )
                .map(|snapshot| MuxCommandCompletion::from_snapshot(config.clone(), snapshot));
            let _ = response_tx.send(result);
            repaint();
            return response_rx;
        }
        self.enqueue_command(
            repaint,
            config,
            command,
            completion,
            Some(response_tx),
            Some((deadline, cancellation)),
        );
        response_rx
    }

    fn command_completion(&self, command: &MuxCommand) -> (Option<String>, Option<String>) {
        (
            Some(command_session_id(command).to_owned()),
            optimistic_window_after_command(
                &self.sessions,
                self.selected_window.as_deref(),
                command,
            ),
        )
    }

    fn execute_native_command(
        &mut self,
        config: &MuxBindingConfig,
        command: MuxCommand,
        preferred_session: Option<String>,
        preferred_window: Option<String>,
    ) -> Result<MuxSnapshot, MuxCommandError> {
        let backend_kind = selected_backend(config);
        if backend_kind != MuxBackendKind::Native {
            return Err(MuxCommandError::Unavailable);
        }
        let backend_kind = self.registry.selected_kind(config);
        let mut backend = self.build_backend(config);
        execute_backend_command(backend.as_mut(), self.scope, command)
            .and_then(|()| {
                backend
                    .snapshot()
                    .map_err(|error| MuxCommandError::Failed(error.to_string()))
            })
            .inspect(|snapshot| {
                self.apply_snapshot(
                    backend_kind,
                    snapshot.clone(),
                    preferred_session,
                    preferred_window,
                );
                self.last_session_refresh = None;
            })
    }

    fn apply_refreshed_snapshot(&mut self, backend: MuxBackendKind, snapshot: MuxSnapshot) -> bool {
        let same_backend = self.current_backend == Some(backend);
        if backend == MuxBackendKind::Rmux
            && !snapshot.sessions.is_empty()
            && !sessions_have_renderable_pane(&snapshot.sessions)
        {
            return false;
        }
        if backend == MuxBackendKind::Rmux
            && same_backend
            && sessions_have_renderable_pane(&self.sessions)
            && !sessions_have_renderable_pane(&snapshot.sessions)
        {
            return false;
        }
        let keep_selection = same_backend || self.current_backend.is_none();
        let current_session = keep_selection
            .then(|| self.selected_session.take())
            .flatten();
        let current_window = keep_selection
            .then(|| self.selected_window.take())
            .flatten();
        self.apply_snapshot(backend, snapshot, current_session, current_window)
    }

    fn apply_snapshot(
        &mut self,
        backend: MuxBackendKind,
        mut snapshot: MuxSnapshot,
        preferred_session: Option<String>,
        preferred_window: Option<String>,
    ) -> bool {
        if !snapshot.disposition.is_authoritative() {
            return false;
        }
        self.backend_session_names = snapshot
            .sessions
            .iter()
            .map(|session| session.name.clone())
            .collect();
        let same_backend = self.current_backend == Some(backend);
        if same_backend {
            snapshot.sessions = stable_session_order(&self.sessions, snapshot.sessions);
        }
        if self.expected_session.as_deref().is_some_and(|expected| {
            snapshot
                .sessions
                .iter()
                .any(|session| session_matches(session, expected))
        }) {
            self.expected_session = None;
        }
        self.set_selected_session(self.selection_within(preferred_session, &snapshot.sessions));
        self.selected_window = selected_window_after_refresh(
            self.selected_session.as_deref(),
            preferred_window,
            self.last_active_window.as_ref(),
            &snapshot,
        );
        self.current_backend = Some(backend);
        self.all_sessions = snapshot.sessions;
        self.sessions = self.all_sessions.clone();
        self.last_active_window =
            active_window_of(&self.sessions, self.selected_session.as_deref());
    }

    fn apply_optimistic_command_selection(&mut self, command: &MuxCommand) -> Option<String> {
        let session_id = command_session_id(command).to_owned();
        let window_id = optimistic_window_after_command(
            &self.sessions,
            self.selected_window.as_deref(),
            command,
        )?;
        self.set_selected_session(Some(session_id));
        self.selected_window = Some(window_id.clone());
        Some(window_id)
    }

    fn ensure_command_worker(&mut self, repaint: &RepaintHandle) {
        if self.mux_command_tx.is_some() && self.mux_command_rx.is_some() {
            return;
        }

        let (request_tx, request_rx) = mpsc::channel::<MuxCommandJob>();
        let (result_tx, result_rx) = mpsc::channel::<MuxCommandResult>();
        let repaint = repaint.clone();
        let factory = self.backend_factory.clone();
        let command_config = Arc::clone(&self.command_config);
        thread::spawn(move || {
            while let Ok(job) = request_rx.recv() {
                let cancellation = job.cancellation.as_ref();
                let state = command_config
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let result = if state.generation != job.config_generation {
                    Err(MuxCommandError::Stale)
                } else if cancellation.is_some_and(CommandCancellation::is_cancelled) {
                    Err(MuxCommandError::Cancelled)
                } else if job
                    .deadline
                    .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    if let Some(cancellation) = cancellation {
                        cancellation.cancel();
                    }
                    Err(MuxCommandError::DeadlineExceeded)
                } else if cancellation.is_some_and(|cancellation| !cancellation.try_start()) {
                    Err(MuxCommandError::Cancelled)
                } else {
                    drop(state);
                    let mut backend = build_backend_with(factory.as_ref(), &job.config);
                    let reconcile_workspace_membership = matches!(
                        job.command,
                        MuxCommand::CreateProjectSession { .. }
                            | MuxCommand::CreateWorktreeSession { .. }
                            | MuxCommand::RenameSession { .. }
                            | MuxCommand::DitchSession { .. }
                    );
                    execute_backend_command(backend.as_mut(), job.scope, job.command).and_then(
                        |()| {
                            if job.response.is_some() || reconcile_workspace_membership {
                                backend
                                    .snapshot()
                                    .map(|snapshot| {
                                        MuxCommandCompletion::from_snapshot(
                                            job.config.clone(),
                                            snapshot,
                                        )
                                    })
                                    .map_err(|error| MuxCommandError::Failed(error.to_string()))
                            } else {
                                Ok(job.completion)
                            }
                        },
                    )
                };
                if let Some(response) = job.response {
                    let _ = response.send(result);
                } else if result_tx.send(result).is_err() {
                    break;
                }
                repaint();
            }
        });
        self.mux_command_tx = Some(request_tx);
        self.mux_command_rx = Some(result_rx);
    }

    fn enqueue_command(
        &mut self,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
        command: MuxCommand,
        completion: MuxCommandCompletion,
        response: Option<mpsc::Sender<MuxCommandResult>>,
        execution: Option<(Instant, CommandCancellation)>,
    ) {
        let (deadline, cancellation) = execution
            .map(|(deadline, cancellation)| (Some(deadline), Some(cancellation)))
            .unwrap_or_default();
        let config_generation = self.observe_command_config(config);
        self.ensure_command_worker(repaint);
        let job = MuxCommandJob {
            scope: self.scope,
            config: config.clone(),
            command,
            completion,
            response,
            deadline,
            cancellation,
            config_generation,
        };
        let Some(tx) = &self.mux_command_tx else {
            return;
        };
        if let Err(error) = tx.send(job) {
            self.mux_command_tx = None;
            self.mux_command_rx = None;
            self.ensure_command_worker(repaint);
            if let Some(tx) = &self.mux_command_tx {
                let _ = tx.send(error.0);
            }
        }
    }
}
