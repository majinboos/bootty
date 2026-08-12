use std::{cell::RefCell, collections::BTreeMap, path::Path, rc::Rc};

use anyhow::Result;

use crate::{
    backend::{
        MuxAllocatedResources, MuxAllocatedWindow, MuxBackend, MuxBackendOperationError,
        MuxEventTarget,
    },
    capability::{BindingOperation, BindingOperationAvailability, BindingOperationOutcome},
    command::{
        MuxCommand, MuxDirection, MuxPaneLaunch, MuxPaneLaunchPlan, MuxPaneResize,
        MuxSessionLaunchPlan, MuxSplitDirection, MuxSplitLaunch, MuxWindowLaunchPlan,
    },
    controller::{BindingId, MuxScope, SpaceId},
    native::NativeBackend,
    process::{CommandOutput, CommandRunner},
    rmux::{RmuxBackend, RmuxSessionClient},
    snapshot::{MuxPaneAnchor, MuxSession, MuxSnapshot, MuxWindow},
    tmux::TmuxBackend,
};

#[derive(Clone, Copy)]
enum RecordingIdentity {
    SessionName,
    Tmux,
}

struct RecordingPane {
    id: String,
    terminal_id: String,
    launch: MuxPaneLaunch,
    columns: u16,
    rows: u16,
}

fn recording_shell_launch(cwd: impl Into<String>) -> MuxPaneLaunch {
    MuxPaneLaunch {
        cwd: cwd.into(),
        command: None,
        argv: None,
        environment: BTreeMap::new(),
        title: None,
    }
}

struct RecordingWindow {
    id: String,
    index: u32,
    name: String,
    active_pane_id: String,
    previous_pane_id: Option<String>,
    panes: Vec<RecordingPane>,
    zoomed: bool,
}

struct RecordingSession {
    id: String,
    name: String,
    environment: BTreeMap<String, String>,
    active_window_id: Option<String>,
    previous_window_id: Option<String>,
    windows: Vec<RecordingWindow>,
}

/// A deliberately small in-memory mux server used by both recording adapters. It accepts only
/// the backend's real SDK/CLI requests, and snapshots state through that backend's public seam.
struct RecordingMuxState {
    identity: RecordingIdentity,
    sessions: Vec<RecordingSession>,
    active_session_id: Option<String>,
    next_session: u32,
    next_window: u32,
    next_pane: u32,
    next_fault: Option<MuxBackendOperationError>,
}

impl RecordingMuxState {
    fn rmux() -> Self {
        Self::new(RecordingIdentity::SessionName)
    }

    fn tmux() -> Self {
        Self::new(RecordingIdentity::Tmux)
    }

    fn new(identity: RecordingIdentity) -> Self {
        Self {
            identity,
            sessions: Vec::new(),
            active_session_id: None,
            next_session: 0,
            next_window: 0,
            next_pane: 0,
            next_fault: None,
        }
    }

    fn fault_next(&mut self) {
        self.next_fault = Some(MuxBackendOperationError::Failed(
            "recording mux rejected the mutation".to_owned(),
        ));
    }

    fn take_fault(&mut self) -> Result<()> {
        match self.next_fault.take() {
            Some(error) => Err(error.into()),
            None => Ok(()),
        }
    }

    fn stale(kind: &str, target: &str) -> anyhow::Error {
        MuxBackendOperationError::stale(format!("recording {kind} {target:?} no longer exists"))
            .into()
    }

    fn failed(message: impl Into<String>) -> anyhow::Error {
        MuxBackendOperationError::Failed(message.into()).into()
    }

    fn snapshot(&self) -> MuxSnapshot {
        MuxSnapshot {
            active_session_id: self.active_session_id.clone(),
            sessions: self
                .sessions
                .iter()
                .map(|session| self.snapshot_session(session))
                .collect(),
        }
    }

    fn snapshot_session(&self, session: &RecordingSession) -> MuxSession {
        let active = self.active_session_id.as_deref() == Some(session.id.as_str());
        let windows = session
            .windows
            .iter()
            .map(|window| {
                self.snapshot_window(
                    &session.id,
                    active && session.active_window_id.as_deref() == Some(window.id.as_str()),
                    window,
                )
            })
            .collect::<Vec<_>>();
        let anchor = session
            .active_window_id
            .as_deref()
            .and_then(|id| windows.iter().find(|window| window.id == id))
            .or_else(|| windows.first())
            .map(|window| window.anchor.clone())
            .unwrap_or_else(|| MuxPaneAnchor {
                session_id: session.id.clone(),
                pane_id: None,
                terminal_id: None,
                pane_pid: None,
                cwd: None,
                process: None,
                occupant_id: None,
            });

        MuxSession {
            id: session.id.clone(),
            name: session.name.clone(),
            active,
            anchor,
            active_window_id: session.active_window_id.clone(),
            windows,
        }
    }

    fn snapshot_window(
        &self,
        session_id: &str,
        active: bool,
        window: &RecordingWindow,
    ) -> MuxWindow {
        let panes = window
            .panes
            .iter()
            .map(|pane| Self::snapshot_pane(session_id, pane))
            .collect::<Vec<_>>();
        let anchor = window
            .panes
            .iter()
            .find(|pane| pane.id == window.active_pane_id)
            .or_else(|| window.panes.first())
            .map(|pane| Self::snapshot_pane(session_id, pane))
            .unwrap_or_else(|| MuxPaneAnchor {
                session_id: session_id.to_owned(),
                pane_id: None,
                terminal_id: None,
                pane_pid: None,
                cwd: None,
                process: None,
                occupant_id: None,
            });

        MuxWindow {
            id: window.id.clone(),
            index: window.index,
            name: window.name.clone(),
            active,
            anchor,
            panes,
            layout: None,
            progress: None,
        }
    }

    fn snapshot_pane(session_id: &str, pane: &RecordingPane) -> MuxPaneAnchor {
        MuxPaneAnchor {
            session_id: session_id.to_owned(),
            pane_id: Some(pane.id.clone()),
            terminal_id: Some(pane.terminal_id.clone()),
            pane_pid: None,
            cwd: Some(pane.launch.cwd.clone()),
            process: Some("shell".to_owned()),
            occupant_id: Some(format!("recording:{}", pane.id)),
        }
    }

    fn tmux_snapshot(&self) -> String {
        let mut output = String::new();
        for session in &self.sessions {
            let active_pane = session
                .active_window_id
                .as_deref()
                .and_then(|window_id| session.windows.iter().find(|window| window.id == window_id))
                .and_then(|window| {
                    window
                        .panes
                        .iter()
                        .find(|pane| pane.id == window.active_pane_id)
                        .or_else(|| window.panes.first())
                });
            let (pane_id, pane_pid, cwd) = active_pane.map_or_else(
                || (String::new(), String::new(), String::new()),
                |pane| {
                    (
                        pane.id.clone(),
                        tmux_pane_pid(&pane.id),
                        pane.launch.cwd.clone(),
                    )
                },
            );
            let attached = if self.active_session_id.as_deref() == Some(session.id.as_str()) {
                "1"
            } else {
                "0"
            };
            output.push_str(&format!(
                "s\x1f{}\x1f{}\x1f{attached}\x1f{}\x1f{pane_id}\x1f{pane_pid}\x1f{cwd}\x1fshell\n",
                session.id,
                session.name,
                session.windows.len(),
            ));
            for window in &session.windows {
                let window_active =
                    if session.active_window_id.as_deref() == Some(window.id.as_str()) {
                        "1"
                    } else {
                        "0"
                    };
                for pane in &window.panes {
                    let pane_active = if pane.id == window.active_pane_id {
                        "1"
                    } else {
                        "0"
                    };
                    output.push_str(&format!(
                        "p\x1f{}\x1f{}\x1f{}\x1f{}\x1f{window_active}\x1f{pane_active}\x1f{}\x1f{}\x1fhidden\x1f\x1f{}\x1fshell\n",
                        session.id,
                        window.id,
                        window.index,
                        window.name,
                        pane.id,
                        tmux_pane_pid(&pane.id),
                        pane.launch.cwd,
                    ));
                }
            }
        }
        output
    }

    fn session_index(&self, target: &str) -> Result<usize> {
        self.sessions
            .iter()
            .position(|session| session.id == target || session.name == target)
            .ok_or_else(|| Self::stale("session", target))
    }

    fn active_session_index(&self) -> Result<usize> {
        self.active_session_id
            .as_deref()
            .ok_or_else(|| Self::stale("active session", ""))
            .and_then(|id| self.session_index(id))
    }

    fn window_index(&self, session_index: usize, window_id: &str) -> Result<usize> {
        self.sessions[session_index]
            .windows
            .iter()
            .position(|window| window.id == window_id)
            .ok_or_else(|| Self::stale("window", window_id))
    }

    fn active_window_index(&self, session_index: usize) -> Result<usize> {
        self.sessions[session_index]
            .active_window_id
            .as_deref()
            .ok_or_else(|| Self::stale("active window", ""))
            .and_then(|id| self.window_index(session_index, id))
    }

    fn pane_location(&self, session_index: usize, pane_id: &str) -> Result<(usize, usize)> {
        self.sessions[session_index]
            .windows
            .iter()
            .enumerate()
            .find_map(|(window_index, window)| {
                window
                    .panes
                    .iter()
                    .position(|pane| pane.id == pane_id)
                    .map(|pane_index| (window_index, pane_index))
            })
            .ok_or_else(|| Self::stale("pane", pane_id))
    }

    fn make_pane(&mut self, launch: MuxPaneLaunch) -> RecordingPane {
        self.next_pane = self.next_pane.saturating_add(1);
        RecordingPane {
            id: format!("%{}", self.next_pane),
            terminal_id: format!("t{}", self.next_pane),
            launch,
            columns: 80,
            rows: 24,
        }
    }

    fn make_window(
        &mut self,
        index: u32,
        name: impl Into<String>,
        pane_launches: impl IntoIterator<Item = MuxPaneLaunch>,
    ) -> RecordingWindow {
        self.next_window = self.next_window.saturating_add(1);
        let panes = pane_launches
            .into_iter()
            .map(|launch| self.make_pane(launch))
            .collect::<Vec<_>>();
        let active_pane_id = panes
            .first()
            .map(|pane| pane.id.clone())
            .expect("recording window has at least one pane");
        RecordingWindow {
            id: format!("@{}", self.next_window),
            index,
            name: name.into(),
            active_pane_id,
            previous_pane_id: None,
            panes,
            zoomed: false,
        }
    }

    fn allocate_session_id(&mut self, name: &str) -> String {
        match self.identity {
            RecordingIdentity::SessionName => name.to_owned(),
            RecordingIdentity::Tmux => {
                self.next_session = self.next_session.saturating_add(1);
                format!("${}", self.next_session)
            }
        }
    }

    fn create_session(
        &mut self,
        name: &str,
        launch: MuxPaneLaunch,
        window_name: &str,
        activate: bool,
    ) -> Result<(String, String, String)> {
        if self
            .sessions
            .iter()
            .any(|session| session.id == name || session.name == name)
        {
            return Err(Self::failed(format!(
                "recording session {name:?} already exists"
            )));
        }
        let id = self.allocate_session_id(name);
        let window = self.make_window(1, window_name, [launch]);
        let window_id = window.id.clone();
        let pane_id = window.active_pane_id.clone();
        self.sessions.push(RecordingSession {
            id: id.clone(),
            name: name.to_owned(),
            environment: BTreeMap::new(),
            active_window_id: Some(window_id.clone()),
            previous_window_id: None,
            windows: vec![window],
        });
        if activate || self.active_session_id.is_none() {
            self.active_session_id = Some(id.clone());
        }
        Ok((id, window_id, pane_id))
    }

    fn ensure_session(&mut self, session_name: &str, cwd: &str) -> Result<()> {
        match self.session_index(session_name) {
            Ok(index) => {
                self.active_session_id = Some(self.sessions[index].id.clone());
                Ok(())
            }
            Err(error)
                if matches!(
                    error.downcast_ref::<MuxBackendOperationError>(),
                    Some(MuxBackendOperationError::Stale(_))
                ) =>
            {
                self.create_session(session_name, recording_shell_launch(cwd), "shell", true)
                    .map(|_| ())
            }
            Err(error) => Err(error),
        }
    }

