use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::Result;

use bootty_mux::{
    backend::MuxBackend,
    capability::{BindingCapabilityDescriptor, BindingOperation},
    command::MuxCommand,
    controller::SpaceId,
    snapshot::{MuxPaneAnchor, MuxSession, MuxSessionTag, MuxSnapshot, MuxWindow},
    terminal::{
        BackendPanePolicy, MuxPaneTarget, PaneLayoutResizeRequest, PaneStartRequest,
        ScopedMuxPaneTarget, StartingNativeTerminal, TerminalRuntime,
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct NativePane {
    id: String,
    cwd: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NativeWindow {
    id: String,
    index: u32,
    name: String,
    active_pane_id: String,
    panes: Vec<NativePane>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NativeSession {
    id: String,
    name: String,
    active_window_id: String,
    windows: Vec<NativeWindow>,
    /// Native sessions live and die with the process, so this is only ever the tag the workspace
    /// handed over when it asked for the session — including when it is recreating one it
    /// persisted. Nothing outside Bootty can write here, and nothing survives a restart.
    tag: MuxSessionTag,
}

#[derive(Debug)]
struct NativeMuxState {
    active_session_id: String,
    sessions: Vec<NativeSession>,
    next_pane: u64,
}

impl NativeMuxState {
    fn new() -> Self {
        Self {
            active_session_id: String::new(),
            sessions: Vec::new(),
            next_pane: 1,
        }
    }

    fn ensure_session(&mut self, session_id: &str, cwd: impl Into<PathBuf>, tag: MuxSessionTag) {
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            if !tag.is_empty() {
                session.tag = tag;
            }
            session_id.clone_into(&mut self.active_session_id);
            return;
        }

        let pane_id = self.next_pane_id();
        let cwd = cwd.into();
        let window = NativeWindow {
            id: "tab-1".to_owned(),
            index: 1,
            name: default_window_name(),
            active_pane_id: pane_id.clone(),
            panes: vec![NativePane { id: pane_id, cwd }],
        };
        self.sessions.push(NativeSession {
            id: session_id.to_owned(),
            name: session_id.to_owned(),
            active_window_id: window.id.clone(),
            windows: vec![window],
            tag,
        });
        session_id.clone_into(&mut self.active_session_id);
    }

    fn stamp_session(&mut self, session_id: &str, tag: MuxSessionTag) {
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.tag = tag;
        }
    }

    fn activate_window(&mut self, session_id: &str, window_id: &str) {
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            && session.windows.iter().any(|window| window.id == window_id)
        {
            window_id.clone_into(&mut session.active_window_id);
            session_id.clone_into(&mut self.active_session_id);
        }
    }
    fn rename_window(&mut self, session_id: &str, window_id: &str, name: String) {
        if let Some(window) = self.window_mut(session_id, window_id) {
            window.name = name;
        }
    }

    fn rename_session(&mut self, session_id: &str, name: String) {
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.name = name;
        }
    }

    fn kill_session(&mut self, session_id: &str) {
        self.sessions.retain(|session| session.id != session_id);
        if self.active_session_id == session_id {
            self.active_session_id = self
                .sessions
                .first()
                .map_or_else(String::new, |session| session.id.clone());
        }
    }

    fn active_session_mut(&mut self, session_id: &str) -> Option<&mut NativeSession> {
        self.sessions
            .iter_mut()
            .find(|session| session.id == session_id)
    }

    fn window_mut(&mut self, session_id: &str, window_id: &str) -> Option<&mut NativeWindow> {
        self.active_session_mut(session_id)?
            .windows
            .iter_mut()
            .find(|window| window.id == window_id)
    }

    fn new_window(&mut self, session_id: &str, cwd: Option<PathBuf>) {
        let pane_id = self.next_pane_id();
        if let Some(session) = self.active_session_mut(session_id) {
            let cwd = cwd.unwrap_or_else(|| {
                session
                    .windows
                    .iter()
                    .find(|window| window.id == session.active_window_id)
                    .and_then(|window| window.panes.first())
                    .map_or_else(|| PathBuf::from("."), |pane| pane.cwd.clone())
            });
            let index = session.windows.len() as u32 + 1;
            let window = NativeWindow {
                id: next_window_id(session),
                index,
                name: default_window_name(),
                active_pane_id: pane_id.clone(),
                panes: vec![NativePane { id: pane_id, cwd }],
            };
            session.active_window_id.clone_from(&window.id);
            session.windows.push(window);
            session_id.clone_into(&mut self.active_session_id);
        }
    }

    fn activate_relative_window(&mut self, session_id: &str, delta: i32) {
        if let Some(session) = self.active_session_mut(session_id)
            && let Some(index) = session
                .windows
                .iter()
                .position(|window| window.id == session.active_window_id)
        {
            let next = wrap_index(index, delta, session.windows.len());
            session
                .active_window_id
                .clone_from(&session.windows[next].id);
            session_id.clone_into(&mut self.active_session_id);
        }
    }

    fn activate_window_index(&mut self, session_id: &str, index: u32) {
        if let Some(session) = self.active_session_mut(session_id)
            && let Some(window) = session.windows.iter().find(|window| window.index == index)
        {
            session.active_window_id.clone_from(&window.id);
            session_id.clone_into(&mut self.active_session_id);
        }
    }

    fn move_window(&mut self, session_id: &str, window_id: Option<&str>, delta: i32) {
        if let Some(session) = self.active_session_mut(session_id) {
            let target = window_id.unwrap_or(&session.active_window_id).to_owned();
            if let Some(index) = session
                .windows
                .iter()
                .position(|window| window.id == target)
            {
                let next = clamp_move_index(index, delta, session.windows.len());
                let window = session.windows.remove(index);
                session.windows.insert(next, window);
                session.active_window_id = target;
                for (index, window) in session.windows.iter_mut().enumerate() {
                    window.index = index as u32 + 1;
                }
            }
        }
    }

    fn active_window_mut(&mut self, session_id: &str) -> Option<&mut NativeWindow> {
        let session = self.active_session_mut(session_id)?;
        let active_window_id = session.active_window_id.clone();
        self.window_mut(session_id, &active_window_id)
    }

    fn split_pane(&mut self, session_id: &str, source_pane_id: Option<&str>) {
        let pane_id = self.next_pane_id();
        if let Some(window) = self.active_window_mut(session_id) {
            // Seed the new pane's cwd from the pane being split (the focused one), falling back to
            // the active pane and then the first pane.
            let cwd = source_pane_id
                .and_then(|id| window.panes.iter().find(|pane| pane.id == id))
                .or_else(|| {
                    window
                        .panes
                        .iter()
                        .find(|pane| pane.id == window.active_pane_id)
                })
                .or_else(|| window.panes.first())
                .map_or_else(|| PathBuf::from("."), |pane| pane.cwd.clone());
            window.active_pane_id.clone_from(&pane_id);
            window.panes.push(NativePane { id: pane_id, cwd });
            session_id.clone_into(&mut self.active_session_id);
        }
    }

    fn set_active_pane(&mut self, session_id: &str, pane_id: &str) {
        if let Some(window) = self.active_window_mut(session_id)
            && window.panes.iter().any(|pane| pane.id == pane_id)
        {
            pane_id.clone_into(&mut window.active_pane_id);
        }
    }

    fn select_relative_pane(&mut self, session_id: &str, delta: i32) {
        if let Some(window) = self.active_window_mut(session_id)
            && let Some(index) = window
                .panes
                .iter()
                .position(|pane| pane.id == window.active_pane_id)
        {
            let next = wrap_index(index, delta, window.panes.len());
            window.active_pane_id.clone_from(&window.panes[next].id);
            session_id.clone_into(&mut self.active_session_id);
        }
    }

    fn select_pane(&mut self, session_id: &str, window_id: Option<&str>, delta: i32) {
        if let Some(window_id) = window_id {
            self.activate_window(session_id, window_id);
        }
        self.select_relative_pane(session_id, delta);
    }

    fn kill_active_pane(&mut self, session_id: &str) {
        if let Some(window) = self.active_window_mut(session_id) {
            if window.panes.len() <= 1 {
                return;
            }
            if let Some(index) = window
                .panes
                .iter()
                .position(|pane| pane.id == window.active_pane_id)
            {
                window.panes.remove(index);
                window
                    .active_pane_id
                    .clone_from(&window.panes[index.min(window.panes.len() - 1)].id);
            }
        }
    }

    // Close the requested pane; when it was the last pane in its window, cascade to remove that
    // window. The target can belong to an inactive tab, so never route through active_window_mut.
    fn close_pane(&mut self, session_id: &str, pane_id: Option<&str>) {
        let changed_active_session = {
            let Some(session) = self.active_session_mut(session_id) else {
                return;
            };
            let window_index = pane_id
                .and_then(|pane_id| {
                    session
                        .windows
                        .iter()
                        .position(|window| window.panes.iter().any(|pane| pane.id == pane_id))
                })
                .or_else(|| {
                    session
                        .windows
                        .iter()
                        .position(|window| window.id == session.active_window_id)
                });
            let Some(window_index) = window_index else {
                return;
            };
            let target_was_active = session.windows[window_index].id == session.active_window_id;
            let pane_index = {
                let window = &session.windows[window_index];
                pane_id
                    .and_then(|pane_id| window.panes.iter().position(|pane| pane.id == pane_id))
                    .or_else(|| {
                        window
                            .panes
                            .iter()
                            .position(|pane| pane.id == window.active_pane_id)
                    })
            };
            let Some(pane_index) = pane_index else {
                return;
            };
            let window = &mut session.windows[window_index];
            let removed_active_pane = window.panes[pane_index].id == window.active_pane_id;
            window.panes.remove(pane_index);
            if window.panes.is_empty() {
                session.windows.remove(window_index);
                for (position, window) in session.windows.iter_mut().enumerate() {
                    window.index = position as u32 + 1;
                }
                if target_was_active {
                    session.active_window_id = session
                        .windows
                        .get(window_index.min(session.windows.len().saturating_sub(1)))
                        .map_or_else(String::new, |window| window.id.clone());
                }
            } else if removed_active_pane {
                window
                    .active_pane_id
                    .clone_from(&window.panes[pane_index.min(window.panes.len() - 1)].id);
            }
            target_was_active
        };
        if changed_active_session {
            session_id.clone_into(&mut self.active_session_id);
        }
    }

    fn snapshot(&self) -> MuxSnapshot {
        MuxSnapshot {
            active_session_id: (!self.active_session_id.is_empty())
                .then(|| self.active_session_id.clone()),
            sessions: self
                .sessions
                .iter()
                .map(|session| self.snapshot_session(session))
                .collect(),
            ..MuxSnapshot::default()
        }
    }

    fn snapshot_session(&self, session: &NativeSession) -> MuxSession {
        let active = session.id == self.active_session_id;
        let windows = session
            .windows
            .iter()
            .map(|window| {
                let anchor = anchor_for_window(&session.id, window);
                let panes = window
                    .panes
                    .iter()
                    .map(|pane| anchor_for_pane(&session.id, pane))
                    .collect();
                MuxWindow {
                    id: window.id.clone(),
                    index: window.index,
                    name: window.name.clone(),
                    active: active && window.id == session.active_window_id,
                    anchor,
                    panes,
                    layout: None,
                    // Native panes each own a PTY, so their progress arrives as OSC 9;4.
                    progress: None,
                }
            })
            .collect::<Vec<_>>();
        let anchor = windows
            .iter()
            .find(|window| window.id == session.active_window_id)
            .or_else(|| windows.first())
            .map_or_else(
                || MuxPaneAnchor {
                    session_id: session.id.clone(),
                    pane_id: None,
                    pane_pid: None,
                    cwd: None,
                    process: None,
                },
                |window| window.anchor.clone(),
            );

        MuxSession {
            id: session.id.clone(),
            name: session.name.clone(),
            active,
            anchor,
            active_window_id: Some(session.active_window_id.clone()),
            windows,
            tag: session.tag.clone(),
        }
    }

    fn next_pane_id(&mut self) -> String {
        let id = format!("pane-{}", self.next_pane);
        self.next_pane += 1;
        id
    }
}

fn anchor_for_window(session_id: &str, window: &NativeWindow) -> MuxPaneAnchor {
    let pane = window
        .panes
        .iter()
        .find(|pane| pane.id == window.active_pane_id)
        .or_else(|| window.panes.first());
    anchor_for_optional_pane(session_id, pane)
}

fn anchor_for_pane(session_id: &str, pane: &NativePane) -> MuxPaneAnchor {
    anchor_for_optional_pane(session_id, Some(pane))
}

fn anchor_for_optional_pane(session_id: &str, pane: Option<&NativePane>) -> MuxPaneAnchor {
    MuxPaneAnchor {
        session_id: session_id.to_owned(),
        pane_id: pane.map(|pane| pane.id.clone()),
        pane_pid: None,
        cwd: pane.map(|pane| pane.cwd.to_string_lossy().into_owned()),
        process: Some("shell".to_owned()),
    }
}

fn next_window_id(session: &NativeSession) -> String {
    let next = session
        .windows
        .iter()
        .filter_map(|window| window.id.strip_prefix("tab-"))
        .filter_map(|suffix| suffix.parse::<u32>().ok())
        .max()
        .unwrap_or(0)
        + 1;
    format!("tab-{next}")
}

fn default_window_name() -> String {
    std::env::var("BOOTTY_SHELL")
        .ok()
        .or_else(|| std::env::var("SHELL").ok())
        .and_then(|shell| {
            Path::new(&shell)
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "shell".to_owned())
}

pub struct NativeBackend {
    state: Arc<Mutex<NativeMuxState>>,
}

fn wrap_index(index: usize, delta: i32, len: usize) -> usize {
    (index as i32 + delta).rem_euclid(len as i32) as usize
}

fn clamp_move_index(index: usize, delta: i32, len: usize) -> usize {
    (index as i32 + delta).clamp(0, len.saturating_sub(1) as i32) as usize
}

impl NativeBackend {
    /// A backend on the state shared by every caller that has no workspace to name.
    pub fn new() -> Self {
        Self::for_workspace(Path::new(""))
    }

    /// The mux state belonging to `workspace`, creating it on first use.
    ///
    /// Native sessions live in this process, not in a server, so they have to outlive any single
    /// `AppState` -- closing and reopening a window keeps its sessions. Keying by workspace gives
    /// that while stopping two unrelated workspaces from seeing each other's sessions, which is what
    /// a single process-wide state could not do: in tests it accumulated every session every test
    /// created, so any assertion on a session list saw all of them and flaked.
    pub fn for_workspace(workspace: &Path) -> Self {
        static STATES: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<NativeMuxState>>>>> =
            OnceLock::new();
        let mut states = STATES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .expect("native mux state registry");
        Self {
            state: Arc::clone(
                states
                    .entry(workspace.to_path_buf())
                    .or_insert_with(|| Arc::new(Mutex::new(NativeMuxState::new()))),
            ),
        }
    }
}

impl Default for NativeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MuxBackend for NativeBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        self.state
            .lock()
            .map(|state| state.snapshot())
            .map_err(|_| anyhow::anyhow!("native mux state lock poisoned"))
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("native mux state lock poisoned"))?;
        match command {
            MuxCommand::ActivateWindow {
                session_id,
                window_id,
            } => state.activate_window(&session_id, &window_id),
            MuxCommand::NewWindow { session_id, cwd } => {
                state.new_window(&session_id, cwd.map(PathBuf::from));
            }
            MuxCommand::RenameWindow {
                session_id,
                window_id,
                name,
            } => {
                state.rename_window(&session_id, &window_id, name);
            }
            MuxCommand::ActivateNextWindow { session_id } => {
                state.activate_relative_window(&session_id, 1);
            }
            MuxCommand::ActivatePreviousWindow { session_id }
            | MuxCommand::ActivateLastWindow { session_id } => {
                state.activate_relative_window(&session_id, -1);
            }
            MuxCommand::ActivateWindowIndex { session_id, index } => {
                state.activate_window_index(&session_id, index);
            }
            MuxCommand::MoveWindow {
                session_id,
                window_id,
                delta,
            } => {
                state.move_window(&session_id, window_id.as_deref(), delta);
            }
            MuxCommand::MoveWindowPreservingSelection {
                session_id,
                window_id,
                delta,
                selected_window_id,
            } => {
                state.move_window(&session_id, Some(&window_id), delta);
                state.activate_window(&session_id, &selected_window_id);
            }
            MuxCommand::SplitPane {
                session_id,
                pane_id,
                ..
            } => state.split_pane(&session_id, pane_id.as_deref()),
            MuxCommand::SelectPane {
                session_id,
                window_id,
                direction,
            } => {
                let delta = match direction {
                    bootty_mux::command::MuxDirection::Left
                    | bootty_mux::command::MuxDirection::Up => -1,
                    bootty_mux::command::MuxDirection::Right
                    | bootty_mux::command::MuxDirection::Down => 1,
                };
                state.select_pane(&session_id, window_id.as_deref(), delta);
            }
            MuxCommand::SelectNextPane {
                session_id,
                window_id,
            } => state.select_pane(&session_id, window_id.as_deref(), 1),
            MuxCommand::SelectPreviousPane {
                session_id,
                window_id,
            } => state.select_pane(&session_id, window_id.as_deref(), -1),
            MuxCommand::KillPane {
                session_id,
                pane_id,
            } => {
                if let Some(pane_id) = pane_id {
                    state.set_active_pane(&session_id, &pane_id);
                }
                state.kill_active_pane(&session_id);
            }
            MuxCommand::ClosePane {
                session_id,
                pane_id,
            } => state.close_pane(&session_id, pane_id.as_deref()),
            MuxCommand::TogglePaneZoom { .. } => {}
            MuxCommand::CreateProjectSession {
                session_id,
                cwd,
                tag,
            }
            | MuxCommand::CreateWorktreeSession {
                session_id,
                cwd,
                tag,
            } => {
                state.ensure_session(&session_id, cwd, tag);
            }
            MuxCommand::RenameSession { session_id, name } => {
                state.rename_session(&session_id, name);
            }
            MuxCommand::DitchSession { session_id } => state.kill_session(&session_id),
            MuxCommand::StampSession { session_id, tag } => state.stamp_session(&session_id, tag),
        }
        Ok(())
    }
}

