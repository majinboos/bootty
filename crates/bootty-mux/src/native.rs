use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::Result;

use super::{
    backend::MuxBackend,
    capability::{BindingCapabilityDescriptor, BindingOperation},
    command::MuxCommand,
    controller::MuxScope,
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
        Self {
            active_session_id: String::new(),
            sessions: Vec::new(),
            next_pane: 1,
        }
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
            name: default_window_name(),
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
    fn rename_window(&mut self, session_id: &str, window_id: &str, name: String) {
        if let Some(window) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            .and_then(|session| {
                session
                    .windows
                    .iter_mut()
                    .find(|window| window.id == window_id)
            })
        {
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
                .map(|session| session.id.clone())
                .unwrap_or_default();
        }
    }

    fn active_session_mut(&mut self, session_id: &str) -> Option<&mut NativeSession> {
        self.sessions
            .iter_mut()
            .find(|session| session.id == session_id)
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
                    .map(|pane| pane.cwd.clone())
                    .unwrap_or_else(|| PathBuf::from("."))
            });
            let index = session.windows.len() as u32 + 1;
            let window = NativeWindow {
                id: next_window_id(session),
                index,
                name: default_window_name(),
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
        session
            .windows
            .iter_mut()
            .find(|window| window.id == active_window_id)
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
                .map(|pane| pane.cwd.clone())
                .unwrap_or_else(|| PathBuf::from("."));
            window.active_pane_id = pane_id.clone();
            window.panes.push(NativePane { id: pane_id, cwd });
            self.active_session_id = session_id.to_owned();
        }
    }

    fn set_active_pane(&mut self, session_id: &str, pane_id: &str) {
        if let Some(window) = self.active_window_mut(session_id)
            && window.panes.iter().any(|pane| pane.id == pane_id)
        {
            window.active_pane_id = pane_id.to_owned();
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

    // Close the requested pane; when it was the last pane in its window, cascade to remove that
    // window. The target can belong to an inactive tab, so never route through active_window_mut.
    fn close_pane(&mut self, session_id: &str, pane_id: Option<&str>) {
        let mut changed_active_session = false;
        {
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
            let target_window_id = session.windows[window_index].id.clone();
            let target_was_active = target_window_id == session.active_window_id;
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
            if !window.panes.is_empty() {
                if removed_active_pane {
                    window.active_pane_id = window.panes[pane_index.min(window.panes.len() - 1)]
                        .id
                        .clone();
                }
                changed_active_session = target_was_active;
            } else {
                session.windows.remove(window_index);
                for (position, window) in session.windows.iter_mut().enumerate() {
                    window.index = position as u32 + 1;
                }
                if target_was_active {
                    session.active_window_id = session
                        .windows
                        .get(window_index.min(session.windows.len().saturating_sub(1)))
                        .map(|window| window.id.clone())
                        .unwrap_or_default();
                    changed_active_session = true;
                }
            }
        }
        if changed_active_session {
            self.active_session_id = session_id.to_owned();
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

fn anchor_for_pane(session_id: &str, pane: &NativePane) -> MuxPaneAnchor {
    MuxPaneAnchor {
        session_id: session_id.to_owned(),
        pane_id: Some(pane.id.clone()),
        cwd: Some(pane.cwd.to_string_lossy().into_owned()),
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
    fn snapshot(&self) -> Result<MuxSnapshot> {
        self.state
            .lock()
            .map(|state| state.snapshot())
            .map_err(|_| anyhow::anyhow!("native mux state lock poisoned"))
    }

    fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor {
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
                BindingOperation::CreateWorktreeSession,
                BindingOperation::RenameSession,
                BindingOperation::DitchSession,
            ],
        )
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
            MuxCommand::ActivatePreviousWindow { session_id } => {
                state.activate_relative_window(&session_id, -1);
            }
            MuxCommand::ActivateLastWindow { session_id } => {
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
            MuxCommand::SelectPreviousPane { session_id } => {
                state.select_relative_pane(&session_id, -1)
            }
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
    use crate::command::MuxSplitDirection;

    fn local_state() -> NativeMuxState {
        let mut state = NativeMuxState::new();
        state.ensure_session("local", ".");
        state
    }

    #[test]
    fn native_backend_starts_without_a_bootty_owned_session() {
        let backend = NativeBackend::with_state(NativeMuxState::new());

        let snapshot = backend.snapshot().unwrap();

        assert_eq!(snapshot.active_session_id, None);
        assert!(snapshot.sessions.is_empty());
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
            .execute(MuxCommand::RenameSession {
                session_id: "project".to_owned(),
                name: "renamed".to_owned(),
            })
            .unwrap();

        let snapshot = backend.snapshot().unwrap();

        assert_eq!(snapshot.active_session_id.as_deref(), Some("project"));
        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.sessions[0].name, "renamed");
        assert_eq!(snapshot.sessions[0].anchor.cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn rename_window_command_updates_tab_title() {
        let mut backend = NativeBackend::with_state(local_state());

        backend
            .execute(MuxCommand::RenameWindow {
                session_id: "local".to_owned(),
                window_id: "tab-1".to_owned(),
                name: "editor".to_owned(),
            })
            .unwrap();

        let snapshot = backend.snapshot().unwrap();
        assert_eq!(snapshot.sessions[0].windows[0].name, "editor");
    }

    #[test]
    fn close_pane_command_removes_last_tab_and_leaves_session_without_a_pane() {
        let mut backend = NativeBackend::with_state(local_state());

        backend
            .execute(MuxCommand::ClosePane {
                session_id: "local".to_owned(),
                pane_id: None,
            })
            .unwrap();

        let snapshot = backend.snapshot().unwrap();
        assert_eq!(
            snapshot.sessions.len(),
            1,
            "empty session stays in the sidebar"
        );
        assert!(
            snapshot.sessions[0].windows.is_empty(),
            "its last tab is gone"
        );
        // No pane means sync_mux_anchor renders idle instead of spawning a fresh shell.
        assert!(snapshot.sessions[0].anchor.pane_id.is_none());
    }

    #[test]
    fn close_pane_command_targets_the_named_pane_not_just_the_active_one() {
        let mut backend = NativeBackend::with_state(local_state());
        backend
            .execute(MuxCommand::SplitPane {
                session_id: "local".to_owned(),
                pane_id: None,
                direction: MuxSplitDirection::Right,
            })
            .unwrap();
        assert_eq!(
            backend.snapshot().unwrap().sessions[0].windows[0]
                .panes
                .len(),
            2
        );

        // The split made pane-2 active; closing pane-1 by id must remove pane-1, leaving pane-2.
        backend
            .execute(MuxCommand::ClosePane {
                session_id: "local".to_owned(),
                pane_id: Some("pane-1".to_owned()),
            })
            .unwrap();

        let snapshot = backend.snapshot().unwrap();
        let panes = &snapshot.sessions[0].windows[0].panes;
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].pane_id.as_deref(), Some("pane-2"));
    }

    #[test]
    fn close_pane_command_targets_a_pane_in_an_inactive_tab() {
        let mut backend = NativeBackend::with_state(local_state());
        backend
            .execute(MuxCommand::NewWindow {
                session_id: "local".to_owned(),
                cwd: None,
            })
            .unwrap();
        let inactive_pane = backend.snapshot().unwrap().sessions[0].windows[0]
            .anchor
            .pane_id
            .clone()
            .expect("first tab should have a pane");

        backend
            .execute(MuxCommand::ClosePane {
                session_id: "local".to_owned(),
                pane_id: Some(inactive_pane),
            })
            .unwrap();

        let session = &backend.snapshot().unwrap().sessions[0];
        assert_eq!(session.windows.len(), 1);
        assert_eq!(session.windows[0].id, "tab-2");
        assert_eq!(session.active_window_id.as_deref(), Some("tab-2"));
    }

    #[test]
    fn close_pane_in_a_split_tab_keeps_the_tab() {
        let mut state = local_state();
        state.split_pane("local", None);

        state.close_pane("local", None);

        let session = &state.sessions[0];
        assert_eq!(session.windows.len(), 1);
        assert_eq!(session.windows[0].panes.len(), 1);
    }

    #[test]
    fn new_window_revives_an_empty_session() {
        let mut state = local_state();
        state.close_pane("local", None);
        assert!(state.sessions[0].windows.is_empty());

        state.new_window("local", None);

        let session = &state.sessions[0];
        assert_eq!(session.windows.len(), 1);
        assert_eq!(session.active_window_id, "tab-1");
        assert_eq!(session.windows[0].panes.len(), 1);
    }

    #[test]
    fn close_pane_on_last_pane_removes_the_tab_and_selects_a_neighbor() {
        let mut state = local_state();
        state.new_window("local", None);
        state.new_window("local", None);

        state.close_pane("local", None);

        let session = &state.sessions[0];
        assert_eq!(session.windows.len(), 2);
        assert_eq!(session.active_window_id, "tab-2");
        assert_eq!(
            session
                .windows
                .iter()
                .map(|window| window.index)
                .collect::<Vec<_>>(),
            vec![1, 2],
            "remaining tabs are reindexed"
        );
    }

    #[test]
    fn move_window_can_target_a_non_active_tab() {
        let mut state = local_state();
        state.new_window("local", None);
        state.new_window("local", None);
        assert_eq!(state.sessions[0].active_window_id, "tab-3");

        state.move_window("local", Some("tab-1"), 1);

        let session = &state.sessions[0];
        let ids = session
            .windows
            .iter()
            .map(|window| window.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["tab-2", "tab-1", "tab-3"]);
        assert_eq!(session.active_window_id, "tab-1");
        assert_eq!(
            session
                .windows
                .iter()
                .map(|window| window.index)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn context_move_window_preserves_the_active_tab() {
        let mut state = local_state();
        state.new_window("local", None);
        state.new_window("local", None);
        let mut backend = NativeBackend::with_state(state);

        backend
            .execute(MuxCommand::MoveWindowPreservingSelection {
                session_id: "local".to_owned(),
                window_id: "tab-1".to_owned(),
                delta: 1,
                selected_window_id: "tab-3".to_owned(),
            })
            .unwrap();

        let session = &backend.snapshot().unwrap().sessions[0];
        assert_eq!(
            session
                .windows
                .iter()
                .map(|window| window.id.as_str())
                .collect::<Vec<_>>(),
            vec!["tab-2", "tab-1", "tab-3"]
        );
        assert_eq!(session.active_window_id.as_deref(), Some("tab-3"));
    }

    #[test]
    fn move_window_inserts_target_for_multi_step_delta() {
        let mut state = local_state();
        state.new_window("local", None);
        state.new_window("local", None);
        state.move_window("local", Some("tab-1"), 2);

        let session = &state.sessions[0];
        let ids = session
            .windows
            .iter()
            .map(|window| window.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["tab-2", "tab-3", "tab-1"]);
        assert_eq!(session.active_window_id, "tab-1");
    }
    #[test]
    fn new_window_after_closing_middle_tab_keeps_window_ids_unique() {
        let mut state = local_state();
        state.new_window("local", None);
        state.new_window("local", None);
        state.new_window("local", None);
        state.activate_window("local", "tab-2");

        state.close_pane("local", None);
        state.new_window("local", None);

        let ids = state.sessions[0]
            .windows
            .iter()
            .map(|window| window.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["tab-1", "tab-3", "tab-4", "tab-5"]);
    }
}