    fn launch(&mut self, plan: &MuxSessionLaunchPlan) -> Result<MuxAllocatedResources> {
        plan.validate()
            .map_err(|error| MuxBackendOperationError::Failed(error.to_string()))?;
        if self
            .sessions
            .iter()
            .any(|session| session.id == plan.session_id || session.name == plan.session_id)
        {
            return Err(Self::failed(format!(
                "recording session {:?} already exists",
                plan.session_id
            )));
        }

        let session_id = self.allocate_session_id(&plan.session_id);
        let mut allocated_windows = Vec::with_capacity(plan.windows.len());
        let mut windows = Vec::with_capacity(plan.windows.len());
        for (position, window_plan) in plan.windows.iter().enumerate() {
            let mut pane_launches = Vec::new();
            collect_launch_pane_intents(&window_plan.layout, &plan.environment, &mut pane_launches);
            let window = self.make_window(
                position as u32 + 1,
                window_plan
                    .name
                    .clone()
                    .unwrap_or_else(|| "shell".to_owned()),
                pane_launches,
            );
            allocated_windows.push(MuxAllocatedWindow {
                window_id: window.id.clone(),
                pane_ids: window.panes.iter().map(|pane| pane.id.clone()).collect(),
            });
            windows.push(window);
        }
        let active_window_id = windows
            .get(plan.focused_window)
            .map(|window| window.id.clone())
            .expect("validated launch plan has a focused window");
        self.sessions.push(RecordingSession {
            id: session_id.clone(),
            name: plan.session_id.clone(),
            environment: plan.environment.clone(),
            active_window_id: Some(active_window_id),
            previous_window_id: None,
            windows,
        });
        if plan.focus || self.active_session_id.is_none() {
            self.active_session_id = Some(session_id.clone());
        }

        Ok(MuxAllocatedResources {
            session_id,
            windows: allocated_windows,
        })
    }

    fn add_window(
        &mut self,
        session_index: usize,
        launch: MuxPaneLaunch,
        name: String,
        activate: bool,
    ) -> Result<(String, String)> {
        let index = self.sessions[session_index].windows.len() as u32 + 1;
        let window = self.make_window(index, name, [launch]);
        let window_id = window.id.clone();
        let pane_id = window.active_pane_id.clone();
        let previous = self.sessions[session_index].active_window_id.clone();
        self.sessions[session_index].windows.push(window);
        if activate {
            if previous.as_deref() != Some(window_id.as_str()) {
                self.sessions[session_index].previous_window_id = previous;
            }
            self.sessions[session_index].active_window_id = Some(window_id.clone());
            self.active_session_id = Some(self.sessions[session_index].id.clone());
        }
        Ok((window_id, pane_id))
    }

    fn new_window(&mut self, session_id: &str, cwd: Option<&str>) -> Result<()> {
        let session_index = self.session_index(session_id)?;
        let inherited_cwd = self.sessions[session_index]
            .active_window_id
            .as_deref()
            .and_then(|window_id| {
                self.sessions[session_index]
                    .windows
                    .iter()
                    .find(|window| window.id == window_id)
            })
            .and_then(|window| {
                window
                    .panes
                    .iter()
                    .find(|pane| pane.id == window.active_pane_id)
                    .or_else(|| window.panes.first())
            })
            .map(|pane| pane.launch.cwd.clone())
            .unwrap_or_else(|| ".".to_owned());
        self.add_window(
            session_index,
            recording_shell_launch(cwd.unwrap_or(&inherited_cwd)),
            "shell".to_owned(),
            true,
        )?;
        Ok(())
    }

    fn set_active_window(&mut self, session_index: usize, window_index: usize) {
        let window_id = self.sessions[session_index].windows[window_index]
            .id
            .clone();
        let previous = self.sessions[session_index].active_window_id.clone();
        if previous.as_deref() != Some(window_id.as_str()) {
            self.sessions[session_index].previous_window_id = previous;
        }
        self.sessions[session_index].active_window_id = Some(window_id);
        self.active_session_id = Some(self.sessions[session_index].id.clone());
    }

    fn activate_window(&mut self, session_id: &str, window_id: &str) -> Result<()> {
        let session_index = self.session_index(session_id)?;
        let window_index = self.window_index(session_index, window_id)?;
        self.set_active_window(session_index, window_index);
        Ok(())
    }

    fn activate_relative_window(&mut self, session_id: &str, delta: i32) -> Result<()> {
        let session_index = self.session_index(session_id)?;
        let current = self.active_window_index(session_index)?;
        let len = self.sessions[session_index].windows.len();
        let next = wrap_index(current, delta, len);
        self.set_active_window(session_index, next);
        Ok(())
    }

    fn activate_last_window(&mut self, session_id: &str) -> Result<()> {
        let session_index = self.session_index(session_id)?;
        let previous = self.sessions[session_index]
            .previous_window_id
            .clone()
            .ok_or_else(|| Self::stale("last window", session_id))?;
        let window_index = self.window_index(session_index, &previous)?;
        self.set_active_window(session_index, window_index);
        Ok(())
    }

    fn activate_window_index(&mut self, session_id: &str, index: u32) -> Result<()> {
        let session_index = self.session_index(session_id)?;
        let window_index = self.sessions[session_index]
            .windows
            .iter()
            .position(|window| window.index == index)
            .ok_or_else(|| Self::stale("window index", &index.to_string()))?;
        self.set_active_window(session_index, window_index);
        Ok(())
    }

    fn rename_window(&mut self, session_id: &str, window_id: &str, name: &str) -> Result<()> {
        let session_index = self.session_index(session_id)?;
        let window_index = self.window_index(session_index, window_id)?;
        self.sessions[session_index].windows[window_index].name = name.to_owned();
        Ok(())
    }

    fn move_window(&mut self, session_id: &str, window_id: Option<&str>, delta: i32) -> Result<()> {
        let session_index = self.session_index(session_id)?;
        let source = match window_id {
            Some(window_id) => self.window_index(session_index, window_id)?,
            None => self.active_window_index(session_index)?,
        };
        let target_index = clamp_index(source, delta, self.sessions[session_index].windows.len());
        let window = self.sessions[session_index].windows.remove(source);
        let window_id = window.id.clone();
        self.sessions[session_index]
            .windows
            .insert(target_index, window);
        for (index, window) in self.sessions[session_index].windows.iter_mut().enumerate() {
            window.index = index as u32 + 1;
        }
        self.set_active_window(session_index, target_index);
        debug_assert_eq!(
            self.sessions[session_index].active_window_id.as_deref(),
            Some(window_id.as_str())
        );
        Ok(())
    }

    fn set_active_pane(&mut self, session_index: usize, window_index: usize, pane_index: usize) {
        let pane_id = self.sessions[session_index].windows[window_index].panes[pane_index]
            .id
            .clone();
        let previous = self.sessions[session_index].windows[window_index]
            .active_pane_id
            .clone();
        if previous != pane_id {
            self.sessions[session_index].windows[window_index].previous_pane_id = Some(previous);
        }
        self.sessions[session_index].windows[window_index].active_pane_id = pane_id;
        self.set_active_window(session_index, window_index);
    }

    fn select_relative_pane(
        &mut self,
        session_index: usize,
        window_index: usize,
        delta: i32,
    ) -> Result<()> {
        let current = self.sessions[session_index].windows[window_index]
            .panes
            .iter()
            .position(|pane| {
                pane.id == self.sessions[session_index].windows[window_index].active_pane_id
            })
            .ok_or_else(|| Self::stale("active pane", ""))?;
        let len = self.sessions[session_index].windows[window_index]
            .panes
            .len();
        self.set_active_pane(session_index, window_index, wrap_index(current, delta, len));
        Ok(())
    }

    fn split_pane(&mut self, session_id: &str, pane_id: Option<&str>) -> Result<String> {
        self.split_pane_with_launch(session_id, pane_id, None)
    }

    fn split_pane_with_launch(
        &mut self,
        session_id: &str,
        pane_id: Option<&str>,
        launch: Option<MuxPaneLaunch>,
    ) -> Result<String> {
        let session_index = self.session_index(session_id)?;
        let (window_index, pane_index) = match pane_id {
            Some(pane_id) => self.pane_location(session_index, pane_id)?,
            None => {
                let window_index = self.active_window_index(session_index)?;
                let pane_index = self.sessions[session_index].windows[window_index]
                    .panes
                    .iter()
                    .position(|pane| {
                        pane.id == self.sessions[session_index].windows[window_index].active_pane_id
                    })
                    .ok_or_else(|| Self::stale("active pane", ""))?;
                (window_index, pane_index)
            }
        };
        let launch = launch.unwrap_or_else(|| {
            recording_shell_launch(
                self.sessions[session_index].windows[window_index].panes[pane_index]
                    .launch
                    .cwd
                    .clone(),
            )
        });
        let pane = self.make_pane(launch);
        let new_pane_id = pane.id.clone();
        self.sessions[session_index].windows[window_index]
            .panes
            .push(pane);
        let new_pane_index = self.sessions[session_index].windows[window_index]
            .panes
            .len()
            - 1;
        self.set_active_pane(session_index, window_index, new_pane_index);
        Ok(new_pane_id)
    }

    fn select_pane(
        &mut self,
        session_id: &str,
        window_id: Option<&str>,
        direction: MuxDirection,
    ) -> Result<()> {
        let session_index = self.session_index(session_id)?;
        let window_index = match window_id {
            Some(window_id) => self.window_index(session_index, window_id)?,
            None => self.active_window_index(session_index)?,
        };
        self.set_active_window(session_index, window_index);
        let delta = match direction {
            MuxDirection::Left | MuxDirection::Up => -1,
            MuxDirection::Down | MuxDirection::Right => 1,
        };
        self.select_relative_pane(session_index, window_index, delta)
    }

    fn select_last_pane(&mut self, session_id: &str, window_id: Option<&str>) -> Result<()> {
        let session_index = self.session_index(session_id)?;
        let window_index = match window_id {
            Some(window_id) => self.window_index(session_index, window_id)?,
            None => self.active_window_index(session_index)?,
        };
        let previous = self.sessions[session_index].windows[window_index]
            .previous_pane_id
            .clone()
            .ok_or_else(|| Self::stale("last pane", session_id))?;
        let pane_index = self.sessions[session_index].windows[window_index]
            .panes
            .iter()
            .position(|pane| pane.id == previous)
            .ok_or_else(|| Self::stale("last pane", &previous))?;
        self.set_active_pane(session_index, window_index, pane_index);
        Ok(())
    }

    fn close_pane(&mut self, session_id: &str, pane_id: Option<&str>) -> Result<()> {
        let session_index = self.session_index(session_id)?;
        let (window_index, pane_index) = match pane_id {
            Some(pane_id) => self.pane_location(session_index, pane_id)?,
            None => {
                let window_index = self.active_window_index(session_index)?;
                let pane_index = self.sessions[session_index].windows[window_index]
                    .panes
                    .iter()
                    .position(|pane| {
                        pane.id == self.sessions[session_index].windows[window_index].active_pane_id
                    })
                    .ok_or_else(|| Self::stale("active pane", ""))?;
                (window_index, pane_index)
            }
        };
        let window_id = self.sessions[session_index].windows[window_index]
            .id
            .clone();
        let window_was_active =
            self.sessions[session_index].active_window_id.as_deref() == Some(window_id.as_str());
        let removed = self.sessions[session_index].windows[window_index]
            .panes
            .remove(pane_index);
        if self.sessions[session_index].windows[window_index]
            .panes
            .is_empty()
        {
            self.sessions[session_index].windows.remove(window_index);
            for (index, window) in self.sessions[session_index].windows.iter_mut().enumerate() {
                window.index = index as u32 + 1;
            }
            if window_was_active {
                let next_active_window_id = self.sessions[session_index]
                    .windows
                    .get(
                        window_index
                            .min(self.sessions[session_index].windows.len().saturating_sub(1)),
                    )
                    .map(|window| window.id.clone());
                self.sessions[session_index].active_window_id = next_active_window_id;
            }
        } else {
            let window = &mut self.sessions[session_index].windows[window_index];
            if removed.id == window.active_pane_id {
                window.active_pane_id = window.panes[pane_index.min(window.panes.len() - 1)]
                    .id
                    .clone();
            }
            if window.previous_pane_id.as_deref() == Some(removed.id.as_str()) {
                window.previous_pane_id = None;
            }
        }
        self.active_session_id = Some(self.sessions[session_index].id.clone());
        Ok(())
    }

