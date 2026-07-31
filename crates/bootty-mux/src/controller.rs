use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use bootty_config::config::{MultiplexerBackendConfig, MultiplexerConfig};
use serde::{Deserialize, Serialize};

use crate::{
    RepaintHandle,
    backend::MuxBackend,
    capability::BindingOperationOutcome,
    command::MuxCommand,
    config::{build_backend, selected_backend},
    snapshot::{MuxSession, MuxSnapshot, selection_after_refresh},
};

pub const MUX_SESSION_REFRESH_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewMuxSessionRequest {
    pub session_id: String,
    pub cwd: String,
}

type SessionRefreshSnapshot = std::result::Result<(MultiplexerBackendConfig, MuxSnapshot), String>;
type SessionRefreshResult = (u64, SessionRefreshSnapshot);

struct SessionRefreshRequest {
    generation: u64,
    config: MultiplexerConfig,
}

type MuxCommandResult = std::result::Result<MuxCommandCompletion, String>;

struct MuxCommandCompletion {
    selected_session: Option<String>,
    selected_window: Option<String>,
}

struct MuxCommandJob {
    scope: Option<MuxScope>,
    config: MultiplexerConfig,
    command: MuxCommand,
    completion: MuxCommandCompletion,
}

fn execute_backend_command(
    backend: &mut dyn MuxBackend,
    scope: Option<MuxScope>,
    command: MuxCommand,
) -> Result<(), String> {
    let Some(scope) = scope else {
        return backend.execute(command).map_err(|error| error.to_string());
    };
    match backend.execute_checked(scope, command) {
        BindingOperationOutcome::Supported(result) => result.map_err(|error| error.to_string()),
        BindingOperationOutcome::Unsupported => Err("mux operation is unsupported".to_owned()),
        BindingOperationOutcome::Unavailable => Err("mux operation is unavailable".to_owned()),
        BindingOperationOutcome::Stale => Err("mux operation capability is stale".to_owned()),
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

fn sessions_have_renderable_pane(sessions: &[MuxSession]) -> bool {
    sessions.iter().any(|session| {
        session
            .windows
            .iter()
            .any(|window| !window.panes.is_empty())
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
        | MuxCommand::SelectNextPane { session_id }
        | MuxCommand::SelectPreviousPane { session_id }
        | MuxCommand::KillPane { session_id, .. }
        | MuxCommand::ClosePane { session_id, .. }
        | MuxCommand::TogglePaneZoom { session_id }
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

pub struct BindingMuxController {
    controller: MuxController,
    last_error: Option<String>,
    refresh_completed: bool,
}

impl Default for BindingMuxController {
    fn default() -> Self {
        Self::new_unscoped()
    }
}

impl BindingMuxController {
    pub fn new(scope: MuxScope) -> Self {
        Self {
            controller: MuxController::with_scope(scope),
            last_error: None,
            refresh_completed: false,
        }
    }

    fn new_unscoped() -> Self {
        Self {
            controller: MuxController::new(),
            last_error: None,
            refresh_completed: false,
        }
    }
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn set_error(&mut self, error: Option<String>) {
        self.last_error = error;
    }

    pub fn take_refresh_completed(&mut self) -> bool {
        std::mem::take(&mut self.refresh_completed)
    }

    pub fn refresh_sessions(
        &mut self,
        repaint: &RepaintHandle,
        config: &MultiplexerConfig,
    ) -> Option<String> {
        let error = self.controller.refresh_sessions(repaint, config);
        if let Some(error) = &error {
            self.last_error = Some(error.clone());
        } else if self.controller.take_refresh_completed() {
            self.last_error = None;
            self.refresh_completed = true;
        }
        error
    }

    pub fn poll_command(&mut self) -> Option<Result<(), String>> {
        let result = self.controller.poll_command();
        if let Some(result) = &result {
            self.last_error = result.as_ref().err().cloned();
        }
        result
    }
}

impl std::ops::Deref for BindingMuxController {
    type Target = MuxController;

    fn deref(&self) -> &Self::Target {
        &self.controller
    }
}

impl std::ops::DerefMut for BindingMuxController {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.controller
    }
}

#[derive(Default)]
pub struct MuxController {
    scope: Option<MuxScope>,
    sessions: Vec<MuxSession>,
    all_sessions: Vec<MuxSession>,
    backend_session_names: Vec<String>,
    selected_session: Option<String>,
    previous_selected_session: Option<String>,
    selected_window: Option<String>,
    /// The selected session's active window from the previous snapshot, used to detect window
    /// switches made outside bootty so the highlight follows them.
    last_active_window: Option<ActiveWindow>,
    current_backend: Option<MultiplexerBackendConfig>,
    last_session_refresh: Option<Instant>,
    refresh_completed: bool,
    session_refresh_generation: u64,
    session_refresh_tx: Option<mpsc::Sender<SessionRefreshRequest>>,
    session_refresh_rx: Option<mpsc::Receiver<SessionRefreshResult>>,
    session_refresh_pending: bool,
    mux_command_tx: Option<mpsc::Sender<MuxCommandJob>>,
    mux_command_rx: Option<mpsc::Receiver<MuxCommandResult>>,
}

impl MuxController {
    pub fn new() -> Self {
        Self {
            last_session_refresh: Some(Instant::now() - Duration::from_secs(2)),
            ..Default::default()
        }
    }

    fn with_scope(scope: MuxScope) -> Self {
        Self {
            scope: Some(scope),
            ..Self::new()
        }
    }

    pub fn refresh_on_next_frame(&mut self) {
        self.current_backend = None;
        self.last_session_refresh = Some(Instant::now() - MUX_SESSION_REFRESH_INTERVAL);
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
        if self.selected_session.as_deref().is_none_or(|selected| {
            !self
                .sessions
                .iter()
                .any(|session| session.id == selected || session.name == selected)
        }) {
            self.set_selected_session(self.sessions.first().map(|session| session.id.clone()));
            self.selected_window = None;
        }
    }

    pub fn selected_window(&self) -> Option<&str> {
        self.selected_window.as_deref()
    }

    fn take_refresh_completed(&mut self) -> bool {
        std::mem::take(&mut self.refresh_completed)
    }

    pub fn refresh_sessions(
        &mut self,
        repaint: &RepaintHandle,
        config: &MultiplexerConfig,
    ) -> Option<String> {
        while let Some((generation, result)) = self.poll_session_refresh() {
            if generation != self.session_refresh_generation {
                continue;
            }
            match result {
                Ok((backend, snapshot)) => {
                    self.refresh_completed |= self.apply_refreshed_snapshot(backend, snapshot);
                }
                Err(error) => return Some(error),
            }
        }

        let backend = selected_backend(config);
        if self
            .last_session_refresh
            .is_some_and(|last| last.elapsed() < MUX_SESSION_REFRESH_INTERVAL)
        {
            return None;
        }

        if backend == MultiplexerBackendConfig::Native {
            return self.refresh_native_sessions(config);
        }

        if self.session_refresh_pending {
            return None;
        }

        self.ensure_session_refresh_worker(repaint);
        let Some(tx) = &self.session_refresh_tx else {
            return Some("mux session refresh worker did not start".to_owned());
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
                None
            }
            Err(_) => {
                self.session_refresh_tx = None;
                self.session_refresh_rx = None;
                self.session_refresh_pending = false;
                Some("mux session refresh worker stopped".to_owned())
            }
        }
    }

    fn refresh_native_sessions(&mut self, config: &MultiplexerConfig) -> Option<String> {
        match build_backend(config).snapshot() {
            Ok(snapshot) => {
                self.refresh_completed |=
                    self.apply_refreshed_snapshot(MultiplexerBackendConfig::Native, snapshot);
                self.last_session_refresh = Some(Instant::now());
                None
            }
            Err(error) => Some(error.to_string()),
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
                    return Some(Err("mux command worker stopped".to_owned()));
                }
            };
            completed = true;
            match result {
                Ok(completion) => {
                    match (completion.selected_session, completion.selected_window) {
                        (Some(session), Some(window)) => {
                            self.set_selected_session(Some(session));
                            self.selected_window = Some(window);
                        }
                        (Some(session), None) => self.activate_session(&session),
                        (None, Some(window)) => self.selected_window = Some(window),
                        (None, None) => {}
                    }
                    self.last_session_refresh = Some(Instant::now() - MUX_SESSION_REFRESH_INTERVAL);
                    self.session_refresh_generation =
                        self.session_refresh_generation.wrapping_add(1);
                    self.session_refresh_pending = false;
                }
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }

        completed.then(|| first_error.map_or(Ok(()), Err))
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

    pub fn activate_session(&mut self, session_id: &str) {
        self.set_selected_session(Some(session_id.to_owned()));
        self.selected_window = None;
    }

    pub fn activate_window(
        &mut self,
        session_id: &str,
        window_id: &str,
        repaint: &RepaintHandle,
        config: &MultiplexerConfig,
    ) {
        self.set_selected_session(Some(session_id.to_owned()));
        self.selected_window = Some(window_id.to_owned());
        let command = MuxCommand::ActivateWindow {
            session_id: session_id.to_owned(),
            window_id: window_id.to_owned(),
        };
        if self
            .execute_native_command(
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
            MuxCommandCompletion {
                selected_session: Some(session_id.to_owned()),
                selected_window: Some(window_id.to_owned()),
            },
        );
    }
    pub fn rename_window(
        &mut self,
        session_id: &str,
        window_id: &str,
        name: String,
        repaint: &RepaintHandle,
        config: &MultiplexerConfig,
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
        config: &MultiplexerConfig,
    ) {
        if selected_backend(config) != MultiplexerBackendConfig::Native {
            self.apply_optimistic_session_rename(session_id, &name);
        }
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
        config: &MultiplexerConfig,
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
        config: &MultiplexerConfig,
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
        config: &MultiplexerConfig,
        command: MuxCommand,
    ) {
        if self
            .execute_native_command(
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
            MuxCommandCompletion {
                selected_session: None,
                selected_window: None,
            },
        );
    }

    pub fn create_project_session(
        &mut self,
        request: NewMuxSessionRequest,
        repaint: &RepaintHandle,
        config: &MultiplexerConfig,
    ) {
        let command = MuxCommand::CreateProjectSession {
            session_id: request.session_id.clone(),
            cwd: request.cwd,
        };
        if self
            .execute_native_command(
                config,
                command.clone(),
                Some(request.session_id.clone()),
                None,
            )
            .is_ok()
        {
            repaint();
            return;
        }
        self.activate_session(&request.session_id);
        self.enqueue_command(
            repaint,
            config,
            command,
            MuxCommandCompletion {
                selected_session: Some(request.session_id),
                selected_window: None,
            },
        );
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
        thread::spawn(move || {
            while let Ok(request) = request_rx.recv() {
                let backend_kind = selected_backend(&request.config);
                let result = build_backend(&request.config)
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
        config: &MultiplexerConfig,
        command: MuxCommand,
    ) {
        let selected_session = Some(command_session_id(&command).to_owned());
        let preferred_window = optimistic_window_after_command(
            &self.sessions,
            self.selected_window.as_deref(),
            &command,
        );
        if self
            .execute_native_command(
                config,
                command.clone(),
                selected_session.clone(),
                preferred_window.clone(),
            )
            .is_ok()
        {
            repaint();
            return;
        }
        let selected_window = self.apply_optimistic_command_selection(&command);
        self.enqueue_command(
            repaint,
            config,
            command,
            MuxCommandCompletion {
                selected_session,
                selected_window,
            },
        );
    }

    fn execute_native_command(
        &mut self,
        config: &MultiplexerConfig,
        command: MuxCommand,
        preferred_session: Option<String>,
        preferred_window: Option<String>,
    ) -> Result<(), String> {
        let backend_kind = selected_backend(config);
        if backend_kind != MultiplexerBackendConfig::Native {
            return Err("not synchronous-native".to_owned());
        }
        let mut backend = build_backend(config);
        execute_backend_command(backend.as_mut(), self.scope, command)
            .and_then(|()| backend.snapshot().map_err(|error| error.to_string()))
            .map(|snapshot| {
                self.apply_snapshot(backend_kind, snapshot, preferred_session, preferred_window);
                self.last_session_refresh = Some(Instant::now() - MUX_SESSION_REFRESH_INTERVAL);
            })
    }

    fn apply_refreshed_snapshot(
        &mut self,
        backend: MultiplexerBackendConfig,
        snapshot: MuxSnapshot,
    ) -> bool {
        let same_backend = self.current_backend == Some(backend);
        if backend == MultiplexerBackendConfig::Rmux
            && !snapshot.sessions.is_empty()
            && !sessions_have_renderable_pane(&snapshot.sessions)
        {
            return false;
        }
        if backend == MultiplexerBackendConfig::Rmux
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
        self.apply_snapshot(backend, snapshot, current_session, current_window);
        true
    }

    fn apply_snapshot(
        &mut self,
        backend: MultiplexerBackendConfig,
        mut snapshot: MuxSnapshot,
        preferred_session: Option<String>,
        preferred_window: Option<String>,
    ) {
        self.backend_session_names = snapshot
            .sessions
            .iter()
            .map(|session| session.name.clone())
            .collect();
        let same_backend = self.current_backend == Some(backend);
        if same_backend {
            snapshot.sessions = stable_session_order(&self.sessions, snapshot.sessions);
        }
        self.set_selected_session(selection_after_refresh(preferred_session, &snapshot));
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

    fn apply_optimistic_session_rename(&mut self, session_id: &str, name: &str) {
        let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id || session.name == session_id)
        else {
            return;
        };
        let old_name = std::mem::replace(&mut session.name, name.to_owned());
        if let Some(backend_name) = self
            .backend_session_names
            .iter_mut()
            .find(|backend_name| **backend_name == old_name)
        {
            *backend_name = name.to_owned();
        }
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
        thread::spawn(move || {
            while let Ok(job) = request_rx.recv() {
                let mut backend = build_backend(&job.config);
                let result = execute_backend_command(backend.as_mut(), job.scope, job.command)
                    .map(|()| job.completion);
                if result_tx.send(result).is_err() {
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
        config: &MultiplexerConfig,
        command: MuxCommand,
        completion: MuxCommandCompletion,
    ) {
        self.ensure_command_worker(repaint);
        let job = MuxCommandJob {
            scope: self.scope,
            config: config.clone(),
            command,
            completion,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{MuxPaneAnchor, MuxWindow};

    #[test]
    fn selected_session_anchor_resolves_by_backend_id_or_session_name() {
        let anchor = MuxPaneAnchor {
            session_id: "$7".to_owned(),
            pane_id: Some("%9".to_owned()),
            cwd: None,
            process: None,
        };
        let mut controller = MuxController {
            sessions: vec![MuxSession {
                id: "$7".to_owned(),
                name: "piu".to_owned(),
                active: false,
                anchor: anchor.clone(),
                active_window_id: Some("@2".to_owned()),
                windows: vec![MuxWindow {
                    id: "@2".to_owned(),
                    index: 1,
                    name: "editor".to_owned(),
                    active: true,
                    anchor: MuxPaneAnchor {
                        session_id: "$7".to_owned(),
                        pane_id: Some("%11".to_owned()),
                        cwd: None,
                        process: Some("nvim".to_owned()),
                    },
                    panes: Vec::new(),
                    layout: None,
                    progress: None,
                }],
            }],
            selected_session: Some("piu".to_owned()),
            ..Default::default()
        };

        assert_eq!(
            controller
                .selected_session_anchor()
                .map(|anchor| anchor.session_id.as_str()),
            Some("$7")
        );

        controller.selected_session = Some("$7".to_owned());
        assert_eq!(
            controller
                .selected_session_anchor()
                .and_then(|anchor| anchor.pane_id.as_deref()),
            Some("%11")
        );

        controller.selected_window = Some("@2".to_owned());
        assert_eq!(
            controller
                .selected_session_anchor()
                .and_then(|anchor| anchor.pane_id.as_deref()),
            Some("%11")
        );
    }

    #[test]
    fn selected_session_anchor_falls_back_to_active_window_pane() {
        let mut active = window("@2", 2);
        active.anchor = MuxPaneAnchor {
            session_id: "$1".to_owned(),
            pane_id: Some("%2".to_owned()),
            cwd: None,
            process: Some("fish".to_owned()),
        };
        let mut inactive = window("@1", 1);
        inactive.anchor = MuxPaneAnchor {
            session_id: "$1".to_owned(),
            pane_id: Some("%1".to_owned()),
            cwd: None,
            process: Some("zsh".to_owned()),
        };
        let mut work = session("$1", "work");
        work.active_window_id = Some("@2".to_owned());
        work.windows = vec![inactive, active];
        let mut controller = MuxController {
            sessions: vec![work],
            selected_session: Some("$1".to_owned()),
            ..Default::default()
        };

        assert_eq!(
            controller
                .selected_session_anchor()
                .and_then(|anchor| anchor.pane_id.as_deref()),
            Some("%2")
        );

        controller.selected_window = Some("@missing".to_owned());
        assert_eq!(
            controller
                .selected_session_anchor()
                .and_then(|anchor| anchor.pane_id.as_deref()),
            Some("%2")
        );

        controller.selected_window = Some("@1".to_owned());
        assert_eq!(
            controller
                .selected_session_anchor()
                .and_then(|anchor| anchor.pane_id.as_deref()),
            Some("%1")
        );
    }

    #[test]
    fn stable_session_order_preserves_existing_order_and_appends_new_sessions() {
        let previous = vec![
            session("$2", "work"),
            session("$1", "main"),
            session("$4", "old"),
        ];
        let refreshed = vec![
            session("$1", "main"),
            session("$3", "new"),
            session("$2", "work"),
        ];

        let ordered = stable_session_order(&previous, refreshed);

        assert_eq!(
            ordered
                .iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            vec!["$2", "$1", "$3"]
        );
    }

    #[test]
    fn apply_session_order_filters_sessions_outside_binding_membership() {
        let mut controller = MuxController {
            sessions: vec![
                session("$1", "main"),
                session("$2", "work"),
                session("$3", "new"),
            ],
            all_sessions: vec![
                session("$1", "main"),
                session("$2", "work"),
                session("$3", "new"),
            ],
            selected_session: Some("$3".to_owned()),
            selected_window: Some("@3".to_owned()),
            ..Default::default()
        };

        controller.apply_session_order(&["work".to_owned(), "main".to_owned()]);

        assert_eq!(
            controller
                .sessions()
                .iter()
                .map(|session| session.name.as_str())
                .collect::<Vec<_>>(),
            vec!["work", "main"]
        );
        assert_eq!(controller.selected_session(), Some("$2"));
        assert_eq!(controller.selected_window(), None);

        controller.apply_session_order(&["work".to_owned(), "main".to_owned(), "new".to_owned()]);
        assert_eq!(
            controller
                .sessions()
                .iter()
                .map(|session| session.name.as_str())
                .collect::<Vec<_>>(),
            vec!["work", "main", "new"]
        );
    }

    #[test]
    fn optimistic_session_rename_keeps_external_session_visible() {
        let mut controller = MuxController {
            sessions: vec![session("$1", "old")],
            backend_session_names: vec!["old".to_owned()],
            selected_session: Some("$1".to_owned()),
            ..Default::default()
        };

        controller.apply_optimistic_session_rename("$1", "new");
        controller.apply_session_order(&["new".to_owned()]);

        assert_eq!(controller.sessions()[0].name, "new");
        assert_eq!(controller.backend_session_names(), &["new".to_owned()]);
        assert_eq!(controller.selected_session(), Some("$1"));
    }

    #[test]
    fn renaming_an_inactive_session_keeps_selection_for_native_and_rmux() {
        for backend in [
            MultiplexerBackendConfig::Native,
            MultiplexerBackendConfig::Rmux,
        ] {
            let mut controller = MuxController {
                sessions: vec![session("$1", "first"), session("$2", "selected")],
                backend_session_names: vec!["first".to_owned(), "selected".to_owned()],
                selected_session: Some("$2".to_owned()),
                ..Default::default()
            };
            if backend == MultiplexerBackendConfig::Rmux {
                controller.apply_optimistic_session_rename("$1", "renamed");
            }
            let selected_session = controller.selected_session.clone();
            controller.apply_snapshot(
                backend,
                MuxSnapshot {
                    sessions: vec![session("$1", "renamed"), session("$2", "selected")],
                    active_session_id: Some("$1".to_owned()),
                },
                selected_session,
                None,
            );

            assert_eq!(controller.selected_session(), Some("$2"));
            assert_eq!(controller.sessions()[0].name, "renamed");
        }
    }

    #[test]
    fn activate_session_tracks_previous_bootty_selection() {
        let mut controller = MuxController {
            sessions: vec![session("$1", "main"), session("$2", "work")],
            ..Default::default()
        };

        controller.activate_session("$1");
        controller.activate_session("$2");
        assert_eq!(controller.selected_session(), Some("$2"));
        assert_eq!(controller.previous_selected_session(), Some("$1"));

        controller.activate_session("$1");
        assert_eq!(controller.selected_session(), Some("$1"));
        assert_eq!(controller.previous_selected_session(), Some("$2"));
    }

    #[test]
    fn optimistic_tab_commands_select_known_external_windows() {
        let mut work = session("$1", "work");
        work.windows = vec![window("@1", 1), window("@2", 2), window("@3", 3)];
        work.active_window_id = Some("@1".to_owned());
        let mut controller = MuxController {
            sessions: vec![work],
            selected_session: Some("$1".to_owned()),
            selected_window: Some("@2".to_owned()),
            ..Default::default()
        };

        controller.apply_optimistic_command_selection(&MuxCommand::ActivateNextWindow {
            session_id: "$1".to_owned(),
        });
        assert_eq!(controller.selected_window(), Some("@3"));

        controller.apply_optimistic_command_selection(&MuxCommand::ActivateNextWindow {
            session_id: "$1".to_owned(),
        });
        assert_eq!(controller.selected_window(), Some("@1"));

        controller.apply_optimistic_command_selection(&MuxCommand::ActivatePreviousWindow {
            session_id: "$1".to_owned(),
        });
        assert_eq!(controller.selected_window(), Some("@3"));

        controller.apply_optimistic_command_selection(&MuxCommand::ActivateWindowIndex {
            session_id: "$1".to_owned(),
            index: 2,
        });
        assert_eq!(controller.selected_window(), Some("@2"));

        controller.apply_optimistic_command_selection(&MuxCommand::MoveWindow {
            session_id: "$1".to_owned(),
            window_id: Some("@2".to_owned()),
            delta: -1,
        });
        assert_eq!(controller.selected_window(), Some("@2"));

        controller.apply_optimistic_command_selection(&MuxCommand::MoveWindowPreservingSelection {
            session_id: "$1".to_owned(),
            window_id: "@1".to_owned(),
            delta: 1,
            selected_window_id: "@2".to_owned(),
        });
        assert_eq!(controller.selected_window(), Some("@2"));
    }

    #[test]
    fn targeted_command_completion_preserves_selected_window_until_refresh() {
        let mut work = session("$1", "work");
        work.windows = vec![window("@1", 1), window("@2", 2)];
        work.active_window_id = Some("@1".to_owned());
        let (result_tx, rx) = mpsc::channel();
        result_tx
            .send(Ok(MuxCommandCompletion {
                selected_session: Some("$1".to_owned()),
                selected_window: Some("@2".to_owned()),
            }))
            .expect("send command completion");
        let mut controller = MuxController {
            sessions: vec![work],
            selected_session: Some("$1".to_owned()),
            selected_window: Some("@2".to_owned()),
            mux_command_rx: Some(rx),
            ..Default::default()
        };

        assert_eq!(controller.poll_command(), Some(Ok(())));

        assert_eq!(controller.selected_session(), Some("$1"));
        assert_eq!(controller.selected_window(), Some("@2"));
    }

    #[test]
    fn session_only_command_completion_keeps_session_activation_semantics() {
        let mut work = session("$1", "work");
        work.windows = vec![window("@1", 1), window("@2", 2)];
        work.active_window_id = Some("@1".to_owned());
        let (result_tx, rx) = mpsc::channel();
        result_tx
            .send(Ok(MuxCommandCompletion {
                selected_session: Some("$1".to_owned()),
                selected_window: None,
            }))
            .expect("send command completion");
        let mut controller = MuxController {
            sessions: vec![work],
            selected_session: Some("$1".to_owned()),
            selected_window: Some("@2".to_owned()),
            mux_command_rx: Some(rx),
            ..Default::default()
        };

        assert_eq!(controller.poll_command(), Some(Ok(())));

        assert_eq!(controller.selected_session(), Some("$1"));
        assert_eq!(controller.selected_window(), None);
    }

    #[test]
    fn rename_window_completion_does_not_activate_source_session() {
        let mut work = session("$1", "work");
        work.windows = vec![window("@1", 1), window("@2", 2)];
        work.active_window_id = Some("@1".to_owned());
        let mut agents = session("$2", "agents");
        agents.windows = vec![window("@9", 1)];
        agents.active_window_id = Some("@9".to_owned());
        let (result_tx, rx) = mpsc::channel();
        result_tx
            .send(Ok(MuxCommandCompletion {
                selected_session: None,
                selected_window: None,
            }))
            .expect("send rename completion");
        let mut controller = MuxController {
            sessions: vec![work, agents],
            selected_session: Some("$1".to_owned()),
            selected_window: Some("@2".to_owned()),
            mux_command_rx: Some(rx),
            ..Default::default()
        };

        assert_eq!(controller.poll_command(), Some(Ok(())));

        assert_eq!(controller.selected_session(), Some("$1"));
        assert_eq!(controller.selected_window(), Some("@2"));
    }

    #[test]
    fn inactive_session_ditch_completion_keeps_the_current_selection() {
        let mut work = session("$1", "work");
        work.windows = vec![window("@1", 1), window("@2", 2)];
        work.active_window_id = Some("@1".to_owned());
        let mut agents = session("$2", "agents");
        agents.windows = vec![window("@9", 1)];
        agents.active_window_id = Some("@9".to_owned());
        let (result_tx, rx) = mpsc::channel();
        result_tx
            .send(Ok(MuxCommandCompletion {
                selected_session: None,
                selected_window: None,
            }))
            .expect("send ditch completion");
        let mut controller = MuxController {
            sessions: vec![work, agents],
            selected_session: Some("$1".to_owned()),
            selected_window: Some("@2".to_owned()),
            mux_command_rx: Some(rx),
            ..Default::default()
        };

        assert_eq!(controller.poll_command(), Some(Ok(())));

        assert_eq!(controller.selected_session(), Some("$1"));
        assert_eq!(controller.selected_window(), Some("@2"));
    }

    #[test]
    fn initial_refresh_keeps_restored_session_selection() {
        let mut controller = MuxController::new();
        controller.restore_selection("$2".to_owned(), None);
        controller.apply_session_order(&[]);

        controller.apply_refreshed_snapshot(
            MultiplexerBackendConfig::Tmux,
            MuxSnapshot {
                sessions: vec![session("$1", "first"), session("$2", "last-focused")],
                active_session_id: Some("$1".to_owned()),
            },
        );

        assert_eq!(controller.selected_session(), Some("$2"));
    }

    #[test]
    fn native_refresh_keeps_empty_startup_snapshot_without_worker() {
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let config = MultiplexerConfig {
            backend: bootty_config::config::MultiplexerBackendConfig::Native,
            ..Default::default()
        };
        let mut controller = MuxController::new();

        let error = controller.refresh_sessions(&repaint, &config);

        assert_eq!(error, None);
        assert_eq!(
            controller.current_backend,
            Some(MultiplexerBackendConfig::Native)
        );
        assert!(controller.sessions.is_empty());
        assert!(controller.session_refresh_tx.is_none());
        assert!(controller.session_refresh_rx.is_none());
        assert!(!controller.session_refresh_pending);
    }

    fn session(id: &str, name: &str) -> MuxSession {
        MuxSession {
            id: id.to_owned(),
            name: name.to_owned(),
            active: false,
            anchor: MuxPaneAnchor {
                session_id: id.to_owned(),
                pane_id: None,
                cwd: None,
                process: None,
            },
            active_window_id: None,
            windows: Vec::new(),
        }
    }

    fn window(id: &str, index: u32) -> MuxWindow {
        MuxWindow {
            id: id.to_owned(),
            index,
            name: format!("w{index}"),
            active: false,
            anchor: MuxPaneAnchor::default(),
            panes: Vec::new(),
            layout: None,
            progress: None,
        }
    }

    #[test]
    fn selected_window_follows_external_switch_but_keeps_local_selection() {
        let mut work = session("$1", "work");
        work.windows = vec![window("@1", 0), window("@2", 1)];
        work.active_window_id = Some("@2".to_owned());
        let snapshot = MuxSnapshot {
            sessions: vec![work],
            active_session_id: Some("$1".to_owned()),
        };

        // tmux's active window moved (@1 -> @2) since the last snapshot, so the
        // highlight follows it even though the local selection still points at @1.
        assert_eq!(
            selected_window_after_refresh(
                Some("$1"),
                Some("@1".to_owned()),
                Some(&active_window("$1", "@1")),
                &snapshot,
            ),
            Some("@2".to_owned())
        );
        // No external change (@2 unchanged): the optimistic local selection wins,
        // so a just-issued local switch doesn't get reverted by a lagging snapshot.
        assert_eq!(
            selected_window_after_refresh(
                Some("$1"),
                Some("@1".to_owned()),
                Some(&active_window("$1", "@2")),
                &snapshot,
            ),
            Some("@1".to_owned())
        );
    }

    #[test]
    fn refreshed_snapshot_does_not_revert_local_tab_switch_after_other_session_was_active() {
        let mut work = session("$1", "work");
        work.windows = vec![window("@1", 1), window("@2", 2)];
        work.active_window_id = Some("@1".to_owned());
        let mut other = session("$2", "other");
        other.windows = vec![window("@9", 1)];
        other.active_window_id = Some("@9".to_owned());
        let mut controller = MuxController {
            sessions: vec![work.clone(), other],
            selected_session: Some("$1".to_owned()),
            selected_window: Some("@2".to_owned()),
            last_active_window: Some(active_window("$2", "@9")),
            current_backend: Some(MultiplexerBackendConfig::Rmux),
            ..Default::default()
        };

        controller.apply_refreshed_snapshot(
            MultiplexerBackendConfig::Rmux,
            MuxSnapshot {
                sessions: vec![work],
                active_session_id: Some("$1".to_owned()),
            },
        );

        assert_eq!(controller.selected_session(), Some("$1"));
        assert_eq!(controller.selected_window(), Some("@2"));
    }

    #[test]
    fn rmux_refresh_ignores_nonrenderable_snapshot_while_current_state_has_panes() {
        let mut work = session("work", "work");
        let mut editor = window("@1", 1);
        editor.anchor = MuxPaneAnchor {
            session_id: "work".to_owned(),
            pane_id: Some("%1".to_owned()),
            cwd: Some("/repo".to_owned()),
            process: Some("nvim".to_owned()),
        };
        editor.panes = vec![editor.anchor.clone()];
        editor.active = true;
        work.anchor = editor.anchor.clone();
        work.active_window_id = Some("@1".to_owned());
        work.windows = vec![editor];
        let mut controller = MuxController {
            sessions: vec![work],
            selected_session: Some("work".to_owned()),
            selected_window: Some("@1".to_owned()),
            current_backend: Some(MultiplexerBackendConfig::Rmux),
            ..Default::default()
        };

        controller.apply_refreshed_snapshot(MultiplexerBackendConfig::Rmux, MuxSnapshot::default());
        controller.apply_refreshed_snapshot(MultiplexerBackendConfig::Rmux, MuxSnapshot::default());

        assert_eq!(controller.selected_session(), Some("work"));
        assert_eq!(controller.selected_window(), Some("@1"));
        assert_eq!(
            controller
                .selected_session_anchor()
                .and_then(|anchor| anchor.pane_id.as_deref()),
            Some("%1")
        );
        assert_eq!(controller.sessions().len(), 1);

        let mut paneless = session("work", "work");
        paneless.windows = vec![window("@1", 1)];
        paneless.active_window_id = Some("@1".to_owned());
        controller.apply_refreshed_snapshot(
            MultiplexerBackendConfig::Rmux,
            MuxSnapshot {
                sessions: vec![paneless],
                active_session_id: Some("work".to_owned()),
            },
        );

        assert_eq!(
            controller
                .selected_session_anchor()
                .and_then(|anchor| anchor.pane_id.as_deref()),
            Some("%1")
        );
        assert_eq!(controller.sessions().len(), 1);
    }

    #[test]
    fn rmux_refresh_ignores_paneless_snapshot_before_first_renderable_state() {
        let mut paneless = session("work", "work");
        paneless.windows = vec![window("@1", 1), window("@2", 2)];
        paneless.active_window_id = Some("@1".to_owned());
        let mut controller = MuxController::default();

        controller.apply_refreshed_snapshot(
            MultiplexerBackendConfig::Rmux,
            MuxSnapshot {
                sessions: vec![paneless],
                active_session_id: Some("work".to_owned()),
            },
        );

        assert_eq!(controller.selected_session(), None);
        assert!(controller.sessions().is_empty());

        let mut work = session("work", "work");
        let mut editor = window("@1", 1);
        editor.anchor = MuxPaneAnchor {
            session_id: "work".to_owned(),
            pane_id: Some("%1".to_owned()),
            cwd: Some("/repo".to_owned()),
            process: Some("nvim".to_owned()),
        };
        editor.panes = vec![editor.anchor.clone()];
        editor.active = true;
        work.anchor = editor.anchor.clone();
        work.active_window_id = Some("@1".to_owned());
        work.windows = vec![editor];

        controller.apply_refreshed_snapshot(
            MultiplexerBackendConfig::Rmux,
            MuxSnapshot {
                sessions: vec![work],
                active_session_id: Some("work".to_owned()),
            },
        );

        assert_eq!(controller.selected_session(), Some("work"));
        assert_eq!(
            controller
                .selected_session_anchor()
                .and_then(|anchor| anchor.pane_id.as_deref()),
            Some("%1")
        );
    }

    #[test]
    fn binding_controllers_isolate_overlapping_ids_selection_refresh_and_errors() {
        let mut first = BindingMuxController::default();
        let mut second = BindingMuxController::default();
        let first_snapshot = MuxSnapshot {
            sessions: vec![session("$1", "first")],
            active_session_id: Some("$1".to_owned()),
        };
        let second_snapshot = MuxSnapshot {
            sessions: vec![session("$1", "second")],
            active_session_id: Some("$1".to_owned()),
        };

        first.apply_refreshed_snapshot(MultiplexerBackendConfig::Tmux, first_snapshot);
        second.apply_refreshed_snapshot(MultiplexerBackendConfig::Tmux, second_snapshot);
        first.set_error(Some("first binding failed".to_owned()));
        first.apply_refreshed_snapshot(MultiplexerBackendConfig::Tmux, MuxSnapshot::default());
        assert!(first.sessions().is_empty());
        assert_eq!(second.sessions()[0].name, "second");
        assert_eq!(second.selected_session(), Some("$1"));

        first.apply_refreshed_snapshot(
            MultiplexerBackendConfig::Tmux,
            MuxSnapshot {
                sessions: vec![session("$1", "first-reconnected")],
                active_session_id: Some("$1".to_owned()),
            },
        );

        assert_eq!(first.sessions()[0].name, "first-reconnected");
        assert_eq!(first.selected_session(), Some("$1"));
        assert_eq!(second.sessions()[0].name, "second");
        assert_eq!(first.last_error(), Some("first binding failed"));
        assert_eq!(second.last_error(), None);
    }
    #[test]
    fn successful_binding_refresh_clears_only_that_bindings_error() {
        let mut first = BindingMuxController::default();
        let mut second = BindingMuxController::default();
        first.set_error(Some("stale first error".to_owned()));
        second.set_error(Some("second remains failed".to_owned()));
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});

        assert_eq!(
            first.refresh_sessions(&repaint, &MultiplexerConfig::default()),
            None
        );

        assert_eq!(first.last_error(), None);
        assert_eq!(second.last_error(), Some("second remains failed"));
    }

    #[test]
    fn binding_refresh_completion_is_available_once() {
        let (refresh_tx, refresh_rx) = std::sync::mpsc::channel();
        refresh_tx
            .send((
                1,
                Ok((MultiplexerBackendConfig::Rmux, MuxSnapshot::default())),
            ))
            .expect("send completed refresh");
        let mut binding = BindingMuxController::default();
        binding.controller.session_refresh_generation = 1;
        binding.controller.session_refresh_rx = Some(refresh_rx);
        binding.controller.session_refresh_pending = true;
        binding.controller.last_session_refresh = Some(Instant::now());
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let config = MultiplexerConfig {
            backend: MultiplexerBackendConfig::Rmux,
            ..Default::default()
        };

        assert_eq!(binding.refresh_sessions(&repaint, &config), None);
        assert!(binding.take_refresh_completed());
        assert!(!binding.take_refresh_completed());
    }

    fn active_window(session_id: &str, window_id: &str) -> ActiveWindow {
        ActiveWindow {
            session_id: session_id.to_owned(),
            window_id: window_id.to_owned(),
        }
    }
}
