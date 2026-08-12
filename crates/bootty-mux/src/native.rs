use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
};

use anyhow::Result;

use super::{
    backend::{
        MuxBackend, MuxEvent, MuxEventCapability, MuxEventCursor, MuxEventDraft, MuxEventPayload,
        MuxEventProvenance, MuxEventQueue, MuxEventTopic, MuxForegroundState, MuxPaneOption,
        MuxPaneState, MuxScopedExecutionPrecondition, MuxTopologyChange,
    },
    capability::{
        BindingCapabilityDescriptor, BindingOperation, BindingOperationAvailability,
        BindingOperationOutcome,
    },
    command::{
        MuxCommand, MuxDirection, MuxPaneLaunch, MuxPaneLaunchPlan, MuxPaneResize,
        MuxSessionLaunchPlan, MuxSplitDirection,
    },
    controller::MuxScope,
    operation::{
        MuxAllocatedResources, MuxAllocatedWindow, MuxBackendCommandCompletion,
        MuxBackendOperationError, MuxEventTarget, MuxOccupantIdentity,
    },
    snapshot::{
        MuxPaneAnchor, MuxPaneLayout, MuxPaneSplitDirection, MuxSession, MuxSnapshot, MuxWindow,
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct NativePane {
    id: String,
    terminal_id: String,
    occupant_id: String,
    /// Initial cwd copied from immutable launch intent.
    cwd: PathBuf,
    /// Immutable process intent retained with the backend's allocated pane identity.
    launch: MuxPaneLaunch,
}

/// A terminal runtime's immutable lease on one exact native pane occupant.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct NativePaneRuntimeIdentity {
    session_id: String,
    window_id: String,
    pane_id: String,
    occupant_id: String,
}

#[derive(Clone, Debug)]
struct NativePaneRuntimeState {
    title: Option<String>,
    options: BTreeMap<String, String>,
    foreground: MuxForegroundState,
}

impl NativePaneRuntimeState {
    fn pane_state(&self) -> MuxPaneState {
        MuxPaneState {
            title: self.title.clone(),
            options: self
                .options
                .iter()
                .map(|(name, value)| MuxPaneOption {
                    name: name.clone(),
                    value: value.clone(),
                })
                .collect(),
            foreground: Some(self.foreground.clone()),
        }
    }
}

struct NativePaneRuntimeContext {
    identity: NativePaneRuntimeIdentity,
    target: MuxEventTarget,
    initial_title: Option<String>,
    initial_foreground: MuxForegroundState,
}

/// Native terminal process intent, validated before a recursive topology is allocated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeTerminalLaunch {
    cwd: PathBuf,
    environment: BTreeMap<String, String>,
    process: NativeTerminalProcess,
    title: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NativeTerminalProcess {
    DefaultShell,
    ShellCommand(String),
    Argv {
        program: String,
        arguments: Vec<String>,
    },
}

impl NativeTerminalLaunch {
    pub(crate) fn validate_mux_pane_launch(pane: &MuxPaneLaunch) -> Result<()> {
        if pane.cwd.is_empty() {
            anyhow::bail!("native launch cwd must not be empty");
        }
        match (&pane.command, &pane.argv) {
            (Some(command), None) if !command.is_empty() => Ok(()),
            (Some(_), None) => anyhow::bail!("native launch command must not be empty"),
            (None, Some(argv)) if argv.first().is_some_and(|program| !program.is_empty()) => Ok(()),
            (None, Some(_)) => anyhow::bail!("native launch argv must have a program"),
            (None, None) => Ok(()),
            (Some(_), Some(_)) => {
                anyhow::bail!("native launch command and argv are mutually exclusive")
            }
        }
    }

    pub(crate) fn from_mux_pane_launch_with_inherited_environment(
        pane: &MuxPaneLaunch,
        inherited: &BTreeMap<String, String>,
    ) -> Result<Self> {
        Self::validate_mux_pane_launch(pane)?;
        let environment = pane
            .effective_environment(inherited)
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect();
        Ok(Self::from_validated_mux_pane_launch(pane, environment))
    }

    fn from_validated_mux_pane_launch(
        pane: &MuxPaneLaunch,
        environment: BTreeMap<String, String>,
    ) -> Self {
        let process = match (&pane.command, &pane.argv) {
            (Some(command), None) => NativeTerminalProcess::ShellCommand(command.clone()),
            (None, Some(argv)) => {
                let (program, arguments) = argv
                    .split_first()
                    .expect("validated native launch argv has a program");
                NativeTerminalProcess::Argv {
                    program: program.clone(),
                    arguments: arguments.to_vec(),
                }
            }
            (None, None) => NativeTerminalProcess::DefaultShell,
            _ => unreachable!("validated native terminal launch has one process form"),
        };
        Self {
            cwd: PathBuf::from(&pane.cwd),
            environment,
            process,
            title: pane.title.clone(),
        }
    }

    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub(crate) fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }

    pub(crate) fn process(&self) -> &NativeTerminalProcess {
        &self.process
    }

    pub(crate) fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct NativeWindow {
    id: String,
    index: u32,
    name: String,
    active_pane_id: String,
    /// The previously selected live pane in this window, used by `SelectLastPane`.
    last_pane_id: Option<String>,
    /// The pane currently occupying the authoritative zoom surface, if any.
    zoomed_pane_id: Option<String>,
    /// Durable recursive topology in declaration order.
    layout: MuxPaneLayout,
    panes: Vec<NativePane>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct NativeSession {
    id: String,
    name: String,
    active_window_id: String,
    /// The previously selected live window, used by `ActivateLastWindow`.
    last_window_id: Option<String>,
    windows: Vec<NativeWindow>,
}

struct NativeMuxState {
    active_session_id: String,
    sessions: Vec<NativeSession>,
    next_pane: u64,
    next_terminal: u64,
    next_occupant: u64,
    runtime_states: HashMap<NativePaneRuntimeIdentity, NativePaneRuntimeState>,
    events: MuxEventQueue,
}

impl NativeMuxState {
    #[cfg(test)]
    fn new() -> Self {
        Self::with_event_backend_identity("native:unscoped")
    }

    fn with_event_backend_identity(backend_identity: impl Into<String>) -> Self {
        Self {
            active_session_id: String::new(),
            sessions: Vec::new(),
            next_pane: 1,
            next_terminal: 1,
            next_occupant: 1,
            runtime_states: HashMap::new(),
            events: MuxEventQueue::for_backend(backend_identity),
        }
    }

    fn ensure_session(&mut self, session_id: &str, cwd: impl Into<PathBuf>) -> bool {
        if self.sessions.iter().any(|session| session.id == session_id) {
            self.active_session_id = session_id.to_owned();
            return false;
        }

        let pane_id = self.next_pane_id();
        let terminal_id = self.next_terminal_id();
        let occupant_id = self.next_occupant_id();
        let cwd = cwd.into();
        let launch = native_shell_launch(&cwd);
        let window = NativeWindow {
            id: "tab-1".to_owned(),
            index: 1,
            name: default_window_name(),
            active_pane_id: pane_id.clone(),
            last_pane_id: None,
            zoomed_pane_id: None,
            layout: MuxPaneLayout::Pane(pane_id.clone()),
            panes: vec![NativePane {
                id: pane_id,
                terminal_id,
                occupant_id,
                cwd,
                launch,
            }],
        };
        self.sessions.push(NativeSession {
            id: session_id.to_owned(),
            name: session_id.to_owned(),
            active_window_id: window.id.clone(),
            last_window_id: None,
            windows: vec![window],
        });
        self.active_session_id = session_id.to_owned();
        true
    }

    fn create_session_launch(&mut self, plan: &MuxSessionLaunchPlan) -> Result<()> {
        plan.validate()
            .map_err(|error| MuxBackendOperationError::Failed(error.to_string()))?;
        validate_native_terminal_launch_plan(plan)
            .map_err(|error| MuxBackendOperationError::Failed(error.to_string()))?;
        if self
            .sessions
            .iter()
            .any(|session| session.id == plan.session_id)
        {
            anyhow::bail!("native session {:?} already exists", plan.session_id);
        }

        // Allocate the complete tree against local counters first. Every fallible operation happens
        // before the live state changes, so an invalid or unrepresentable request cannot leak a
        // partial session, pane, or identity allocation.
        let mut next_pane = self.next_pane;
        let mut next_terminal = self.next_terminal;
        let mut next_occupant = self.next_occupant;
        let mut windows = Vec::with_capacity(plan.windows.len());
        for (position, window_plan) in plan.windows.iter().enumerate() {
            let (layout, panes) = materialize_native_launch_layout(
                &window_plan.layout,
                &plan.environment,
                &mut next_pane,
                &mut next_occupant,
                &mut next_terminal,
            )?;
            let active_pane_id = panes
                .first()
                .map(|pane| pane.id.clone())
                .expect("validated launch window has a pane");
            windows.push(NativeWindow {
                id: format!("tab-{}", position + 1),
                index: position as u32 + 1,
                name: window_plan.name.clone().unwrap_or_else(default_window_name),
                active_pane_id,
                last_pane_id: None,
                zoomed_pane_id: None,
                layout,
                panes,
            });
        }
        let active_window_id = windows
            .get(plan.focused_window)
            .map(|window| window.id.clone())
            .expect("validated launch plan has a focused window");
        let session = NativeSession {
            id: plan.session_id.clone(),
            name: plan.session_id.clone(),
            active_window_id,
            last_window_id: None,
            windows,
        };

        self.next_pane = next_pane;
        self.next_occupant = next_occupant;
        self.next_terminal = next_terminal;
        self.sessions.push(session);
        if plan.focus {
            self.active_session_id = plan.session_id.clone();
        }
        Ok(())
    }

    fn activate_window(&mut self, session_id: &str, window_id: &str) {
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            && select_native_window(session, window_id)
        {
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

    fn require_session(&self, session_id: &str) -> Result<&NativeSession> {
        self.sessions
            .iter()
            .find(|session| session.id == session_id)
            .ok_or_else(|| {
                MuxBackendOperationError::stale(format!(
                    "native session {session_id:?} no longer exists"
                ))
                .into()
            })
    }

    fn require_window<'a>(
        &self,
        session: &'a NativeSession,
        window_id: &str,
    ) -> Result<&'a NativeWindow> {
        session
            .windows
            .iter()
            .find(|window| window.id == window_id)
            .ok_or_else(|| {
                MuxBackendOperationError::stale(format!(
                    "native window {window_id:?} no longer exists"
                ))
                .into()
            })
    }

    fn require_active_window<'a>(&self, session: &'a NativeSession) -> Result<&'a NativeWindow> {
        self.require_window(session, &session.active_window_id)
    }

    fn require_pane<'a>(
        &self,
        session: &'a NativeSession,
        pane_id: &str,
    ) -> Result<&'a NativePane> {
        session
            .windows
            .iter()
            .flat_map(|window| &window.panes)
            .find(|pane| pane.id == pane_id)
            .ok_or_else(|| {
                MuxBackendOperationError::stale(format!("native pane {pane_id:?} no longer exists"))
                    .into()
            })
    }

    fn require_active_pane<'a>(&self, session: &'a NativeSession) -> Result<&'a NativePane> {
        let window = self.require_active_window(session)?;
        window
            .panes
            .iter()
            .find(|pane| pane.id == window.active_pane_id)
            .ok_or_else(|| {
                MuxBackendOperationError::stale("native active pane no longer exists").into()
            })
    }

    fn validate_command_target(&self, command: &MuxCommand) -> Result<()> {
        let session_id = match command {
            MuxCommand::CreateSession { plan } => {
                if self
                    .sessions
                    .iter()
                    .any(|session| session.id == plan.session_id)
                {
                    return Err(MuxBackendOperationError::Failed(format!(
                        "native session {:?} already exists",
                        plan.session_id
                    ))
                    .into());
                }
                return Ok(());
            }
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
            | MuxCommand::SelectLastPane { session_id, .. }
            | MuxCommand::KillPane { session_id, .. }
            | MuxCommand::ClosePane { session_id, .. }
            | MuxCommand::TogglePaneZoom { session_id, .. }
            | MuxCommand::ResizePane { session_id, .. }
            | MuxCommand::RenameSession { session_id, .. }
            | MuxCommand::DitchSession { session_id } => session_id,
            MuxCommand::CreateProjectSession { .. } | MuxCommand::CreateWorktreeSession { .. } => {
                return Ok(());
            }
        };
        let session = self.require_session(session_id)?;
        match command {
            MuxCommand::ActivateWindow { window_id, .. }
            | MuxCommand::RenameWindow { window_id, .. } => {
                self.require_window(session, window_id)?;
            }
            MuxCommand::ActivateWindowIndex { index, .. } => {
                session
                    .windows
                    .iter()
                    .find(|window| window.index == *index)
                    .ok_or_else(|| {
                        MuxBackendOperationError::stale(format!(
                            "native window index {index} no longer exists"
                        ))
                    })?;
            }
            MuxCommand::MoveWindow {
                window_id: Some(window_id),
                ..
            } => {
                self.require_window(session, window_id)?;
            }
            MuxCommand::MoveWindowPreservingSelection {
                window_id,
                selected_window_id,
                ..
            } => {
                self.require_window(session, window_id)?;
                self.require_window(session, selected_window_id)?;
            }
            MuxCommand::MoveWindow {
                window_id: None, ..
            }
            | MuxCommand::NewWindow { .. }
            | MuxCommand::ActivateNextWindow { .. }
            | MuxCommand::ActivatePreviousWindow { .. }
            | MuxCommand::ActivateLastWindow { .. } => {
                self.require_active_window(session)?;
            }
            MuxCommand::SplitPane {
                pane_id: Some(pane_id),
                ..
            }
            | MuxCommand::KillPane {
                pane_id: Some(pane_id),
                ..
            }
            | MuxCommand::ClosePane {
                pane_id: Some(pane_id),
                ..
            }
            | MuxCommand::TogglePaneZoom {
                pane_id: Some(pane_id),
                ..
            }
            | MuxCommand::ResizePane {
                pane_id: Some(pane_id),
                ..
            } => {
                self.require_pane(session, pane_id)?;
            }
            MuxCommand::SplitPane { pane_id: None, .. }
            | MuxCommand::KillPane { pane_id: None, .. }
            | MuxCommand::ClosePane { pane_id: None, .. }
            | MuxCommand::TogglePaneZoom { pane_id: None, .. }
            | MuxCommand::ResizePane { pane_id: None, .. } => {
                self.require_active_pane(session)?;
            }
            MuxCommand::SelectPane {
                window_id: Some(window_id),
                ..
            }
            | MuxCommand::SelectNextPane {
                window_id: Some(window_id),
                ..
            }
            | MuxCommand::SelectPreviousPane {
                window_id: Some(window_id),
                ..
            }
            | MuxCommand::SelectLastPane {
                window_id: Some(window_id),
                ..
            } => {
                self.require_window(session, window_id)?;
            }
            MuxCommand::SelectPane {
                window_id: None, ..
            }
            | MuxCommand::SelectNextPane {
                window_id: None, ..
            }
            | MuxCommand::SelectPreviousPane {
                window_id: None, ..
            }
            | MuxCommand::SelectLastPane {
                window_id: None, ..
            } => {
                self.require_active_pane(session)?;
            }
            MuxCommand::RenameSession { .. }
            | MuxCommand::DitchSession { .. }
            | MuxCommand::CreateSession { .. }
            | MuxCommand::CreateProjectSession { .. }
            | MuxCommand::CreateWorktreeSession { .. } => {}
        }
        Ok(())
    }

    fn active_session_mut(&mut self, session_id: &str) -> Option<&mut NativeSession> {
        self.sessions
            .iter_mut()
            .find(|session| session.id == session_id)
    }

    fn new_window(&mut self, session_id: &str, cwd: Option<PathBuf>) {
        let pane_id = self.next_pane_id();
        let terminal_id = self.next_terminal_id();
        let occupant_id = self.next_occupant_id();
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
            let launch = native_shell_launch(&cwd);
            let index = session.windows.len() as u32 + 1;
            let window = NativeWindow {
                id: next_window_id(session),
                index,
                name: default_window_name(),
                active_pane_id: pane_id.clone(),
                last_pane_id: None,
                zoomed_pane_id: None,
                layout: MuxPaneLayout::Pane(pane_id.clone()),
                panes: vec![NativePane {
                    id: pane_id,
                    terminal_id,
                    occupant_id,
                    cwd,
                    launch,
                }],
            };
            let window_id = window.id.clone();
            session.windows.push(window);
            select_native_window(session, &window_id);
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
            let window_id = session.windows[next].id.clone();
            select_native_window(session, &window_id);
            self.active_session_id = session_id.to_owned();
        }
    }

    fn activate_last_window(&mut self, session_id: &str) -> bool {
        let Some(session) = self.active_session_mut(session_id) else {
            return false;
        };
        let Some(window_id) = session.last_window_id.clone() else {
            return false;
        };
        if window_id == session.active_window_id || !select_native_window(session, &window_id) {
            return false;
        }
        self.active_session_id = session_id.to_owned();
        true
    }

    fn activate_window_index(&mut self, session_id: &str, index: u32) {
        if let Some(session) = self.active_session_mut(session_id)
            && let Some(window_id) = session
                .windows
                .iter()
                .find(|window| window.index == index)
                .map(|window| window.id.clone())
        {
            select_native_window(session, &window_id);
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
                select_native_window(session, &target);
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

    fn activate_window_containing_pane(&mut self, session_id: &str, pane_id: &str) {
        let Some(session) = self.active_session_mut(session_id) else {
            return;
        };
        let Some(window_id) = session
            .windows
            .iter()
            .find(|window| window.panes.iter().any(|pane| pane.id == pane_id))
            .map(|window| window.id.clone())
        else {
            return;
        };
        if select_native_window(session, &window_id) {
            self.active_session_id = session_id.to_owned();
        }
    }

    fn split_pane(
        &mut self,
        session_id: &str,
        source_pane_id: Option<&str>,
        direction: MuxSplitDirection,
    ) {
        if let Some(source_pane_id) = source_pane_id {
            self.activate_window_containing_pane(session_id, source_pane_id);
        }
        let pane_id = self.next_pane_id();
        let terminal_id = self.next_terminal_id();
        let occupant_id = self.next_occupant_id();
        if let Some(window) = self.active_window_mut(session_id) {
            let source_pane_id = source_pane_id
                .filter(|pane_id| window.panes.iter().any(|pane| pane.id == *pane_id))
                .map(str::to_owned)
                .unwrap_or_else(|| window.active_pane_id.clone());
            let Some(source_index) = window
                .panes
                .iter()
                .position(|pane| pane.id == source_pane_id)
            else {
                return;
            };
            let cwd = window.panes[source_index].cwd.clone();
            let launch = native_shell_launch(&cwd);
            if !split_native_layout(
                &mut window.layout,
                &source_pane_id,
                pane_id.clone(),
                direction,
            ) {
                return;
            }
            window.last_pane_id = Some(window.active_pane_id.clone());
            window.active_pane_id = pane_id.clone();
            window.panes.insert(
                source_index + 1,
                NativePane {
                    id: pane_id,
                    terminal_id,
                    occupant_id,
                    cwd,
                    launch,
                },
            );
            self.active_session_id = session_id.to_owned();
        }
    }

    fn set_active_pane(&mut self, session_id: &str, pane_id: &str) {
        self.activate_window_containing_pane(session_id, pane_id);
        if let Some(window) = self.active_window_mut(session_id)
            && window.panes.iter().any(|pane| pane.id == pane_id)
            && window.active_pane_id != pane_id
        {
            let previous = std::mem::replace(&mut window.active_pane_id, pane_id.to_owned());
            window.last_pane_id = Some(previous);
            self.active_session_id = session_id.to_owned();
        }
    }

    fn select_directional_pane(
        &mut self,
        session_id: &str,
        window_id: Option<&str>,
        direction: MuxDirection,
    ) -> Result<()> {
        let (window_id, pane_id) = {
            let session = self.require_session(session_id)?;
            let window_id = window_id.unwrap_or(&session.active_window_id);
            let window = self.require_window(session, window_id)?;
            let pane_id =
                directional_native_pane_neighbor(&window.layout, &window.active_pane_id, direction)
                    .ok_or_else(|| {
                        MuxBackendOperationError::Failed(format!(
                            "native pane {:?} has no {direction:?} neighbor",
                            window.active_pane_id
                        ))
                    })?;
            (window.id.clone(), pane_id)
        };
        let session = self
            .active_session_mut(session_id)
            .expect("validated native session remains present");
        select_native_window(session, &window_id);
        let window = session
            .windows
            .iter_mut()
            .find(|window| window.id == window_id)
            .expect("validated native window remains present");
        let previous = std::mem::replace(&mut window.active_pane_id, pane_id);
        if previous != window.active_pane_id {
            window.last_pane_id = Some(previous);
        }
        self.active_session_id = session_id.to_owned();
        Ok(())
    }

    fn select_relative_pane(
        &mut self,
        session_id: &str,
        window_id: Option<&str>,
        delta: i32,
    ) -> Result<()> {
        let (window_id, pane_id) = {
            let session = self.require_session(session_id)?;
            let window_id = window_id.unwrap_or(&session.active_window_id);
            let window = self.require_window(session, window_id)?;
            if window.panes.len() < 2 {
                return Err(MuxBackendOperationError::Failed(
                    "native window has no other pane to select".to_owned(),
                )
                .into());
            }
            let index = window
                .panes
                .iter()
                .position(|pane| pane.id == window.active_pane_id)
                .ok_or_else(|| {
                    MuxBackendOperationError::stale("native active pane no longer exists")
                })?;
            let pane_id = window.panes[wrap_index(index, delta, window.panes.len())]
                .id
                .clone();
            (window.id.clone(), pane_id)
        };
        let session = self
            .active_session_mut(session_id)
            .expect("validated native session remains present");
        select_native_window(session, &window_id);
        let window = session
            .windows
            .iter_mut()
            .find(|window| window.id == window_id)
            .expect("validated native window remains present");
        let previous = std::mem::replace(&mut window.active_pane_id, pane_id);
        if previous != window.active_pane_id {
            window.last_pane_id = Some(previous);
        }
        self.active_session_id = session_id.to_owned();
        Ok(())
    }

    fn select_last_pane(&mut self, session_id: &str, window_id: Option<&str>) -> Result<()> {
        let (window_id, pane_id) = {
            let session = self.require_session(session_id)?;
            let window_id = window_id.unwrap_or(&session.active_window_id);
            let window = self.require_window(session, window_id)?;
            let pane_id = window.last_pane_id.clone().ok_or_else(|| {
                MuxBackendOperationError::Failed(
                    "native window has no previous pane to select".to_owned(),
                )
            })?;
            if pane_id == window.active_pane_id
                || !window.panes.iter().any(|pane| pane.id == pane_id)
            {
                return Err(MuxBackendOperationError::Failed(
                    "native window has no live previous pane to select".to_owned(),
                )
                .into());
            }
            (window.id.clone(), pane_id)
        };
        let session = self
            .active_session_mut(session_id)
            .expect("validated native session remains present");
        select_native_window(session, &window_id);
        let window = session
            .windows
            .iter_mut()
            .find(|window| window.id == window_id)
            .expect("validated native window remains present");
        let previous = std::mem::replace(&mut window.active_pane_id, pane_id);
        window.last_pane_id = Some(previous);
        self.active_session_id = session_id.to_owned();
        Ok(())
    }

    fn resize_pane(
        &mut self,
        session_id: &str,
        pane_id: Option<&str>,
        adjustment: MuxPaneResize,
    ) -> Result<()> {
        let session = self.active_session_mut(session_id).ok_or_else(|| {
            MuxBackendOperationError::stale(format!(
                "native session {session_id:?} no longer exists"
            ))
        })?;
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
            })
            .ok_or_else(|| {
                MuxBackendOperationError::stale("native target window no longer exists")
            })?;
        let window = &mut session.windows[window_index];
        let target_pane_id = pane_id
            .map(str::to_owned)
            .unwrap_or_else(|| window.active_pane_id.clone());
        match adjustment {
            MuxPaneResize::Directional { direction, cells } if cells > 0 => {
                if !resize_native_layout(&mut window.layout, &target_pane_id, direction, cells) {
                    return Err(MuxBackendOperationError::unsupported(
                        "native pane resize has no matching ancestor split",
                    )
                    .into());
                }
                Ok(())
            }
            MuxPaneResize::Directional { .. } => Err(MuxBackendOperationError::Failed(
                "native pane resize requires a positive cell count".to_owned(),
            )
            .into()),
            MuxPaneResize::Absolute { .. } => Err(MuxBackendOperationError::unsupported(
                "native pane resize only supports directional cell adjustments",
            )
            .into()),
        }
    }

    fn toggle_pane_zoom(&mut self, session_id: &str, pane_id: Option<&str>) -> Result<()> {
        let session = self.active_session_mut(session_id).ok_or_else(|| {
            MuxBackendOperationError::stale(format!(
                "native session {session_id:?} no longer exists"
            ))
        })?;
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
            })
            .ok_or_else(|| {
                MuxBackendOperationError::stale("native target window no longer exists")
            })?;
        let window = &mut session.windows[window_index];
        let target_pane_id = pane_id
            .map(str::to_owned)
            .unwrap_or_else(|| window.active_pane_id.clone());
        if !window.panes.iter().any(|pane| pane.id == target_pane_id) {
            return Err(
                MuxBackendOperationError::stale("native target pane no longer exists").into(),
            );
        }
        window.zoomed_pane_id = (window.zoomed_pane_id.as_deref() != Some(target_pane_id.as_str()))
            .then_some(target_pane_id);
        Ok(())
    }

    fn kill_active_pane(&mut self, session_id: &str) -> bool {
        let Some(window) = self.active_window_mut(session_id) else {
            return false;
        };
        if window.panes.len() <= 1 {
            return false;
        }
        let Some(index) = window
            .panes
            .iter()
            .position(|pane| pane.id == window.active_pane_id)
        else {
            return false;
        };
        let removed_pane_id = window.panes[index].id.clone();
        let layout = remove_native_layout_pane(&window.layout, &removed_pane_id)
            .expect("a multi-pane native layout keeps a sibling");
        window.panes.remove(index);
        window.layout = layout;
        if window.last_pane_id.as_deref() == Some(removed_pane_id.as_str()) {
            window.last_pane_id = None;
        }
        window.active_pane_id = window.panes[index.min(window.panes.len() - 1)].id.clone();
        true
    }

    // Close the requested pane; when it was the last pane in its window, cascade to remove that
    // window. The target can belong to an inactive tab, so never route through active_window_mut.
    fn close_pane(&mut self, session_id: &str, pane_id: Option<&str>) -> bool {
        let mut changed_active_session = false;
        {
            let Some(session) = self.active_session_mut(session_id) else {
                return false;
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
                return false;
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
                return false;
            };
            let window = &mut session.windows[window_index];
            let removed_pane_id = window.panes[pane_index].id.clone();
            let removed_active_pane = removed_pane_id == window.active_pane_id;
            let remaining_layout = remove_native_layout_pane(&window.layout, &removed_pane_id);
            window.panes.remove(pane_index);
            if window.last_pane_id.as_deref() == Some(removed_pane_id.as_str()) {
                window.last_pane_id = None;
            }
            if let Some(layout) = remaining_layout {
                window.layout = layout;
                if removed_active_pane {
                    window.active_pane_id = window.panes[pane_index.min(window.panes.len() - 1)]
                        .id
                        .clone();
                    if window.last_pane_id.as_deref() == Some(removed_pane_id.as_str()) {
                        window.last_pane_id = None;
                    }
                }
                changed_active_session = target_was_active;
            } else {
                session.windows.remove(window_index);
                if session.last_window_id.as_deref() == Some(target_window_id.as_str()) {
                    session.last_window_id = None;
                }
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
        true
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
                    layout: Some(window.layout.clone()),
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
            active_window_id: Some(session.active_window_id.clone()),
            windows,
        }
    }

    fn next_pane_id(&mut self) -> String {
        let id = format!("pane-{}", self.next_pane);
        self.next_pane += 1;
        id
    }

    fn next_terminal_id(&mut self) -> String {
        let id = format!("terminal-{}", self.next_terminal);
        self.next_terminal = self.next_terminal.saturating_add(1);
        id
    }

    fn next_occupant_id(&mut self) -> String {
        let id = format!("native-occupant-{}", self.next_occupant);
        self.next_occupant = self.next_occupant.saturating_add(1);
        id
    }

    fn pane_target(
        &self,
        session_id: &str,
        requested_pane_id: Option<&str>,
    ) -> Option<MuxEventTarget> {
        let session = self
            .sessions
            .iter()
            .find(|session| session.id == session_id)?;
        let window = requested_pane_id
            .and_then(|pane_id| {
                session
                    .windows
                    .iter()
                    .find(|window| window.panes.iter().any(|pane| pane.id == pane_id))
            })
            .or_else(|| {
                session
                    .windows
                    .iter()
                    .find(|window| window.id == session.active_window_id)
            })
            .or_else(|| session.windows.first())?;
        let pane = requested_pane_id
            .and_then(|pane_id| window.panes.iter().find(|pane| pane.id == pane_id))
            .or_else(|| {
                window
                    .panes
                    .iter()
                    .find(|pane| pane.id == window.active_pane_id)
            })
            .or_else(|| window.panes.first())?;
        Some(native_pane_target(session_id, window, pane))
    }

    fn session_pane_targets(&self, session_id: &str) -> Vec<MuxEventTarget> {
        self.sessions
            .iter()
            .find(|session| session.id == session_id)
            .into_iter()
            .flat_map(|session| {
                session.windows.iter().flat_map(move |window| {
                    window
                        .panes
                        .iter()
                        .map(move |pane| native_pane_target(session_id, window, pane))
                })
            })
            .collect()
    }

    fn forget_runtime_targets(&mut self, targets: &[MuxEventTarget]) {
        self.runtime_states.retain(|identity, _| {
            !targets.iter().any(|target| {
                target.session_id.as_deref() == Some(identity.session_id.as_str())
                    && target.window_id.as_deref() == Some(identity.window_id.as_str())
                    && target.pane_id.as_deref() == Some(identity.pane_id.as_str())
                    && target
                        .occupant
                        .as_ref()
                        .is_some_and(|occupant| occupant.backend_identity == identity.occupant_id)
            })
        });
    }

    fn runtime_context(
        &self,
        session_id: &str,
        pane_id: &str,
        expected: Option<&NativePaneRuntimeIdentity>,
    ) -> Result<NativePaneRuntimeContext> {
        let session = self
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .ok_or_else(|| anyhow::anyhow!("native session {session_id} is not present"))?;
        let window = session
            .windows
            .iter()
            .find(|window| window.panes.iter().any(|pane| pane.id == pane_id))
            .ok_or_else(|| anyhow::anyhow!("native pane {pane_id} is not present"))?;
        let pane = window
            .panes
            .iter()
            .find(|pane| pane.id == pane_id)
            .expect("window was selected by its pane");
        let identity = NativePaneRuntimeIdentity {
            session_id: session.id.clone(),
            window_id: window.id.clone(),
            pane_id: pane.id.clone(),
            occupant_id: pane.occupant_id.clone(),
        };
        if expected.is_some_and(|expected| expected != &identity) {
            anyhow::bail!("native pane {pane_id} has been replaced");
        }
        let process = native_pane_process(pane);
        Ok(NativePaneRuntimeContext {
            identity,
            target: native_pane_target(&session.id, window, pane),
            initial_title: pane.launch.title.clone(),
            initial_foreground: MuxForegroundState {
                pid: None,
                command: Some(process.clone()),
                cwd: Some(pane.cwd.to_string_lossy().into_owned()),
                executable: native_pane_executable(pane),
            },
        })
    }

    fn runtime_state_for(
        &mut self,
        context: &NativePaneRuntimeContext,
    ) -> &mut NativePaneRuntimeState {
        self.runtime_states
            .entry(context.identity.clone())
            .or_insert_with(|| NativePaneRuntimeState {
                title: context.initial_title.clone(),
                options: BTreeMap::new(),
                foreground: context.initial_foreground.clone(),
            })
    }

    fn replace_runtime_occupant(
        &mut self,
        previous: &NativePaneRuntimeIdentity,
    ) -> Result<(NativePaneRuntimeContext, NativePaneRuntimeContext)> {
        let old = self.runtime_context(&previous.session_id, &previous.pane_id, Some(previous))?;
        let occupant_id = self.next_occupant_id();
        let pane = self
            .sessions
            .iter_mut()
            .find(|session| session.id == previous.session_id)
            .and_then(|session| {
                session
                    .windows
                    .iter_mut()
                    .find(|window| window.id == previous.window_id)
            })
            .and_then(|window| {
                window
                    .panes
                    .iter_mut()
                    .find(|pane| pane.id == previous.pane_id)
            })
            .expect("validated native runtime lease remains present");
        pane.occupant_id = occupant_id;
        pane.cwd = PathBuf::from(&pane.launch.cwd);
        self.runtime_states.remove(previous);
        let current = self.runtime_context(&previous.session_id, &previous.pane_id, None)?;
        Ok((old, current))
    }

    fn update_runtime_cwd(
        &mut self,
        identity: &NativePaneRuntimeIdentity,
        cwd: &str,
    ) -> Result<()> {
        self.runtime_context(&identity.session_id, &identity.pane_id, Some(identity))?;
        let pane = self
            .sessions
            .iter_mut()
            .find(|session| session.id == identity.session_id)
            .and_then(|session| {
                session
                    .windows
                    .iter_mut()
                    .find(|window| window.id == identity.window_id)
            })
            .and_then(|window| {
                window
                    .panes
                    .iter_mut()
                    .find(|pane| pane.id == identity.pane_id)
            })
            .expect("validated native runtime lease remains present");
        pane.cwd = PathBuf::from(cwd);
        Ok(())
    }

    #[cfg(test)]
    fn pane_launch(
        &self,
        session_id: &str,
        window_id: &str,
        pane_id: &str,
    ) -> Option<MuxPaneLaunch> {
        self.sessions
            .iter()
            .find(|session| session.id == session_id)?
            .windows
            .iter()
            .find(|window| window.id == window_id)?
            .panes
            .iter()
            .find(|pane| pane.id == pane_id)
            .map(|pane| pane.launch.clone())
    }

    fn window_target(&self, session_id: &str, window_id: &str) -> MuxEventTarget {
        let mut target = MuxEventTarget::session(session_id);
        target.window_id = Some(window_id.to_owned());
        target
    }

    fn target_for_command(&self, command: &MuxCommand) -> Option<MuxEventTarget> {
        match command {
            MuxCommand::ActivateWindow {
                session_id,
                window_id,
            }
            | MuxCommand::RenameWindow {
                session_id,
                window_id,
                ..
            }
            | MuxCommand::MoveWindow {
                session_id,
                window_id: Some(window_id),
                ..
            }
            | MuxCommand::MoveWindowPreservingSelection {
                session_id,
                window_id,
                ..
            } => Some(self.window_target(session_id, window_id)),
            MuxCommand::NewWindow { session_id, .. }
            | MuxCommand::SelectPane { session_id, .. }
            | MuxCommand::SelectNextPane { session_id, .. }
            | MuxCommand::SelectPreviousPane { session_id, .. }
            | MuxCommand::SelectLastPane { session_id, .. } => self.pane_target(session_id, None),
            MuxCommand::SplitPane {
                session_id,
                pane_id,
                ..
            }
            | MuxCommand::KillPane {
                session_id,
                pane_id,
            }
            | MuxCommand::ClosePane {
                session_id,
                pane_id,
            }
            | MuxCommand::TogglePaneZoom {
                session_id,
                pane_id,
            }
            | MuxCommand::ResizePane {
                session_id,
                pane_id,
                ..
            } => self.pane_target(session_id, pane_id.as_deref()),
            MuxCommand::ActivateNextWindow { session_id }
            | MuxCommand::ActivatePreviousWindow { session_id }
            | MuxCommand::ActivateLastWindow { session_id }
            | MuxCommand::ActivateWindowIndex { session_id, .. }
            | MuxCommand::MoveWindow {
                session_id,
                window_id: None,
                ..
            } => Some(MuxEventTarget::session(session_id)),
            MuxCommand::CreateSession { plan } => Some(MuxEventTarget::session(&plan.session_id)),
            MuxCommand::CreateProjectSession { session_id, .. }
            | MuxCommand::CreateWorktreeSession { session_id, .. }
            | MuxCommand::RenameSession { session_id, .. }
            | MuxCommand::DitchSession { session_id } => Some(MuxEventTarget::session(session_id)),
        }
    }

    fn publish_topology_change(&self, target: Option<MuxEventTarget>) {
        self.events.publish(MuxEventDraft::new(
            MuxEventTopic::TopologyChanged,
            MuxEventProvenance::Native,
            target,
            None,
            MuxEventPayload::Topology {
                change: MuxTopologyChange::Mutation,
            },
        ));
    }
}