    fn resize_pane(
        &mut self,
        session_id: &str,
        pane_id: Option<&str>,
        adjustment: MuxPaneResize,
    ) -> Result<()> {
        if !adjustment.is_valid() {
            return Err(Self::failed(
                "recording pane resize requires every supplied dimension to be positive",
            ));
        }
        let session_index = self.session_index(session_id)?;
        let (window_index, pane_index) = match pane_id {
            Some(pane_id) => self.pane_location(session_index, pane_id)?,
            None => {
                let window_index = self.active_window_index(session_index)?;
                let pane_index = self.sessions[session_index].windows[window_index]
                    .panes
                    .iter()
                    .position(|pane| {
                        pane.id == self.sessions[session_index].windows[window_index].active_pane_id
                    })
                    .ok_or_else(|| Self::stale("active pane", ""))?;
                (window_index, pane_index)
            }
        };
        let pane = &mut self.sessions[session_index].windows[window_index].panes[pane_index];
        match adjustment {
            MuxPaneResize::Directional { direction, cells } => match direction {
                MuxDirection::Left => pane.columns = pane.columns.saturating_sub(cells),
                MuxDirection::Right => pane.columns = pane.columns.saturating_add(cells),
                MuxDirection::Up => pane.rows = pane.rows.saturating_sub(cells),
                MuxDirection::Down => pane.rows = pane.rows.saturating_add(cells),
            },
            MuxPaneResize::Absolute { columns, rows } => {
                if let Some(columns) = columns {
                    pane.columns = columns;
                }
                if let Some(rows) = rows {
                    pane.rows = rows;
                }
            }
        }
        Ok(())
    }

    fn toggle_pane_zoom(&mut self, session_id: &str, pane_id: Option<&str>) -> Result<()> {
        let session_index = self.session_index(session_id)?;
        let (window_index, _) = match pane_id {
            Some(pane_id) => self.pane_location(session_index, pane_id)?,
            None => (self.active_window_index(session_index)?, 0),
        };
        let zoomed = self.sessions[session_index].windows[window_index].zoomed;
        self.sessions[session_index].windows[window_index].zoomed = !zoomed;
        Ok(())
    }

    fn rename_session(&mut self, session_id: &str, name: &str) -> Result<()> {
        let session_index = self.session_index(session_id)?;
        self.sessions[session_index].name = name.to_owned();
        Ok(())
    }

    fn kill_session(&mut self, session_id: &str) -> Result<()> {
        let session_index = self.session_index(session_id)?;
        let removed = self.sessions.remove(session_index);
        if self.active_session_id.as_deref() == Some(removed.id.as_str()) {
            self.active_session_id = self.sessions.first().map(|session| session.id.clone());
        }
        Ok(())
    }

    fn pane_geometry(&self, session_id: &str, pane_id: &str) -> Result<(u16, u16)> {
        let session_index = self.session_index(session_id)?;
        let (window_index, pane_index) = self.pane_location(session_index, pane_id)?;
        let pane = &self.sessions[session_index].windows[window_index].panes[pane_index];
        Ok((pane.columns, pane.rows))
    }

    fn pane_zoomed(&self, session_id: &str, pane_id: &str) -> Result<bool> {
        let session_index = self.session_index(session_id)?;
        let (window_index, _) = self.pane_location(session_index, pane_id)?;
        Ok(self.sessions[session_index].windows[window_index].zoomed)
    }

    fn pane_launch(
        &self,
        session_id: &str,
        window_id: &str,
        pane_id: &str,
    ) -> Result<MuxPaneLaunch> {
        let session_index = self.session_index(session_id)?;
        let window_index = self.window_index(session_index, window_id)?;
        self.sessions[session_index].windows[window_index]
            .panes
            .iter()
            .find(|pane| pane.id == pane_id)
            .map(|pane| pane.launch.clone())
            .ok_or_else(|| Self::stale("pane", pane_id))
    }

    fn tmux_command(&mut self, args: &[String]) -> Result<String> {
        if Self::is_authoritative_tmux_snapshot_command(args) {
            return Ok(self.tmux_snapshot());
        }
        let command = args
            .first()
            .map(String::as_str)
            .ok_or_else(|| Self::failed("recording tmux command was empty"))?;
        self.take_fault()?;
        match command {
            "new-session" => self.tmux_new_session(args),
            "new-window" => self.tmux_new_window(args),
            "set-environment" => self.tmux_set_environment(args),
            "rename-session" => {
                let target = tmux_arg(args, "-t")?;
                let name = args
                    .last()
                    .filter(|name| name.as_str() != target)
                    .ok_or_else(|| Self::failed("recording tmux rename-session omitted a name"))?;
                self.rename_session(target, name)?;
                Ok(String::new())
            }
            "kill-session" => {
                self.kill_session(tmux_arg(args, "-t")?)?;
                Ok(String::new())
            }
            "select-window" => {
                self.tmux_select_window(tmux_arg(args, "-t")?)?;
                Ok(String::new())
            }
            "next-window" => {
                self.activate_relative_window(tmux_arg(args, "-t")?, 1)?;
                Ok(String::new())
            }
            "previous-window" => {
                self.activate_relative_window(tmux_arg(args, "-t")?, -1)?;
                Ok(String::new())
            }
            "last-window" => {
                self.activate_last_window(tmux_arg(args, "-t")?)?;
                Ok(String::new())
            }
            "rename-window" => {
                let target = tmux_arg(args, "-t")?;
                let (session_index, window_index) = self.tmux_window_target(target)?;
                let name = args
                    .last()
                    .filter(|name| name.as_str() != target)
                    .ok_or_else(|| Self::failed("recording tmux rename-window omitted a name"))?;
                let session_id = self.sessions[session_index].id.clone();
                let window_id = self.sessions[session_index].windows[window_index]
                    .id
                    .clone();
                self.rename_window(&session_id, &window_id, name)?;
                Ok(String::new())
            }
            "swap-window" => {
                self.tmux_swap_window(tmux_arg(args, "-t")?)?;
                Ok(String::new())
            }
            "split-window" => self.tmux_split_window(args),
            "select-pane" => self.tmux_select_pane(args),
            "last-pane" => {
                let (session_index, window_index) =
                    self.tmux_window_target(tmux_arg(args, "-t")?)?;
                let session_id = self.sessions[session_index].id.clone();
                let window_id = self.sessions[session_index].windows[window_index]
                    .id
                    .clone();
                self.select_last_pane(&session_id, Some(&window_id))?;
                Ok(String::new())
            }
            "kill-pane" => {
                let (session_index, window_index, pane_index) =
                    self.tmux_pane_target(tmux_arg(args, "-t")?)?;
                let session_id = self.sessions[session_index].id.clone();
                let pane_id = self.sessions[session_index].windows[window_index].panes[pane_index]
                    .id
                    .clone();
                self.close_pane(&session_id, Some(&pane_id))?;
                Ok(String::new())
            }
            "resize-pane" => self.tmux_resize_pane(args),
            _ => Err(Self::failed(format!(
                "recording tmux does not recognize command {command:?}"
            ))),
        }
    }
    fn is_authoritative_tmux_snapshot_command(args: &[String]) -> bool {
        let [
            session_format,
            separator,
            command,
            all_panes,
            format_flag,
            pane_format,
        ] = args
        else {
            return false;
        };
        session_format
            == "s\x1f#{session_id}\x1f#{session_name}\x1f#{session_attached}\x1f#{session_windows}\x1f#{pane_id}\x1f#{pane_tty}\x1f#{pane_pid}\x1f#{pane_current_path}\x1f#{pane_current_command}\x1f#{pid}"
            && separator == ";"
            && command == "list-panes"
            && all_panes == "-a"
            && format_flag == "-F"
            && pane_format
                == "p\x1f#{session_id}\x1f#{window_id}\x1f#{window_index}\x1f#{window_name}\x1f#{window_active}\x1f#{pane_active}\x1f#{pane_id}\x1f#{pane_tty}\x1f#{pane_pid}\x1f#{pane_pb_state}\x1f#{pane_pb_progress}\x1f#{pane_current_path}\x1f#{pane_current_command}\x1f#{pid}"
    }

    fn tmux_new_session(&mut self, args: &[String]) -> Result<String> {
        let name = tmux_arg(args, "-s")?.to_owned();
        let launch = recording_tmux_launch_pane(args)?;
        let session_environment = launch.environment.clone();
        let window_name = tmux_optional_arg(args, "-n").unwrap_or("shell");
        let activate = !tmux_has(args, "-d") || self.active_session_id.is_none();
        let (session_id, window_id, pane_id) =
            self.create_session(&name, launch, window_name, activate)?;
        let session_index = self.session_index(&session_id)?;
        self.sessions[session_index].environment = session_environment;
        if tmux_has(args, "-P") {
            Ok(format!("{session_id}\x1f{window_id}\x1f{pane_id}"))
        } else {
            Ok(String::new())
        }
    }

    fn tmux_new_window(&mut self, args: &[String]) -> Result<String> {
        let session_index = self.session_index(tmux_arg(args, "-t")?)?;
        let launch =
            self.tmux_effective_pane_launch(session_index, recording_tmux_launch_pane(args)?);
        let name = tmux_optional_arg(args, "-n").unwrap_or("shell").to_owned();
        let (window_id, pane_id) =
            self.add_window(session_index, launch, name, !tmux_has(args, "-d"))?;
        if tmux_has(args, "-P") {
            Ok(format!("{window_id}\x1f{pane_id}"))
        } else {
            Ok(String::new())
        }
    }

    fn tmux_set_environment(&mut self, args: &[String]) -> Result<String> {
        let session_index = self.session_index(tmux_arg(args, "-t")?)?;
        let mut positional = Vec::new();
        let mut position = 1;
        while let Some(argument) = args.get(position) {
            match argument.as_str() {
                "-t" => position += 2,
                "-u" => position += 1,
                _ => {
                    positional.push(argument.as_str());
                    position += 1;
                }
            }
        }
        let name = positional.first().ok_or_else(|| {
            RecordingMuxState::failed("recording tmux set-environment omitted a name")
        })?;
        if tmux_has(args, "-u") {
            self.sessions[session_index].environment.remove(*name);
        } else {
            let value = positional.get(1).ok_or_else(|| {
                RecordingMuxState::failed("recording tmux set-environment omitted a value")
            })?;
            self.sessions[session_index]
                .environment
                .insert((*name).to_owned(), (*value).to_owned());
        }
        Ok(String::new())
    }

    fn tmux_effective_pane_launch(
        &self,
        session_index: usize,
        mut launch: MuxPaneLaunch,
    ) -> MuxPaneLaunch {
        let mut environment = self.sessions[session_index].environment.clone();
        environment.extend(launch.environment);
        launch.environment = environment;
        launch
    }

    fn tmux_select_window(&mut self, target: &str) -> Result<()> {
        match target {
            "+1" => {
                let session_index = self.active_session_index()?;
                let session_id = self.sessions[session_index].id.clone();
                self.activate_relative_window(&session_id, 1)
            }
            "-1" => {
                let session_index = self.active_session_index()?;
                let session_id = self.sessions[session_index].id.clone();
                self.activate_relative_window(&session_id, -1)
            }
            _ => {
                let (session_index, window_index) = self.tmux_window_target(target)?;
                self.set_active_window(session_index, window_index);
                Ok(())
            }
        }
    }

    fn tmux_window_target(&self, target: &str) -> Result<(usize, usize)> {
        if target.starts_with('@') {
            return self
                .sessions
                .iter()
                .enumerate()
                .find_map(|(session_index, session)| {
                    session
                        .windows
                        .iter()
                        .position(|window| window.id == target)
                        .map(|window_index| (session_index, window_index))
                })
                .ok_or_else(|| Self::stale("window", target));
        }
        if let Some((session_target, index)) = target.rsplit_once(':')
            && !session_target.is_empty()
            && let Ok(index) = index.parse::<u32>()
        {
            let session_index = self.session_index(session_target)?;
            let window_index = self.sessions[session_index]
                .windows
                .iter()
                .position(|window| window.index == index)
                .ok_or_else(|| Self::stale("window index", &index.to_string()))?;
            return Ok((session_index, window_index));
        }
        let session_index = self.session_index(target)?;
        Ok((session_index, self.active_window_index(session_index)?))
    }