pub fn native_capabilities(scope: SpaceId) -> BindingCapabilityDescriptor {
    BindingCapabilityDescriptor::new(
        scope,
        [
            BindingOperation::ActivateWindow,
            BindingOperation::CreateWindow,
            BindingOperation::RenameWindow,
            BindingOperation::NavigateWindow,
            BindingOperation::MoveWindow,
            BindingOperation::SplitPane,
            BindingOperation::NavigatePane,
            BindingOperation::ClosePane,
            BindingOperation::CreateProjectSession,
            BindingOperation::StampSession,
            BindingOperation::CreateWorktreeSession,
            BindingOperation::RenameSession,
            BindingOperation::DitchSession,
        ],
    )
}

pub struct NativePanePolicy;

impl BackendPanePolicy for NativePanePolicy {
    fn remote_target(&self) -> Option<&bootty_mux_model::SshTarget> {
        None
    }

    fn start_terminal(
        &mut self,
        request: PaneStartRequest<'_>,
    ) -> Result<Option<Box<dyn TerminalRuntime>>> {
        if !matches!(request.target.mux_target(), MuxPaneTarget::Pane { .. }) {
            return Ok(None);
        }
        let mut config = request.terminal_config.clone();
        config.launch.working_directory =
            request.target.cwd().map(Path::new).map(Path::to_path_buf);
        config.launch.pane_id = request.target.pane_id().map(str::to_owned);
        config.side_effect_pane_id = request.target.side_effect_pane_id();
        Ok(Some(Box::new(StartingNativeTerminal::spawn(
            request.spawn_geometry,
            config,
            Arc::clone(request.repaint_wakeup),
        ))))
    }

    fn sync_target(&mut self, _target: Option<&ScopedMuxPaneTarget>, _hide_tmux_status: bool) {}

    fn set_layout_window(&mut self, _window_id: Option<&str>) {}

    fn resize_layout_window(&mut self, _request: PaneLayoutResizeRequest<'_>) -> Result<bool> {
        Ok(false)
    }

    fn deactivate(&mut self) {}
}