fn supports_native_session_launch_plan(plan: &MuxSessionLaunchPlan) -> bool {
    // The native renderer owns the full split tree and the local terminal runtime can execute
    // shell commands, direct argv, environment overrides, and initial titles. Materialization is
    // checked before allocation so each accepted plan has an exact terminal handoff.
    validate_native_terminal_launch_plan(plan).is_ok()
}

fn validate_native_terminal_launch_plan(plan: &MuxSessionLaunchPlan) -> Result<()> {
    for window in &plan.windows {
        validate_native_terminal_launch_layout(&window.layout)?;
    }
    Ok(())
}

fn validate_native_terminal_launch_layout(layout: &MuxPaneLaunchPlan) -> Result<()> {
    match layout {
        MuxPaneLaunchPlan::Pane(pane) => NativeTerminalLaunch::validate_mux_pane_launch(pane),
        MuxPaneLaunchPlan::Split(split) => {
            validate_native_terminal_launch_layout(&split.first)?;
            validate_native_terminal_launch_layout(&split.second)
        }
    }
}

fn native_shell_launch(cwd: &Path) -> MuxPaneLaunch {
    MuxPaneLaunch {
        cwd: cwd.to_string_lossy().into_owned(),
        command: None,
        argv: None,
        environment: BTreeMap::new(),
        title: None,
    }
}