    fn tmux_swap_window(&mut self, target: &str) -> Result<()> {
        let delta = match target {
            "+1" => 1,
            "-1" => -1,
            _ => {
                return Err(Self::failed(format!(
                    "recording tmux only accepts relative swap targets, got {target:?}"
                )));
            }
        };
        let session_index = self.active_session_index()?;
        let source = self.active_window_index(session_index)?;
        let destination = wrap_index(source, delta, self.sessions[session_index].windows.len());
        self.sessions[session_index]
            .windows
            .swap(source, destination);
        for (index, window) in self.sessions[session_index].windows.iter_mut().enumerate() {
            window.index = index as u32 + 1;
        }
        // tmux's following select-window relative target should land on the moved source.
        let selected_window_id = self.sessions[session_index].windows[source].id.clone();
        self.sessions[session_index].active_window_id = Some(selected_window_id);
        Ok(())
    }

    fn tmux_pane_target(&self, target: &str) -> Result<(usize, usize, usize)> {
        if target.starts_with('%') {
            return self
                .sessions
                .iter()
                .enumerate()
                .find_map(|(session_index, session)| {
                    session
                        .windows
                        .iter()
                        .enumerate()
                        .find_map(|(window_index, window)| {
                            window
                                .panes
                                .iter()
                                .position(|pane| pane.id == target)
                                .map(|pane_index| (session_index, window_index, pane_index))
                        })
                })
                .ok_or_else(|| Self::stale("pane", target));
        }
        let (session_index, window_index) = self.tmux_window_target(target)?;
        let pane_index = self.sessions[session_index].windows[window_index]
            .panes
            .iter()
            .position(|pane| {
                pane.id == self.sessions[session_index].windows[window_index].active_pane_id
            })
            .ok_or_else(|| Self::stale("active pane", target))?;
        Ok((session_index, window_index, pane_index))
    }

    fn tmux_split_window(&mut self, args: &[String]) -> Result<String> {
        let (session_index, window_index, pane_index) =
            self.tmux_pane_target(tmux_arg(args, "-t")?)?;
        let session_id = self.sessions[session_index].id.clone();
        let pane_id = self.sessions[session_index].windows[window_index].panes[pane_index]
            .id
            .clone();
        let launch = if tmux_has(args, "-P") {
            Some(self.tmux_effective_pane_launch(session_index, recording_tmux_launch_pane(args)?))
        } else {
            None
        };
        let created = self.split_pane_with_launch(&session_id, Some(&pane_id), launch)?;
        if tmux_has(args, "-P") {
            Ok(created)
        } else {
            Ok(String::new())
        }
    }

    fn tmux_select_pane(&mut self, args: &[String]) -> Result<String> {
        let target = tmux_arg(args, "-t")?;
        if let Some(base) = target.strip_suffix(".+") {
            let base = base.trim_end_matches(':');
            let (session_index, window_index) = self.tmux_window_target(base)?;
            self.set_active_window(session_index, window_index);
            self.select_relative_pane(session_index, window_index, 1)?;
            return Ok(String::new());
        }
        if let Some(base) = target.strip_suffix(".-") {
            let base = base.trim_end_matches(':');
            let (session_index, window_index) = self.tmux_window_target(base)?;
            self.set_active_window(session_index, window_index);
            self.select_relative_pane(session_index, window_index, -1)?;
            return Ok(String::new());
        }
        if let Some(title) = tmux_optional_arg(args, "-T") {
            let (session_index, window_index, pane_index) = self.tmux_pane_target(target)?;
            self.sessions[session_index].windows[window_index].panes[pane_index]
                .launch
                .title = Some(title.to_owned());
            return Ok(String::new());
        }
        let (session_index, window_index) = self.tmux_window_target(target)?;
        self.set_active_window(session_index, window_index);
        let direction = if tmux_has(args, "-L") || tmux_has(args, "-U") {
            -1
        } else if tmux_has(args, "-R") || tmux_has(args, "-D") {
            1
        } else {
            return Err(Self::failed(
                "recording tmux select-pane omitted a direction",
            ));
        };
        self.select_relative_pane(session_index, window_index, direction)?;
        Ok(String::new())
    }

    fn tmux_resize_pane(&mut self, args: &[String]) -> Result<String> {
        let (session_index, window_index, pane_index) =
            self.tmux_pane_target(tmux_arg(args, "-t")?)?;
        let session_id = self.sessions[session_index].id.clone();
        let pane_id = self.sessions[session_index].windows[window_index].panes[pane_index]
            .id
            .clone();
        if tmux_has(args, "-Z") {
            self.toggle_pane_zoom(&session_id, Some(&pane_id))?;
            return Ok(String::new());
        }
        let adjustment = tmux_resize_adjustment(args)?;
        self.resize_pane(&session_id, Some(&pane_id), adjustment)?;
        Ok(String::new())
    }
}

fn collect_launch_pane_intents(
    layout: &MuxPaneLaunchPlan,
    inherited_environment: &BTreeMap<String, String>,
    target: &mut Vec<MuxPaneLaunch>,
) {
    match layout {
        MuxPaneLaunchPlan::Pane(pane) => {
            let mut materialized = pane.clone();
            materialized.environment = pane
                .effective_environment(inherited_environment)
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect();
            target.push(materialized);
        }
        MuxPaneLaunchPlan::Split(split) => {
            collect_launch_pane_intents(&split.first, inherited_environment, target);
            collect_launch_pane_intents(&split.second, inherited_environment, target);
        }
    }
}

fn tmux_pane_pid(pane_id: &str) -> String {
    pane_id
        .strip_prefix('%')
        .and_then(|id| id.parse::<u32>().ok())
        .map(|id| (1000_u32.saturating_add(id)).to_string())
        .unwrap_or_else(|| "1000".to_owned())
}

fn tmux_arg<'a>(args: &'a [String], flag: &str) -> Result<&'a str> {
    tmux_optional_arg(args, flag).ok_or_else(|| {
        RecordingMuxState::failed(format!("recording tmux command omitted {flag:?}"))
    })
}

fn tmux_optional_arg<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|argument| argument == flag)
        .and_then(|position| args.get(position + 1))
        .map(String::as_str)
}

fn recording_tmux_launch_pane(args: &[String]) -> Result<MuxPaneLaunch> {
    let mut environment = BTreeMap::new();
    for pair in args.windows(2).filter(|pair| pair[0] == "-e") {
        let (name, value) = pair[1].split_once('=').ok_or_else(|| {
            RecordingMuxState::failed("recording tmux launch environment was malformed")
        })?;
        environment.insert(name.to_owned(), value.to_owned());
    }
    let command = recording_tmux_launch_command(args);
    Ok(MuxPaneLaunch {
        cwd: tmux_optional_arg(args, "-c").unwrap_or(".").to_owned(),
        command,
        argv: None,
        environment,
        title: None,
    })
}

fn recording_tmux_launch_command(args: &[String]) -> Option<String> {
    let mut position = 1;
    while let Some(argument) = args.get(position) {
        match argument.as_str() {
            "-d" | "-P" | "-h" | "-v" => position += 1,
            "-F" | "-s" | "-n" | "-c" | "-e" | "-t" | "-p" => position += 2,
            _ => return Some(argument.clone()),
        }
    }
    None
}

fn tmux_has(args: &[String], flag: &str) -> bool {
    args.iter().any(|argument| argument == flag)
}

fn tmux_resize_adjustment(args: &[String]) -> Result<MuxPaneResize> {
    for (flag, direction) in [
        ("-L", MuxDirection::Left),
        ("-D", MuxDirection::Down),
        ("-U", MuxDirection::Up),
        ("-R", MuxDirection::Right),
    ] {
        if let Some(position) = args.iter().position(|argument| argument == flag) {
            let cells = args
                .get(position + 1)
                .ok_or_else(|| RecordingMuxState::failed("recording tmux resize omitted cells"))?
                .parse::<u16>()
                .map_err(|_| {
                    RecordingMuxState::failed("recording tmux resize cells were invalid")
                })?;
            return Ok(MuxPaneResize::Directional { direction, cells });
        }
    }
    let columns = tmux_optional_arg(args, "-x")
        .map(str::parse::<u16>)
        .transpose()
        .map_err(|_| RecordingMuxState::failed("recording tmux resize columns were invalid"))?;
    let rows = tmux_optional_arg(args, "-y")
        .map(str::parse::<u16>)
        .transpose()
        .map_err(|_| RecordingMuxState::failed("recording tmux resize rows were invalid"))?;
    Ok(MuxPaneResize::Absolute { columns, rows })
}

fn wrap_index(index: usize, delta: i32, len: usize) -> usize {
    (index as i32 + delta).rem_euclid(len as i32) as usize
}

fn clamp_index(index: usize, delta: i32, len: usize) -> usize {
    (index as i32 + delta).clamp(0, len.saturating_sub(1) as i32) as usize
}

#[derive(Clone)]
struct RecordingRmuxSdkClient {
    calls: Rc<RefCell<Vec<Vec<String>>>>,
    state: Rc<RefCell<RecordingMuxState>>,
}

impl RecordingRmuxSdkClient {
    fn new(state: Rc<RefCell<RecordingMuxState>>) -> Self {
        Self {
            calls: Rc::new(RefCell::new(Vec::new())),
            state,
        }
    }

    fn record(&self, call: Vec<String>) {
        self.calls.borrow_mut().push(call);
    }

    fn mutate<T>(&self, mutation: impl FnOnce(&mut RecordingMuxState) -> Result<T>) -> Result<T> {
        let mut state = self.state.borrow_mut();
        state.take_fault()?;
        mutation(&mut state)
    }
}

