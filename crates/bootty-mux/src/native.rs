use std::{
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::Result;

use super::{
    backend::MuxBackend,
    command::MuxCommand,
    config::MuxBackendKind,
    snapshot::{MuxPaneAnchor, MuxSession, MuxSnapshot, MuxWindow},
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
}

#[derive(Debug)]
struct NativeMuxState {
    active_session_id: String,
    sessions: Vec<NativeSession>,
    next_pane: u64,
}

impl NativeMuxState {
    fn new() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut state = Self {
            active_session_id: String::new(),
            sessions: Vec::new(),
            next_pane: 1,
        };
        state.ensure_session("local", cwd);
        state
    }

    fn ensure_session(&mut self, session_id: &str, cwd: impl Into<PathBuf>) {
        if self.sessions.iter().any(|session| session.id == session_id) {
            self.active_session_id = session_id.to_owned();
            return;
        }

        let pane_id = self.next_pane_id();
        let cwd = cwd.into();
        let window = NativeWindow {
            id: "tab-1".to_owned(),
            index: 1,
            name: "shell".to_owned(),
            active_pane_id: pane_id.clone(),
            panes: vec![NativePane { id: pane_id, cwd }],
        };
        self.sessions.push(NativeSession {
            id: session_id.to_owned(),
            name: session_id.to_owned(),
            active_window_id: window.id.clone(),
            windows: vec![window],
        });
        self.active_session_id = session_id.to_owned();
    }

    fn activate_session(&mut self, session_id: &str) {
        if self.sessions.iter().any(|session| session.id == session_id) {
            self.active_session_id = session_id.to_owned();
        }
    }

    fn activate_window(&mut self, session_id: &str, window_id: &str) {
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            && session.windows.iter().any(|window| window.id == window_id)
        {
            session.active_window_id = window_id.to_owned();
            self.active_session_id = session_id.to_owned();
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
                .map(|session| session.id.clone())
                .unwrap_or_default();
        }
    }

    fn active_session_index(&self) -> Option<usize> {
        self.sessions
            .iter()
            .position(|session| session.id == self.active_session_id)
    }

    fn activate_relative_session(&mut self, delta: i32) {
        let Some(index) = self.active_session_index() else {
            return;
        };
        let len = self.sessions.len();
        if len > 1 {
            self.active_session_id = self.sessions[wrap_index(index, delta, len)].id.clone();
        }
    }

    fn activate_session_index(&mut self, index: u32) {
        if let Some(session) = self.sessions.get(index.saturating_sub(1) as usize) {
            self.active_session_id = session.id.clone();
        }
    }

    fn move_active_session(&mut self, delta: i32) {
        let Some(index) = self.active_session_index() else {
            return;
        };
        let next = clamp_move_index(index, delta, self.sessions.len());
        self.sessions.swap(index, next);
    }

    fn active_session_mut(&mut self, session_id: &str) -> Option<&mut NativeSession> {
        self.sessions
            .iter_mut()
            .find(|session| session.id == session_id)
    }

    fn new_window(&mut self, session_id: &str) {
        let pane_id = self.next_pane_id();
        if let Some(session) = self.active_session_mut(session_id) {
            let cwd = session
                .windows
                .iter()
                .find(|window| window.id == session.active_window_id)
                .and_then(|window| window.panes.first())
                .map(|pane| pane.cwd.clone())
                .unwrap_or_else(|| PathBuf::from("."));
            let index = session.windows.len() as u32 + 1;
            let window = NativeWindow {
                id: format!("tab-{index}"),
                index,
                name: "shell".to_owned(),
                active_pane_id: pane_id.clone(),
                panes: vec![NativePane { id: pane_id, cwd }],
            };
            session.active_window_id = window.id.clone();
            session.windows.push(window);
            self.active_session_id = session_id.to_owned();
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
            session.active_window_id = session.windows[next].id.clone();
            self.active_session_id = session_id.to_owned();
        }
    }

    fn activate_window_index(&mut self, session_id: &str, index: u32) {
        if let Some(session) = self.active_session_mut(session_id)
            && let Some(window) = session.windows.iter().find(|window| window.index == index)
        {
            session.active_window_id = window.id.clone();
            self.active_session_id = session_id.to_owned();
        }
    }

    fn move_active_window(&mut self, session_id: &str, delta: i32) {
        if let Some(session) = self.active_session_mut(session_id)
            && let Some(index) = session
                .windows
                .iter()
                .position(|window| window.id == session.active_window_id)
        {
            let next = clamp_move_index(index, delta, session.windows.len());
            session.windows.swap(index, next);
            for (index, window) in session.windows.iter_mut().enumerate() {
                window.index = index as u32 + 1;
            }
        }
    }

    fn active_window_mut(&mut self, session_id: &str) -> Option<&mut NativeWindow> {
        let session = self.active_session_mut(session_id)?;
        let active_window_id = session.active_window_id.clone();
        session
            .windows
            .iter_mut()
            .find(|window| window.id == active_window_id)
    }

    fn split_pane(&mut self, session_id: &str) {
        let pane_id = self.next_pane_id();
        if let Some(window) = self.active_window_mut(session_id) {
            let cwd = window
                .panes
                .first()
                .map(|pane| pane.cwd.clone())
                .unwrap_or_else(|| PathBuf::from("."));
            window.active_pane_id = pane_id.clone();
            window.panes.push(NativePane { id: pane_id, cwd });
            self.active_session_id = session_id.to_owned();
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
            window.active_pane_id = window.panes[next].id.clone();
            self.active_session_id = session_id.to_owned();
        }
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
                window.active_pane_id = window.panes[index.min(window.panes.len() - 1)].id.clone();
            }
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
        }
    }

    fn snapshot_session(&self, session: &NativeSession) -> MuxSession {
        let active = session.id == self.active_session_id;
        let windows = session
            .windows
            .iter()
            .map(|window| {
                let anchor = anchor_for_window(&session.id, window);
                MuxWindow {
                    id: window.id.clone(),
                    index: window.index,
                    name: window.name.clone(),
                    active: active && window.id == session.active_window_id,
                    anchor,
                }
            })
            .collect::<Vec<_>>();
        let anchor = windows
            .iter()
            .find(|window| window.id == session.active_window_id)
            .or_else(|| windows.first())
            .map(|window| window.anchor.clone())
            .unwrap_or_else(|| MuxPaneAnchor {
                session_id: session.id.clone(),
                pane_id: None,
                cwd: None,
                process: None,
            });

        MuxSession {
            id: session.id.clone(),
            name: session.name.clone(),
            active,
            anchor,
            active_window_id: Some(session.active_window_id.clone()),
            windows,
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
    MuxPaneAnchor {
        session_id: session_id.to_owned(),
        pane_id: pane.map(|pane| pane.id.clone()),
        cwd: pane.map(|pane| pane.cwd.to_string_lossy().into_owned()),
        process: Some("shell".to_owned()),
    }
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
    pub fn new() -> Self {
        static STATE: OnceLock<Arc<Mutex<NativeMuxState>>> = OnceLock::new();
        Self {
            state: Arc::clone(STATE.get_or_init(|| Arc::new(Mutex::new(NativeMuxState::new())))),
        }
    }

    #[cfg(test)]
    fn with_state(state: NativeMuxState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }
}

impl Default for NativeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MuxBackend for NativeBackend {
    fn kind(&self) -> MuxBackendKind {
        MuxBackendKind::Native
    }

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
            MuxCommand::ActivateSession { session_id } => state.activate_session(&session_id),
            MuxCommand::ActivateNextSession => state.activate_relative_session(1),
            MuxCommand::ActivatePreviousSession => state.activate_relative_session(-1),
            MuxCommand::ActivateLastSession => state.activate_relative_session(-1),
            MuxCommand::ActivateSessionIndex { index } => state.activate_session_index(index),
            MuxCommand::MoveSession { delta } => state.move_active_session(delta),
            MuxCommand::ActivateWindow {
                session_id,
                window_id,
            } => state.activate_window(&session_id, &window_id),
            MuxCommand::NewWindow { session_id } => state.new_window(&session_id),
            MuxCommand::ActivateNextWindow { session_id } => {
                state.activate_relative_window(&session_id, 1);
            }
            MuxCommand::ActivatePreviousWindow { session_id } => {
                state.activate_relative_window(&session_id, -1);
            }
            MuxCommand::ActivateLastWindow { session_id } => {
                state.activate_relative_window(&session_id, -1);
            }
            MuxCommand::ActivateWindowIndex { session_id, index } => {
                state.activate_window_index(&session_id, index);
            }
            MuxCommand::MoveWindow { session_id, delta } => {
                state.move_active_window(&session_id, delta);
            }
            MuxCommand::SplitPane { session_id } => state.split_pane(&session_id),
            MuxCommand::SelectPane {
                session_id,
                direction,
            } => match direction {
                super::command::MuxDirection::Left | super::command::MuxDirection::Up => {
                    state.select_relative_pane(&session_id, -1);
                }
                super::command::MuxDirection::Right | super::command::MuxDirection::Down => {
                    state.select_relative_pane(&session_id, 1);
                }
            },
            MuxCommand::SelectNextPane { session_id } => state.select_relative_pane(&session_id, 1),
            MuxCommand::KillPane { session_id } => state.kill_active_pane(&session_id),
            MuxCommand::TogglePaneZoom { .. } => {}
            MuxCommand::CreateProjectSession { session_id, cwd }
            | MuxCommand::CreateWorktreeSession { session_id, cwd } => {
                state.ensure_session(&session_id, cwd);
            }
            MuxCommand::RenameSession { session_id, name } => {
                state.rename_session(&session_id, name);
            }
            MuxCommand::DitchSession { session_id } => state.kill_session(&session_id),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_backend_starts_with_local_bootty_owned_session() {
        let backend = NativeBackend::with_state(NativeMuxState::new());

        let snapshot = backend.snapshot().unwrap();

        assert_eq!(snapshot.active_session_id.as_deref(), Some("local"));
        assert_eq!(snapshot.sessions[0].id, "local");
        assert_eq!(snapshot.sessions[0].windows[0].id, "tab-1");
        assert_eq!(
            snapshot.sessions[0].windows[0].anchor.pane_id.as_deref(),
            Some("pane-1")
        );
    }

    #[test]
    fn native_backend_keeps_selection_in_bootty_state() {
        let mut backend = NativeBackend::with_state(NativeMuxState::new());
        backend
            .execute(MuxCommand::CreateProjectSession {
                session_id: "project".to_owned(),
                cwd: "/repo".to_owned(),
            })
            .unwrap();
        backend
            .execute(MuxCommand::ActivateSession {
                session_id: "local".to_owned(),
            })
            .unwrap();
        backend
            .execute(MuxCommand::RenameSession {
                session_id: "project".to_owned(),
                name: "renamed".to_owned(),
            })
            .unwrap();

        let snapshot = backend.snapshot().unwrap();

        assert_eq!(snapshot.active_session_id.as_deref(), Some("local"));
        assert_eq!(snapshot.sessions.len(), 2);
        assert_eq!(snapshot.sessions[1].name, "renamed");
        assert_eq!(snapshot.sessions[1].anchor.cwd.as_deref(), Some("/repo"));
    }
}