fn materialize_native_launch_layout(
    layout: &MuxPaneLaunchPlan,
    session_environment: &BTreeMap<String, String>,
    next_pane: &mut u64,
    next_occupant: &mut u64,
    next_terminal: &mut u64,
) -> Result<(MuxPaneLayout, Vec<NativePane>)> {
    match layout {
        MuxPaneLaunchPlan::Pane(pane) => {
            let pane_id = next_native_identity(next_pane, "pane")?;
            let occupant_id = next_native_identity(next_occupant, "occupant")?;
            let terminal_id = next_native_identity(next_terminal, "terminal")?;
            let launch = MuxPaneLaunch {
                cwd: pane.cwd.clone(),
                command: pane.command.clone(),
                argv: pane.argv.clone(),
                environment: pane
                    .effective_environment(session_environment)
                    .map(|(name, value)| (name.to_owned(), value.to_owned()))
                    .collect(),
                title: pane.title.clone(),
            };
            let native_pane = NativePane {
                id: pane_id.clone(),
                occupant_id: format!("native-{occupant_id}"),
                terminal_id,
                cwd: PathBuf::from(&launch.cwd),
                launch,
            };
            Ok((MuxPaneLayout::Pane(pane_id), vec![native_pane]))
        }
        MuxPaneLaunchPlan::Split(split) => {
            let (first, mut first_panes) = materialize_native_launch_layout(
                &split.first,
                session_environment,
                next_pane,
                next_occupant,
                next_terminal,
            )?;
            let (second, second_panes) = materialize_native_launch_layout(
                &split.second,
                session_environment,
                next_pane,
                next_occupant,
                next_terminal,
            )?;
            first_panes.extend(second_panes);
            Ok((
                MuxPaneLayout::Split {
                    direction: native_split_direction(split.direction),
                    ratio_millis: split.ratio_millis,
                    first: Box::new(first),
                    second: Box::new(second),
                },
                first_panes,
            ))
        }
    }
}