impl RmuxSessionClient for RecordingRmuxSdkClient {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        Ok(self.state.borrow().snapshot())
    }

    fn ensure_session(&self, session_name: &str, cwd: &str) -> Result<()> {
        self.record(vec![
            "ensure_session".to_owned(),
            session_name.to_owned(),
            cwd.to_owned(),
        ]);
        self.mutate(|state| state.ensure_session(session_name, cwd))
    }

    fn rename_session(&self, session_name: &str, name: &str) -> Result<()> {
        self.record(vec![
            "rename_session".to_owned(),
            session_name.to_owned(),
            name.to_owned(),
        ]);
        self.mutate(|state| state.rename_session(session_name, name))
    }

    fn kill_session(&self, session_name: &str) -> Result<()> {
        self.record(vec!["kill_session".to_owned(), session_name.to_owned()]);
        self.mutate(|state| state.kill_session(session_name))
    }

    fn activate_window(&self, session_name: &str, window_id: &str) -> Result<()> {
        self.record(vec![
            "activate_window".to_owned(),
            session_name.to_owned(),
            window_id.to_owned(),
        ]);
        self.mutate(|state| state.activate_window(session_name, window_id))
    }

    fn rename_window(&self, session_name: &str, window_id: &str, name: &str) -> Result<()> {
        self.record(vec![
            "rename_window".to_owned(),
            session_name.to_owned(),
            window_id.to_owned(),
            name.to_owned(),
        ]);
        self.mutate(|state| state.rename_window(session_name, window_id, name))
    }

    fn new_window(&self, session_name: &str, cwd: Option<&str>) -> Result<()> {
        self.record(vec![
            "new_window".to_owned(),
            session_name.to_owned(),
            cwd.unwrap_or_default().to_owned(),
        ]);
        self.mutate(|state| state.new_window(session_name, cwd))
    }

    fn activate_next_window(&self, session_name: &str) -> Result<()> {
        self.record(vec![
            "activate_next_window".to_owned(),
            session_name.to_owned(),
        ]);
        self.mutate(|state| state.activate_relative_window(session_name, 1))
    }

    fn activate_previous_window(&self, session_name: &str) -> Result<()> {
        self.record(vec![
            "activate_previous_window".to_owned(),
            session_name.to_owned(),
        ]);
        self.mutate(|state| state.activate_relative_window(session_name, -1))
    }

    fn activate_last_window(&self, session_name: &str) -> Result<()> {
        self.record(vec![
            "activate_last_window".to_owned(),
            session_name.to_owned(),
        ]);
        self.mutate(|state| state.activate_last_window(session_name))
    }

    fn activate_window_index(&self, session_name: &str, index: u32) -> Result<()> {
        self.record(vec![
            "activate_window_index".to_owned(),
            session_name.to_owned(),
            index.to_string(),
        ]);
        self.mutate(|state| state.activate_window_index(session_name, index))
    }

    fn move_window(&self, session_name: &str, window_id: Option<&str>, delta: i32) -> Result<()> {
        self.record(vec![
            "move_window".to_owned(),
            session_name.to_owned(),
            window_id.unwrap_or_default().to_owned(),
            delta.to_string(),
        ]);
        self.mutate(|state| state.move_window(session_name, window_id, delta))
    }

    fn split_pane(
        &self,
        session_name: &str,
        pane_id: Option<&str>,
        direction: MuxSplitDirection,
    ) -> Result<()> {
        self.record(vec![
            "split_pane".to_owned(),
            session_name.to_owned(),
            pane_id.unwrap_or_default().to_owned(),
            format!("{direction:?}"),
        ]);
        self.mutate(|state| state.split_pane(session_name, pane_id).map(|_| ()))
    }

    fn close_pane(&self, session_name: &str, pane_id: Option<&str>) -> Result<()> {
        self.record(vec![
            "close_pane".to_owned(),
            session_name.to_owned(),
            pane_id.unwrap_or_default().to_owned(),
        ]);
        self.mutate(|state| state.close_pane(session_name, pane_id))
    }

    fn select_pane(
        &self,
        session_name: &str,
        window_id: Option<&str>,
        direction: MuxDirection,
    ) -> Result<()> {
        self.record(vec![
            "select_pane".to_owned(),
            session_name.to_owned(),
            window_id.unwrap_or_default().to_owned(),
            format!("{direction:?}"),
        ]);
        self.mutate(|state| state.select_pane(session_name, window_id, direction))
    }

    fn select_next_pane(&self, session_name: &str, window_id: Option<&str>) -> Result<()> {
        self.record(vec![
            "select_next_pane".to_owned(),
            session_name.to_owned(),
            window_id.unwrap_or_default().to_owned(),
        ]);
        self.mutate(|state| state.select_pane(session_name, window_id, MuxDirection::Right))
    }

    fn select_previous_pane(&self, session_name: &str, window_id: Option<&str>) -> Result<()> {
        self.record(vec![
            "select_previous_pane".to_owned(),
            session_name.to_owned(),
            window_id.unwrap_or_default().to_owned(),
        ]);
        self.mutate(|state| state.select_pane(session_name, window_id, MuxDirection::Left))
    }

    fn select_last_pane(&self, session_name: &str, window_id: Option<&str>) -> Result<()> {
        self.record(vec![
            "select_last_pane".to_owned(),
            session_name.to_owned(),
            window_id.unwrap_or_default().to_owned(),
        ]);
        self.mutate(|state| state.select_last_pane(session_name, window_id))
    }

    fn resize_pane(
        &self,
        session_name: &str,
        pane_id: Option<&str>,
        adjustment: MuxPaneResize,
    ) -> Result<()> {
        self.record(vec![
            "resize_pane".to_owned(),
            session_name.to_owned(),
            pane_id.unwrap_or_default().to_owned(),
            format!("{adjustment:?}"),
        ]);
        self.mutate(|state| state.resize_pane(session_name, pane_id, adjustment))
    }

    fn toggle_pane_zoom(&self, session_name: &str, pane_id: Option<&str>) -> Result<()> {
        self.record(vec![
            "toggle_pane_zoom".to_owned(),
            session_name.to_owned(),
            pane_id.unwrap_or_default().to_owned(),
        ]);
        self.mutate(|state| state.toggle_pane_zoom(session_name, pane_id))
    }

    fn launch_session(&self, plan: MuxSessionLaunchPlan) -> Result<()> {
        self.record(vec!["launch_session".to_owned(), plan.session_id.clone()]);
        self.mutate(|state| state.launch(&plan).map(|_| ()))
    }

    fn launch_session_with_allocation(
        &self,
        plan: MuxSessionLaunchPlan,
    ) -> Result<Option<MuxAllocatedResources>> {
        self.record(vec!["launch_session".to_owned(), plan.session_id.clone()]);
        self.mutate(|state| state.launch(&plan).map(Some))
    }

    fn session_launch_capability(
        &self,
        plan: &MuxSessionLaunchPlan,
    ) -> BindingOperationOutcome<()> {
        plan.validate().is_ok().then_some(()).map_or(
            BindingOperationOutcome::Unsupported,
            BindingOperationOutcome::Supported,
        )
    }
}

#[derive(Clone)]
struct RecordingTmuxRunner {
    calls: Rc<RefCell<Vec<Vec<String>>>>,
    state: Rc<RefCell<RecordingMuxState>>,
}

impl RecordingTmuxRunner {
    fn new(state: Rc<RefCell<RecordingMuxState>>) -> Self {
        Self {
            calls: Rc::new(RefCell::new(Vec::new())),
            state,
        }
    }

    fn record(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        let mut call = Vec::with_capacity(args.len() + 1);
        call.push(program.to_owned());
        call.extend(args.iter().cloned());
        self.calls.borrow_mut().push(call);
        if program != "tmux" {
            return Err(MuxBackendOperationError::Failed(format!(
                "recording tmux expected program \"tmux\", got {program:?}"
            ))
            .into());
        }
        let stdout = self.state.borrow_mut().tmux_command(args)?;
        Ok(CommandOutput {
            success: true,
            stdout,
            stderr: String::new(),
        })
    }
}

impl CommandRunner for RecordingTmuxRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        self.record(program, args)
    }

    fn run_disowned(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        self.record(program, args)
    }
}

#[derive(Clone, Copy)]
enum LaunchIntentEvidence {
    /// The native public snapshot exposes pane cwd but not immutable process launch details.
    SnapshotCwd,
    /// The recording rmux client receives the typed plan unchanged.
    RecordedExact,
    /// tmux exposes argv as one shell-command argument to its CLI.
    RecordedTmuxShellArgv,
}

#[derive(Clone, Copy)]
struct ContractProfile {
    name: &'static str,
    normative_command_launch: bool,
    recursive_split_launch: bool,
    launch_intent: LaunchIntentEvidence,
    last_pane: bool,
    resize: bool,
    zoom: bool,
    full_pane_inventory: bool,
    per_command_target_completion: bool,
}

enum ContractEvidence {
    Native,
    Recorded(Rc<RefCell<Vec<Vec<String>>>>),
}

struct ContractAdapter {
    backend: Box<dyn MuxBackend>,
    profile: ContractProfile,
    evidence: ContractEvidence,
    recording_state: Option<Rc<RefCell<RecordingMuxState>>>,
}

impl ContractAdapter {
    fn call_count(&self) -> usize {
        match &self.evidence {
            ContractEvidence::Native => 0,
            ContractEvidence::Recorded(calls) => calls.borrow().len(),
        }
    }

    fn fault_next(&self) {
        self.recording_state
            .as_ref()
            .expect("recording adapters expose their semantic state")
            .borrow_mut()
            .fault_next();
    }

    fn pane_geometry(&self, session_id: &str, pane_id: &str) -> (u16, u16) {
        self.recording_state
            .as_ref()
            .expect("pane geometry is observable in recording adapters")
            .borrow()
            .pane_geometry(session_id, pane_id)
            .expect("recording pane exists")
    }

    fn pane_zoomed(&self, session_id: &str, pane_id: &str) -> bool {
        self.recording_state
            .as_ref()
            .expect("pane zoom is observable in recording adapters")
            .borrow()
            .pane_zoomed(session_id, pane_id)
            .expect("recording pane exists")
    }

    fn recorded_pane_launch(
        &self,
        session_id: &str,
        window_id: &str,
        pane_id: &str,
    ) -> MuxPaneLaunch {
        self.recording_state
            .as_ref()
            .expect("recorded launch intent requires a recording adapter")
            .borrow()
            .pane_launch(session_id, window_id, pane_id)
            .expect("recorded launch pane exists")
    }
}

fn contract_adapters() -> Vec<ContractAdapter> {
    let native = ContractAdapter {
        backend: Box::new(NativeBackend::for_workspace(Path::new(
            "backend-contract-stateful-harness",
        ))),
        profile: ContractProfile {
            name: "native",
            normative_command_launch: true,
            recursive_split_launch: true,
            launch_intent: LaunchIntentEvidence::SnapshotCwd,
            last_pane: true,
            resize: true,
            zoom: true,
            full_pane_inventory: true,
            per_command_target_completion: true,
        },
        evidence: ContractEvidence::Native,
        recording_state: None,
    };

    let rmux_state = Rc::new(RefCell::new(RecordingMuxState::rmux()));
    let rmux_client = RecordingRmuxSdkClient::new(rmux_state.clone());
    let rmux = ContractAdapter {
        backend: Box::new(RmuxBackend::with_client(rmux_client.clone())),
        profile: ContractProfile {
            name: "rmux-sdk",
            normative_command_launch: true,
            recursive_split_launch: true,
            launch_intent: LaunchIntentEvidence::RecordedExact,
            last_pane: true,
            resize: true,
            zoom: true,
            full_pane_inventory: true,
            per_command_target_completion: false,
        },
        evidence: ContractEvidence::Recorded(rmux_client.calls),
        recording_state: Some(rmux_state),
    };

    let tmux_state = Rc::new(RefCell::new(RecordingMuxState::tmux()));
    let tmux_runner = RecordingTmuxRunner::new(tmux_state.clone());
    let tmux = ContractAdapter {
        backend: Box::new(TmuxBackend::with_runner("tmux", tmux_runner.clone())),
        profile: ContractProfile {
            name: "tmux",
            normative_command_launch: true,
            recursive_split_launch: true,
            launch_intent: LaunchIntentEvidence::RecordedTmuxShellArgv,
            last_pane: true,
            resize: true,
            zoom: true,
            // tmux deliberately exposes one attach anchor per window, not its full pane list.
            full_pane_inventory: false,
            per_command_target_completion: false,
        },
        evidence: ContractEvidence::Recorded(tmux_runner.calls),
        recording_state: Some(tmux_state),
    };

    vec![native, rmux, tmux]
}

fn pane(cwd: &str) -> MuxPaneLaunch {
    MuxPaneLaunch {
        cwd: cwd.to_owned(),
        command: None,
        argv: None,
        environment: BTreeMap::new(),
        title: None,
    }
}

fn launch_plan(session_id: &str) -> MuxSessionLaunchPlan {
    MuxSessionLaunchPlan {
        session_id: session_id.to_owned(),
        focus: true,
        default_cwd: "/repo".to_owned(),
        environment: BTreeMap::new(),
        windows: vec![
            MuxWindowLaunchPlan {
                name: Some("launch-one".to_owned()),
                focus: false,
                layout: MuxPaneLaunchPlan::Pane(pane("/repo/one")),
            },
            MuxWindowLaunchPlan {
                name: Some("launch-two".to_owned()),
                focus: true,
                layout: MuxPaneLaunchPlan::Pane(pane("/repo/two")),
            },
        ],
        focused_window: 1,
    }
}

fn recursive_launch_plan(session_id: &str) -> MuxSessionLaunchPlan {
    let default_cwd = "/repo";
    let mut root_cwd = pane("/repo/first");
    root_cwd.command = Some("printf '%s' first".to_owned());
    root_cwd.environment = BTreeMap::from([
        ("FIRST_ONLY".to_owned(), "one".to_owned()),
        ("OVERRIDDEN".to_owned(), "first".to_owned()),
    ]);
    root_cwd.title = Some("first pane".to_owned());

    let mut overridden_cwd = pane("/repo/overridden");
    overridden_cwd.argv = Some(vec![
        "worker".to_owned(),
        "--watch".to_owned(),
        "two words".to_owned(),
    ]);
    overridden_cwd.environment = BTreeMap::from([("SECOND_ONLY".to_owned(), "two".to_owned())]);
    overridden_cwd.title = Some("second pane".to_owned());

    let mut nested_cwd = pane("/repo/nested");
    nested_cwd.environment = BTreeMap::from([
        ("OVERRIDDEN".to_owned(), "third".to_owned()),
        ("THIRD_ONLY".to_owned(), "three".to_owned()),
    ]);
    nested_cwd.title = Some("third pane".to_owned());

    MuxSessionLaunchPlan {
        session_id: session_id.to_owned(),
        focus: true,
        default_cwd: default_cwd.to_owned(),
        environment: BTreeMap::from([
            ("OVERRIDDEN".to_owned(), "session".to_owned()),
            ("SESSION_ONLY".to_owned(), "shared".to_owned()),
        ]),
        windows: vec![MuxWindowLaunchPlan {
            name: Some("recursive".to_owned()),
            focus: true,
            layout: MuxPaneLaunchPlan::Split(MuxSplitLaunch {
                direction: MuxSplitDirection::Right,
                ratio_millis: 500,
                first: Box::new(MuxPaneLaunchPlan::Pane(root_cwd)),
                second: Box::new(MuxPaneLaunchPlan::Split(MuxSplitLaunch {
                    direction: MuxSplitDirection::Down,
                    ratio_millis: 500,
                    first: Box::new(MuxPaneLaunchPlan::Pane(overridden_cwd)),
                    second: Box::new(MuxPaneLaunchPlan::Pane(nested_cwd)),
                })),
            }),
        }],
        focused_window: 0,
    }
}