fn next_native_identity(next: &mut u64, kind: &str) -> Result<String> {
    let value = *next;
    *next = next.checked_add(1).ok_or_else(|| {
        MuxBackendOperationError::Failed(format!("native {kind} identity space is exhausted"))
    })?;
    Ok(format!("{kind}-{value}"))
}

fn native_split_direction(direction: MuxSplitDirection) -> MuxPaneSplitDirection {
    match direction {
        MuxSplitDirection::Right => MuxPaneSplitDirection::Right,
        MuxSplitDirection::Down => MuxPaneSplitDirection::Down,
    }
}

fn select_native_window(session: &mut NativeSession, window_id: &str) -> bool {
    if !session.windows.iter().any(|window| window.id == window_id) {
        return false;
    }
    if session.active_window_id != window_id {
        let previous = std::mem::replace(&mut session.active_window_id, window_id.to_owned());
        session.last_window_id = Some(previous);
    }
    true
}

fn split_native_layout(
    layout: &mut MuxPaneLayout,
    source_pane_id: &str,
    new_pane_id: String,
    direction: MuxSplitDirection,
) -> bool {
    match layout {
        MuxPaneLayout::Pane(pane_id) if pane_id == source_pane_id => {
            let first = MuxPaneLayout::Pane(pane_id.clone());
            *layout = MuxPaneLayout::Split {
                direction: native_split_direction(direction),
                ratio_millis: 500,
                first: Box::new(first),
                second: Box::new(MuxPaneLayout::Pane(new_pane_id)),
            };
            true
        }
        MuxPaneLayout::Pane(_) => false,
        MuxPaneLayout::Split { first, second, .. } => {
            split_native_layout(first, source_pane_id, new_pane_id.clone(), direction)
                || split_native_layout(second, source_pane_id, new_pane_id, direction)
        }
    }
}

fn remove_native_layout_pane(layout: &MuxPaneLayout, pane_id: &str) -> Option<MuxPaneLayout> {
    match layout {
        MuxPaneLayout::Pane(current) => {
            (current != pane_id).then(|| MuxPaneLayout::Pane(current.clone()))
        }
        MuxPaneLayout::Split {
            direction,
            ratio_millis,
            first,
            second,
        } => match (
            remove_native_layout_pane(first, pane_id),
            remove_native_layout_pane(second, pane_id),
        ) {
            (Some(first), Some(second)) => Some(MuxPaneLayout::Split {
                direction: direction.clone(),
                ratio_millis: *ratio_millis,
                first: Box::new(first),
                second: Box::new(second),
            }),
            (Some(sibling), None) | (None, Some(sibling)) => Some(sibling),
            (None, None) => None,
        },
    }
}

fn directional_native_pane_neighbor(
    layout: &MuxPaneLayout,
    current_pane_id: &str,
    direction: MuxDirection,
) -> Option<String> {
    let MuxPaneLayout::Split {
        direction: split_direction,
        first,
        second,
        ..
    } = layout
    else {
        return None;
    };
    let current_is_first = native_layout_contains(first, current_pane_id);
    let current_is_second = !current_is_first && native_layout_contains(second, current_pane_id);
    if !current_is_first && !current_is_second {
        return None;
    }
    let (current_branch, sibling) = if current_is_first {
        (&**first, &**second)
    } else {
        (&**second, &**first)
    };
    if let Some(neighbor) =
        directional_native_pane_neighbor(current_branch, current_pane_id, direction)
    {
        return Some(neighbor);
    }
    let crosses_split = matches!(
        (split_direction, direction, current_is_first),
        (MuxPaneSplitDirection::Right, MuxDirection::Right, true)
            | (MuxPaneSplitDirection::Right, MuxDirection::Left, false)
            | (MuxPaneSplitDirection::Down, MuxDirection::Down, true)
            | (MuxPaneSplitDirection::Down, MuxDirection::Up, false)
    );
    crosses_split.then(|| native_layout_edge(sibling, opposite_direction(direction)))?
}

fn native_layout_contains(layout: &MuxPaneLayout, pane_id: &str) -> bool {
    match layout {
        MuxPaneLayout::Pane(current) => current == pane_id,
        MuxPaneLayout::Split { first, second, .. } => {
            native_layout_contains(first, pane_id) || native_layout_contains(second, pane_id)
        }
    }
}

fn native_layout_edge(layout: &MuxPaneLayout, edge: MuxDirection) -> Option<String> {
    match layout {
        MuxPaneLayout::Pane(pane_id) => Some(pane_id.clone()),
        MuxPaneLayout::Split {
            direction,
            first,
            second,
            ..
        } => {
            let branch = match (direction, edge) {
                (MuxPaneSplitDirection::Right, MuxDirection::Left)
                | (MuxPaneSplitDirection::Down, MuxDirection::Up) => &**first,
                (MuxPaneSplitDirection::Right, MuxDirection::Right)
                | (MuxPaneSplitDirection::Down, MuxDirection::Down) => &**second,
                _ => &**first,
            };
            native_layout_edge(branch, edge)
        }
    }
}

fn opposite_direction(direction: MuxDirection) -> MuxDirection {
    match direction {
        MuxDirection::Left => MuxDirection::Right,
        MuxDirection::Right => MuxDirection::Left,
        MuxDirection::Up => MuxDirection::Down,
        MuxDirection::Down => MuxDirection::Up,
    }
}

const NATIVE_RESIZE_STEP_MILLIS: u16 = 10;

fn resize_native_layout(
    layout: &mut MuxPaneLayout,
    pane_id: &str,
    direction: MuxDirection,
    cells: u16,
) -> bool {
    let positive_axis = matches!(direction, MuxDirection::Right | MuxDirection::Down);
    let matching_axis = |split_direction: &MuxPaneSplitDirection| {
        matches!(
            (split_direction, direction),
            (
                MuxPaneSplitDirection::Right,
                MuxDirection::Left | MuxDirection::Right
            ) | (
                MuxPaneSplitDirection::Down,
                MuxDirection::Up | MuxDirection::Down
            )
        )
    };
    let delta = cells.saturating_mul(NATIVE_RESIZE_STEP_MILLIS);

    fn visit(
        layout: &mut MuxPaneLayout,
        pane_id: &str,
        positive_axis: bool,
        matching_axis: &impl Fn(&MuxPaneSplitDirection) -> bool,
        delta: u16,
    ) -> bool {
        let MuxPaneLayout::Split {
            direction: split_direction,
            ratio_millis,
            first,
            second,
        } = layout
        else {
            return false;
        };
        let in_first = native_layout_contains(first, pane_id);
        let in_second = !in_first && native_layout_contains(second, pane_id);
        if !in_first && !in_second {
            return false;
        }
        // Recurse first so the nearest matching ancestor wins.
        if (in_first && visit(first, pane_id, positive_axis, matching_axis, delta))
            || (in_second && visit(second, pane_id, positive_axis, matching_axis, delta))
        {
            return true;
        }
        if !matching_axis(split_direction) {
            return false;
        }
        // A pane in the first branch grows toward Right/Down by increasing the first
        // branch's ratio. A pane in the second branch grows toward Right/Down by
        // decreasing that ratio.
        let increase = if in_first {
            positive_axis
        } else {
            !positive_axis
        };
        *ratio_millis = if increase {
            ratio_millis.saturating_add(delta).clamp(
                crate::command::MIN_LAUNCH_RATIO_MILLIS,
                crate::command::MAX_LAUNCH_RATIO_MILLIS,
            )
        } else {
            ratio_millis.saturating_sub(delta).clamp(
                crate::command::MIN_LAUNCH_RATIO_MILLIS,
                crate::command::MAX_LAUNCH_RATIO_MILLIS,
            )
        };
        true
    }

    visit(layout, pane_id, positive_axis, &matching_axis, delta)
}

fn native_allocated_resources(
    state: &NativeMuxState,
    plan: &MuxSessionLaunchPlan,
) -> Option<MuxAllocatedResources> {
    native_allocated_session(state, &plan.session_id)
}

fn native_allocated_session(
    state: &NativeMuxState,
    session_id: &str,
) -> Option<MuxAllocatedResources> {
    let session = state
        .sessions
        .iter()
        .find(|session| session.id == session_id)?;
    Some(MuxAllocatedResources {
        session_id: session.id.clone(),
        windows: session
            .windows
            .iter()
            .map(|window| MuxAllocatedWindow {
                window_id: window.id.clone(),
                pane_ids: window.panes.iter().map(|pane| pane.id.clone()).collect(),
            })
            .collect(),
    })
}

fn native_pane_process(pane: &NativePane) -> String {
    pane.launch
        .argv
        .as_ref()
        .and_then(|argv| argv.first())
        .cloned()
        .or_else(|| pane.launch.command.clone())
        .unwrap_or_else(|| "shell".to_owned())
}

fn native_pane_executable(pane: &NativePane) -> Option<String> {
    pane.launch
        .argv
        .as_ref()
        .and_then(|argv| argv.first())
        .cloned()
}

fn native_pane_occupant(pane: &NativePane) -> MuxOccupantIdentity {
    MuxOccupantIdentity {
        backend_identity: pane.occupant_id.clone(),
        pid: None,
        process: Some(native_pane_process(pane)),
    }
}

fn native_pane_target(
    session_id: &str,
    window: &NativeWindow,
    pane: &NativePane,
) -> MuxEventTarget {
    MuxEventTarget::pane(
        session_id,
        &window.id,
        &pane.id,
        pane.terminal_id.clone(),
        Some(native_pane_occupant(pane)),
    )
}

fn native_output_cursor(runtime: &NativePaneRuntimeIdentity, sequence: u64) -> MuxEventCursor {
    MuxEventCursor::new(
        format!(
            "native-output:{}:{}:{}:{}",
            runtime.session_id, runtime.window_id, runtime.pane_id, runtime.occupant_id
        ),
        sequence,
    )
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
        terminal_id: pane.map(|pane| pane.terminal_id.clone()),
        pane_pid: None,
        cwd: pane.map(|pane| pane.cwd.to_string_lossy().into_owned()),
        process: pane.map(native_pane_process),
        occupant_id: pane.map(|pane| pane.occupant_id.clone()),
    }
}

fn anchor_for_pane(session_id: &str, pane: &NativePane) -> MuxPaneAnchor {
    MuxPaneAnchor {
        session_id: session_id.to_owned(),
        pane_id: Some(pane.id.clone()),
        pane_pid: None,
        terminal_id: Some(pane.terminal_id.clone()),
        cwd: Some(pane.cwd.to_string_lossy().into_owned()),
        process: Some(native_pane_process(pane)),
        occupant_id: Some(pane.occupant_id.clone()),
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

#[cfg(test)]
type NativeMutationBarrier = Arc<(
    std::sync::mpsc::SyncSender<()>,
    Mutex<std::sync::mpsc::Receiver<()>>,
)>;

pub struct NativeBackend {
    state: Arc<Mutex<NativeMuxState>>,
    // This result belongs to the backend instance that executed the command, not its shared
    // workspace state.
    authoritative_completion: Option<MuxBackendCommandCompletion>,
    #[cfg(test)]
    mutation_barrier: Option<NativeMutationBarrier>,
}

impl Clone for NativeBackend {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
            authoritative_completion: None,
            #[cfg(test)]
            mutation_barrier: self.mutation_barrier.clone(),
        }
    }
}

fn wrap_index(index: usize, delta: i32, len: usize) -> usize {
    (index as i32 + delta).rem_euclid(len as i32) as usize
}

fn clamp_move_index(index: usize, delta: i32, len: usize) -> usize {
    (index as i32 + delta).clamp(0, len.saturating_sub(1) as i32) as usize
}

type NativeWorkspaceStates = HashMap<PathBuf, Arc<Mutex<NativeMuxState>>>;

static NATIVE_WORKSPACE_STATES: LazyLock<Mutex<NativeWorkspaceStates>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn native_event_backend_identity(workspace: &Path) -> String {
    let workspace = workspace.to_string_lossy();
    if workspace.is_empty() {
        "native:default-workspace".to_owned()
    } else {
        format!("native:{workspace}")
    }
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
        let mut states = NATIVE_WORKSPACE_STATES
            .lock()
            .expect("native mux state registry");
        let backend_identity = native_event_backend_identity(workspace);
        Self {
            state: Arc::clone(states.entry(workspace.to_path_buf()).or_insert_with(|| {
                Arc::new(Mutex::new(NativeMuxState::with_event_backend_identity(
                    backend_identity,
                )))
            })),
            authoritative_completion: None,
            #[cfg(test)]
            mutation_barrier: None,
        }
    }

    #[cfg(test)]
    fn with_state(state: NativeMuxState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
            authoritative_completion: None,
            mutation_barrier: None,
        }
    }

    #[cfg(test)]
    fn set_mutation_barrier(&mut self, barrier: NativeMutationBarrier) {
        self.mutation_barrier = Some(barrier);
    }

    #[cfg(test)]
    fn pane_launch_for_test(
        &self,
        session_id: &str,
        window_id: &str,
        pane_id: &str,
    ) -> Result<MuxPaneLaunch> {
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("native mux state lock poisoned"))?
            .pane_launch(session_id, window_id, pane_id)
            .ok_or_else(|| anyhow::anyhow!("native pane {pane_id} is not present"))
    }

    /// Begins observing one live terminal runtime. The lease is pinned to the current opaque
    /// occupant, so a stale PTY can never publish output for a replacement process.
    pub(crate) fn start_pane_runtime(
        &self,
        session_id: &str,
        pane_id: &str,
    ) -> Result<NativePaneRuntimeIdentity> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("native mux state lock poisoned"))?;
        let context = state.runtime_context(session_id, pane_id, None)?;
        Self::publish_runtime_start(&mut state, &context);
        Ok(context.identity)
    }

    /// Replaces the occupant only when a newly spawned PTY supersedes a live runtime lease.
    pub(crate) fn restart_pane_runtime(
        &self,
        previous: &NativePaneRuntimeIdentity,
    ) -> Result<NativePaneRuntimeIdentity> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("native mux state lock poisoned"))?;
        let (old, current) = state.replace_runtime_occupant(previous)?;
        let old_occupant = old.target.occupant.clone();
        let new_occupant = current.target.occupant.clone();
        state.events.publish(MuxEventDraft::new(
            MuxEventTopic::PaneOccupantReplaced,
            MuxEventProvenance::Native,
            Some(current.target.clone()),
            None,
            MuxEventPayload::OccupantReplaced {
                old_occupant,
                new_occupant,
            },
        ));
        Self::publish_runtime_start(&mut state, &current);
        Ok(current.identity)
    }

    pub(crate) fn publish_runtime_output(
        &self,
        runtime: &NativePaneRuntimeIdentity,
        sequence: u64,
        bytes: Vec<u8>,
    ) -> Result<()> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("native mux state lock poisoned"))?;
        let context =
            state.runtime_context(&runtime.session_id, &runtime.pane_id, Some(runtime))?;
        state.events.publish(MuxEventDraft::new(
            MuxEventTopic::TerminalOutput,
            MuxEventProvenance::Native,
            Some(context.target),
            Some(native_output_cursor(runtime, sequence)),
            MuxEventPayload::Output { bytes },
        ));
        Ok(())
    }

    pub(crate) fn publish_runtime_output_lag(
        &self,
        runtime: &NativePaneRuntimeIdentity,
        expected_sequence: u64,
        resume_sequence: u64,
        missed_events: u64,
    ) -> Result<()> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("native mux state lock poisoned"))?;
        let context =
            state.runtime_context(&runtime.session_id, &runtime.pane_id, Some(runtime))?;
        state.events.publish_gap(
            MuxEventProvenance::Native,
            Some(context.target),
            Some(native_output_cursor(runtime, resume_sequence)),
            expected_sequence,
            resume_sequence,
            missed_events,
        );
        Ok(())
    }

    pub(crate) fn publish_runtime_side_effect_lag(
        &self,
        runtime: &NativePaneRuntimeIdentity,
    ) -> Result<()> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("native mux state lock poisoned"))?;
        state.runtime_context(&runtime.session_id, &runtime.pane_id, Some(runtime))?;
        state.events.publish_rebase(MuxEventProvenance::Native);
        Ok(())
    }

    pub(crate) fn observe_runtime_title(
        &self,
        runtime: &NativePaneRuntimeIdentity,
        title: String,
    ) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("native mux state lock poisoned"))?;
        let context =
            state.runtime_context(&runtime.session_id, &runtime.pane_id, Some(runtime))?;
        let (old_title, pane_state) = {
            let runtime_state = state.runtime_state_for(&context);
            if runtime_state.title.as_deref() == Some(title.as_str()) {
                return Ok(());
            }
            let old_title = runtime_state.title.replace(title.clone());
            (old_title, runtime_state.pane_state())
        };
        state.events.publish(MuxEventDraft::new(
            MuxEventTopic::PaneTitleChanged,
            MuxEventProvenance::Native,
            Some(context.target.clone()),
            None,
            MuxEventPayload::Title {
                old_title,
                new_title: Some(title),
            },
        ));
        Self::publish_runtime_state(&mut state, context.target, pane_state);
        Ok(())
    }

    pub(crate) fn observe_runtime_option(
        &self,
        runtime: &NativePaneRuntimeIdentity,
        name: String,
        value: String,
    ) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("native mux state lock poisoned"))?;
        let context =
            state.runtime_context(&runtime.session_id, &runtime.pane_id, Some(runtime))?;
        let (old_value, pane_state) = {
            let runtime_state = state.runtime_state_for(&context);
            let old_value = runtime_state.options.insert(name.clone(), value.clone());
            if old_value.as_deref() == Some(value.as_str()) {
                return Ok(());
            }
            (old_value, runtime_state.pane_state())
        };
        state.events.publish(MuxEventDraft::new(
            MuxEventTopic::PaneOptionsChanged,
            MuxEventProvenance::Native,
            Some(context.target.clone()),
            None,
            MuxEventPayload::Option {
                name,
                old_value,
                new_value: Some(value),
            },
        ));
        Self::publish_runtime_state(&mut state, context.target, pane_state);
        Ok(())
    }

    pub(crate) fn observe_runtime_cwd(
        &self,
        runtime: &NativePaneRuntimeIdentity,
        cwd: String,
    ) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("native mux state lock poisoned"))?;
        let context =
            state.runtime_context(&runtime.session_id, &runtime.pane_id, Some(runtime))?;
        let (old_state, new_state, pane_state) = {
            let runtime_state = state.runtime_state_for(&context);
            if runtime_state.foreground.cwd.as_deref() == Some(cwd.as_str()) {
                return Ok(());
            }
            let old_state = runtime_state.foreground.clone();
            runtime_state.foreground.cwd = Some(cwd.clone());
            let new_state = runtime_state.foreground.clone();
            (old_state, new_state, runtime_state.pane_state())
        };
        state.update_runtime_cwd(runtime, &cwd)?;
        state.events.publish(MuxEventDraft::new(
            MuxEventTopic::PaneCwdChanged,
            MuxEventProvenance::Native,
            Some(context.target.clone()),
            None,
            MuxEventPayload::Cwd {
                old_cwd: old_state.cwd.clone(),
                new_cwd: new_state.cwd.clone(),
            },
        ));
        state.events.publish(MuxEventDraft::new(
            MuxEventTopic::PaneForegroundChanged,
            MuxEventProvenance::Native,
            Some(context.target.clone()),
            None,
            MuxEventPayload::Foreground {
                old_state: Some(old_state),
                new_state: Some(new_state),
            },
        ));
        Self::publish_runtime_state(&mut state, context.target, pane_state);
        Ok(())
    }

    fn publish_runtime_start(state: &mut NativeMuxState, context: &NativePaneRuntimeContext) {
        let pane_state = state.runtime_state_for(context).pane_state();
        if let Some(title) = context.initial_title.clone() {
            state.events.publish(MuxEventDraft::new(
                MuxEventTopic::PaneTitleChanged,
                MuxEventProvenance::Native,
                Some(context.target.clone()),
                None,
                MuxEventPayload::Title {
                    old_title: None,
                    new_title: Some(title),
                },
            ));
        }
        Self::publish_runtime_state(state, context.target.clone(), pane_state);
    }

    fn publish_runtime_state(
        state: &mut NativeMuxState,
        target: MuxEventTarget,
        pane_state: MuxPaneState,
    ) {
        state.events.publish(MuxEventDraft::new(
            MuxEventTopic::PaneStateChanged,
            MuxEventProvenance::Native,
            Some(target),
            None,
            MuxEventPayload::PaneState { state: pane_state },
        ));
    }
}