fn command_launch_plan(session_id: &str) -> MuxSessionLaunchPlan {
    let mut plan = launch_plan(session_id);
    plan.windows.truncate(1);
    plan.focused_window = 0;
    let MuxPaneLaunchPlan::Pane(pane) = &mut plan.windows[0].layout else {
        unreachable!("command launch fixture has one root pane");
    };
    pane.command = Some("printf '%s' normative".to_owned());
    plan
}

fn invalid_launch_plan() -> MuxSessionLaunchPlan {
    let mut plan = launch_plan("contract-invalid");
    plan.default_cwd.clear();
    plan
}

fn assert_supported(label: &str, outcome: BindingOperationOutcome<Result<()>>) {
    match outcome {
        BindingOperationOutcome::Supported(Ok(())) => {}
        BindingOperationOutcome::Supported(Err(error)) => panic!("{label} failed: {error:#}"),
        BindingOperationOutcome::Unsupported => panic!("{label} was unexpectedly unsupported"),
        BindingOperationOutcome::Unavailable => panic!("{label} was unexpectedly unavailable"),
        BindingOperationOutcome::Denied => panic!("{label} was unexpectedly denied"),
        BindingOperationOutcome::Stale => panic!("{label} was unexpectedly stale"),
    }
}

fn assert_unsupported(label: &str, outcome: BindingOperationOutcome<Result<()>>) {
    assert!(
        matches!(outcome, BindingOperationOutcome::Unsupported),
        "{label} should be rejected as unsupported"
    );
}

fn assert_failed(label: &str, outcome: BindingOperationOutcome<Result<()>>) {
    let BindingOperationOutcome::Supported(Err(error)) = outcome else {
        panic!("{label} should have reported a backend failure");
    };
    assert!(
        matches!(
            error.downcast_ref::<MuxBackendOperationError>(),
            Some(MuxBackendOperationError::Failed(_))
        ),
        "{label} should retain MuxBackendOperationError::Failed, got {error:#}"
    );
}

fn assert_stale(label: &str, outcome: BindingOperationOutcome<Result<()>>) {
    let BindingOperationOutcome::Supported(Err(error)) = outcome else {
        panic!("{label} should have reported a stale target");
    };
    assert!(
        matches!(
            error.downcast_ref::<MuxBackendOperationError>(),
            Some(MuxBackendOperationError::Stale(_))
        ),
        "{label} should retain MuxBackendOperationError::Stale, got {error:#}"
    );
}

fn snapshot(adapter: &ContractAdapter, label: &str) -> MuxSnapshot {
    adapter.backend.snapshot().unwrap_or_else(|error| {
        panic!(
            "{label}: {} snapshot failed: {error:#}",
            adapter.profile.name
        )
    })
}

fn session_by_id<'a>(snapshot: &'a MuxSnapshot, id: &str) -> &'a MuxSession {
    snapshot
        .sessions
        .iter()
        .find(|session| session.id == id)
        .unwrap_or_else(|| panic!("snapshot did not contain session id {id:?}: {snapshot:?}"))
}

fn session_by_name<'a>(snapshot: &'a MuxSnapshot, name: &str) -> &'a MuxSession {
    snapshot
        .sessions
        .iter()
        .find(|session| session.name == name)
        .unwrap_or_else(|| panic!("snapshot did not contain session name {name:?}: {snapshot:?}"))
}

fn window_by_id<'a>(session: &'a MuxSession, id: &str) -> &'a MuxWindow {
    session
        .windows
        .iter()
        .find(|window| window.id == id)
        .unwrap_or_else(|| panic!("session {:?} did not contain window id {id:?}", session.id))
}

fn active_window(session: &MuxSession) -> &MuxWindow {
    let id = session
        .active_window_id
        .as_deref()
        .unwrap_or_else(|| panic!("session {:?} had no active window", session.id));
    window_by_id(session, id)
}

fn active_pane_id(window: &MuxWindow) -> String {
    window
        .anchor
        .pane_id
        .clone()
        .unwrap_or_else(|| panic!("window {:?} had no active pane", window.id))
}

fn session_target(session_id: &str) -> MuxEventTarget {
    MuxEventTarget::session(session_id)
}

fn window_target(session_id: &str, window_id: &str) -> MuxEventTarget {
    let mut target = session_target(session_id);
    target.window_id = Some(window_id.to_owned());
    target
}

fn assert_capability_descriptor(adapter: &ContractAdapter, scope: MuxScope) {
    let descriptor = adapter.backend.capabilities(scope);
    for operation in [
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
    ] {
        assert!(
            descriptor.supports(operation),
            "{} must declare {operation:?}",
            adapter.profile.name
        );
    }
    assert_eq!(
        descriptor.supports(BindingOperation::LastPane),
        adapter.profile.last_pane,
        "{} last-pane descriptor truth",
        adapter.profile.name
    );
    assert_eq!(
        descriptor.supports(BindingOperation::ResizePane),
        adapter.profile.resize,
        "{} resize descriptor truth",
        adapter.profile.name
    );
    assert_eq!(
        descriptor.supports(BindingOperation::TogglePaneZoom),
        adapter.profile.zoom,
        "{} zoom descriptor truth",
        adapter.profile.name
    );

    let request = descriptor.request(BindingOperation::RenameSession);
    let mut invoked = 0;
    assert_eq!(
        descriptor.invoke(request, BindingOperationAvailability::Unavailable, || {
            invoked += 1;
        }),
        BindingOperationOutcome::Unavailable,
        "{} must preserve unavailable capability state",
        adapter.profile.name
    );
    assert_eq!(
        descriptor.invoke(request, BindingOperationAvailability::Denied, || {
            invoked += 1;
        }),
        BindingOperationOutcome::Denied,
        "{} must preserve denied capability state",
        adapter.profile.name
    );
    let mut stale_request = request;
    stale_request.descriptor_version = stale_request.descriptor_version.saturating_add(1);
    assert_eq!(
        descriptor.invoke(
            stale_request,
            BindingOperationAvailability::Available,
            || {
                invoked += 1;
            }
        ),
        BindingOperationOutcome::Stale,
        "{} must preserve stale capability state",
        adapter.profile.name
    );
    assert_eq!(
        invoked, 0,
        "{} must not run a rejected capability operation",
        adapter.profile.name
    );
}

fn assert_launch_completion(
    adapter: &mut ContractAdapter,
    plan: &MuxSessionLaunchPlan,
    snapshot: &MuxSnapshot,
) {
    let completion = adapter
        .backend
        .take_authoritative_completion()
        .unwrap_or_else(|| panic!("{} omitted launch completion", adapter.profile.name));
    let target = completion.target;
    let allocated = completion.allocated.unwrap_or_else(|| {
        panic!(
            "{} omitted launch allocation for {:?}",
            adapter.profile.name, plan.session_id
        )
    });
    assert_eq!(
        target,
        Some(session_target(&allocated.session_id)),
        "{} launch completion must target the allocated backend session id",
        adapter.profile.name
    );
    assert_eq!(
        allocated.windows.len(),
        plan.windows.len(),
        "{} launch window cardinality",
        adapter.profile.name
    );
    let session = session_by_id(snapshot, &allocated.session_id);
    assert_eq!(
        session.name, plan.session_id,
        "launch name remains discoverable"
    );
    assert_eq!(
        session.windows.len(),
        plan.windows.len(),
        "{} launch snapshot window cardinality",
        adapter.profile.name
    );
    assert_eq!(
        session.active_window_id.as_deref(),
        Some(allocated.windows[plan.focused_window].window_id.as_str()),
        "{} launch focused window must resolve to its allocated ref",
        adapter.profile.name
    );

    let mut seen_windows = Vec::new();
    let mut seen_panes = Vec::new();
    for (window_index, (allocation, plan_window)) in
        allocated.windows.iter().zip(&plan.windows).enumerate()
    {
        assert!(
            !allocation.window_id.is_empty() && !seen_windows.contains(&allocation.window_id),
            "{} launch returned a unique non-empty window id at index {window_index}",
            adapter.profile.name
        );
        seen_windows.push(allocation.window_id.clone());
        assert_eq!(
            allocation.pane_ids.len(),
            plan_window.layout.pane_count(),
            "{} launch pane cardinality for {:?}",
            adapter.profile.name,
            allocation.window_id
        );
        let window = window_by_id(session, &allocation.window_id);
        assert_eq!(
            window.name,
            plan_window
                .name
                .clone()
                .unwrap_or_else(|| "shell".to_owned()),
            "{} launch window name for {:?}",
            adapter.profile.name,
            allocation.window_id
        );
        if adapter.profile.full_pane_inventory {
            let snapshot_panes = window
                .panes
                .iter()
                .filter_map(|pane| pane.pane_id.clone())
                .collect::<Vec<_>>();
            assert_eq!(
                snapshot_panes, allocation.pane_ids,
                "{} snapshot must preserve the allocated pane refs in declaration order",
                adapter.profile.name
            );
        } else {
            let anchor = active_pane_id(window);
            assert!(
                allocation.pane_ids.contains(&anchor),
                "{} attach anchor must reference one allocated pane",
                adapter.profile.name
            );
            assert_eq!(
                window.panes.len(),
                1,
                "{} truthfully exposes only its attach anchor",
                adapter.profile.name
            );
        }
        for pane_id in &allocation.pane_ids {
            assert!(
                !pane_id.is_empty() && !seen_panes.contains(pane_id),
                "{} launch returned a unique non-empty pane id {pane_id:?}",
                adapter.profile.name
            );
            seen_panes.push(pane_id.clone());
        }
    }
    assert_launch_pane_intent(adapter, plan, session, &allocated);
}

fn assert_launch_pane_intent(
    adapter: &ContractAdapter,
    plan: &MuxSessionLaunchPlan,
    session: &MuxSession,
    allocated: &MuxAllocatedResources,
) {
    for (window_index, (allocation, plan_window)) in
        allocated.windows.iter().zip(&plan.windows).enumerate()
    {
        let window = window_by_id(session, &allocation.window_id);
        let mut declared_panes = Vec::new();
        collect_declared_launch_panes(&plan_window.layout, &mut declared_panes);
        assert_eq!(
            allocation.pane_ids.len(),
            declared_panes.len(),
            "{} launch intent pane cardinality for window {window_index}",
            adapter.profile.name
        );

        for (pane_index, (pane_id, declared)) in
            allocation.pane_ids.iter().zip(declared_panes).enumerate()
        {
            let expected = effective_launch_pane(declared, &plan.environment);
            let label = format!(
                "{} launch intent for declared pane {window_index}:{pane_index} ({pane_id})",
                adapter.profile.name
            );
            match adapter.profile.launch_intent {
                LaunchIntentEvidence::SnapshotCwd => {
                    let actual = window
                        .panes
                        .iter()
                        .find(|pane| pane.pane_id.as_deref() == Some(pane_id.as_str()))
                        .unwrap_or_else(|| panic!("{label} was absent from the snapshot"));
                    assert_eq!(
                        actual.cwd.as_deref(),
                        Some(expected.cwd.as_str()),
                        "{label} must preserve its declared cwd"
                    );
                }
                LaunchIntentEvidence::RecordedExact => {
                    assert_eq!(
                        adapter.recorded_pane_launch(
                            &allocated.session_id,
                            &allocation.window_id,
                            pane_id,
                        ),
                        expected,
                        "{label} must preserve cwd, process, environment, and title"
                    );
                }
                LaunchIntentEvidence::RecordedTmuxShellArgv => {
                    let actual = adapter.recorded_pane_launch(
                        &allocated.session_id,
                        &allocation.window_id,
                        pane_id,
                    );
                    assert_eq!(
                        actual.cwd, expected.cwd,
                        "{label} must preserve its declared cwd"
                    );
                    assert_eq!(
                        actual.environment, expected.environment,
                        "{label} must merge inherited environment before pane overrides"
                    );
                    assert_eq!(
                        actual.title, expected.title,
                        "{label} must preserve its title"
                    );
                    assert_tmux_launch_process(&actual, &expected, &label);
                }
            }
        }
    }
}