impl Default for NativeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeBackend {
    fn execute_with_precondition(
        &mut self,
        command: MuxCommand,
        precondition: Option<&MuxScopedExecutionPrecondition>,
    ) -> Result<()> {
        self.authoritative_completion = None;
        let event_command = command.clone();
        #[cfg(test)]
        if let Some(barrier) = &self.mutation_barrier {
            barrier.0.send(()).expect("native mutation barrier entry");
            barrier
                .1
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recv()
                .expect("native mutation barrier release");
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("native mux state lock poisoned"))?;
        if let Some(precondition) = precondition
            && !precondition.matches_snapshot(&state.snapshot())
        {
            return Err(MuxBackendOperationError::stale(
                "native mux command target changed before mutation",
            )
            .into());
        }
        state.validate_command_target(&command)?;

        // A destructive mutation must retain the precise lease it consumed. In particular,
        // choosing a sibling after close/kill must not retarget an event or completion to the
        // survivor's occupant generation.
        let pre_mutation_target = state.target_for_command(&command);
        let mut closed_targets = Vec::new();
        let mut project_session_allocation = None;

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
                if !state.activate_last_window(&session_id) {
                    return Err(MuxBackendOperationError::Failed(
                        "native session has no previous window to activate".to_owned(),
                    )
                    .into());
                }
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
                direction,
            } => state.split_pane(&session_id, pane_id.as_deref(), direction),
            MuxCommand::SelectPane {
                session_id,
                window_id,
                direction,
            } => state.select_directional_pane(&session_id, window_id.as_deref(), direction)?,
            MuxCommand::SelectNextPane {
                session_id,
                window_id,
            } => state.select_relative_pane(&session_id, window_id.as_deref(), 1)?,
            MuxCommand::SelectPreviousPane {
                session_id,
                window_id,
            } => state.select_relative_pane(&session_id, window_id.as_deref(), -1)?,
            MuxCommand::SelectLastPane {
                session_id,
                window_id,
            } => state.select_last_pane(&session_id, window_id.as_deref())?,
            MuxCommand::KillPane {
                session_id,
                pane_id,
            } => {
                if let Some(pane_id) = pane_id {
                    state.set_active_pane(&session_id, &pane_id);
                }
                if state.kill_active_pane(&session_id)
                    && let Some(target) = pre_mutation_target.clone()
                {
                    closed_targets.push((target, "native pane killed".to_owned()));
                }
            }
            MuxCommand::ClosePane {
                session_id,
                pane_id,
            } => {
                if state.close_pane(&session_id, pane_id.as_deref())
                    && let Some(target) = pre_mutation_target.clone()
                {
                    closed_targets.push((target, "native pane closed".to_owned()));
                }
            }
            MuxCommand::TogglePaneZoom {
                session_id,
                pane_id,
            } => state.toggle_pane_zoom(&session_id, pane_id.as_deref())?,
            MuxCommand::ResizePane {
                session_id,
                pane_id,
                adjustment,
            } => state.resize_pane(&session_id, pane_id.as_deref(), adjustment)?,
            MuxCommand::CreateSession { plan } => state.create_session_launch(&plan)?,
            MuxCommand::CreateProjectSession { session_id, cwd }
            | MuxCommand::CreateWorktreeSession { session_id, cwd } => {
                if state.ensure_session(&session_id, cwd) {
                    project_session_allocation = native_allocated_session(&state, &session_id);
                }
            }
            MuxCommand::RenameSession { session_id, name } => {
                state.rename_session(&session_id, name);
            }
            MuxCommand::DitchSession { session_id } => {
                closed_targets.extend(
                    state
                        .session_pane_targets(&session_id)
                        .into_iter()
                        .map(|target| (target, "native session ditched".to_owned())),
                );
                state.kill_session(&session_id);
            }
        }

        state.forget_runtime_targets(
            &closed_targets
                .iter()
                .map(|(target, _)| target.clone())
                .collect::<Vec<_>>(),
        );

        // Selection and window creation resolve the newly active leaf. All other commands keep
        // their pre-mutation resource target so explicit pane identities never retarget.
        let target = match &event_command {
            MuxCommand::NewWindow { session_id, .. }
            | MuxCommand::SelectPane { session_id, .. }
            | MuxCommand::SelectNextPane { session_id, .. }
            | MuxCommand::SelectPreviousPane { session_id, .. }
            | MuxCommand::SelectLastPane { session_id, .. } => state.pane_target(session_id, None),
            _ => pre_mutation_target,
        };
        let allocated = match &event_command {
            MuxCommand::CreateSession { plan } => native_allocated_resources(&state, plan),
            MuxCommand::CreateProjectSession { .. } | MuxCommand::CreateWorktreeSession { .. } => {
                project_session_allocation
            }
            _ => None,
        };
        let completion = MuxBackendCommandCompletion {
            allocated,
            target: target.clone(),
        };
        for (closed_target, reason) in closed_targets {
            state.events.publish(MuxEventDraft::new(
                MuxEventTopic::PaneClosed,
                MuxEventProvenance::Native,
                Some(closed_target),
                None,
                MuxEventPayload::Closed { reason },
            ));
        }
        state.publish_topology_change(target);
        drop(state);
        self.authoritative_completion = Some(completion);
        Ok(())
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
                BindingOperation::LastPane,
                BindingOperation::ResizePane,
                BindingOperation::TogglePaneZoom,
                BindingOperation::ClosePane,
                BindingOperation::CreateProjectSession,
                BindingOperation::CreateWorktreeSession,
                BindingOperation::RenameSession,
                BindingOperation::DitchSession,
            ],
        )
    }

    fn event_capabilities(&self) -> Vec<MuxEventCapability> {
        MuxEventTopic::ALL
            .into_iter()
            .map(|topic| match topic {
                MuxEventTopic::TopologyChanged
                | MuxEventTopic::TerminalOutput
                | MuxEventTopic::PaneStateChanged
                | MuxEventTopic::PaneOccupantReplaced
                | MuxEventTopic::PaneClosed
                | MuxEventTopic::BackendLagged
                | MuxEventTopic::SnapshotRebased => MuxEventCapability::available(topic),
                MuxEventTopic::PaneTitleChanged
                | MuxEventTopic::PaneOptionsChanged
                | MuxEventTopic::PaneForegroundChanged
                | MuxEventTopic::PaneCwdChanged => MuxEventCapability::best_effort(
                    topic,
                    "the native terminal runtime must publish this direct observation; no polling adapter fabricates it",
                ),
                MuxEventTopic::BackendDisconnected => MuxEventCapability::unsupported(
                    topic,
                    "the in-process native backend has no independently disconnectable event transport",
                ),
            })
            .collect()
    }

    fn start_event_stream(&mut self) {
        // Bootstrap is scoped to the first `drain_events` call for each binding.
    }
    fn drain_events(&mut self, scope: MuxScope, maximum: usize) -> Vec<MuxEvent> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .events
            .drain_with_initial_rebase(scope, maximum, MuxEventProvenance::Native)
    }
    fn release_event_scope(&mut self, scope: MuxScope) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.events.remove_scope(scope);
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        self.execute_with_precondition(command, None)
    }

    fn execute_checked(
        &mut self,
        scope: MuxScope,
        command: MuxCommand,
        precondition: Option<&MuxScopedExecutionPrecondition>,
    ) -> BindingOperationOutcome<Result<()>> {
        self.authoritative_completion = None;
        if precondition.is_some_and(|precondition| precondition.scope != scope) {
            return BindingOperationOutcome::Supported(Err(MuxBackendOperationError::stale(
                "native mux binding scope changed",
            )
            .into()));
        }
        let descriptor = self.capabilities(scope);
        descriptor.invoke(
            descriptor.request(command.operation()),
            BindingOperationAvailability::Available,
            || self.execute_with_precondition(command, precondition),
        )
    }

    fn execute_session_launch(
        &mut self,
        plan: MuxSessionLaunchPlan,
    ) -> BindingOperationOutcome<Result<()>> {
        self.authoritative_completion = None;
        if plan.validate().is_err() || !supports_native_session_launch_plan(&plan) {
            return BindingOperationOutcome::Unsupported;
        }
        BindingOperationOutcome::Supported(self.execute(MuxCommand::CreateSession { plan }))
    }

    fn session_launch_capability(
        &self,
        plan: &MuxSessionLaunchPlan,
    ) -> BindingOperationOutcome<()> {
        (plan.validate().is_ok() && supports_native_session_launch_plan(plan))
            .then_some(())
            .map_or(
                BindingOperationOutcome::Unsupported,
                BindingOperationOutcome::Supported,
            )
    }

    fn take_authoritative_completion(&mut self) -> Option<MuxBackendCommandCompletion> {
        self.authoritative_completion.take()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{Arc, Barrier, Mutex},
        thread,
    };

    use super::*;
    use crate::{
        backend::MuxRebaseReason,
        capability::BindingOperationOutcome,
        command::{
            MuxPaneLaunch, MuxPaneLaunchPlan, MuxSessionLaunchPlan, MuxSplitDirection,
            MuxSplitLaunch, MuxWindowLaunchPlan,
        },
    };

    #[test]
    fn native_runtime_restart_replaces_the_exact_occupant_once() {
        let mut backend = NativeBackend::with_state(local_state());
        let scope = MuxScope::new(
            crate::controller::SpaceId::from_persistence(1),
            crate::controller::BindingId::from_persistence(2),
        );
        let previous = backend
            .start_pane_runtime("local", "pane-1")
            .expect("start native runtime");
        let _ = backend.drain_events(scope, 8);
        let replacement = backend
            .restart_pane_runtime(&previous)
            .expect("restart native runtime");
        let events = backend.drain_events(scope, 8);
        let replacements = events
            .iter()
            .filter(|event| event.topic == MuxEventTopic::PaneOccupantReplaced)
            .collect::<Vec<_>>();
        assert_eq!(replacements.len(), 1);
        let event = replacements[0];

        assert_eq!(
            event
                .target
                .as_ref()
                .and_then(|target| target.session_id.as_deref()),
            Some("local")
        );
        assert_eq!(
            event
                .target
                .as_ref()
                .and_then(|target| target.window_id.as_deref()),
            Some("tab-1")
        );
        assert_eq!(
            event
                .target
                .as_ref()
                .and_then(|target| target.pane_id.as_deref()),
            Some("pane-1")
        );
        assert_eq!(
            event
                .target
                .as_ref()
                .and_then(|target| target.occupant.as_ref())
                .map(|occupant| occupant.backend_identity.as_str()),
            Some(replacement.occupant_id.as_str())
        );
        assert!(matches!(
            &event.payload,
            MuxEventPayload::OccupantReplaced {
                old_occupant: Some(old_occupant),
                new_occupant: Some(new_occupant),
            } if old_occupant.backend_identity == previous.occupant_id
                && new_occupant.backend_identity == replacement.occupant_id
        ));
    }

    #[test]
    fn native_event_capabilities_only_claim_registered_producers() {
        let backend = NativeBackend::with_state(local_state());
        let capabilities = backend.event_capabilities();
        let availability = |topic| {
            capabilities
                .iter()
                .find(|capability| capability.topic == topic)
                .map(|capability| &capability.availability)
                .expect("every stable event topic is described")
        };

        assert!(matches!(
            availability(MuxEventTopic::PaneClosed),
            &crate::backend::MuxEventAvailability::Available
        ));
        assert!(matches!(
            availability(MuxEventTopic::PaneCwdChanged),
            &crate::backend::MuxEventAvailability::BestEffort { .. }
        ));
        assert!(matches!(
            availability(MuxEventTopic::BackendDisconnected),
            &crate::backend::MuxEventAvailability::Unsupported { .. }
        ));
    }

    #[test]
    fn native_event_stream_gives_each_scope_a_bootstrap_rebase() {
        let mut backend = NativeBackend::with_state(NativeMuxState::new());
        let mut clone = backend.clone();
        let first_scope = native_scope();
        let second_scope = native_scope_for_binding(3);

        backend.start_event_stream();
        clone.start_event_stream();

        let first = backend.drain_events(first_scope, 8);
        let second = clone.drain_events(second_scope, 8);
        for (events, scope) in [(&first, first_scope), (&second, second_scope)] {
            assert_eq!(events.len(), 1);
            let event = &events[0];
            assert_eq!(event.topic, MuxEventTopic::SnapshotRebased);
            assert_eq!(event.provenance, MuxEventProvenance::Native);
            assert_eq!(event.scope, scope);
            assert_eq!(event.revision, 1);
            assert!(matches!(
                &event.payload,
                MuxEventPayload::Rebase {
                    reason: MuxRebaseReason::Bootstrap
                }
            ));
        }
        assert!(backend.drain_events(first_scope, 8).is_empty());
        assert!(clone.drain_events(second_scope, 8).is_empty());
    }
    #[test]
    fn native_scope_release_allows_a_recreated_binding_to_bootstrap_again() {
        let mut backend = NativeBackend::with_state(NativeMuxState::new());
        let scope = native_scope();

        let first = backend.drain_events(scope, 8);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].revision, 1);
        backend.release_event_scope(scope);

        let recreated = backend.drain_events(scope, 8);
        assert_eq!(recreated.len(), 1);
        assert_eq!(recreated[0].revision, 1);
        assert!(matches!(
            &recreated[0].payload,
            MuxEventPayload::Rebase {
                reason: MuxRebaseReason::Bootstrap
            }
        ));
    }

    fn native_scope() -> MuxScope {
        native_scope_for_binding(2)
    }

    fn native_scope_for_binding(binding_id: i64) -> MuxScope {
        MuxScope::new(
            crate::controller::SpaceId::from_persistence(1),
            crate::controller::BindingId::from_persistence(binding_id),
        )
    }

    #[test]
    fn native_runtime_output_and_state_events_keep_the_live_lease() {
        let mut backend = NativeBackend::with_state(local_state());
        let runtime = backend
            .start_pane_runtime("local", "pane-1")
            .expect("start native runtime");
        let _ = backend.drain_events(native_scope(), 8);

        backend
            .publish_runtime_output(&runtime, 7, b"exact bytes".to_vec())
            .expect("publish runtime output");
        backend
            .observe_runtime_option(
                &runtime,
                "terminal.colors".to_owned(),
                "configured".to_owned(),
            )
            .expect("publish runtime state");

        let events = backend.drain_events(native_scope(), 8);
        let output = events
            .iter()
            .find(|event| event.topic == MuxEventTopic::TerminalOutput)
            .expect("terminal output event");
        let state = events
            .iter()
            .find(|event| event.topic == MuxEventTopic::PaneStateChanged)
            .expect("pane state event");
        for event in [output, state] {
            let target = event.target.as_ref().expect("exact native target");
            assert_eq!(target.session_id.as_deref(), Some("local"));
            assert_eq!(target.window_id.as_deref(), Some("tab-1"));
            assert_eq!(target.pane_id.as_deref(), Some("pane-1"));
            assert_eq!(target.terminal_id.as_deref(), Some("terminal-1"));
            assert_eq!(
                target
                    .occupant
                    .as_ref()
                    .map(|occupant| occupant.backend_identity.as_str()),
                Some(runtime.occupant_id.as_str())
            );
        }
        assert!(matches!(
            (&output.cursor, &output.payload),
            (
                Some(cursor),
                MuxEventPayload::Output { bytes },
            ) if cursor.sequence == 7
                && cursor.stream.ends_with(runtime.occupant_id.as_str())
                && bytes == b"exact bytes"
        ));
    }
    fn local_state() -> NativeMuxState {
        let mut state = NativeMuxState::new();
        state.ensure_session("local", ".");
        state
    }

    #[test]
    fn native_pane_history_resize_and_zoom_are_authoritative() {
        let mut state = local_state();
        state.split_pane("local", None, MuxSplitDirection::Right);

        let first = state
            .require_active_window(state.require_session("local").unwrap())
            .unwrap()
            .panes[0]
            .id
            .clone();
        let second = state
            .require_active_window(state.require_session("local").unwrap())
            .unwrap()
            .panes[1]
            .id
            .clone();
        state.set_active_pane("local", &first);
        state.select_last_pane("local", None).unwrap();
        assert_eq!(
            state
                .require_active_window(state.require_session("local").unwrap())
                .unwrap()
                .active_pane_id,
            second
        );
        state.select_last_pane("local", None).unwrap();
        assert_eq!(
            state
                .require_active_window(state.require_session("local").unwrap())
                .unwrap()
                .active_pane_id,
            first
        );

        state
            .resize_pane(
                "local",
                Some(&first),
                MuxPaneResize::Directional {
                    direction: MuxDirection::Right,
                    cells: 4,
                },
            )
            .unwrap();
        let ratio = match &state
            .require_active_window(state.require_session("local").unwrap())
            .unwrap()
            .layout
        {
            MuxPaneLayout::Split { ratio_millis, .. } => *ratio_millis,
            MuxPaneLayout::Pane(_) => panic!("split layout disappeared"),
        };
        assert_eq!(ratio, 540);
        state
            .resize_pane(
                "local",
                Some(&first),
                MuxPaneResize::Directional {
                    direction: MuxDirection::Left,
                    cells: 100,
                },
            )
            .unwrap();
        let ratio = match &state
            .require_active_window(state.require_session("local").unwrap())
            .unwrap()
            .layout
        {
            MuxPaneLayout::Split { ratio_millis, .. } => *ratio_millis,
            MuxPaneLayout::Pane(_) => panic!("split layout disappeared"),
        };
        assert_eq!(ratio, 50);
        state.toggle_pane_zoom("local", Some(&first)).unwrap();
        assert_eq!(
            state
                .require_active_window(state.require_session("local").unwrap())
                .unwrap()
                .zoomed_pane_id
                .as_deref(),
            Some(first.as_str())
        );
        state.toggle_pane_zoom("local", Some(&first)).unwrap();
        assert_eq!(
            state
                .require_active_window(state.require_session("local").unwrap())
                .unwrap()
                .zoomed_pane_id,
            None
        );
    }

    #[test]
    fn native_backend_starts_without_a_bootty_owned_session() {
        let backend = NativeBackend::with_state(NativeMuxState::new());

        let snapshot = backend.snapshot().unwrap();

        assert_eq!(snapshot.active_session_id, None);
        assert!(snapshot.sessions.is_empty());
    }

    #[test]
    fn native_session_launch_materializes_flat_windows_with_allocated_identities() {
        let mut backend = NativeBackend::with_state(NativeMuxState::new());
        let plan = MuxSessionLaunchPlan {
            session_id: "review".to_owned(),
            focus: true,
            default_cwd: "/repo".to_owned(),
            environment: BTreeMap::new(),
            windows: vec![
                MuxWindowLaunchPlan {
                    name: Some("code".to_owned()),
                    focus: false,
                    layout: MuxPaneLaunchPlan::Pane(MuxPaneLaunch {
                        cwd: "/repo/code".to_owned(),
                        command: None,
                        argv: None,
                        environment: BTreeMap::new(),
                        title: None,
                    }),
                },
                MuxWindowLaunchPlan {
                    name: Some("shell".to_owned()),
                    focus: true,
                    layout: MuxPaneLaunchPlan::Pane(MuxPaneLaunch {
                        cwd: "/repo".to_owned(),
                        command: None,
                        argv: None,
                        environment: BTreeMap::new(),
                        title: None,
                    }),
                },
            ],
            focused_window: 1,
        };

        assert!(matches!(
            backend.execute_session_launch(plan),
            BindingOperationOutcome::Supported(Ok(()))
        ));

        let snapshot = backend.snapshot().unwrap();
        assert_eq!(snapshot.active_session_id.as_deref(), Some("review"));
        let session = &snapshot.sessions[0];
        assert_eq!(session.id, "review");
        assert_eq!(session.active_window_id.as_deref(), Some("tab-2"));
        assert_eq!(
            session
                .windows
                .iter()
                .map(|window| {
                    (
                        window.id.as_str(),
                        window.name.as_str(),
                        window.panes[0].pane_id.as_deref(),
                        window.panes[0].cwd.as_deref(),
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                ("tab-1", "code", Some("pane-1"), Some("/repo/code")),
                ("tab-2", "shell", Some("pane-2"), Some("/repo")),
            ]
        );
    }

    #[test]
    fn native_session_launch_materializes_recursive_windows_and_authoritative_launches() {
        let mut backend = NativeBackend::with_state(NativeMuxState::new());
        let plan = MuxSessionLaunchPlan {
            session_id: "review".to_owned(),
            focus: true,
            default_cwd: "/repo".to_owned(),
            environment: BTreeMap::from([("BASE".to_owned(), "session".to_owned())]),
            windows: vec![
                MuxWindowLaunchPlan {
                    name: Some("work".to_owned()),
                    focus: false,
                    layout: MuxPaneLaunchPlan::Split(MuxSplitLaunch {
                        direction: MuxSplitDirection::Right,
                        ratio_millis: 375,
                        first: Box::new(MuxPaneLaunchPlan::Pane(MuxPaneLaunch {
                            cwd: "/repo/api".to_owned(),
                            command: Some("printf api".to_owned()),
                            argv: None,
                            environment: BTreeMap::from([("BASE".to_owned(), "api".to_owned())]),
                            title: Some("API".to_owned()),
                        })),
                        second: Box::new(MuxPaneLaunchPlan::Split(MuxSplitLaunch {
                            direction: MuxSplitDirection::Down,
                            ratio_millis: 625,
                            first: Box::new(MuxPaneLaunchPlan::Pane(MuxPaneLaunch {
                                cwd: "/repo/web".to_owned(),
                                command: None,
                                argv: Some(vec!["web-server".to_owned(), "--watch".to_owned()]),
                                environment: BTreeMap::from([("WEB".to_owned(), "1".to_owned())]),
                                title: Some("Web".to_owned()),
                            })),
                            second: Box::new(MuxPaneLaunchPlan::Pane(MuxPaneLaunch {
                                cwd: "/repo/shell".to_owned(),
                                command: None,
                                argv: None,
                                environment: BTreeMap::new(),
                                title: None,
                            })),
                        })),
                    }),
                },
                MuxWindowLaunchPlan {
                    name: Some("logs".to_owned()),
                    focus: true,
                    layout: MuxPaneLaunchPlan::Pane(MuxPaneLaunch {
                        cwd: "/repo/logs".to_owned(),
                        command: Some("tail -f app.log".to_owned()),
                        argv: None,
                        environment: BTreeMap::new(),
                        title: Some("Logs".to_owned()),
                    }),
                },
            ],
            focused_window: 1,
        };

        assert!(matches!(
            backend.execute_session_launch(plan),
            BindingOperationOutcome::Supported(Ok(()))
        ));
        assert_eq!(
            backend
                .take_authoritative_completion()
                .and_then(|completion| completion.allocated),
            Some(MuxAllocatedResources {
                session_id: "review".to_owned(),
                windows: vec![
                    MuxAllocatedWindow {
                        window_id: "tab-1".to_owned(),
                        pane_ids: vec![
                            "pane-1".to_owned(),
                            "pane-2".to_owned(),
                            "pane-3".to_owned(),
                        ],
                    },
                    MuxAllocatedWindow {
                        window_id: "tab-2".to_owned(),
                        pane_ids: vec!["pane-4".to_owned()],
                    },
                ],
            })
        );

        let snapshot = backend.snapshot().unwrap();
        let session = &snapshot.sessions[0];
        assert_eq!(session.active_window_id.as_deref(), Some("tab-2"));
        assert_eq!(
            session.windows[0].layout,
            Some(MuxPaneLayout::Split {
                direction: MuxPaneSplitDirection::Right,
                ratio_millis: 375,
                first: Box::new(MuxPaneLayout::Pane("pane-1".to_owned())),
                second: Box::new(MuxPaneLayout::Split {
                    direction: MuxPaneSplitDirection::Down,
                    ratio_millis: 625,
                    first: Box::new(MuxPaneLayout::Pane("pane-2".to_owned())),
                    second: Box::new(MuxPaneLayout::Pane("pane-3".to_owned())),
                }),
            })
        );

        let api = backend
            .pane_launch_for_test("review", "tab-1", "pane-1")
            .unwrap();
        assert_eq!(api.cwd, "/repo/api");
        assert_eq!(api.command.as_deref(), Some("printf api"));
        assert_eq!(api.environment["BASE"], "api");
        assert_eq!(api.title.as_deref(), Some("API"));

        let web = backend
            .pane_launch_for_test("review", "tab-1", "pane-2")
            .unwrap();
        assert_eq!(
            web.argv.as_deref(),
            Some(["web-server".to_owned(), "--watch".to_owned()].as_slice())
        );
        assert_eq!(web.environment["BASE"], "session");
        assert_eq!(web.environment["WEB"], "1");

        let shell = backend
            .pane_launch_for_test("review", "tab-1", "pane-3")
            .unwrap();
        assert_eq!(shell.environment["BASE"], "session");
    }

    #[test]
    fn native_session_launch_rejects_malformed_plan_before_state_mutation() {
        let mut backend = NativeBackend::with_state(NativeMuxState::new());
        let plan = MuxSessionLaunchPlan {
            session_id: "review".to_owned(),
            focus: true,
            default_cwd: "/repo".to_owned(),
            environment: BTreeMap::new(),
            windows: vec![MuxWindowLaunchPlan {
                name: None,
                focus: true,
                layout: MuxPaneLaunchPlan::Pane(MuxPaneLaunch {
                    cwd: String::new(),
                    command: None,
                    argv: None,
                    environment: BTreeMap::new(),
                    title: None,
                }),
            }],
            focused_window: 0,
        };

        assert!(matches!(
            backend.execute_session_launch(plan),
            BindingOperationOutcome::Unsupported
        ));
        assert!(backend.snapshot().unwrap().sessions.is_empty());
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
    fn new_project_session_completion_allocates_only_the_new_native_resources() {
        let mut backend = NativeBackend::with_state(NativeMuxState::new());
        let command = MuxCommand::CreateProjectSession {
            session_id: "project".to_owned(),
            cwd: "/repo".to_owned(),
        };

        backend
            .execute(command.clone())
            .expect("create project session");
        let first = backend
            .take_authoritative_completion()
            .and_then(|completion| completion.allocated)
            .expect("new project allocation");
        assert_eq!(first.session_id, "project");
        assert_eq!(first.windows.len(), 1);
        assert_eq!(first.windows[0].window_id, "tab-1");
        assert_eq!(first.windows[0].pane_ids, vec!["pane-1".to_owned()]);

        backend.execute(command).expect("reuse project session");
        assert!(
            backend
                .take_authoritative_completion()
                .is_some_and(|completion| completion.allocated.is_none()),
            "an idempotent project command must not re-register an existing PTY"
        );
    }

    #[test]
    fn native_clones_keep_concurrent_completions_with_their_callers() {
        let backend = NativeBackend::with_state(NativeMuxState::new());
        let mut first = backend.clone();
        let mut second = backend;
        let start = Arc::new(Barrier::new(3));
        let executed = Arc::new(Barrier::new(3));

        let first_start = Arc::clone(&start);
        let first_executed = Arc::clone(&executed);
        let first_caller = thread::spawn(move || {
            first_start.wait();
            first
                .execute(MuxCommand::CreateProjectSession {
                    session_id: "completion-a".to_owned(),
                    cwd: "/repo/a".to_owned(),
                })
                .expect("first concurrent create");
            first_executed.wait();
            (
                first.take_authoritative_completion(),
                first.take_authoritative_completion(),
            )
        });

        let second_start = Arc::clone(&start);
        let second_executed = Arc::clone(&executed);
        let second_caller = thread::spawn(move || {
            second_start.wait();
            second
                .execute(MuxCommand::CreateProjectSession {
                    session_id: "completion-b".to_owned(),
                    cwd: "/repo/b".to_owned(),
                })
                .expect("second concurrent create");
            second_executed.wait();
            (
                second.take_authoritative_completion(),
                second.take_authoritative_completion(),
            )
        });

        start.wait();
        executed.wait();
        let (first_completion, repeated_first_completion) =
            first_caller.join().expect("first concurrent caller");
        let (second_completion, repeated_second_completion) =
            second_caller.join().expect("second concurrent caller");

        assert!(repeated_first_completion.is_none());
        assert!(repeated_second_completion.is_none());
        let first_completion = first_completion.expect("first caller completion");
        let second_completion = second_completion.expect("second caller completion");
        assert_eq!(
            first_completion.target,
            Some(MuxEventTarget::session("completion-a"))
        );
        assert_eq!(
            second_completion.target,
            Some(MuxEventTarget::session("completion-b"))
        );
        let first_allocation = first_completion.allocated.expect("first caller allocation");
        let second_allocation = second_completion
            .allocated
            .expect("second caller allocation");
        assert_eq!(first_allocation.session_id, "completion-a");
        assert_eq!(second_allocation.session_id, "completion-b");
        assert_eq!(first_allocation.windows.len(), 1);
        assert_eq!(second_allocation.windows.len(), 1);
        assert_eq!(first_allocation.windows[0].pane_ids.len(), 1);
        assert_eq!(second_allocation.windows[0].pane_ids.len(), 1);
        assert_ne!(
            first_allocation.windows[0].pane_ids[0],
            second_allocation.windows[0].pane_ids[0]
        );
    }

    #[test]
    fn failed_native_commands_clear_pending_completion() {
        let mut backend = NativeBackend::with_state(local_state());

        backend
            .execute(MuxCommand::RenameSession {
                session_id: "local".to_owned(),
                name: "first".to_owned(),
            })
            .expect("seed completion before failure");
        assert!(backend.authoritative_completion.is_some());
        assert!(matches!(
            backend.execute_checked(
                native_scope(),
                MuxCommand::ActivateLastWindow {
                    session_id: "local".to_owned(),
                },
                None,
            ),
            BindingOperationOutcome::Supported(Err(_))
        ));
        assert!(backend.take_authoritative_completion().is_none());

        backend
            .execute(MuxCommand::RenameSession {
                session_id: "local".to_owned(),
                name: "second".to_owned(),
            })
            .expect("seed completion before rejected command");
        assert!(backend.authoritative_completion.is_some());
        assert!(matches!(
            backend.execute_checked(
                native_scope(),
                MuxCommand::ResizePane {
                    session_id: "local".to_owned(),
                    pane_id: None,
                    adjustment: MuxPaneResize::Absolute {
                        columns: Some(120),
                        rows: Some(40),
                    },
                },
                None,
            ),
            BindingOperationOutcome::Supported(Err(_))
        ));
        assert!(backend.take_authoritative_completion().is_none());
    }

    #[test]
    fn native_checked_command_rejects_replaced_occupant_at_mutation_boundary() {
        let mut backend = NativeBackend::with_state(local_state());
        let scope = native_scope();
        let previous = backend
            .start_pane_runtime("local", "pane-1")
            .expect("start native runtime");
        let before = backend.snapshot().expect("snapshot before queued command");
        let pane = &before.sessions[0].windows[0].anchor;
        let occupant_id = pane.occupant_id.clone().expect("native occupant identity");
        let precondition = MuxScopedExecutionPrecondition {
            scope,
            target: MuxEventTarget::pane(
                "local",
                "tab-1",
                "pane-1",
                pane.terminal_id.clone().expect("native terminal identity"),
                Some(MuxOccupantIdentity {
                    backend_identity: occupant_id.clone(),
                    pid: pane.pane_pid,
                    process: pane.process.clone(),
                }),
            ),
            occupant_fingerprint: Some(occupant_id),
            binding_generation: None,
            occupant_generation: Some(1),
        };
        assert!(precondition.matches_snapshot(&before));

        let command_precondition = precondition.clone();
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        backend.set_mutation_barrier(Arc::new((entered_tx, Mutex::new(release_rx))));

        let mut command_backend = backend.clone();
        let command = thread::spawn(move || {
            command_backend.execute_checked(
                scope,
                MuxCommand::ClosePane {
                    session_id: "local".to_owned(),
                    pane_id: Some("pane-1".to_owned()),
                },
                Some(&command_precondition),
            )
        });
        entered_rx
            .recv()
            .expect("checked command reached the final backend boundary");

        let replacement_backend = backend.clone();
        replacement_backend
            .restart_pane_runtime(&previous)
            .expect("replace native occupant while command is paused");
        release_tx
            .send(())
            .expect("release checked command after replacement");

        let outcome = command.join().expect("checked command thread");
        let error = match outcome {
            BindingOperationOutcome::Supported(Err(error)) => error,
            other => panic!("replaced occupant must be stale, got {other:?}"),
        };
        assert!(matches!(
            error.downcast_ref::<MuxBackendOperationError>(),
            Some(MuxBackendOperationError::Stale(_))
        ));
        assert!(
            backend
                .snapshot()
                .expect("snapshot after stale command")
                .sessions[0]
                .windows[0]
                .panes
                .iter()
                .any(|pane| pane.pane_id.as_deref() == Some("pane-1")),
            "replacement pane must remain unchanged"
        );
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
        let events = backend.drain_events(
            MuxScope::new(
                crate::controller::SpaceId::from_persistence(1),
                crate::controller::BindingId::from_persistence(2),
            ),
            8,
        );
        let closed = events
            .iter()
            .find(|event| event.topic == MuxEventTopic::PaneClosed)
            .expect("pane closed event");
        assert_eq!(
            closed
                .target
                .as_ref()
                .and_then(|target| target.session_id.as_deref()),
            Some("local")
        );
        assert_eq!(
            closed
                .target
                .as_ref()
                .and_then(|target| target.window_id.as_deref()),
            Some("tab-1")
        );
        assert_eq!(
            closed
                .target
                .as_ref()
                .and_then(|target| target.pane_id.as_deref()),
            Some("pane-1")
        );
        let target = closed.target.as_ref().expect("closed target");
        assert_eq!(target.terminal_id.as_deref(), Some("terminal-1"));
        assert_eq!(
            target
                .occupant
                .as_ref()
                .map(|occupant| occupant.backend_identity.as_str()),
            Some("native-occupant-1")
        );

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
    fn kill_and_ditch_publish_each_retired_native_pane_once() {
        let mut state = local_state();
        state.split_pane("local", None, MuxSplitDirection::Right);
        let mut backend = NativeBackend::with_state(state);

        backend
            .execute(MuxCommand::KillPane {
                session_id: "local".to_owned(),
                pane_id: Some("pane-1".to_owned()),
            })
            .expect("kill named pane");
        let killed = backend
            .drain_events(native_scope(), 8)
            .into_iter()
            .filter(|event| event.topic == MuxEventTopic::PaneClosed)
            .collect::<Vec<_>>();
        assert_eq!(killed.len(), 1);
        let killed_target = killed[0].target.as_ref().expect("killed pane target");
        assert_eq!(killed_target.pane_id.as_deref(), Some("pane-1"));
        assert_eq!(killed_target.terminal_id.as_deref(), Some("terminal-1"));
        assert_eq!(
            killed_target
                .occupant
                .as_ref()
                .map(|occupant| occupant.backend_identity.as_str()),
            Some("native-occupant-1")
        );

        let mut ditch_state = local_state();
        ditch_state.split_pane("local", None, MuxSplitDirection::Right);
        ditch_state.new_window("local", None);
        let mut ditch_backend = NativeBackend::with_state(ditch_state);
        ditch_backend
            .execute(MuxCommand::DitchSession {
                session_id: "local".to_owned(),
            })
            .expect("ditch session");
        let mut ditched = ditch_backend
            .drain_events(native_scope(), 8)
            .into_iter()
            .filter(|event| event.topic == MuxEventTopic::PaneClosed)
            .map(|event| event.target.expect("ditched pane target"))
            .collect::<Vec<_>>();
        ditched.sort_by(|left, right| left.pane_id.cmp(&right.pane_id));
        assert_eq!(ditched.len(), 3);
        assert_eq!(
            ditched
                .iter()
                .map(|target| {
                    (
                        target.pane_id.as_deref(),
                        target.terminal_id.as_deref(),
                        target
                            .occupant
                            .as_ref()
                            .map(|occupant| occupant.backend_identity.as_str()),
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                (
                    Some("pane-1"),
                    Some("terminal-1"),
                    Some("native-occupant-1")
                ),
                (
                    Some("pane-2"),
                    Some("terminal-2"),
                    Some("native-occupant-2")
                ),
                (
                    Some("pane-3"),
                    Some("terminal-3"),
                    Some("native-occupant-3")
                ),
            ]
        );
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
        state.split_pane("local", None, MuxSplitDirection::Right);

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
    fn move_window_preserving_selection_rejects_a_stale_selected_window() {
        let mut state = local_state();
        state.new_window("local", None);
        let mut backend = NativeBackend::with_state(state);

        let error = backend
            .execute(MuxCommand::MoveWindowPreservingSelection {
                session_id: "local".to_owned(),
                window_id: "tab-1".to_owned(),
                delta: 1,
                selected_window_id: "tab-missing".to_owned(),
            })
            .expect_err("a vanished selected window must reject the move");

        assert!(matches!(
            error.downcast_ref::<MuxBackendOperationError>(),
            Some(MuxBackendOperationError::Stale(message))
                if message == "native window \"tab-missing\" no longer exists"
        ));
        let session = &backend.snapshot().unwrap().sessions[0];
        assert_eq!(
            session
                .windows
                .iter()
                .map(|window| window.id.as_str())
                .collect::<Vec<_>>(),
            vec!["tab-1", "tab-2"]
        );
        assert_eq!(session.active_window_id.as_deref(), Some("tab-2"));
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