fn collect_declared_launch_panes<'a>(
    layout: &'a MuxPaneLaunchPlan,
    target: &mut Vec<&'a MuxPaneLaunch>,
) {
    match layout {
        MuxPaneLaunchPlan::Pane(pane) => target.push(pane),
        MuxPaneLaunchPlan::Split(split) => {
            collect_declared_launch_panes(&split.first, target);
            collect_declared_launch_panes(&split.second, target);
        }
    }
}

fn effective_launch_pane(
    pane: &MuxPaneLaunch,
    session_environment: &BTreeMap<String, String>,
) -> MuxPaneLaunch {
    let mut materialized = pane.clone();
    materialized.environment = pane
        .effective_environment(session_environment)
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
    materialized
}

fn assert_tmux_launch_process(actual: &MuxPaneLaunch, expected: &MuxPaneLaunch, label: &str) {
    assert!(
        actual.argv.is_none(),
        "{label} must send tmux its one process argument rather than fabricate argv"
    );
    match (&expected.command, &expected.argv) {
        (Some(command), None) => assert_eq!(
            actual.command.as_deref(),
            Some(command.as_str()),
            "{label} must preserve its shell command"
        ),
        (None, Some(argv)) => {
            let command = expected_tmux_argv_command(argv);
            assert_eq!(
                actual.command.as_deref(),
                Some(command.as_str()),
                "{label} must preserve its argv through tmux's shell-command form"
            );
        }
        (None, None) => assert_eq!(
            actual.command, None,
            "{label} must not invent a process command"
        ),
        (Some(_), Some(_)) => unreachable!("launch plan validation rejects command plus argv"),
    }
}

fn expected_tmux_argv_command(argv: &[String]) -> String {
    let mut command = String::from("exec");
    for argument in argv {
        command.push(' ');
        command.push('\'');
        let mut segments = argument.split('\'');
        if let Some(first) = segments.next() {
            command.push_str(first);
        }
        for segment in segments {
            command.push_str("'\"'\"'");
            command.push_str(segment);
        }
        command.push('\'');
    }
    command
}

fn assert_direct_target(adapter: &mut ContractAdapter, target: MuxEventTarget) {
    let completion = adapter.backend.take_authoritative_completion();
    if adapter.profile.per_command_target_completion {
        let completion = completion.unwrap_or_else(|| {
            panic!(
                "{} omitted authoritative mutation target {target:?}",
                adapter.profile.name
            )
        });
        assert_eq!(
            completion.target,
            Some(target),
            "{} reported the wrong mutation target",
            adapter.profile.name
        );
        assert!(
            completion.allocated.is_none(),
            "{} should not fabricate launch allocation for a regular mutation",
            adapter.profile.name
        );
    } else {
        assert!(
            completion.is_none(),
            "{} must not leave an old completion attached to a later mutation",
            adapter.profile.name
        );
    }
}

fn assert_created_session_completion(adapter: &mut ContractAdapter, session: &MuxSession) {
    let completion = adapter.backend.take_authoritative_completion();
    if adapter.profile.per_command_target_completion {
        let completion = completion.unwrap_or_else(|| {
            panic!(
                "{} omitted authoritative session creation completion for {:?}",
                adapter.profile.name, session.id
            )
        });
        assert_eq!(
            completion.target,
            Some(session_target(&session.id)),
            "{} reported the wrong created session target",
            adapter.profile.name
        );
        let allocated = completion.allocated.unwrap_or_else(|| {
            panic!(
                "{} omitted authoritative allocation for created session {:?}",
                adapter.profile.name, session.id
            )
        });
        assert_eq!(allocated.session_id, session.id);
        assert_eq!(allocated.windows.len(), 1);
        assert_eq!(allocated.windows[0].window_id, session.windows[0].id);
        assert!(
            allocated.windows[0]
                .pane_ids
                .contains(&active_pane_id(&session.windows[0]))
        );
    } else {
        assert!(
            completion.is_none(),
            "{} must not fabricate a completion without authoritative resource IDs",
            adapter.profile.name
        );
    }
}

fn assert_no_completion(adapter: &mut ContractAdapter, label: &str) {
    assert!(
        adapter.backend.take_authoritative_completion().is_none(),
        "{} {label} left a stale completion behind",
        adapter.profile.name
    );
}

fn discard_completion(adapter: &mut ContractAdapter) {
    let _ = adapter.backend.take_authoritative_completion();
}

fn assert_invalid_launch_is_rejected(adapter: &mut ContractAdapter) {
    let plan = invalid_launch_plan();
    let before = snapshot(adapter, "before invalid launch");
    let calls = adapter.call_count();
    assert!(matches!(
        adapter.backend.session_launch_capability(&plan),
        BindingOperationOutcome::Unsupported
    ));
    assert_unsupported(
        "invalid recursive launch",
        adapter.backend.execute_session_launch(plan),
    );
    assert_eq!(
        adapter.call_count(),
        calls,
        "invalid launch must not reach a backend adapter"
    );
    assert_eq!(snapshot(adapter, "after invalid launch"), before);
    assert_no_completion(adapter, "invalid launch");
}

fn assert_optional_launch(
    adapter: &mut ContractAdapter,
    plan: MuxSessionLaunchPlan,
    supported: bool,
    label: &str,
) {
    let before = snapshot(adapter, &format!("before {label}"));
    let calls = adapter.call_count();
    assert_eq!(
        matches!(
            adapter.backend.session_launch_capability(&plan),
            BindingOperationOutcome::Supported(())
        ),
        supported,
        "{} {label} preflight truth",
        adapter.profile.name
    );
    let outcome = adapter.backend.execute_session_launch(plan.clone());
    if supported {
        assert_supported(label, outcome);
        let after = snapshot(adapter, &format!("after {label}"));
        assert_launch_completion(adapter, &plan, &after);
    } else {
        assert_unsupported(label, outcome);
        assert_eq!(
            adapter.call_count(),
            calls,
            "{label} must not reach an incapable backend"
        );
        assert_eq!(snapshot(adapter, &format!("after {label}")), before);
        assert_no_completion(adapter, label);
    }
}

fn run_contract(mut adapter: ContractAdapter) {
    let scope = MuxScope::new(
        SpaceId::from_persistence(91),
        BindingId::from_persistence(92),
    );
    assert_capability_descriptor(&adapter, scope);
    assert_invalid_launch_is_rejected(&mut adapter);

    let launch = launch_plan("contract-launch");
    assert!(matches!(
        adapter.backend.session_launch_capability(&launch),
        BindingOperationOutcome::Supported(())
    ));
    assert_supported(
        "two-window launch",
        adapter.backend.execute_session_launch(launch.clone()),
    );
    let launch_snapshot = snapshot(&adapter, "after two-window launch");
    assert_launch_completion(&mut adapter, &launch, &launch_snapshot);

    let supports_normative_command_launch = adapter.profile.normative_command_launch;
    assert_optional_launch(
        &mut adapter,
        command_launch_plan("contract-command"),
        supports_normative_command_launch,
        "normative command launch",
    );
    let supports_recursive_split_launch = adapter.profile.recursive_split_launch;
    assert_optional_launch(
        &mut adapter,
        recursive_launch_plan("contract-recursive"),
        supports_recursive_split_launch,
        "recursive split launch",
    );

    let project_name = "contract-project";
    assert_supported(
        "project session create",
        adapter.backend.execute_checked(
            scope,
            MuxCommand::CreateProjectSession {
                session_id: project_name.to_owned(),
                cwd: "/repo".to_owned(),
            },
            None,
        ),
    );
    let mut current = snapshot(&adapter, "after project create");
    let project_session_id = session_by_name(&current, project_name).id.clone();
    let project = session_by_id(&current, &project_session_id);
    assert_eq!(
        project.windows.len(),
        1,
        "project create window cardinality"
    );
    let primary_window_id = active_window(project).id.clone();
    let primary_pane_id = active_pane_id(active_window(project));
    assert_eq!(
        active_window(project).anchor.cwd.as_deref(),
        Some("/repo"),
        "project create preserves its cwd"
    );
    assert_created_session_completion(&mut adapter, project);

    assert_supported(
        "worktree session create",
        adapter.backend.execute_checked(
            scope,
            MuxCommand::CreateWorktreeSession {
                session_id: "contract-worktree".to_owned(),
                cwd: "/worktree".to_owned(),
            },
            None,
        ),
    );
    current = snapshot(&adapter, "after worktree create");
    let worktree_session_id = session_by_name(&current, "contract-worktree").id.clone();
    assert_eq!(
        active_window(session_by_id(&current, &worktree_session_id))
            .anchor
            .cwd
            .as_deref(),
        Some("/worktree"),
        "worktree create preserves its cwd"
    );
    assert_created_session_completion(&mut adapter, session_by_id(&current, &worktree_session_id));
    assert_supported(
        "worktree session ditch",
        adapter.backend.execute_checked(
            scope,
            MuxCommand::DitchSession {
                session_id: worktree_session_id.clone(),
            },
            None,
        ),
    );
    current = snapshot(&adapter, "after worktree ditch");
    assert!(
        !current
            .sessions
            .iter()
            .any(|session| session.id == worktree_session_id),
        "ditch removes exactly the requested worktree session"
    );
    assert_direct_target(&mut adapter, session_target(&worktree_session_id));

    assert_supported(
        "new window",
        adapter.backend.execute_checked(
            scope,
            MuxCommand::NewWindow {
                session_id: project_session_id.clone(),
                cwd: Some("/repo/child".to_owned()),
            },
            None,
        ),
    );
    current = snapshot(&adapter, "after new window");
    let project = session_by_id(&current, &project_session_id);
    assert_eq!(project.windows.len(), 2, "new window changes cardinality");
    let secondary_window_id = project
        .windows
        .iter()
        .find(|window| window.id != primary_window_id)
        .map(|window| window.id.clone())
        .expect("new window has a distinct backend id");
    assert_eq!(active_window(project).id, secondary_window_id);
    assert_eq!(
        active_window(project).anchor.cwd.as_deref(),
        Some("/repo/child"),
        "new window receives its requested cwd"
    );

    assert_supported(
        "rename window",
        adapter.backend.execute_checked(
            scope,
            MuxCommand::RenameWindow {
                session_id: project_session_id.clone(),
                window_id: secondary_window_id.clone(),
                name: "renamed-child".to_owned(),
            },
            None,
        ),
    );
    current = snapshot(&adapter, "after rename window");
    assert_eq!(
        window_by_id(
            session_by_id(&current, &project_session_id),
            &secondary_window_id
        )
        .name,
        "renamed-child"
    );
    assert_direct_target(
        &mut adapter,
        window_target(&project_session_id, &secondary_window_id),
    );

    assert_supported(
        "activate first window",
        adapter.backend.execute_checked(
            scope,
            MuxCommand::ActivateWindow {
                session_id: project_session_id.clone(),
                window_id: primary_window_id.clone(),
            },
            None,
        ),
    );
    current = snapshot(&adapter, "after first window activation");
    assert_eq!(
        active_window(session_by_id(&current, &project_session_id)).id,
        primary_window_id
    );
    assert_direct_target(
        &mut adapter,
        window_target(&project_session_id, &primary_window_id),
    );

    for (label, command, expected_window) in [
        (
            "next window",
            MuxCommand::ActivateNextWindow {
                session_id: project_session_id.clone(),
            },
            secondary_window_id.clone(),
        ),
        (
            "previous window",
            MuxCommand::ActivatePreviousWindow {
                session_id: project_session_id.clone(),
            },
            primary_window_id.clone(),
        ),
        (
            "last window",
            MuxCommand::ActivateLastWindow {
                session_id: project_session_id.clone(),
            },
            secondary_window_id.clone(),
        ),
    ] {
        assert_supported(label, adapter.backend.execute_checked(scope, command, None));
        current = snapshot(&adapter, label);
        assert_eq!(
            active_window(session_by_id(&current, &project_session_id)).id,
            expected_window,
            "{} {label} selected the wrong window",
            adapter.profile.name
        );
    }

    assert_supported(
        "move selected window",
        adapter.backend.execute_checked(
            scope,
            MuxCommand::MoveWindow {
                session_id: project_session_id.clone(),
                window_id: Some(secondary_window_id.clone()),
                delta: -1,
            },
            None,
        ),
    );
    current = snapshot(&adapter, "after move window");
    let project = session_by_id(&current, &project_session_id);
    assert_eq!(
        project
            .windows
            .iter()
            .map(|window| window.id.as_str())
            .collect::<Vec<_>>(),
        vec![secondary_window_id.as_str(), primary_window_id.as_str()],
        "move reorders the exact targeted window"
    );
    assert_eq!(
        project
            .windows
            .iter()
            .map(|window| window.index)
            .collect::<Vec<_>>(),
        vec![1, 2],
        "move renumbers the listed windows"
    );
    assert_eq!(active_window(project).id, secondary_window_id);
    assert_direct_target(
        &mut adapter,
        window_target(&project_session_id, &secondary_window_id),
    );

    assert_supported(
        "activate window index",
        adapter.backend.execute_checked(
            scope,
            MuxCommand::ActivateWindowIndex {
                session_id: project_session_id.clone(),
                index: 2,
            },
            None,
        ),
    );
    current = snapshot(&adapter, "after window index activation");
    assert_eq!(
        active_window(session_by_id(&current, &project_session_id)).id,
        primary_window_id,
        "index activation follows the post-move list"
    );
    assert_supported(
        "reactivate split window",
        adapter.backend.execute_checked(
            scope,
            MuxCommand::ActivateWindow {
                session_id: project_session_id.clone(),
                window_id: secondary_window_id.clone(),
            },
            None,
        ),
    );
    current = snapshot(&adapter, "before pane mutations");
    let source_pane_id = active_pane_id(window_by_id(
        session_by_id(&current, &project_session_id),
        &secondary_window_id,
    ));

    assert_supported(
        "split exact pane",
        adapter.backend.execute_checked(
            scope,
            MuxCommand::SplitPane {
                session_id: project_session_id.clone(),
                pane_id: Some(source_pane_id.clone()),
                direction: MuxSplitDirection::Right,
            },
            None,
        ),
    );
    current = snapshot(&adapter, "after split pane");
    let split_window = window_by_id(
        session_by_id(&current, &project_session_id),
        &secondary_window_id,
    );
    let split_pane_id = active_pane_id(split_window);
    assert_ne!(
        split_pane_id, source_pane_id,
        "split allocates a distinct pane ref"
    );
    if adapter.profile.full_pane_inventory {
        assert_eq!(
            split_window.panes.len(),
            2,
            "split changes pane cardinality"
        );
        assert!(
            split_window
                .panes
                .iter()
                .filter_map(|pane| pane.pane_id.as_deref())
                .eq([source_pane_id.as_str(), split_pane_id.as_str()]),
            "split snapshot preserves both exact pane refs"
        );
    } else {
        assert_eq!(
            split_window.panes.len(),
            1,
            "tmux exposes its active attach pane only"
        );
        assert_eq!(
            split_window.anchor.pane_id.as_deref(),
            Some(split_pane_id.as_str())
        );
    }

    for (label, command, expected_pane) in [
        (
            "previous pane",
            MuxCommand::SelectPreviousPane {
                session_id: project_session_id.clone(),
                window_id: None,
            },
            source_pane_id.clone(),
        ),
        (
            "next pane",
            MuxCommand::SelectNextPane {
                session_id: project_session_id.clone(),
                window_id: None,
            },
            split_pane_id.clone(),
        ),
        (
            "directional pane",
            MuxCommand::SelectPane {
                session_id: project_session_id.clone(),
                window_id: Some(secondary_window_id.clone()),
                direction: MuxDirection::Left,
            },
            source_pane_id.clone(),
        ),
    ] {
        assert_supported(label, adapter.backend.execute_checked(scope, command, None));
        current = snapshot(&adapter, label);
        assert_eq!(
            active_pane_id(active_window(session_by_id(&current, &project_session_id))),
            expected_pane,
            "{} {label} selected the wrong pane",
            adapter.profile.name
        );
    }

    discard_completion(&mut adapter);

    let last_before = snapshot(&adapter, "before last pane");
    let last = adapter.backend.execute_checked(
        scope,
        MuxCommand::SelectLastPane {
            session_id: project_session_id.clone(),
            window_id: None,
        },
        None,
    );
    if adapter.profile.last_pane {
        assert_supported("last pane", last);
        current = snapshot(&adapter, "after last pane");
        assert_eq!(
            active_pane_id(active_window(session_by_id(&current, &project_session_id))),
            split_pane_id,
            "last-pane restores the prior exact pane"
        );
    } else {
        assert_unsupported("native last pane", last);
        assert_eq!(
            snapshot(&adapter, "after unsupported last pane"),
            last_before
        );
        assert_no_completion(&mut adapter, "unsupported last pane");
    }

    let invalid_resize = MuxPaneResize::Directional {
        direction: MuxDirection::Right,
        cells: 0,
    };
    let invalid_before = snapshot(&adapter, "before invalid resize");
    let calls_before = adapter.call_count();
    let invalid_outcome = adapter.backend.execute_checked(
        scope,
        MuxCommand::ResizePane {
            session_id: project_session_id.clone(),
            pane_id: Some(split_pane_id.clone()),
            adjustment: invalid_resize,
        },
        None,
    );
    if adapter.profile.resize {
        assert!(
            matches!(invalid_outcome, BindingOperationOutcome::Supported(Err(_))),
            "{} invalid resize must fail rather than silently succeed",
            adapter.profile.name
        );
    } else {
        assert_unsupported("native invalid resize", invalid_outcome);
    }
    assert_eq!(
        adapter.call_count(),
        calls_before,
        "invalid resize must not reach a backend adapter"
    );
    assert_eq!(snapshot(&adapter, "after invalid resize"), invalid_before);
    assert_no_completion(&mut adapter, "invalid resize");

    let resize_before = snapshot(&adapter, "before resize");
    let geometry_before = adapter
        .recording_state
        .as_ref()
        .filter(|_| adapter.profile.resize)
        .map(|_| adapter.pane_geometry(&project_session_id, &split_pane_id));
    let resize = adapter.backend.execute_checked(
        scope,
        MuxCommand::ResizePane {
            session_id: project_session_id.clone(),
            pane_id: Some(split_pane_id.clone()),
            adjustment: MuxPaneResize::Directional {
                direction: MuxDirection::Right,
                cells: 3,
            },
        },
        None,
    );
    if let Some((columns, rows)) = geometry_before {
        assert_supported("resize pane", resize);
        assert_eq!(
            adapter.pane_geometry(&project_session_id, &split_pane_id),
            (columns.saturating_add(3), rows),
            "resize reaches the selected exact pane"
        );
    } else if adapter.profile.resize {
        assert_supported("resize pane", resize);
        assert!(
            adapter.backend.take_authoritative_completion().is_some(),
            "native resize publishes an authoritative completion"
        );
    } else {
        assert_unsupported("native resize pane", resize);
        assert_eq!(
            snapshot(&adapter, "after unsupported resize"),
            resize_before
        );
        assert_no_completion(&mut adapter, "unsupported resize");
    }

    let zoom_before = snapshot(&adapter, "before zoom");
    let zoom = adapter.backend.execute_checked(
        scope,
        MuxCommand::TogglePaneZoom {
            session_id: project_session_id.clone(),
            pane_id: Some(split_pane_id.clone()),
        },
        None,
    );
    if adapter.profile.zoom {
        assert_supported("toggle zoom", zoom);
        if adapter.recording_state.is_some() {
            assert!(
                adapter.pane_zoomed(&project_session_id, &split_pane_id),
                "zoom reaches the selected exact pane"
            );
        } else {
            assert!(
                adapter.backend.take_authoritative_completion().is_some(),
                "native zoom publishes an authoritative completion"
            );
        }
    } else {
        assert_unsupported("native toggle zoom", zoom);
        assert_eq!(snapshot(&adapter, "after unsupported zoom"), zoom_before);
        assert_no_completion(&mut adapter, "unsupported zoom");
    }

    assert_supported(
        "kill selected source pane",
        adapter.backend.execute_checked(
            scope,
            MuxCommand::KillPane {
                session_id: project_session_id.clone(),
                pane_id: Some(source_pane_id.clone()),
            },
            None,
        ),
    );
    current = snapshot(&adapter, "after kill pane");
    let split_window = window_by_id(
        session_by_id(&current, &project_session_id),
        &secondary_window_id,
    );
    assert_eq!(active_pane_id(split_window), split_pane_id);
    if adapter.profile.full_pane_inventory {
        assert_eq!(split_window.panes.len(), 1, "kill removes exactly one pane");
        assert_eq!(
            split_window.panes[0].pane_id.as_deref(),
            Some(split_pane_id.as_str())
        );
    }

    discard_completion(&mut adapter);

    let stale_before = snapshot(&adapter, "before stale pane close");
    assert_stale(
        "removed pane close",
        adapter.backend.execute_checked(
            scope,
            MuxCommand::ClosePane {
                session_id: project_session_id.clone(),
                pane_id: Some(source_pane_id.clone()),
            },
            None,
        ),
    );
    assert_eq!(
        snapshot(&adapter, "after stale pane close"),
        stale_before,
        "a stale target must not turn into a silent no-op"
    );
    assert_no_completion(&mut adapter, "stale pane close");

    assert_supported(
        "close final pane",
        adapter.backend.execute_checked(
            scope,
            MuxCommand::ClosePane {
                session_id: project_session_id.clone(),
                pane_id: Some(split_pane_id.clone()),
            },
            None,
        ),
    );
    current = snapshot(&adapter, "after close final pane");
    let project = session_by_id(&current, &project_session_id);
    assert_eq!(
        project.windows.len(),
        1,
        "close cascades an empty window away"
    );
    assert_eq!(active_window(project).id, primary_window_id);
    assert_eq!(active_pane_id(active_window(project)), primary_pane_id);

    discard_completion(&mut adapter);

    let failed_before = snapshot(&adapter, "before failed mutation");
    if adapter.recording_state.is_some() {
        adapter.fault_next();
        assert_failed(
            "backend mutation failure",
            adapter.backend.execute_checked(
                scope,
                MuxCommand::RenameWindow {
                    session_id: project_session_id.clone(),
                    window_id: primary_window_id.clone(),
                    name: "must-not-apply".to_owned(),
                },
                None,
            ),
        );
    } else {
        assert_failed(
            "native invalid launch",
            adapter.backend.execute_checked(
                scope,
                MuxCommand::CreateSession {
                    plan: invalid_launch_plan(),
                },
                None,
            ),
        );
    }
    assert_eq!(
        snapshot(&adapter, "after failed mutation"),
        failed_before,
        "failed mutation must not silently change state"
    );
    assert_no_completion(&mut adapter, "failed mutation");

    assert_supported(
        "rename session",
        adapter.backend.execute_checked(
            scope,
            MuxCommand::RenameSession {
                session_id: project_session_id.clone(),
                name: "contract-project-renamed".to_owned(),
            },
            None,
        ),
    );
    current = snapshot(&adapter, "after session rename");
    assert_eq!(
        session_by_id(&current, &project_session_id).name,
        "contract-project-renamed",
        "rename changes only the session label, not its backend id"
    );
    assert_direct_target(&mut adapter, session_target(&project_session_id));

    assert_supported(
        "ditch project session",
        adapter.backend.execute_checked(
            scope,
            MuxCommand::DitchSession {
                session_id: project_session_id.clone(),
            },
            None,
        ),
    );
    current = snapshot(&adapter, "after project ditch");
    assert!(
        !current
            .sessions
            .iter()
            .any(|session| session.id == project_session_id),
        "ditch removes exactly the renamed project session"
    );
    assert!(
        current
            .sessions
            .iter()
            .any(|session| session.name == "contract-launch"),
        "ditch must not remove unrelated launched sessions"
    );
    assert_direct_target(&mut adapter, session_target(&project_session_id));
}

#[test]
fn native_rmux_and_tmux_share_stateful_backend_command_semantics() {
    for adapter in contract_adapters() {
        run_contract(adapter);
    }
}
