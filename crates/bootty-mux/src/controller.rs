use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use bootty_config::config::{MultiplexerBackendConfig, MultiplexerConfig};
use serde::{Deserialize, Serialize};

use crate::{
    RepaintHandle,
    backend::{
        MuxBackend, MuxBackendCommandCompletion, MuxBackendOperationError, MuxEvent,
        MuxEventCapability, MuxEventPayload, MuxEventTarget, MuxOccupantIdentity, MuxRebaseReason,
        MuxScopedExecutionPrecondition, snapshot_occupant_fingerprint,
    },
    capability::{BindingOperation, BindingOperationAvailability, BindingOperationOutcome},
    command::{MuxCommand, MuxPaneLaunchPlan, MuxSessionLaunchPlan, MuxSessionLaunchPlanError},
    config::{BackendFactory, build_backend_with, selected_backend},
    rmux::rmux_operation_requires_checked_boundary,
    snapshot::{
        MuxPaneAnchor, MuxPaneLayout, MuxSession, MuxSnapshot, selection_after_refresh,
        session_matches,
    },
};

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

type SessionRefreshSnapshot = std::result::Result<(MultiplexerBackendConfig, MuxSnapshot), String>;
type SessionRefreshResult = (u64, SessionRefreshSnapshot);

struct SessionRefreshRequest {
    generation: u64,
    config: MultiplexerConfig,
}

#[derive(Clone, Debug)]
pub struct CommandCancellation {
    state: Arc<AtomicU8>,
    requested: Arc<AtomicBool>,
}

impl Default for CommandCancellation {
    fn default() -> Self {
        Self {
            state: Arc::new(AtomicU8::new(Self::PENDING)),
            requested: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl CommandCancellation {
    const PENDING: u8 = 0;
    const STARTED: u8 = 1;
    const CANCELLED: u8 = 2;

    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation without changing a started operation's lifecycle state.
    ///
    /// A pending operation still transitions to `CANCELLED` so queue consumers can skip it.
    /// Once an operation has started, the independent request flag lets the operation observe
    /// cancellation and reconcile its actual completion without pretending it never ran.
    pub fn request_cancel(&self) -> bool {
        self.requested.store(true, Ordering::Release);
        self.state
            .compare_exchange(
                Self::PENDING,
                Self::CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub fn cancel(&self) -> bool {
        self.request_cancel()
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::Acquire) == Self::CANCELLED
    }

    pub fn is_cancel_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    pub fn is_started(&self) -> bool {
        self.state.load(Ordering::Acquire) == Self::STARTED
    }

    pub fn try_start(&self) -> bool {
        if self.is_cancel_requested() {
            return false;
        }
        self.state
            .compare_exchange(
                Self::PENDING,
                Self::STARTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
            && !self.is_cancel_requested()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MuxCommandError {
    Cancelled,
    DeadlineExceeded,
    Unsupported,
    Unavailable,
    Denied,
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
            Self::Denied => formatter.write_str("mux operation was denied"),
            Self::Stale => formatter.write_str("mux operation target is stale"),
            Self::Failed(message) => formatter.write_str(message),
        }
    }
}

pub type MuxCommandResult = std::result::Result<MuxCommandCompletion, MuxCommandError>;

pub use crate::backend::{MuxAllocatedResources, MuxAllocatedWindow};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MuxCommandCompletion {
    pub selected_session: Option<String>,
    pub selected_window: Option<String>,
    allocated: Option<MuxAllocatedResources>,
    resolved_target: Option<MuxEventTarget>,
    execution_precondition: Option<MuxScopedExecutionPrecondition>,
    snapshot: Option<(MultiplexerConfig, MuxSnapshot)>,
}

impl MuxCommandCompletion {
    fn requested(selected_session: Option<String>, selected_window: Option<String>) -> Self {
        Self {
            selected_session,
            selected_window,
            allocated: None,
            resolved_target: None,
            execution_precondition: None,
            snapshot: None,
        }
    }

    fn with_execution_precondition(
        mut self,
        execution_precondition: Option<MuxScopedExecutionPrecondition>,
    ) -> Self {
        self.execution_precondition = execution_precondition;
        self
    }

    /// Resource identities allocated by an authoritative session-create command.
    pub fn allocated(&self) -> Option<&MuxAllocatedResources> {
        self.allocated.as_ref()
    }

    /// Exact backend resource resolved after this command's mutation.
    pub fn resolved_target(&self) -> Option<&MuxEventTarget> {
        self.resolved_target.as_ref()
    }

    /// The binding-scoped identity that was rechecked immediately before the mutation.
    pub fn execution_precondition(&self) -> Option<&MuxScopedExecutionPrecondition> {
        self.execution_precondition.as_ref()
    }

    fn from_snapshot(config: MultiplexerConfig, snapshot: MuxSnapshot) -> Self {
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
            allocated: None,
            resolved_target: None,
            execution_precondition: None,
            snapshot: Some((config, snapshot)),
        }
    }

    fn from_command_snapshot(
        config: MultiplexerConfig,
        snapshot: MuxSnapshot,
        command: &MuxCommand,
        execution_precondition: Option<MuxScopedExecutionPrecondition>,
        authoritative: Option<MuxBackendCommandCompletion>,
    ) -> Result<Self, MuxCommandError> {
        let authoritative = authoritative.unwrap_or_default();
        let allocated = match command {
            MuxCommand::CreateSession { plan } => {
                let allocated = authoritative
                    .allocated
                    .map(|allocated| {
                        validate_authoritative_allocated_resources(plan, &snapshot, allocated)
                    })
                    .transpose()?
                    .map(Ok)
                    .unwrap_or_else(|| allocated_resources_for_plan(plan, &snapshot))?;
                Some(allocated)
            }
            MuxCommand::CreateProjectSession { session_id, .. }
            | MuxCommand::CreateWorktreeSession { session_id, .. } => authoritative
                .allocated
                .map(|allocated| {
                    validate_authoritative_single_pane_allocation(session_id, &snapshot, allocated)
                })
                .transpose()?,
            _ => None,
        };
        let mut completion = Self::from_snapshot(config, snapshot.clone());
        if let (MuxCommand::CreateSession { plan }, Some(allocated)) = (command, allocated.as_ref())
            && plan.focus
        {
            completion.selected_session = Some(allocated.session_id.clone());
            completion.selected_window = allocated
                .windows
                .get(plan.focused_window)
                .map(|window| window.window_id.clone());
        }
        completion.allocated = allocated;
        completion.resolved_target = authoritative
            .target
            .or_else(|| resolved_target_for_command(command, &snapshot));
        completion.execution_precondition = execution_precondition;
        Ok(completion)
    }
}

/// Validates identities captured by a backend's create transaction.
///
/// The transaction is the authority for recursive DFS order. Some backends intentionally expose
/// only one attach anchor in their snapshots, so requiring a synthetic split tree here would turn
/// a successfully committed launch into a false failure after mutation.
fn validate_authoritative_allocated_resources(
    plan: &MuxSessionLaunchPlan,
    snapshot: &MuxSnapshot,
    allocated: MuxAllocatedResources,
) -> Result<MuxAllocatedResources, MuxCommandError> {
    let observed_windows = validate_allocated_resource_shape(plan, snapshot, &allocated)?;
    for ((allocated_window, expected), observed_window) in allocated
        .windows
        .iter()
        .zip(&plan.windows)
        .zip(observed_windows)
    {
        if matches!(&expected.layout, MuxPaneLaunchPlan::Pane(_)) {
            let expected_pane_ids = pane_ids_for_plan_window(observed_window, &expected.layout)?;
            if allocated_window.pane_ids != expected_pane_ids {
                return Err(MuxCommandError::Failed(format!(
                    "backend allocated pane identities that do not match DFS declaration order for window {:?}",
                    allocated_window.window_id
                )));
            }
        } else {
            for pane_id in observed_window
                .panes
                .iter()
                .chain(std::iter::once(&observed_window.anchor))
                .filter_map(|pane| pane.pane_id.as_deref())
            {
                if !allocated_window.pane_ids.iter().any(|id| id == pane_id) {
                    return Err(MuxCommandError::Failed(format!(
                        "backend allocated pane identities that do not include observed pane {:?} for window {:?}",
                        pane_id, allocated_window.window_id
                    )));
                }
            }
        }
    }
    Ok(allocated)
}

fn validate_allocated_resources(
    plan: &MuxSessionLaunchPlan,
    snapshot: &MuxSnapshot,
    allocated: MuxAllocatedResources,
) -> Result<MuxAllocatedResources, MuxCommandError> {
    let observed_windows = validate_allocated_resource_shape(plan, snapshot, &allocated)?;
    for ((allocated_window, expected), observed_window) in allocated
        .windows
        .iter()
        .zip(&plan.windows)
        .zip(observed_windows)
    {
        let expected_pane_ids = pane_ids_for_plan_window(observed_window, &expected.layout)?;
        if allocated_window.pane_ids != expected_pane_ids {
            return Err(MuxCommandError::Failed(format!(
                "backend allocated pane identities that do not match DFS declaration order for window {:?}",
                allocated_window.window_id
            )));
        }
    }
    Ok(allocated)
}

fn validate_allocated_resource_shape<'a>(
    plan: &MuxSessionLaunchPlan,
    snapshot: &'a MuxSnapshot,
    allocated: &MuxAllocatedResources,
) -> Result<Vec<&'a crate::snapshot::MuxWindow>, MuxCommandError> {
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == plan.session_id || session.name == plan.session_id)
        .ok_or_else(|| {
            MuxCommandError::Failed(format!(
                "backend did not report the created session {:?}",
                plan.session_id
            ))
        })?;
    if allocated.session_id != session.id {
        return Err(MuxCommandError::Failed(format!(
            "backend allocated session {:?}, but the created session is {:?}",
            allocated.session_id, session.id
        )));
    }

    let mut observed_windows = session.windows.iter().collect::<Vec<_>>();
    observed_windows.sort_by_key(|window| window.index);
    if allocated.windows.len() != plan.windows.len() || observed_windows.len() != plan.windows.len()
    {
        return Err(MuxCommandError::Failed(format!(
            "backend allocated {} windows for session {:?}, expected {}",
            allocated.windows.len(),
            plan.session_id,
            plan.windows.len()
        )));
    }

    for (index, ((allocated_window, expected), observed_window)) in allocated
        .windows
        .iter()
        .zip(&plan.windows)
        .zip(&observed_windows)
        .enumerate()
    {
        if allocated_window.window_id.is_empty()
            || allocated.windows[..index]
                .iter()
                .any(|window| window.window_id == allocated_window.window_id)
        {
            return Err(MuxCommandError::Failed(
                "backend allocated non-unique window identities".to_owned(),
            ));
        }
        if allocated_window.window_id != observed_window.id {
            return Err(MuxCommandError::Failed(format!(
                "backend allocated window {:?}, but declaration index {} is {:?}",
                allocated_window.window_id, index, observed_window.id
            )));
        }

        let expected_panes = expected.layout.pane_count();
        if allocated_window.pane_ids.len() != expected_panes {
            return Err(MuxCommandError::Failed(format!(
                "backend allocated {} panes for window {:?}, expected {}",
                allocated_window.pane_ids.len(),
                allocated_window.window_id,
                expected_panes
            )));
        }
        if allocated_window.pane_ids.iter().any(String::is_empty)
            || allocated_window
                .pane_ids
                .iter()
                .enumerate()
                .any(|(pane_index, pane_id)| {
                    allocated_window.pane_ids[..pane_index].contains(pane_id)
                        || allocated.windows[..index]
                            .iter()
                            .any(|window| window.pane_ids.contains(pane_id))
                })
        {
            return Err(MuxCommandError::Failed(format!(
                "backend allocated non-unique pane identities for window {:?}",
                allocated_window.window_id
            )));
        }
    }
    Ok(observed_windows)
}

/// Validate the exact one-window/one-pane allocation produced by the simple project/worktree
/// session create path. Unlike a recursive launch, `None` remains meaningful: an idempotent
/// ensure may have selected a pre-existing session rather than creating a terminal.
fn validate_authoritative_single_pane_allocation(
    requested_session_id: &str,
    snapshot: &MuxSnapshot,
    allocated: MuxAllocatedResources,
) -> Result<MuxAllocatedResources, MuxCommandError> {
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == requested_session_id || session.name == requested_session_id)
        .ok_or_else(|| {
            MuxCommandError::Failed(format!(
                "backend did not report the created session {requested_session_id:?}",
            ))
        })?;
    if allocated.session_id != session.id {
        return Err(MuxCommandError::Failed(format!(
            "backend allocated session {:?}, but the created session is {:?}",
            allocated.session_id, session.id
        )));
    }
    if allocated.windows.len() != 1 || session.windows.len() != 1 {
        return Err(MuxCommandError::Failed(format!(
            "backend allocated a simple session with {} windows, expected one",
            allocated.windows.len()
        )));
    }
    let allocated_window = &allocated.windows[0];
    let window = &session.windows[0];
    if allocated_window.window_id != window.id {
        return Err(MuxCommandError::Failed(format!(
            "backend allocated window {:?}, but the created window is {:?}",
            allocated_window.window_id, window.id
        )));
    }
    if allocated_window.pane_ids.len() != 1 {
        return Err(MuxCommandError::Failed(format!(
            "backend allocated {} panes for simple window {:?}, expected one",
            allocated_window.pane_ids.len(),
            allocated_window.window_id
        )));
    }
    let pane_id = &allocated_window.pane_ids[0];
    if pane_id.is_empty()
        || !std::iter::once(&window.anchor)
            .chain(&window.panes)
            .any(|pane| pane.pane_id.as_deref() == Some(pane_id.as_str()))
    {
        return Err(MuxCommandError::Failed(format!(
            "backend allocated pane {:?}, but it is absent from window {:?}",
            pane_id, allocated_window.window_id
        )));
    }
    Ok(allocated)
}

fn allocated_resources_for_plan(
    plan: &MuxSessionLaunchPlan,
    snapshot: &MuxSnapshot,
) -> Result<MuxAllocatedResources, MuxCommandError> {
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == plan.session_id || session.name == plan.session_id)
        .ok_or_else(|| {
            MuxCommandError::Failed(format!(
                "backend did not report the created session {:?}",
                plan.session_id
            ))
        })?;
    let mut windows = session.windows.iter().collect::<Vec<_>>();
    windows.sort_by_key(|window| window.index);
    if windows.len() != plan.windows.len() {
        return Err(MuxCommandError::Failed(format!(
            "backend created {} windows for session {:?}, expected {}",
            windows.len(),
            plan.session_id,
            plan.windows.len()
        )));
    }
    let mut allocated_windows = Vec::with_capacity(windows.len());
    for (window, expected) in windows.into_iter().zip(&plan.windows) {
        allocated_windows.push(MuxAllocatedWindow {
            window_id: window.id.clone(),
            pane_ids: pane_ids_for_plan_window(window, &expected.layout)?,
        });
    }
    validate_allocated_resources(
        plan,
        snapshot,
        MuxAllocatedResources {
            session_id: session.id.clone(),
            windows: allocated_windows,
        },
    )
}

fn pane_ids_for_plan_window(
    window: &crate::snapshot::MuxWindow,
    expected: &MuxPaneLaunchPlan,
) -> Result<Vec<String>, MuxCommandError> {
    if matches!(expected, MuxPaneLaunchPlan::Split(_)) {
        let observed = window.layout.as_ref().ok_or_else(|| {
            MuxCommandError::Failed(format!(
                "backend did not report recursive pane topology for window {:?}",
                window.id
            ))
        })?;
        if !layout_matches_launch_plan(observed, expected) {
            return Err(MuxCommandError::Failed(format!(
                "backend reported topology that differs from recursive launch for window {:?}",
                window.id
            )));
        }
        let mut pane_ids = Vec::new();
        collect_layout_pane_ids(observed, &mut pane_ids);
        return Ok(pane_ids);
    }
    Ok(window
        .panes
        .iter()
        .filter_map(|pane| pane.pane_id.clone())
        .collect())
}

fn layout_matches_launch_plan(observed: &MuxPaneLayout, expected: &MuxPaneLaunchPlan) -> bool {
    match (observed, expected) {
        (MuxPaneLayout::Pane(_), MuxPaneLaunchPlan::Pane(_)) => true,
        (
            MuxPaneLayout::Split {
                direction: observed_direction,
                ratio_millis: observed_ratio,
                first: observed_first,
                second: observed_second,
            },
            MuxPaneLaunchPlan::Split(expected_split),
        ) => {
            *observed_ratio == expected_split.ratio_millis
                && matches!(
                    (observed_direction, expected_split.direction),
                    (
                        crate::snapshot::MuxPaneSplitDirection::Right,
                        crate::command::MuxSplitDirection::Right
                    ) | (
                        crate::snapshot::MuxPaneSplitDirection::Down,
                        crate::command::MuxSplitDirection::Down
                    )
                )
                && layout_matches_launch_plan(observed_first, &expected_split.first)
                && layout_matches_launch_plan(observed_second, &expected_split.second)
        }
        _ => false,
    }
}

fn collect_layout_pane_ids(layout: &MuxPaneLayout, pane_ids: &mut Vec<String>) {
    match layout {
        MuxPaneLayout::Pane(pane_id) => pane_ids.push(pane_id.clone()),
        MuxPaneLayout::Split { first, second, .. } => {
            collect_layout_pane_ids(first, pane_ids);
            collect_layout_pane_ids(second, pane_ids);
        }
    }
}

fn resolved_target_for_command(
    command: &MuxCommand,
    snapshot: &MuxSnapshot,
) -> Option<MuxEventTarget> {
    resolved_target_for_sessions(command, &snapshot.sessions)
}

fn resolved_target_for_sessions(
    command: &MuxCommand,
    sessions: &[MuxSession],
) -> Option<MuxEventTarget> {
    let session_id = match command {
        MuxCommand::CreateSession { plan } => &plan.session_id,
        _ => command_session_id(command),
    };
    let session = sessions
        .iter()
        .find(|session| session.id == session_id || session.name == session_id)?;

    // Targets stop at the resource this command mutates: a pane replacement cannot stale a
    // session- or window-scoped mutation.
    match command {
        MuxCommand::CreateSession { .. }
        | MuxCommand::CreateProjectSession { .. }
        | MuxCommand::CreateWorktreeSession { .. }
        | MuxCommand::RenameSession { .. }
        | MuxCommand::DitchSession { .. } => Some(MuxEventTarget::session(session.id.clone())),
        MuxCommand::ActivateWindow { window_id, .. }
        | MuxCommand::RenameWindow { window_id, .. }
        | MuxCommand::MoveWindow {
            window_id: Some(window_id),
            ..
        }
        | MuxCommand::MoveWindowPreservingSelection { window_id, .. }
        | MuxCommand::SelectPane {
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
        } => window_target_for_command(session, window_id),
        MuxCommand::ActivateWindowIndex { index, .. } => session
            .windows
            .iter()
            .find(|window| window.index == *index)
            .map(|window| window_target_for_window(session, window)),
        MuxCommand::NewWindow { .. } => active_window_for_command(session)
            .map(|window| window_target_for_window(session, window))
            .or_else(|| Some(MuxEventTarget::session(session.id.clone()))),
        MuxCommand::ActivateNextWindow { .. } => adjacent_window_for_command(session, 1)
            .map(|window| window_target_for_window(session, window)),
        MuxCommand::ActivatePreviousWindow { .. } => adjacent_window_for_command(session, -1)
            .map(|window| window_target_for_window(session, window)),
        MuxCommand::ActivateLastWindow { .. }
        | MuxCommand::MoveWindow {
            window_id: None, ..
        }
        | MuxCommand::SelectPane {
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
        } => active_window_for_command(session)
            .map(|window| window_target_for_window(session, window)),
        MuxCommand::SplitPane { pane_id, .. }
        | MuxCommand::KillPane { pane_id, .. }
        | MuxCommand::ClosePane { pane_id, .. }
        | MuxCommand::TogglePaneZoom { pane_id, .. }
        | MuxCommand::ResizePane { pane_id, .. } => {
            let (window, pane) = pane_for_command(session, pane_id.as_deref())?;
            Some(pane_event_target(&session.id, &window.id, pane))
        }
    }
}

fn window_target_for_command(session: &MuxSession, window_id: &str) -> Option<MuxEventTarget> {
    session
        .windows
        .iter()
        .find(|window| window.id == window_id)
        .map(|window| window_target_for_window(session, window))
}

fn window_target_for_window(
    session: &MuxSession,
    window: &crate::snapshot::MuxWindow,
) -> MuxEventTarget {
    let mut target = MuxEventTarget::session(session.id.clone());
    target.window_id = Some(window.id.clone());
    target
}

fn active_window_for_command(session: &MuxSession) -> Option<&crate::snapshot::MuxWindow> {
    session
        .active_window_id
        .as_deref()
        .and_then(|window_id| session.windows.iter().find(|window| window.id == window_id))
        .or_else(|| session.windows.first())
}

fn adjacent_window_for_command(
    session: &MuxSession,
    step: i32,
) -> Option<&crate::snapshot::MuxWindow> {
    let active_window = active_window_for_command(session)?;
    let active_index = session
        .windows
        .iter()
        .position(|window| window.id == active_window.id)?;
    let target_index =
        (active_index as i32 + step).rem_euclid(session.windows.len() as i32) as usize;
    session.windows.get(target_index)
}

fn pane_for_command<'a>(
    session: &'a MuxSession,
    requested_pane_id: Option<&str>,
) -> Option<(&'a crate::snapshot::MuxWindow, &'a MuxPaneAnchor)> {
    if let Some(pane_id) = requested_pane_id {
        let window = session.windows.iter().find(|window| {
            std::iter::once(&window.anchor)
                .chain(window.panes.iter())
                .any(|pane| pane.pane_id.as_deref() == Some(pane_id))
        })?;
        let pane = std::iter::once(&window.anchor)
            .chain(window.panes.iter())
            .find(|pane| pane.pane_id.as_deref() == Some(pane_id))?;
        return Some((window, pane));
    }

    let window = active_window_for_command(session)?;
    let pane = window
        .anchor
        .pane_id
        .as_ref()
        .map(|_| &window.anchor)
        .or_else(|| window.panes.first())?;
    Some((window, pane))
}

/// Resolves focus-relative selectors before a command crosses into the worker.
fn freeze_implicit_command_target(
    command: &mut MuxCommand,
    sessions: &[MuxSession],
) -> Result<(), MuxCommandError> {
    let needs_target = matches!(
        &*command,
        MuxCommand::MoveWindow {
            window_id: None,
            ..
        } | MuxCommand::SelectPane {
            window_id: None,
            ..
        } | MuxCommand::SelectNextPane {
            window_id: None,
            ..
        } | MuxCommand::SelectPreviousPane {
            window_id: None,
            ..
        } | MuxCommand::SelectLastPane {
            window_id: None,
            ..
        } | MuxCommand::SplitPane { pane_id: None, .. }
            | MuxCommand::KillPane { pane_id: None, .. }
            | MuxCommand::ClosePane { pane_id: None, .. }
            | MuxCommand::TogglePaneZoom { pane_id: None, .. }
            | MuxCommand::ResizePane { pane_id: None, .. }
    );
    if !needs_target {
        return Ok(());
    }

    let target = resolved_target_for_sessions(&*command, sessions).ok_or(MuxCommandError::Stale)?;
    match command {
        MuxCommand::MoveWindow { window_id, .. }
        | MuxCommand::SelectPane { window_id, .. }
        | MuxCommand::SelectNextPane { window_id, .. }
        | MuxCommand::SelectPreviousPane { window_id, .. }
        | MuxCommand::SelectLastPane { window_id, .. } => {
            *window_id = Some(target.window_id.ok_or(MuxCommandError::Stale)?);
        }
        MuxCommand::SplitPane { pane_id, .. }
        | MuxCommand::KillPane { pane_id, .. }
        | MuxCommand::ClosePane { pane_id, .. }
        | MuxCommand::TogglePaneZoom { pane_id, .. }
        | MuxCommand::ResizePane { pane_id, .. } => {
            *pane_id = Some(target.pane_id.ok_or(MuxCommandError::Stale)?);
        }
        _ => unreachable!("only implicit pane and window commands need freezing"),
    }
    Ok(())
}

fn command_requires_precondition(command: &MuxCommand) -> bool {
    !matches!(
        command,
        MuxCommand::CreateSession { .. }
            | MuxCommand::CreateProjectSession { .. }
            | MuxCommand::CreateWorktreeSession { .. }
    )
}

fn capture_execution_precondition(
    scope: Option<MuxScope>,
    sessions: &[MuxSession],
    command: &MuxCommand,
) -> Result<Option<MuxScopedExecutionPrecondition>, MuxCommandError> {
    let Some(scope) = scope else {
        return Ok(None);
    };
    if !command_requires_precondition(command) {
        return Ok(None);
    }
    let target = resolved_target_for_sessions(command, sessions).ok_or(MuxCommandError::Stale)?;
    let occupant_fingerprint = target
        .occupant
        .as_ref()
        .map(|occupant| occupant.backend_identity.clone());
    Ok(Some(MuxScopedExecutionPrecondition {
        scope,
        target,
        occupant_fingerprint,
        binding_generation: None,
        occupant_generation: None,
    }))
}

fn pane_event_target(session_id: &str, window_id: &str, pane: &MuxPaneAnchor) -> MuxEventTarget {
    let pane_id = pane
        .pane_id
        .as_deref()
        .expect("pane event target requires a pane id");
    let occupant = pane
        .occupant_id
        .clone()
        .or_else(|| snapshot_occupant_fingerprint(pane))
        .map(|backend_identity| MuxOccupantIdentity {
            backend_identity,
            pid: pane.pane_pid,
            process: pane.process.clone(),
        });
    MuxEventTarget {
        session_id: Some(session_id.to_owned()),
        window_id: Some(window_id.to_owned()),
        pane_id: Some(pane_id.to_owned()),
        terminal_id: pane.terminal_id.clone(),
        occupant,
    }
}
#[derive(Default)]
struct CommandConfigState {
    config: Option<MultiplexerConfig>,
    generation: u64,
}

struct MuxCommandJob {
    scope: Option<MuxScope>,
    config: MultiplexerConfig,
    command: MuxCommand,
    completion: MuxCommandCompletion,
    response: Option<mpsc::Sender<MuxCommandResult>>,
    deadline: Option<Instant>,
    cancellation: Option<CommandCancellation>,
    precondition_failure: Option<MuxCommandError>,
    resource_generation_guard: Option<MuxResourceGenerationGuard>,
    binding_generation_guard: Option<MuxBindingGenerationGuard>,
    config_generation: u64,
}

fn matches_target_resource(resolved: Option<&MuxEventTarget>, expected: &MuxEventTarget) -> bool {
    resolved.is_some_and(|target| {
        target.session_id == expected.session_id
            && target.window_id == expected.window_id
            && target.pane_id == expected.pane_id
            && target.terminal_id == expected.terminal_id
    })
}

fn recheck_execution_precondition(
    backend: &mut dyn MuxBackend,
    command: &MuxCommand,
    precondition: Option<&MuxScopedExecutionPrecondition>,
    resource_generation_guard: Option<&MuxResourceGenerationGuard>,
    binding_generation_guard: Option<&MuxBindingGenerationGuard>,
) -> Result<(), MuxCommandError> {
    if binding_generation_guard.is_some_and(|guard| !guard.is_current())
        || resource_generation_guard.is_some_and(|guard| !guard.is_current())
    {
        return Err(MuxCommandError::Stale);
    }
    let Some(precondition) = precondition else {
        return Ok(());
    };
    let snapshot = backend.snapshot().map_err(command_error_from_backend)?;
    if !matches_target_resource(
        resolved_target_for_command(command, &snapshot).as_ref(),
        &precondition.target,
    ) {
        return Err(MuxCommandError::Stale);
    }
    if !backend
        .validate_execution_precondition(precondition, &snapshot)
        .map_err(command_error_from_backend)?
    {
        return Err(MuxCommandError::Stale);
    }
    if binding_generation_guard.is_some_and(|guard| !guard.is_current())
        || resource_generation_guard.is_some_and(|guard| !guard.is_current())
    {
        return Err(MuxCommandError::Stale);
    }
    Ok(())
}

fn execute_backend_command(
    backend: &mut dyn MuxBackend,
    scope: Option<MuxScope>,
    command: MuxCommand,
    precondition: Option<&MuxScopedExecutionPrecondition>,
) -> Result<(), MuxCommandError> {
    execute_backend_command_with_generation_guards(
        backend,
        scope,
        command,
        precondition,
        None,
        None,
    )
}

fn execute_backend_command_with_generation_guards(
    backend: &mut dyn MuxBackend,
    scope: Option<MuxScope>,
    command: MuxCommand,
    precondition: Option<&MuxScopedExecutionPrecondition>,
    resource_generation_guard: Option<&MuxResourceGenerationGuard>,
    binding_generation_guard: Option<&MuxBindingGenerationGuard>,
) -> Result<(), MuxCommandError> {
    if let MuxCommand::CreateSession { plan } = &command {
        plan.validate()
            .map_err(|error| MuxCommandError::Failed(error.to_string()))?;
        let outcome = match scope {
            Some(scope) => {
                let descriptor = backend.capabilities(scope);
                if !descriptor.supports(BindingOperation::CreateProjectSession) {
                    return Err(MuxCommandError::Unsupported);
                }
                recheck_execution_precondition(
                    backend,
                    &command,
                    precondition,
                    resource_generation_guard,
                    binding_generation_guard,
                )?;
                let MuxCommand::CreateSession { plan } = command else {
                    unreachable!("checked the command variant above");
                };
                match descriptor.invoke(
                    descriptor.request(BindingOperation::CreateProjectSession),
                    BindingOperationAvailability::Available,
                    || backend.execute_session_launch(plan),
                ) {
                    BindingOperationOutcome::Supported(outcome) => outcome,
                    BindingOperationOutcome::Unsupported => BindingOperationOutcome::Unsupported,
                    BindingOperationOutcome::Unavailable => BindingOperationOutcome::Unavailable,
                    BindingOperationOutcome::Denied => BindingOperationOutcome::Denied,
                    BindingOperationOutcome::Stale => BindingOperationOutcome::Stale,
                }
            }
            None => {
                recheck_execution_precondition(
                    backend,
                    &command,
                    precondition,
                    resource_generation_guard,
                    binding_generation_guard,
                )?;
                let MuxCommand::CreateSession { plan } = command else {
                    unreachable!("checked the command variant above");
                };
                backend.execute_session_launch(plan)
            }
        };
        return execution_outcome(outcome);
    }

    let Some(scope) = scope else {
        recheck_execution_precondition(
            backend,
            &command,
            precondition,
            resource_generation_guard,
            binding_generation_guard,
        )?;
        return backend.execute(command).map_err(command_error_from_backend);
    };
    let descriptor = backend.capabilities(scope);
    if !descriptor.supports(command.operation()) {
        return Err(MuxCommandError::Unsupported);
    }
    recheck_execution_precondition(
        backend,
        &command,
        precondition,
        resource_generation_guard,
        binding_generation_guard,
    )?;
    execution_outcome(backend.execute_checked(scope, command, precondition))
}

fn command_error_from_backend(error: anyhow::Error) -> MuxCommandError {
    match error.downcast_ref::<MuxBackendOperationError>() {
        Some(MuxBackendOperationError::Unsupported(_)) => MuxCommandError::Unsupported,
        Some(MuxBackendOperationError::Unavailable(_)) => MuxCommandError::Unavailable,
        Some(MuxBackendOperationError::Denied(_)) => MuxCommandError::Denied,
        Some(MuxBackendOperationError::Stale(_)) => MuxCommandError::Stale,
        Some(MuxBackendOperationError::Failed(message)) => MuxCommandError::Failed(message.clone()),
        None => match error.chain().find_map(|cause| {
            cause
                .downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind)
        }) {
            Some(std::io::ErrorKind::NotFound) => MuxCommandError::Unavailable,
            Some(std::io::ErrorKind::PermissionDenied) => MuxCommandError::Denied,
            _ => MuxCommandError::Failed(error.to_string()),
        },
    }
}

fn execution_outcome(
    outcome: BindingOperationOutcome<anyhow::Result<()>>,
) -> Result<(), MuxCommandError> {
    match outcome {
        BindingOperationOutcome::Supported(result) => result.map_err(command_error_from_backend),
        BindingOperationOutcome::Unsupported => Err(MuxCommandError::Unsupported),
        BindingOperationOutcome::Unavailable => Err(MuxCommandError::Unavailable),
        BindingOperationOutcome::Denied => Err(MuxCommandError::Denied),
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
        MuxCommand::CreateSession { plan } => &plan.session_id,
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
    Terminal(String, String, String),
}

impl MuxResourceKey {
    fn is_current_in(
        &self,
        observed: &BTreeMap<MuxResourceKey, String>,
        authoritative_allocations: &BTreeSet<MuxResourceKey>,
    ) -> bool {
        observed.contains_key(self) || authoritative_allocations.contains(self)
    }

    fn generation_in(
        &self,
        generations: &BTreeMap<MuxResourceKey, u64>,
        observed: &BTreeMap<MuxResourceKey, String>,
        authoritative_allocations: &BTreeSet<MuxResourceKey>,
    ) -> Option<u64> {
        self.is_current_in(observed, authoritative_allocations)
            .then(|| generations.get(self).copied())
            .flatten()
    }

    fn matches_target(&self, target: &MuxEventTarget) -> bool {
        let Some(session_id) = target.session_id.as_deref() else {
            return false;
        };
        match self {
            Self::Session(current_session_id) => {
                current_session_id == session_id
                    && target.window_id.is_none()
                    && target.pane_id.is_none()
                    && target.terminal_id.is_none()
            }
            Self::Window(current_session_id, current_window_id) => {
                current_session_id == session_id
                    && target.window_id.as_deref() == Some(current_window_id.as_str())
                    && target.pane_id.is_none()
                    && target.terminal_id.is_none()
            }
            Self::Pane(current_session_id, current_window_id, current_pane_id) => {
                current_session_id == session_id
                    && target.window_id.as_deref() == Some(current_window_id.as_str())
                    && target.pane_id.as_deref() == Some(current_pane_id.as_str())
                    && target.terminal_id.is_none()
            }
            Self::Terminal(current_session_id, current_window_id, current_terminal_id) => {
                current_session_id == session_id
                    && target.window_id.as_deref() == Some(current_window_id.as_str())
                    && target.terminal_id.as_deref() == Some(current_terminal_id.as_str())
            }
        }
    }
}

#[derive(Clone)]
struct MuxResourceGenerationGuard {
    key: MuxResourceKey,
    generation: u64,
    generations: Arc<Mutex<BTreeMap<MuxResourceKey, u64>>>,
}

impl MuxResourceGenerationGuard {
    fn is_current(&self) -> bool {
        self.generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&self.key)
            .copied()
            == Some(self.generation)
    }
}

/// Queue-time binding epoch, retained even for mutations without an occupant target.
#[derive(Clone)]
struct MuxBindingGenerationGuard {
    generation: u64,
    current: Arc<AtomicU64>,
}

impl MuxBindingGenerationGuard {
    fn is_current(&self) -> bool {
        self.current.load(Ordering::Acquire) == self.generation
    }
}
/// A backend event paired with the binding and resource generations that were
/// authoritative at its exact position in the observed stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MuxEventObservation {
    pub event: MuxEvent,
    /// The binding generation after any preceding reconnect rebase.
    pub binding_generation: u64,
    /// The event target's resource generation after this event's transition.
    pub target_generation: Option<u64>,
    /// The replaced or closed target's resource generation before its
    /// transition, when the event retires a target resource.
    pub retired_target_generation: Option<u64>,
}
const MAX_DEFERRED_UNKNOWN_TARGET_EVENTS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct DeferredMuxEventKey {
    binding_generation: u64,
    backend_identity: String,
    session_id: Option<String>,
    window_id: Option<String>,
    pane_id: Option<String>,
    terminal_id: Option<String>,
    occupant_generation: Option<String>,
}

#[derive(Clone, Debug)]
struct DeferredMuxEvent {
    key: DeferredMuxEventKey,
    event: MuxEvent,
}
fn is_authoritative_close_event(event: &MuxEvent) -> bool {
    event.topic == crate::backend::MuxEventTopic::PaneClosed
        && matches!(&event.payload, MuxEventPayload::Closed { .. })
}

pub struct BindingMuxController {
    controller: MuxController,
    last_error: Option<String>,
    availability_error: Option<String>,
    refresh_completed: bool,
    refresh_failed: bool,
    binding_generation: Arc<AtomicU64>,
    resource_generations: BTreeMap<MuxResourceKey, u64>,
    /// Retired generations remain addressable for delayed authoritative close/replacement events.
    tombstoned_resource_generations: BTreeMap<MuxResourceKey, u64>,
    next_resource_generation: u64,
    observed_resources: BTreeMap<MuxResourceKey, String>,
    authoritative_occupants: BTreeMap<MuxResourceKey, String>,
    /// Exact resources committed by a backend create completion. A flat tmux
    /// snapshot reports only its attach anchor, so it cannot retire siblings.
    authoritative_allocations: BTreeSet<MuxResourceKey>,
    /// Pane close evidence retained until an authoritative snapshot rebase. Event batches can
    /// split one session/window teardown across multiple drains.
    authoritative_closed_targets: BTreeMap<MuxResourceKey, MuxEventTarget>,
    execution_resource_generations: Arc<Mutex<BTreeMap<MuxResourceKey, u64>>>,
    observed_backend: Option<MultiplexerBackendConfig>,
    /// Cache key is the complete binding configuration, not merely backend kind: remote server
    /// identity and transport changes must never retain another server's event stream.
    event_backend: Option<(MultiplexerConfig, Box<dyn MuxBackend>)>,
    deferred_unknown_events: VecDeque<DeferredMuxEvent>,
    deferred_refresh_completed: bool,
}

impl Default for BindingMuxController {
    fn default() -> Self {
        Self::new_unscoped()
    }
}

impl BindingMuxController {
    pub fn new(scope: MuxScope) -> Self {
        let execution_resource_generations = Arc::new(Mutex::new(BTreeMap::new()));
        let binding_generation = Arc::new(AtomicU64::new(next_binding_generation()));
        let mut controller = MuxController::with_scope(scope);
        controller.set_execution_resource_generations(Arc::clone(&execution_resource_generations));
        controller.set_execution_binding_generation(Arc::clone(&binding_generation));
        Self {
            controller,
            last_error: None,
            availability_error: None,
            refresh_completed: false,
            refresh_failed: false,
            binding_generation,
            resource_generations: BTreeMap::new(),
            tombstoned_resource_generations: BTreeMap::new(),
            next_resource_generation: 1,
            observed_resources: BTreeMap::new(),
            authoritative_occupants: BTreeMap::new(),
            authoritative_allocations: BTreeSet::new(),
            authoritative_closed_targets: BTreeMap::new(),
            execution_resource_generations,
            observed_backend: None,
            event_backend: None,
            deferred_unknown_events: VecDeque::new(),
            deferred_refresh_completed: false,
        }
    }

    fn new_unscoped() -> Self {
        let execution_resource_generations = Arc::new(Mutex::new(BTreeMap::new()));
        let binding_generation = Arc::new(AtomicU64::new(next_binding_generation()));
        let mut controller = MuxController::new();
        controller.set_execution_resource_generations(Arc::clone(&execution_resource_generations));
        controller.set_execution_binding_generation(Arc::clone(&binding_generation));
        Self {
            controller,
            last_error: None,
            availability_error: None,
            refresh_completed: false,
            refresh_failed: false,
            binding_generation,
            resource_generations: BTreeMap::new(),
            tombstoned_resource_generations: BTreeMap::new(),
            next_resource_generation: 1,
            observed_resources: BTreeMap::new(),
            authoritative_occupants: BTreeMap::new(),
            authoritative_allocations: BTreeSet::new(),
            authoritative_closed_targets: BTreeMap::new(),
            execution_resource_generations,
            deferred_unknown_events: VecDeque::new(),
            deferred_refresh_completed: false,
            observed_backend: None,
            event_backend: None,
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

    /// Advertises the event streams of the backend currently selected for this binding.
    pub fn event_capabilities(&mut self, config: &MultiplexerConfig) -> Vec<MuxEventCapability> {
        self.ensure_event_backend(config)
            .map_or_else(Vec::new, |backend| backend.event_capabilities())
    }

    /// Drains already-buffered backend observations without doing backend I/O. A rebase requests
    /// an immediate normal snapshot refresh; output/state observations leave the refresh cadence
    /// untouched.
    pub fn drain_events(
        &mut self,
        config: &MultiplexerConfig,
        maximum: usize,
    ) -> Vec<MuxEventObservation> {
        let Some(scope) = self.controller.scope else {
            return Vec::new();
        };
        let events = self
            .ensure_event_backend(config)
            .map_or_else(Vec::new, |backend| backend.drain_events(scope, maximum));
        self.observe_backend_events(events)
    }
    fn release_event_scope(&mut self) {
        let Some(scope) = self.controller.scope else {
            return;
        };
        if let Some((_, backend)) = self.event_backend.as_mut() {
            backend.release_event_scope(scope);
        }
    }

    fn ensure_event_backend(
        &mut self,
        config: &MultiplexerConfig,
    ) -> Option<&mut (dyn MuxBackend + 'static)> {
        if self
            .event_backend
            .as_ref()
            .is_none_or(|(cached, _)| cached != config)
        {
            let replaced = self.event_backend.is_some();
            if replaced {
                self.release_event_scope();
            }
            let mut backend = self.controller.build_backend(config);
            backend.start_event_stream();
            self.event_backend = Some((config.clone(), backend));
            if replaced {
                self.advance_binding_generation();
                self.observed_resources.clear();
                self.tombstoned_resource_generations.clear();
                self.authoritative_occupants.clear();
                self.authoritative_allocations.clear();
                self.authoritative_closed_targets.clear();
                self.deferred_unknown_events.clear();
                self.deferred_refresh_completed = false;
                self.synchronize_execution_resource_generations();
                self.controller.apply_snapshot(
                    selected_backend(config),
                    MuxSnapshot::default(),
                    None,
                    None,
                );
                self.controller.refresh_on_next_frame();
            }
        }
        self.event_backend
            .as_mut()
            .map(|(_, backend)| backend.as_mut())
    }

    fn observe_backend_events(&mut self, events: Vec<MuxEvent>) -> Vec<MuxEventObservation> {
        let mut refresh = false;
        let binding_generation = self.binding_generation();
        self.deferred_unknown_events
            .retain(|deferred| deferred.key.binding_generation == binding_generation);
        let mut pending_events =
            Vec::with_capacity(events.len() + self.deferred_unknown_events.len());
        if self.deferred_refresh_completed {
            self.deferred_refresh_completed = false;
            let deferred = std::mem::take(&mut self.deferred_unknown_events);
            pending_events.extend(deferred.into_iter().filter_map(|deferred| {
                (deferred.key.binding_generation == binding_generation
                    && self.event_belongs_to_binding(&deferred.event)
                    && !self.deferred_event_is_stale(&deferred))
                .then_some(deferred.event)
            }));
        }
        pending_events.extend(events);
        let mut observations = Vec::with_capacity(pending_events.len());
        for event in pending_events {
            if self.has_deferred_event_target(&event) {
                refresh |= self.defer_unknown_target_event(event, &mut observations);
                continue;
            }
            if !self.event_belongs_to_binding(&event) {
                refresh |= self.defer_unknown_target_event(event, &mut observations);
                continue;
            }
            if matches!(
                &event.payload,
                MuxEventPayload::Rebase {
                    reason: MuxRebaseReason::Reconnect
                }
            ) {
                self.advance_binding_generation();
                self.observed_resources.clear();
                self.tombstoned_resource_generations.clear();
                self.authoritative_occupants.clear();
                self.authoritative_allocations.clear();
                self.authoritative_closed_targets.clear();
                self.deferred_unknown_events.clear();
                self.deferred_refresh_completed = false;
                self.synchronize_execution_resource_generations();
            }
            if self.has_stale_non_replacement_pane_occupant(&event) {
                refresh = true;
                let event = MuxEvent {
                    topic: crate::backend::MuxEventTopic::SnapshotRebased,
                    target: None,
                    payload: MuxEventPayload::Rebase {
                        reason: MuxRebaseReason::SequenceGap,
                    },
                    ..event
                };
                observations.push(MuxEventObservation {
                    binding_generation: self.binding_generation(),
                    target_generation: None,
                    retired_target_generation: None,
                    event,
                });
                continue;
            }
            let retired_target_generation = (event.topic
                == crate::backend::MuxEventTopic::PaneOccupantReplaced
                || is_authoritative_close_event(&event))
            .then(|| self.retired_event_target_generation(&event))
            .flatten();
            if is_authoritative_close_event(&event)
                && let Some(target) = event.target.as_ref()
            {
                self.remember_closed_target(target);
                self.retire_event_target(target);
                self.retire_closed_parent_generations();
            }
            if event.topic != crate::backend::MuxEventTopic::PaneClosed
                && let Some(target) = &event.target
            {
                self.record_authoritative_occupant(target);
            }
            refresh |= event.topic == crate::backend::MuxEventTopic::TopologyChanged
                || matches!(&event.payload, MuxEventPayload::Rebase { .. });
            observations.push(MuxEventObservation {
                binding_generation: self.binding_generation(),
                target_generation: self.event_target_generation(&event),
                retired_target_generation,
                event,
            });
        }
        if refresh {
            self.controller.refresh_on_next_frame();
        }
        observations
    }

    fn deferred_event_key(&self, event: &MuxEvent) -> Option<DeferredMuxEventKey> {
        let target = event.target.as_ref()?;
        Some(DeferredMuxEventKey {
            binding_generation: self.binding_generation(),
            backend_identity: event.backend_identity.clone(),
            session_id: target.session_id.clone(),
            window_id: target.window_id.clone(),
            pane_id: target.pane_id.clone(),
            terminal_id: target.terminal_id.clone(),
            occupant_generation: target
                .occupant
                .as_ref()
                .map(|occupant| occupant.backend_identity.clone())
                .or_else(|| event.cursor.as_ref().map(|cursor| cursor.stream.clone())),
        })
    }

    fn deferred_event_is_stale(&self, deferred: &DeferredMuxEvent) -> bool {
        let Some(target) = deferred.event.target.as_ref() else {
            return false;
        };
        let Some(expected_occupant) = target
            .occupant
            .as_ref()
            .map(|occupant| occupant.backend_identity.as_str())
        else {
            return false;
        };
        let (Some(session_id), Some(window_id), Some(resource_id)) = (
            target.session_id.as_deref(),
            target.window_id.as_deref(),
            target.terminal_id.as_deref().or(target.pane_id.as_deref()),
        ) else {
            return false;
        };
        let key = if target.terminal_id.is_some() {
            MuxResourceKey::Terminal(
                session_id.to_owned(),
                window_id.to_owned(),
                resource_id.to_owned(),
            )
        } else {
            MuxResourceKey::Pane(
                session_id.to_owned(),
                window_id.to_owned(),
                resource_id.to_owned(),
            )
        };
        self.authoritative_occupants
            .get(&key)
            .is_some_and(|current| current != expected_occupant)
    }

    fn has_deferred_event_target(&self, event: &MuxEvent) -> bool {
        let Some(key) = self.deferred_event_key(event) else {
            return false;
        };
        self.deferred_unknown_events.iter().any(|deferred| {
            deferred.key.binding_generation == key.binding_generation
                && deferred.key.backend_identity == key.backend_identity
                && deferred.key.session_id == key.session_id
                && deferred.key.window_id == key.window_id
                && deferred.key.pane_id == key.pane_id
                && deferred.key.terminal_id == key.terminal_id
        })
    }

    fn defer_unknown_target_event(
        &mut self,
        event: MuxEvent,
        observations: &mut Vec<MuxEventObservation>,
    ) -> bool {
        if self.controller.scope != Some(event.scope) {
            return false;
        }
        let Some(target) = event.target.as_ref() else {
            return false;
        };
        if target.session_id.is_none() {
            return false;
        }
        let Some(key) = self.deferred_event_key(&event) else {
            return false;
        };
        if self.deferred_unknown_events.len() >= MAX_DEFERRED_UNKNOWN_TARGET_EVENTS {
            self.deferred_unknown_events.clear();
            let overflow_event = MuxEvent {
                topic: crate::backend::MuxEventTopic::SnapshotRebased,
                cursor: None,
                target: None,
                payload: MuxEventPayload::Rebase {
                    reason: MuxRebaseReason::QueueOverflow,
                },
                ..event
            };
            observations.push(MuxEventObservation {
                binding_generation: self.binding_generation(),
                target_generation: None,
                retired_target_generation: None,
                event: overflow_event,
            });
            return true;
        }
        self.deferred_unknown_events
            .push_back(DeferredMuxEvent { key, event });
        true
    }

    fn event_belongs_to_binding(&self, event: &MuxEvent) -> bool {
        if self.controller.scope != Some(event.scope) {
            return false;
        }
        if event.topic == crate::backend::MuxEventTopic::TopologyChanged {
            return true;
        }
        let Some(target) = event.target.as_ref() else {
            return true;
        };
        let Some(session_id) = target.session_id.as_deref() else {
            return false;
        };
        if self
            .controller
            .sessions()
            .iter()
            .any(|session| session.id == session_id)
        {
            return true;
        }
        matches!(
            event.topic,
            crate::backend::MuxEventTopic::PaneOccupantReplaced
                | crate::backend::MuxEventTopic::PaneClosed
        ) && self.target_is_known_or_tombstoned(target)
    }

    fn event_target_generation(&self, event: &MuxEvent) -> Option<u64> {
        let target = event.target.as_ref()?;
        let session_id = target.session_id.as_deref()?;
        match (
            target.window_id.as_deref(),
            target.pane_id.as_deref(),
            target.terminal_id.as_deref(),
        ) {
            (Some(window_id), Some(_), Some(terminal_id)) => {
                self.terminal_generation(session_id, window_id, terminal_id)
            }
            (Some(window_id), Some(pane_id), None) => {
                self.pane_generation(session_id, window_id, pane_id)
            }
            (Some(window_id), None, _) => self.window_generation(session_id, window_id),
            (None, None, None) => self.session_generation(session_id),
            _ => None,
        }
    }

    fn target_is_known_or_tombstoned(&self, target: &MuxEventTarget) -> bool {
        self.resource_generations
            .keys()
            .chain(self.tombstoned_resource_generations.keys())
            .any(|key| key.matches_target(target))
    }

    fn retired_event_target_generation(&self, event: &MuxEvent) -> Option<u64> {
        self.event_target_generation(event)
            .or_else(|| self.tombstoned_event_target_generation(event))
    }

    fn tombstoned_event_target_generation(&self, event: &MuxEvent) -> Option<u64> {
        let target = event.target.as_ref()?;
        let session_id = target.session_id.as_deref()?;
        let key = match (
            target.window_id.as_deref(),
            target.pane_id.as_deref(),
            target.terminal_id.as_deref(),
        ) {
            (Some(window_id), Some(_), Some(terminal_id)) => MuxResourceKey::Terminal(
                session_id.to_owned(),
                window_id.to_owned(),
                terminal_id.to_owned(),
            ),
            (Some(window_id), Some(pane_id), None) => MuxResourceKey::Pane(
                session_id.to_owned(),
                window_id.to_owned(),
                pane_id.to_owned(),
            ),
            (Some(window_id), None, _) => {
                MuxResourceKey::Window(session_id.to_owned(), window_id.to_owned())
            }
            (None, None, None) => MuxResourceKey::Session(session_id.to_owned()),
            _ => return None,
        };
        self.resource_generations
            .get(&key)
            .copied()
            .or_else(|| self.tombstoned_resource_generations.get(&key).copied())
    }

    fn has_stale_non_replacement_pane_occupant(&self, event: &MuxEvent) -> bool {
        if event.topic == crate::backend::MuxEventTopic::PaneOccupantReplaced {
            return false;
        }
        let Some(target) = event.target.as_ref() else {
            return false;
        };
        let (Some(session_id), Some(window_id), Some(pane_id), Some(occupant)) = (
            target.session_id.as_deref(),
            target.window_id.as_deref(),
            target.pane_id.as_deref(),
            target.occupant.as_ref(),
        ) else {
            return false;
        };
        if let Some(terminal_id) = target.terminal_id.as_deref() {
            return self
                .authoritative_occupants
                .iter()
                .any(|(key, current_occupant)| {
                    matches!(
                        key,
                        MuxResourceKey::Terminal(
                            current_session_id,
                            current_window_id,
                            current_terminal_id
                        ) if current_session_id == session_id
                            && current_window_id == window_id
                            && current_terminal_id == terminal_id
                    ) && current_occupant != &occupant.backend_identity
                });
        }
        self.authoritative_occupants
            .iter()
            .any(|(key, current_occupant)| {
                matches!(
                    key,
                    MuxResourceKey::Pane(current_session_id, current_window_id, current_pane_id)
                        if current_session_id == session_id
                            && current_window_id == window_id
                            && current_pane_id == pane_id
                ) && current_occupant != &occupant.backend_identity
            })
    }

    fn record_authoritative_occupant(&mut self, target: &MuxEventTarget) {
        let (Some(session_id), Some(window_id), Some(pane_id), Some(occupant)) = (
            target.session_id.as_deref(),
            target.window_id.as_deref(),
            target.pane_id.as_deref(),
            target.occupant.as_ref(),
        ) else {
            return;
        };
        let fingerprint = occupant.backend_identity.clone();
        let pane_key = MuxResourceKey::Pane(
            session_id.to_owned(),
            window_id.to_owned(),
            pane_id.to_owned(),
        );
        let mut updates = vec![(pane_key, fingerprint.clone())];
        if let Some(terminal_id) = target.terminal_id.as_deref() {
            updates.push((
                MuxResourceKey::Terminal(
                    session_id.to_owned(),
                    window_id.to_owned(),
                    terminal_id.to_owned(),
                ),
                fingerprint,
            ));
        }
        let mut changed_keys = Vec::new();
        for (key, fingerprint) in updates {
            if self.record_authoritative_occupant_for_key(key.clone(), fingerprint) {
                changed_keys.push(key);
            }
        }
        self.assign_resource_generation_epoch(changed_keys);
        self.synchronize_execution_resource_generations();
    }

    fn record_authoritative_occupant_for_key(
        &mut self,
        key: MuxResourceKey,
        fingerprint: String,
    ) -> bool {
        let previous_authoritative = self
            .authoritative_occupants
            .insert(key.clone(), fingerprint.clone());
        let was_current = self.resource_is_current(&key);
        let generation_changed = previous_authoritative
            .as_deref()
            .is_some_and(|previous| previous != fingerprint.as_str())
            || !was_current
            || !self.resource_generations.contains_key(&key);
        self.observed_resources.insert(key, fingerprint);
        generation_changed
    }
    fn remember_closed_target(&mut self, target: &MuxEventTarget) {
        let (Some(session_id), Some(window_id), Some(pane_id)) = (
            target.session_id.as_deref(),
            target.window_id.as_deref(),
            target.pane_id.as_deref(),
        ) else {
            return;
        };
        self.authoritative_closed_targets.insert(
            MuxResourceKey::Pane(
                session_id.to_owned(),
                window_id.to_owned(),
                pane_id.to_owned(),
            ),
            target.clone(),
        );
    }

    fn retire_event_target(&mut self, target: &MuxEventTarget) {
        let Some(session_id) = target.session_id.as_deref() else {
            return;
        };
        let keys = match (
            target.window_id.as_deref(),
            target.pane_id.as_deref(),
            target.terminal_id.as_deref(),
        ) {
            (Some(window_id), Some(pane_id), Some(terminal_id)) => vec![
                MuxResourceKey::Pane(
                    session_id.to_owned(),
                    window_id.to_owned(),
                    pane_id.to_owned(),
                ),
                MuxResourceKey::Terminal(
                    session_id.to_owned(),
                    window_id.to_owned(),
                    terminal_id.to_owned(),
                ),
            ],
            (Some(window_id), Some(pane_id), None) => vec![MuxResourceKey::Pane(
                session_id.to_owned(),
                window_id.to_owned(),
                pane_id.to_owned(),
            )],
            (Some(window_id), None, None) => vec![MuxResourceKey::Window(
                session_id.to_owned(),
                window_id.to_owned(),
            )],
            (None, None, None) => vec![MuxResourceKey::Session(session_id.to_owned())],
            _ => return,
        };
        for key in keys {
            self.retire_resource_key(key);
        }
        self.synchronize_execution_resource_generations();
    }

    fn tombstone_resource_key(&mut self, key: &MuxResourceKey) {
        if let Some(generation) = self.resource_generations.remove(key) {
            self.tombstoned_resource_generations
                .insert(key.clone(), generation);
        }
    }

    fn retire_resource_key(&mut self, key: MuxResourceKey) {
        self.observed_resources.remove(&key);
        self.authoritative_occupants.remove(&key);
        self.authoritative_allocations.remove(&key);
        self.tombstone_resource_key(&key);
    }

    fn retire_closed_parent_generations(&mut self) {
        let closed_targets = self
            .authoritative_closed_targets
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let closed_panes = closed_targets
            .iter()
            .filter_map(|target| {
                Some((
                    target.session_id.as_deref()?.to_owned(),
                    target.window_id.as_deref()?.to_owned(),
                    target.pane_id.as_deref()?.to_owned(),
                ))
            })
            .collect::<BTreeSet<_>>();
        if closed_panes.is_empty() {
            return;
        }

        let mut closed_windows = BTreeSet::new();
        let mut closed_sessions = BTreeSet::new();
        for session in self.controller.sessions() {
            let mut session_panes = Vec::new();
            for window in &session.windows {
                let pane_ids = std::iter::once(&window.anchor)
                    .chain(&window.panes)
                    .filter_map(|pane| pane.pane_id.as_deref())
                    .collect::<Vec<_>>();
                if pane_ids.is_empty() {
                    continue;
                }
                session_panes.extend(
                    pane_ids
                        .iter()
                        .map(|pane_id| (window.id.as_str(), *pane_id)),
                );
                if pane_ids.iter().all(|pane_id| {
                    closed_panes.contains(&(
                        session.id.clone(),
                        window.id.clone(),
                        (*pane_id).to_owned(),
                    ))
                }) {
                    closed_windows.insert((session.id.clone(), window.id.clone()));
                }
            }
            if !session_panes.is_empty()
                && session_panes.iter().all(|(window_id, pane_id)| {
                    closed_panes.contains(&(
                        session.id.clone(),
                        (*window_id).to_owned(),
                        (*pane_id).to_owned(),
                    ))
                })
            {
                closed_sessions.insert(session.id.clone());
            }
        }

        for (session_id, window_id) in closed_windows {
            let key = MuxResourceKey::Window(session_id, window_id);
            if self.resource_is_current(&key) {
                self.retire_resource_key(key);
            }
        }
        for session_id in closed_sessions {
            let key = MuxResourceKey::Session(session_id);
            if self.resource_is_current(&key) {
                self.retire_resource_key(key);
            }
        }
        self.synchronize_execution_resource_generations();
    }

    fn advance_binding_generation(&self) {
        let _ = self.binding_generation.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |generation| Some(generation.saturating_add(1)),
        );
    }

    pub fn binding_generation(&self) -> u64 {
        self.binding_generation.load(Ordering::Acquire)
    }

    pub fn operation_outcome(
        &self,
        config: &MultiplexerConfig,
        operation: BindingOperation,
    ) -> BindingOperationOutcome<()> {
        let Some(scope) = self.controller.scope else {
            return BindingOperationOutcome::Supported(());
        };
        if self.availability_error.is_some() {
            return BindingOperationOutcome::Unavailable;
        }
        if matches!(selected_backend(config), MultiplexerBackendConfig::Rmux)
            && rmux_operation_requires_checked_boundary(operation)
        {
            // rmux has no server-side CAS for a queued target mutation. Keep the command
            // registry's dynamic outcome aligned with the backend's fail-closed checked seam.
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

    /// Validates immutable recursive launch intent against the backend without queuing or
    /// mutating. The worker repeats this capability check immediately before mutation.
    pub fn preflight_session_launch(
        &self,
        config: &MultiplexerConfig,
        plan: &MuxSessionLaunchPlan,
    ) -> Result<BindingOperationOutcome<()>, MuxSessionLaunchPlanError> {
        plan.validate()?;
        if self.availability_error.is_some() {
            return Ok(BindingOperationOutcome::Unavailable);
        }

        let backend = self.controller.build_backend(config);
        let capability = backend.session_launch_capability(plan);
        let Some(scope) = self.controller.scope else {
            return Ok(capability);
        };
        let descriptor = backend.capabilities(scope);
        Ok(
            match descriptor.invoke(
                descriptor.request(BindingOperation::CreateProjectSession),
                BindingOperationAvailability::Available,
                || capability,
            ) {
                BindingOperationOutcome::Supported(outcome) => outcome,
                BindingOperationOutcome::Unsupported => BindingOperationOutcome::Unsupported,
                BindingOperationOutcome::Unavailable => BindingOperationOutcome::Unavailable,
                BindingOperationOutcome::Denied => BindingOperationOutcome::Denied,
                BindingOperationOutcome::Stale => BindingOperationOutcome::Stale,
            },
        )
    }

    fn resource_is_current(&self, key: &MuxResourceKey) -> bool {
        key.is_current_in(&self.observed_resources, &self.authoritative_allocations)
    }

    pub fn session_generation(&self, session_id: &str) -> Option<u64> {
        MuxResourceKey::Session(session_id.to_owned()).generation_in(
            &self.resource_generations,
            &self.observed_resources,
            &self.authoritative_allocations,
        )
    }

    pub fn window_generation(&self, session_id: &str, window_id: &str) -> Option<u64> {
        MuxResourceKey::Window(session_id.to_owned(), window_id.to_owned()).generation_in(
            &self.resource_generations,
            &self.observed_resources,
            &self.authoritative_allocations,
        )
    }

    pub fn pane_generation(&self, session_id: &str, window_id: &str, pane_id: &str) -> Option<u64> {
        MuxResourceKey::Pane(
            session_id.to_owned(),
            window_id.to_owned(),
            pane_id.to_owned(),
        )
        .generation_in(
            &self.resource_generations,
            &self.observed_resources,
            &self.authoritative_allocations,
        )
    }

    pub fn terminal_generation(
        &self,
        session_id: &str,
        window_id: &str,
        terminal_id: &str,
    ) -> Option<u64> {
        MuxResourceKey::Terminal(
            session_id.to_owned(),
            window_id.to_owned(),
            terminal_id.to_owned(),
        )
        .generation_in(
            &self.resource_generations,
            &self.observed_resources,
            &self.authoritative_allocations,
        )
    }
    /// Returns the backend terminal identity hosted by an exact pane.
    pub fn terminal_id_for_pane(
        &self,
        session_id: &str,
        window_id: &str,
        pane_id: &str,
    ) -> Option<&str> {
        self.controller
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| session.windows.iter().find(|window| window.id == window_id))
            .and_then(|window| {
                std::iter::once(&window.anchor)
                    .chain(&window.panes)
                    .find(|pane| pane.pane_id.as_deref() == Some(pane_id))
            })
            .and_then(|pane| pane.terminal_id.as_deref())
    }

    fn synchronize_execution_resource_generations(&self) {
        let mut generations = self
            .execution_resource_generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        generations.clear();
        generations.extend(
            self.resource_generations
                .iter()
                .filter(|(key, _)| self.resource_is_current(key))
                .map(|(key, generation)| (key.clone(), *generation)),
        );
    }
    fn allocate_resource_generation(&mut self) -> u64 {
        let generation = self.next_resource_generation;
        self.next_resource_generation = self.next_resource_generation.saturating_add(1);
        generation
    }

    fn assign_resource_generation_epoch(&mut self, keys: impl IntoIterator<Item = MuxResourceKey>) {
        let keys = keys.into_iter().collect::<BTreeSet<_>>();
        if keys.is_empty() {
            return;
        }
        let generation = self.allocate_resource_generation();
        for key in keys {
            self.resource_generations.insert(key.clone(), generation);
            self.tombstoned_resource_generations.remove(&key);
        }
    }

    fn record_authoritative_allocation(&mut self, allocated: &MuxAllocatedResources) {
        let mut keys = vec![MuxResourceKey::Session(allocated.session_id.clone())];
        for window in &allocated.windows {
            keys.push(MuxResourceKey::Window(
                allocated.session_id.clone(),
                window.window_id.clone(),
            ));
            for pane_id in &window.pane_ids {
                keys.push(MuxResourceKey::Pane(
                    allocated.session_id.clone(),
                    window.window_id.clone(),
                    pane_id.clone(),
                ));
                // A session allocation proves the pane exists, but does not invent a
                // terminal identity. Snapshot or completion targets supply that distinct ID.
            }
        }
        let new_keys = keys
            .iter()
            .filter(|key| {
                !self.resource_is_current(key) || !self.resource_generations.contains_key(*key)
            })
            .cloned()
            .collect::<BTreeSet<_>>();
        if !new_keys.is_empty() {
            let session_key = MuxResourceKey::Session(allocated.session_id.clone());
            let generation = self
                .resource_generations
                .get(&session_key)
                .copied()
                .or_else(|| {
                    keys.iter()
                        .find_map(|key| self.resource_generations.get(key).copied())
                })
                .unwrap_or_else(|| self.allocate_resource_generation());
            for key in new_keys {
                self.resource_generations.insert(key.clone(), generation);
                self.tombstoned_resource_generations.remove(&key);
            }
        }
        for key in keys {
            self.authoritative_allocations.insert(key);
        }
    }

    fn record_authoritative_target(&mut self, target: &MuxEventTarget) {
        let Some(session_id) = target.session_id.as_deref() else {
            return;
        };
        let keys = match (
            target.window_id.as_deref(),
            target.pane_id.as_deref(),
            target.terminal_id.as_deref(),
        ) {
            (Some(window_id), Some(pane_id), Some(terminal_id)) => vec![
                MuxResourceKey::Pane(
                    session_id.to_owned(),
                    window_id.to_owned(),
                    pane_id.to_owned(),
                ),
                MuxResourceKey::Terminal(
                    session_id.to_owned(),
                    window_id.to_owned(),
                    terminal_id.to_owned(),
                ),
            ],
            (Some(window_id), Some(pane_id), None) => vec![MuxResourceKey::Pane(
                session_id.to_owned(),
                window_id.to_owned(),
                pane_id.to_owned(),
            )],
            (Some(window_id), None, None) => vec![MuxResourceKey::Window(
                session_id.to_owned(),
                window_id.to_owned(),
            )],
            (None, None, None) => vec![MuxResourceKey::Session(session_id.to_owned())],
            _ => return,
        };
        let new_keys = keys
            .iter()
            .filter(|key| {
                !self.resource_is_current(key) || !self.resource_generations.contains_key(*key)
            })
            .cloned()
            .collect::<BTreeSet<_>>();
        self.assign_resource_generation_epoch(new_keys);
        for key in keys {
            self.authoritative_allocations.insert(key);
        }
        if target.occupant.is_some() {
            self.record_authoritative_occupant(target);
        }
    }

    fn record_resource_snapshot(&mut self) {
        let mut current = BTreeMap::new();
        let mut flat_windows: BTreeSet<(&str, &str)> = BTreeSet::new();
        for session in self.controller.sessions() {
            current.insert(MuxResourceKey::Session(session.id.clone()), String::new());
            for window in &session.windows {
                current.insert(
                    MuxResourceKey::Window(session.id.clone(), window.id.clone()),
                    String::new(),
                );
                if window.layout.is_none() {
                    flat_windows.insert((session.id.as_str(), window.id.as_str()));
                }
                for pane in std::iter::once(&window.anchor).chain(&window.panes) {
                    let Some(pane_id) = &pane.pane_id else {
                        continue;
                    };
                    let snapshot_occupant = pane
                        .occupant_id
                        .clone()
                        .unwrap_or_else(|| format!("{:?}:{:?}", pane.pane_pid, pane.process));
                    let pane_key = MuxResourceKey::Pane(
                        session.id.clone(),
                        window.id.clone(),
                        pane_id.clone(),
                    );
                    let pane_occupant = self
                        .authoritative_occupants
                        .get(&pane_key)
                        .cloned()
                        .unwrap_or_else(|| snapshot_occupant.clone());
                    current.insert(pane_key, pane_occupant);

                    let Some(terminal_id) = &pane.terminal_id else {
                        continue;
                    };
                    let terminal_key = MuxResourceKey::Terminal(
                        session.id.clone(),
                        window.id.clone(),
                        terminal_id.clone(),
                    );
                    let terminal_occupant = self
                        .authoritative_occupants
                        .get(&terminal_key)
                        .cloned()
                        .unwrap_or(snapshot_occupant);
                    current.insert(terminal_key, terminal_occupant);
                }
            }
        }
        self.authoritative_allocations.retain(|key| match key {
            MuxResourceKey::Session(_) | MuxResourceKey::Window(_, _) => current.contains_key(key),
            MuxResourceKey::Pane(session_id, window_id, _)
            | MuxResourceKey::Terminal(session_id, window_id, _) => {
                current.contains_key(key)
                    || flat_windows.contains(&(session_id.as_str(), window_id.as_str()))
            }
        });
        let retired_keys = {
            let authoritative_allocations = &self.authoritative_allocations;
            self.resource_generations
                .keys()
                .filter(|key| {
                    !current.contains_key(*key) && !authoritative_allocations.contains(*key)
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        for key in retired_keys {
            self.tombstone_resource_key(&key);
        }
        self.tombstoned_resource_generations
            .retain(|key, _| !current.contains_key(key));
        let authoritative_allocations = &self.authoritative_allocations;
        self.resource_generations
            .retain(|key, _| current.contains_key(key) || authoritative_allocations.contains(key));
        let mut changed_keys = Vec::new();
        for (key, fingerprint) in &current {
            let reappeared = !self.observed_resources.contains_key(key);
            let occupant_changed = self
                .observed_resources
                .get(key)
                .is_some_and(|previous| previous != fingerprint);
            let issued_authoritatively = self.authoritative_allocations.contains(key);
            if (reappeared && !issued_authoritatively)
                || occupant_changed
                || !self.resource_generations.contains_key(key)
            {
                changed_keys.push(key.clone());
            }
        }
        self.assign_resource_generation_epoch(changed_keys);
        self.authoritative_occupants
            .retain(|key, _| current.contains_key(key));
        self.observed_resources = current;
        self.synchronize_execution_resource_generations();
    }

    pub fn synchronize_resource_generations(&mut self) {
        self.record_resource_snapshot();
        self.authoritative_closed_targets.clear();
    }

    /// Queue or synchronously execute one immutable recursive session launch.
    pub fn create_session(
        &mut self,
        plan: MuxSessionLaunchPlan,
        repaint: &RepaintHandle,
        config: &MultiplexerConfig,
    ) {
        self.controller.create_session(plan, repaint, config);
        self.record_resource_snapshot();
    }

    pub fn create_project_session(
        &mut self,
        request: NewMuxSessionRequest,
        repaint: &RepaintHandle,
        config: &MultiplexerConfig,
    ) {
        self.controller
            .create_project_session(request, repaint, config);
        self.record_resource_snapshot();
    }

    pub fn execute_command(
        &mut self,
        repaint: &RepaintHandle,
        config: &MultiplexerConfig,
        command: MuxCommand,
    ) {
        self.controller.execute_command(repaint, config, command);
        self.record_resource_snapshot();
    }
    /// Compensates a backend-success/local-persistence-failure session create synchronously.
    ///
    /// This is an exceptional recovery path: the authoritative allocation must be removed and
    /// re-snapshotted before the failed create can be reported to its caller.
    pub fn compensate_created_session(
        &mut self,
        session_id: &str,
        config: &MultiplexerConfig,
    ) -> Result<(), MuxCommandError> {
        let backend_kind = selected_backend(config);
        let mut backend = self.controller.build_backend(config);
        let command = MuxCommand::DitchSession {
            session_id: session_id.to_owned(),
        };
        match self.controller.scope {
            Some(scope) => execution_outcome(backend.execute_checked(scope, command, None))?,
            None => backend
                .execute(command)
                .map_err(command_error_from_backend)?,
        }
        let snapshot = backend.snapshot().map_err(command_error_from_backend)?;
        self.controller
            .apply_refreshed_snapshot(backend_kind, snapshot);
        self.record_resource_snapshot();
        self.authoritative_closed_targets.clear();
        Ok(())
    }

    pub fn refresh_sessions(
        &mut self,
        repaint: &RepaintHandle,
        config: &MultiplexerConfig,
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
                self.advance_binding_generation();
                self.observed_resources.clear();
                self.tombstoned_resource_generations.clear();
                self.authoritative_occupants.clear();
                self.authoritative_allocations.clear();
                self.deferred_unknown_events.clear();
                self.deferred_refresh_completed = false;
            }
            self.observed_backend = backend;
            self.last_error = None;
            self.availability_error = None;
            self.refresh_failed = false;
            self.refresh_completed = true;
            self.record_resource_snapshot();
            if !self.deferred_unknown_events.is_empty() {
                self.deferred_refresh_completed = true;
            }
            self.authoritative_closed_targets.clear();
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
        config: &MultiplexerConfig,
    ) -> MuxCommandResult {
        let result = self
            .controller
            .complete_authoritative_command(result, Some(config));
        self.last_error = result.as_ref().err().map(ToString::to_string);
        if let Ok(completion) = &result {
            self.record_resource_snapshot();
            if completion.snapshot.is_some() {
                self.authoritative_closed_targets.clear();
            }
            if let Some(allocated) = completion.allocated() {
                self.record_authoritative_allocation(allocated);
            }
            if let Some(target) = completion.resolved_target() {
                self.record_authoritative_target(target);
            }
            self.synchronize_execution_resource_generations();
        }
        result
    }
}
impl Drop for BindingMuxController {
    fn drop(&mut self) {
        self.advance_binding_generation();
        self.release_event_scope();
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

/// Resolution of a canonical session selector against one binding's authoritative snapshot.
///
/// A selector can match a backend id, display name, or a one-based ordinal. More than one
/// distinct session is deliberately never collapsed into an arbitrary selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionSelectorResolution {
    Missing,
    Resolved { session_id: String },
    Ambiguous { session_ids: Vec<String> },
}

#[derive(Default)]
pub struct MuxController {
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
    current_backend: Option<MultiplexerBackendConfig>,
    last_session_refresh: Option<Instant>,
    /// Cadence cap for [`Self::refresh_sessions`], driven by window focus.
    refresh_interval: Option<Duration>,
    refresh_completed: bool,
    session_refresh_generation: u64,
    session_refresh_tx: Option<mpsc::Sender<SessionRefreshRequest>>,
    session_refresh_rx: Option<mpsc::Receiver<SessionRefreshResult>>,
    session_refresh_pending: bool,
    mux_command_tx: Option<mpsc::Sender<MuxCommandJob>>,
    mux_command_rx: Option<mpsc::Receiver<MuxCommandResult>>,
    backend_factory: Option<BackendFactory>,
    command_config: Arc<Mutex<CommandConfigState>>,
    execution_resource_generations: Option<Arc<Mutex<BTreeMap<MuxResourceKey, u64>>>>,
    execution_binding_generation: Option<Arc<AtomicU64>>,
}

impl MuxController {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolves a canonical selector without allowing an id/name/ordinal collision to retarget
    /// selection. The ordinal follows the legacy one-based session action convention.
    pub fn resolve_session_selector(&self, selector: &str) -> SessionSelectorResolution {
        let ordinal = selector
            .parse::<usize>()
            .ok()
            .and_then(|ordinal| ordinal.checked_sub(1));
        let mut session_ids = Vec::new();
        for (index, session) in self.sessions.iter().enumerate() {
            if (session.id == selector || session.name == selector || Some(index) == ordinal)
                && !session_ids
                    .iter()
                    .any(|session_id| session_id == &session.id)
            {
                session_ids.push(session.id.clone());
            }
        }
        match session_ids.len() {
            0 => SessionSelectorResolution::Missing,
            1 => SessionSelectorResolution::Resolved {
                session_id: session_ids.pop().expect("one resolved session"),
            },
            _ => SessionSelectorResolution::Ambiguous { session_ids },
        }
    }

    /// Cap how often the backend is polled for sessions. Callers set this from window focus each
    /// frame; forced refreshes ([`Self::refresh_on_next_frame`], completed commands) ignore it.
    pub fn set_refresh_interval(&mut self, interval: Duration) {
        self.refresh_interval = Some(interval);
    }

    fn refresh_interval(&self) -> Duration {
        self.refresh_interval
            .unwrap_or(MUX_SESSION_REFRESH_INTERVAL)
    }

    fn with_scope(scope: MuxScope) -> Self {
        Self {
            scope: Some(scope),
            ..Self::new()
        }
    }

    /// Build backends with `factory` instead of the configured one. Set before the first refresh:
    /// the refresh and command workers capture it when they start.
    pub fn set_backend_factory(&mut self, factory: BackendFactory) {
        self.backend_factory = Some(factory);
    }

    fn set_execution_resource_generations(
        &mut self,
        generations: Arc<Mutex<BTreeMap<MuxResourceKey, u64>>>,
    ) {
        self.execution_resource_generations = Some(generations);
    }

    fn set_execution_binding_generation(&mut self, generation: Arc<AtomicU64>) {
        self.execution_binding_generation = Some(generation);
    }

    fn build_backend(&self, config: &MultiplexerConfig) -> Box<dyn MuxBackend> {
        build_backend_with(self.backend_factory.as_ref(), config)
    }

    fn observe_command_config(&mut self, config: &MultiplexerConfig) -> u64 {
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
        if self.session_refresh_pending {
            self.session_refresh_generation = self.session_refresh_generation.wrapping_add(1);
            self.session_refresh_pending = false;
        }
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

    fn take_refresh_completed(&mut self) -> bool {
        std::mem::take(&mut self.refresh_completed)
    }

    pub fn refresh_sessions(
        &mut self,
        repaint: &RepaintHandle,
        config: &MultiplexerConfig,
    ) -> Option<String> {
        self.observe_command_config(config);
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
            .is_some_and(|last| last.elapsed() < self.refresh_interval())
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
        match self.build_backend(config).snapshot() {
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
            if let Err(error) = self.complete_authoritative_command(result, None)
                && first_error.is_none()
            {
                first_error = Some(error.to_string());
            }
        }

        completed.then(|| first_error.map_or(Ok(()), Err))
    }

    fn complete_authoritative_command(
        &mut self,
        result: MuxCommandResult,
        active_config: Option<&MultiplexerConfig>,
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
        // Names change here; ids do not. Pin the selection to its id first so it still resolves once
        // the session answers to the new name, whichever backend applies the rename.
        self.selected_session = self.selected_session_id();
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
            MuxCommandCompletion::requested(None, None),
            None,
            None,
        );
    }

    /// Preserve the immutable launch plan all the way to the backend instead of decomposing it at
    /// UI call sites. Backends receive one validated mutation and can either execute it or return
    /// their typed capability outcome before changing topology.
    pub fn create_session(
        &mut self,
        plan: MuxSessionLaunchPlan,
        repaint: &RepaintHandle,
        config: &MultiplexerConfig,
    ) {
        let session_id = plan.session_id.clone();
        let focus = plan.focus;
        let command = MuxCommand::CreateSession { plan };
        let preferred_session = focus.then(|| session_id.clone());
        if focus {
            self.expected_session = Some(session_id.clone());
        }
        if self
            .execute_native_command(config, command.clone(), preferred_session.clone(), None)
            .is_ok()
        {
            repaint();
            return;
        }
        if focus {
            self.activate_session(&session_id);
        }
        self.enqueue_command(
            repaint,
            config,
            command,
            MuxCommandCompletion::requested(preferred_session, None),
            None,
            None,
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
        self.expected_session = Some(request.session_id.clone());
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
            MuxCommandCompletion::requested(Some(request.session_id), None),
            None,
            None,
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
        let factory = self.backend_factory.clone();
        thread::spawn(move || {
            while let Ok(request) = request_rx.recv() {
                let backend_kind = selected_backend(&request.config);
                let result = build_backend_with(factory.as_ref(), &request.config)
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
        let (selected_session, preferred_window) = self.command_completion(&command);
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
            MuxCommandCompletion::requested(selected_session, selected_window),
            None,
            None,
        );
    }

    pub fn execute_command_authoritatively(
        &mut self,
        repaint: &RepaintHandle,
        config: &MultiplexerConfig,
        mut command: MuxCommand,
        deadline: Instant,
        cancellation: CommandCancellation,
    ) -> mpsc::Receiver<MuxCommandResult> {
        let (response_tx, response_rx) = mpsc::channel();
        let (selected_session, selected_window) = self.command_completion(&command);
        if let Err(error) = freeze_implicit_command_target(&mut command, &self.sessions) {
            let _ = response_tx.send(Err(error));
            return response_rx;
        }
        let execution_precondition = match self.capture_execution_precondition(&command) {
            Ok(precondition) => precondition,
            Err(error) => {
                let _ = response_tx.send(Err(error));
                return response_rx;
            }
        };
        let completion = MuxCommandCompletion::requested(selected_session, selected_window)
            .with_execution_precondition(execution_precondition.clone());
        let command_for_completion = command.clone();
        if cancellation.is_cancelled() {
            let _ = response_tx.send(Err(MuxCommandError::Cancelled));
            return response_rx;
        }
        if Instant::now() >= deadline {
            cancellation.cancel();
            let _ = response_tx.send(Err(MuxCommandError::DeadlineExceeded));
            return response_rx;
        }
        if selected_backend(config) == MultiplexerBackendConfig::Native && !cancellation.try_start()
        {
            let _ = response_tx.send(Err(MuxCommandError::Cancelled));
            return response_rx;
        }
        if selected_backend(config) == MultiplexerBackendConfig::Native {
            let result = self
                .execute_native_command_with_completion(
                    config,
                    command,
                    completion.selected_session.clone(),
                    completion.selected_window.clone(),
                    execution_precondition.as_ref(),
                )
                .and_then(|(snapshot, authoritative)| {
                    MuxCommandCompletion::from_command_snapshot(
                        config.clone(),
                        snapshot,
                        &command_for_completion,
                        execution_precondition,
                        authoritative,
                    )
                });
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
        if matches!(command, MuxCommand::CreateSession { plan } if !plan.focus) {
            return (None, None);
        }
        (
            Some(command_session_id(command).to_owned()),
            optimistic_window_after_command(
                &self.sessions,
                self.selected_window.as_deref(),
                command,
            ),
        )
    }

    fn capture_execution_precondition(
        &self,
        command: &MuxCommand,
    ) -> Result<Option<MuxScopedExecutionPrecondition>, MuxCommandError> {
        self.capture_execution_precondition_at_binding_generation(
            command,
            self.execution_binding_generation(),
        )
    }

    fn capture_execution_precondition_at_binding_generation(
        &self,
        command: &MuxCommand,
        binding_generation: Option<u64>,
    ) -> Result<Option<MuxScopedExecutionPrecondition>, MuxCommandError> {
        let mut precondition = capture_execution_precondition(self.scope, &self.sessions, command)?;
        if let Some(precondition) = &mut precondition {
            precondition.binding_generation = binding_generation;
            precondition.occupant_generation =
                self.execution_resource_generation(&precondition.target);
        }
        Ok(precondition)
    }

    fn execution_binding_generation(&self) -> Option<u64> {
        self.execution_binding_generation
            .as_ref()
            .map(|generation| generation.load(Ordering::Acquire))
    }

    fn execution_resource_key(target: &MuxEventTarget) -> Option<MuxResourceKey> {
        let session_id = target.session_id.clone()?;
        match (
            target.window_id.as_deref(),
            target.pane_id.as_deref(),
            target.terminal_id.as_deref(),
        ) {
            (None, None, None) => Some(MuxResourceKey::Session(session_id)),
            (Some(window_id), None, None) => {
                Some(MuxResourceKey::Window(session_id, window_id.to_owned()))
            }
            (Some(window_id), Some(pane_id), None) => Some(MuxResourceKey::Pane(
                session_id,
                window_id.to_owned(),
                pane_id.to_owned(),
            )),
            (Some(window_id), Some(_), Some(terminal_id)) => Some(MuxResourceKey::Terminal(
                session_id,
                window_id.to_owned(),
                terminal_id.to_owned(),
            )),
            _ => None,
        }
    }

    fn execution_resource_generation(&self, target: &MuxEventTarget) -> Option<u64> {
        let key = Self::execution_resource_key(target)?;
        self.execution_resource_generations
            .as_ref()?
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .copied()
    }

    fn execution_resource_generation_guard(
        &self,
        precondition: Option<&MuxScopedExecutionPrecondition>,
    ) -> Option<MuxResourceGenerationGuard> {
        let precondition = precondition?;
        let generation = precondition.occupant_generation?;
        let key = Self::execution_resource_key(&precondition.target)?;
        Some(MuxResourceGenerationGuard {
            key,
            generation,
            generations: Arc::clone(self.execution_resource_generations.as_ref()?),
        })
    }

    fn execution_binding_generation_guard(
        &self,
        generation: Option<u64>,
    ) -> Option<MuxBindingGenerationGuard> {
        Some(MuxBindingGenerationGuard {
            generation: generation?,
            current: Arc::clone(self.execution_binding_generation.as_ref()?),
        })
    }

    fn execute_native_command(
        &mut self,
        config: &MultiplexerConfig,
        mut command: MuxCommand,
        preferred_session: Option<String>,
        preferred_window: Option<String>,
    ) -> Result<MuxSnapshot, MuxCommandError> {
        freeze_implicit_command_target(&mut command, &self.sessions)?;
        let precondition = self.capture_execution_precondition(&command)?;
        self.execute_native_command_with_completion(
            config,
            command,
            preferred_session,
            preferred_window,
            precondition.as_ref(),
        )
        .map(|(snapshot, _)| snapshot)
    }

    fn execute_native_command_with_completion(
        &mut self,
        config: &MultiplexerConfig,
        command: MuxCommand,
        preferred_session: Option<String>,
        preferred_window: Option<String>,
        precondition: Option<&MuxScopedExecutionPrecondition>,
    ) -> Result<(MuxSnapshot, Option<MuxBackendCommandCompletion>), MuxCommandError> {
        let backend_kind = selected_backend(config);
        if backend_kind != MultiplexerBackendConfig::Native {
            return Err(MuxCommandError::Unavailable);
        }
        let mut backend = self.build_backend(config);
        execute_backend_command(backend.as_mut(), self.scope, command, precondition)
            .and_then(|()| {
                let authoritative = backend.take_authoritative_completion();
                backend
                    .snapshot()
                    .map(|snapshot| (snapshot, authoritative))
                    .map_err(command_error_from_backend)
            })
            .inspect(|(snapshot, _)| {
                self.apply_snapshot(
                    backend_kind,
                    snapshot.clone(),
                    preferred_session,
                    preferred_window,
                );
                self.last_session_refresh = None;
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
        let factory = self.backend_factory.clone();
        let command_config = Arc::clone(&self.command_config);
        thread::spawn(move || {
            while let Ok(job) = request_rx.recv() {
                let cancellation = job.cancellation.as_ref();
                let state = command_config
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let result = if state.generation != job.config_generation
                    || job
                        .binding_generation_guard
                        .as_ref()
                        .is_some_and(|guard| !guard.is_current())
                {
                    Err(MuxCommandError::Stale)
                } else if let Some(error) = job.precondition_failure.clone() {
                    Err(error)
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
                    let resource_generation_guard = job.resource_generation_guard;
                    let binding_generation_guard = job.binding_generation_guard;
                    let command_for_completion = job.command.clone();
                    let execution_precondition = job.completion.execution_precondition.clone();
                    let mut backend = build_backend_with(factory.as_ref(), &job.config);
                    execute_backend_command_with_generation_guards(
                        backend.as_mut(),
                        job.scope,
                        job.command,
                        execution_precondition.as_ref(),
                        resource_generation_guard.as_ref(),
                        binding_generation_guard.as_ref(),
                    )
                    .and_then(|()| {
                        let authoritative = backend.take_authoritative_completion();
                        if job.response.is_some() {
                            backend
                                .snapshot()
                                .map_err(command_error_from_backend)
                                .and_then(|snapshot| {
                                    MuxCommandCompletion::from_command_snapshot(
                                        job.config.clone(),
                                        snapshot,
                                        &command_for_completion,
                                        execution_precondition,
                                        authoritative,
                                    )
                                })
                        } else {
                            Ok(job.completion)
                        }
                    })
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
        config: &MultiplexerConfig,
        mut command: MuxCommand,
        mut completion: MuxCommandCompletion,
        response: Option<mpsc::Sender<MuxCommandResult>>,
        execution: Option<(Instant, CommandCancellation)>,
    ) {
        let (deadline, cancellation) = execution
            .map(|(deadline, cancellation)| (Some(deadline), Some(cancellation)))
            .unwrap_or_default();
        let binding_generation = completion
            .execution_precondition
            .as_ref()
            .and_then(|precondition| precondition.binding_generation)
            .or_else(|| self.execution_binding_generation());
        if let Some(precondition) = &mut completion.execution_precondition {
            precondition.binding_generation = binding_generation;
        }
        let captured_precondition = freeze_implicit_command_target(&mut command, &self.sessions)
            .and_then(|()| {
                if completion.execution_precondition.is_some() {
                    Ok(completion.execution_precondition.clone())
                } else {
                    self.capture_execution_precondition_at_binding_generation(
                        &command,
                        binding_generation,
                    )
                }
            });
        let (completion, precondition_failure) = match captured_precondition {
            Ok(precondition) => (completion.with_execution_precondition(precondition), None),
            Err(error) => (completion, Some(error)),
        };
        let resource_generation_guard =
            self.execution_resource_generation_guard(completion.execution_precondition.as_ref());
        let binding_generation_guard = self.execution_binding_generation_guard(binding_generation);
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
            precondition_failure,
            resource_generation_guard,
            binding_generation_guard,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capability::BindingCapabilityDescriptor,
        command::{MuxDirection, MuxPaneResize, MuxSplitDirection},
        snapshot::{MuxPaneAnchor, MuxWindow},
    };

    #[test]
    fn backend_error_mapping_preserves_typed_and_spawn_failures() {
        assert_eq!(
            command_error_from_backend(
                MuxBackendOperationError::unsupported("not implemented").into()
            ),
            MuxCommandError::Unsupported
        );
        assert_eq!(
            command_error_from_backend(
                MuxBackendOperationError::Denied("policy".to_owned()).into()
            ),
            MuxCommandError::Denied
        );
        assert_eq!(
            command_error_from_backend(MuxBackendOperationError::stale("reused pane").into()),
            MuxCommandError::Stale
        );
        assert_eq!(
            command_error_from_backend(std::io::Error::from(std::io::ErrorKind::NotFound).into()),
            MuxCommandError::Unavailable
        );
        assert_eq!(
            command_error_from_backend(
                std::io::Error::from(std::io::ErrorKind::PermissionDenied).into()
            ),
            MuxCommandError::Denied
        );
        assert_eq!(
            command_error_from_backend(anyhow::anyhow!("backend exited unexpectedly")),
            MuxCommandError::Failed("backend exited unexpectedly".to_owned())
        );
    }

    #[test]
    fn selected_session_anchor_resolves_by_backend_id_or_session_name() {
        let anchor = MuxPaneAnchor {
            session_id: "$7".to_owned(),
            pane_id: Some("%9".to_owned()),
            terminal_id: Some("t9".to_owned()),
            pane_pid: None,
            cwd: None,
            process: None,
            occupant_id: None,
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
                        terminal_id: Some("t11".to_owned()),
                        pane_pid: None,
                        cwd: None,
                        process: Some("nvim".to_owned()),
                        occupant_id: None,
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
    fn pane_and_terminal_ids_stay_distinct_in_event_completion_and_stale_checks() {
        let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(1));
        let pane = MuxPaneAnchor {
            session_id: "$1".to_owned(),
            pane_id: Some("%p".to_owned()),
            terminal_id: Some("t1".to_owned()),
            pane_pid: None,
            cwd: None,
            process: Some("shell".to_owned()),
            occupant_id: None,
        };
        let snapshot = MuxSnapshot {
            active_session_id: Some("$1".to_owned()),
            sessions: vec![MuxSession {
                id: "$1".to_owned(),
                name: "work".to_owned(),
                active: true,
                anchor: pane.clone(),
                active_window_id: Some("@1".to_owned()),
                windows: vec![MuxWindow {
                    id: "@1".to_owned(),
                    index: 0,
                    name: "editor".to_owned(),
                    active: true,
                    anchor: pane.clone(),
                    panes: vec![pane.clone()],
                    layout: None,
                    progress: None,
                }],
            }],
        };
        let target = pane_event_target("$1", "@1", &pane);
        assert_eq!(target.pane_id.as_deref(), Some("%p"));
        assert_eq!(target.terminal_id.as_deref(), Some("t1"));

        let precondition = MuxScopedExecutionPrecondition {
            scope,
            target: target.clone(),
            occupant_fingerprint: None,
            binding_generation: None,
            occupant_generation: None,
        };
        assert!(precondition.matches_snapshot(&snapshot));
        let mut stale_terminal = precondition.clone();
        stale_terminal.target.terminal_id = Some("%p".to_owned());
        assert!(!stale_terminal.matches_snapshot(&snapshot));
        let mut stale_pane = precondition.clone();
        stale_pane.target.pane_id = Some("%other".to_owned());
        assert!(!stale_pane.matches_snapshot(&snapshot));

        let command = MuxCommand::ResizePane {
            session_id: "$1".to_owned(),
            pane_id: Some("%p".to_owned()),
            adjustment: MuxPaneResize::Directional {
                direction: MuxDirection::Right,
                cells: 1,
            },
        };
        let completion = MuxCommandCompletion::from_command_snapshot(
            MultiplexerConfig::default(),
            snapshot,
            &command,
            Some(precondition),
            None,
        )
        .expect("build completion");
        assert_eq!(completion.resolved_target(), Some(&target));
        assert_eq!(
            completion
                .execution_precondition()
                .map(|precondition| &precondition.target),
            Some(&target)
        );
    }

    #[test]
    fn canonical_session_selector_resolves_ids_names_ordinals_and_collisions() {
        let controller = MuxController {
            sessions: vec![
                session("alpha-id", "main"),
                session("beta-id", "work"),
                session("gamma-id", "other"),
            ],
            ..Default::default()
        };

        assert_eq!(
            controller.resolve_session_selector("beta-id"),
            SessionSelectorResolution::Resolved {
                session_id: "beta-id".to_owned(),
            }
        );
        assert_eq!(
            controller.resolve_session_selector("work"),
            SessionSelectorResolution::Resolved {
                session_id: "beta-id".to_owned(),
            }
        );
        assert_eq!(
            controller.resolve_session_selector("2"),
            SessionSelectorResolution::Resolved {
                session_id: "beta-id".to_owned(),
            }
        );

        let collision = MuxController {
            sessions: vec![session("first-id", "shared"), session("shared", "second")],
            ..Default::default()
        };
        assert_eq!(
            collision.resolve_session_selector("shared"),
            SessionSelectorResolution::Ambiguous {
                session_ids: vec!["first-id".to_owned(), "shared".to_owned()],
            }
        );
    }

    #[test]
    fn canonical_session_selector_deduplicates_repeated_identity() {
        let controller = MuxController {
            sessions: vec![
                session("first-id", "main"),
                session("2", "2"),
                session("2", "other"),
            ],
            ..Default::default()
        };

        assert_eq!(
            controller.resolve_session_selector("2"),
            SessionSelectorResolution::Resolved {
                session_id: "2".to_owned(),
            }
        );
    }

    #[test]
    fn canonical_session_selector_resolves_one_id_name_ordinal_match() {
        let controller = MuxController {
            sessions: vec![session("first-id", "main"), session("2", "2")],
            ..Default::default()
        };

        assert_eq!(
            controller.resolve_session_selector("2"),
            SessionSelectorResolution::Resolved {
                session_id: "2".to_owned(),
            }
        );
    }

    #[test]
    fn canonical_session_selector_keeps_distinct_ordinal_collision_ambiguous() {
        let controller = MuxController {
            sessions: vec![session("first-id", "2"), session("second-id", "other")],
            ..Default::default()
        };

        assert_eq!(
            controller.resolve_session_selector("2"),
            SessionSelectorResolution::Ambiguous {
                session_ids: vec!["first-id".to_owned(), "second-id".to_owned()],
            }
        );
    }

    #[test]
    fn selected_session_anchor_falls_back_to_active_window_pane() {
        let mut active = window("@2", 2);
        active.anchor = MuxPaneAnchor {
            session_id: "$1".to_owned(),
            pane_id: Some("%2".to_owned()),
            terminal_id: Some("t2".to_owned()),
            pane_pid: None,
            cwd: None,
            process: Some("fish".to_owned()),
            occupant_id: None,
        };
        let mut inactive = window("@1", 1);
        inactive.anchor = MuxPaneAnchor {
            session_id: "$1".to_owned(),
            pane_id: Some("%1".to_owned()),
            terminal_id: Some("t1".to_owned()),
            pane_pid: None,
            cwd: None,
            process: Some("zsh".to_owned()),
            occupant_id: None,
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
            .send(Ok(MuxCommandCompletion::requested(
                Some("$1".to_owned()),
                Some("@2".to_owned()),
            )))
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
            .send(Ok(MuxCommandCompletion::requested(
                Some("$1".to_owned()),
                None,
            )))
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
            .send(Ok(MuxCommandCompletion::requested(None, None)))
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
            .send(Ok(MuxCommandCompletion::requested(None, None)))
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
    fn authoritative_completion_rejects_a_snapshot_from_an_inactive_config() {
        let stale_config = MultiplexerConfig {
            backend: MultiplexerBackendConfig::Tmux,
            ..Default::default()
        };
        let active_config = MultiplexerConfig {
            backend: MultiplexerBackendConfig::Zellij,
            ..Default::default()
        };
        let original = session("$1", "original");
        let replacement = session("$2", "replacement");
        let mut controller = MuxController {
            sessions: vec![original.clone()],
            ..Default::default()
        };
        let completion =
            MuxCommandCompletion::from_snapshot(stale_config, snapshot_of(vec![replacement]));

        assert_eq!(
            controller.complete_authoritative_command(Ok(completion), Some(&active_config)),
            Err(MuxCommandError::Stale)
        );
        assert_eq!(controller.sessions(), &[original]);
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
                terminal_id: None,
                pane_pid: None,
                cwd: None,
                process: None,
                occupant_id: None,
            },
            active_window_id: None,
            windows: Vec::new(),
        }
    }

    /// A backend whose session list the test owns and whose commands change nothing, standing in for
    /// a backend that has not caught up with a command yet.
    #[derive(Clone)]
    struct StaticBackend {
        sessions: Vec<MuxSession>,
    }

    impl MuxBackend for StaticBackend {
        fn snapshot(&self) -> anyhow::Result<MuxSnapshot> {
            Ok(MuxSnapshot {
                active_session_id: self.sessions.first().map(|session| session.id.clone()),
                sessions: self.sessions.clone(),
            })
        }

        fn execute(&mut self, _command: MuxCommand) -> anyhow::Result<()> {
            Ok(())
        }

        fn execute_checked(
            &mut self,
            scope: MuxScope,
            command: MuxCommand,
            _precondition: Option<&MuxScopedExecutionPrecondition>,
        ) -> BindingOperationOutcome<anyhow::Result<()>> {
            let descriptor = self.capabilities(scope);
            descriptor.invoke(
                descriptor.request(command.operation()),
                BindingOperationAvailability::Available,
                || self.execute(command),
            )
        }
    }

    fn controller_with_backend(sessions: Vec<MuxSession>) -> MuxController {
        let backend = StaticBackend { sessions };
        let mut controller = MuxController::new();
        controller.set_backend_factory(std::sync::Arc::new(move |_| Box::new(backend.clone())));
        controller
    }

    fn snapshot_of(sessions: Vec<MuxSession>) -> MuxSnapshot {
        MuxSnapshot {
            active_session_id: sessions.first().map(|session| session.id.clone()),
            sessions,
        }
    }

    fn command_capabilities(scope: MuxScope) -> BindingCapabilityDescriptor {
        BindingCapabilityDescriptor::new(
            scope,
            [
                BindingOperation::ClosePane,
                BindingOperation::ActivateWindow,
                BindingOperation::CreateWindow,
                BindingOperation::LastPane,
                BindingOperation::MoveWindow,
                BindingOperation::NavigatePane,
                BindingOperation::NavigateWindow,
                BindingOperation::RenameSession,
                BindingOperation::RenameWindow,
                BindingOperation::ResizePane,
                BindingOperation::SplitPane,
                BindingOperation::TogglePaneZoom,
            ],
        )
    }

    fn command_scope() -> MuxScope {
        MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(2))
    }

    fn target_window(
        window_id: &str,
        index: u32,
        pane_id: &str,
        terminal_id: &str,
        occupant_id: &str,
    ) -> MuxWindow {
        MuxWindow {
            id: window_id.to_owned(),
            index,
            name: window_id.to_owned(),
            active: window_id == "@old",
            anchor: MuxPaneAnchor {
                session_id: "$1".to_owned(),
                pane_id: Some(pane_id.to_owned()),
                terminal_id: Some(terminal_id.to_owned()),
                pane_pid: None,
                cwd: None,
                process: Some("shell".to_owned()),
                occupant_id: Some(occupant_id.to_owned()),
            },
            panes: Vec::new(),
            layout: None,
            progress: None,
        }
    }

    fn targeted_snapshot() -> MuxSnapshot {
        let mut work = session("$1", "work");
        work.active = true;
        work.active_window_id = Some("@old".to_owned());
        work.windows = vec![
            target_window("@old", 1, "%old", "t-old", "occupant-old"),
            target_window("@new", 2, "%new", "t-new", "occupant-new"),
        ];
        MuxSnapshot {
            sessions: vec![work],
            active_session_id: Some("$1".to_owned()),
        }
    }

    struct RecordingBackend {
        snapshot: MuxSnapshot,
        commands: Vec<MuxCommand>,
    }

    impl MuxBackend for RecordingBackend {
        fn snapshot(&self) -> anyhow::Result<MuxSnapshot> {
            Ok(self.snapshot.clone())
        }

        fn execute(&mut self, command: MuxCommand) -> anyhow::Result<()> {
            self.commands.push(command);
            Ok(())
        }

        fn execute_checked(
            &mut self,
            scope: MuxScope,
            command: MuxCommand,
            _precondition: Option<&MuxScopedExecutionPrecondition>,
        ) -> BindingOperationOutcome<anyhow::Result<()>> {
            let descriptor = self.capabilities(scope);
            descriptor.invoke(
                descriptor.request(command.operation()),
                BindingOperationAvailability::Available,
                || self.execute(command),
            )
        }

        fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor {
            command_capabilities(scope)
        }
    }

    #[derive(Clone)]
    struct BlockingRecordingBackend {
        state: Arc<Mutex<BlockingRecordingState>>,
        started: mpsc::SyncSender<()>,
        release: Arc<Mutex<mpsc::Receiver<()>>>,
    }

    struct BlockingRecordingState {
        snapshot: MuxSnapshot,
        commands: Vec<MuxCommand>,
    }

    impl MuxBackend for BlockingRecordingBackend {
        fn snapshot(&self) -> anyhow::Result<MuxSnapshot> {
            Ok(self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .snapshot
                .clone())
        }

        fn execute(&mut self, command: MuxCommand) -> anyhow::Result<()> {
            let block = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.commands.push(command);
                state.commands.len() == 1
            };
            if block {
                self.started.send(()).expect("signal first command start");
                self.release
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .recv()
                    .expect("release first command");
            }
            Ok(())
        }

        fn execute_checked(
            &mut self,
            scope: MuxScope,
            command: MuxCommand,
            _precondition: Option<&MuxScopedExecutionPrecondition>,
        ) -> BindingOperationOutcome<anyhow::Result<()>> {
            let descriptor = self.capabilities(scope);
            descriptor.invoke(
                descriptor.request(command.operation()),
                BindingOperationAvailability::Available,
                || self.execute(command),
            )
        }

        fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor {
            command_capabilities(scope)
        }
    }

    #[test]
    fn queued_command_is_rejected_when_backend_config_changes_before_start() {
        #[derive(Clone)]
        struct BlockingBackend {
            execute_count: Arc<std::sync::atomic::AtomicUsize>,
            started: mpsc::SyncSender<()>,
            release: Arc<Mutex<mpsc::Receiver<()>>>,
        }

        impl MuxBackend for BlockingBackend {
            fn snapshot(&self) -> anyhow::Result<MuxSnapshot> {
                Ok(snapshot_of(Vec::new()))
            }

            fn execute(&mut self, _command: MuxCommand) -> anyhow::Result<()> {
                if self.execute_count.fetch_add(1, Ordering::AcqRel) == 0 {
                    self.started.send(()).expect("signal first command start");
                    self.release
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .recv()
                        .expect("release first command");
                }
                Ok(())
            }

            fn execute_checked(
                &mut self,
                scope: MuxScope,
                command: MuxCommand,
                _precondition: Option<&MuxScopedExecutionPrecondition>,
            ) -> BindingOperationOutcome<anyhow::Result<()>> {
                let descriptor = self.capabilities(scope);
                descriptor.invoke(
                    descriptor.request(command.operation()),
                    BindingOperationAvailability::Available,
                    || self.execute(command),
                )
            }
        }

        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let execute_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let backend = BlockingBackend {
            execute_count: Arc::clone(&execute_count),
            started: started_tx,
            release: Arc::new(Mutex::new(release_rx)),
        };
        let mut controller = MuxController::new();
        controller.set_backend_factory(Arc::new(move |_| Box::new(backend.clone())));
        let repaint: RepaintHandle = Arc::new(|| {});
        let queued_config = MultiplexerConfig {
            backend: MultiplexerBackendConfig::Tmux,
            ..Default::default()
        };
        let active_config = MultiplexerConfig {
            backend: MultiplexerBackendConfig::Zellij,
            ..Default::default()
        };
        let command = || MuxCommand::NewWindow {
            session_id: "$1".to_owned(),
            cwd: None,
        };

        let first = controller.execute_command_authoritatively(
            &repaint,
            &queued_config,
            command(),
            Instant::now() + Duration::from_secs(1),
            CommandCancellation::new(),
        );
        let second = controller.execute_command_authoritatively(
            &repaint,
            &queued_config,
            command(),
            Instant::now() + Duration::from_secs(1),
            CommandCancellation::new(),
        );
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first command started");
        controller.observe_command_config(&active_config);
        release_tx.send(()).expect("release first command");

        assert!(first.recv_timeout(Duration::from_secs(1)).unwrap().is_ok());
        assert_eq!(
            second.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(MuxCommandError::Stale)
        );
        assert_eq!(execute_count.load(Ordering::Acquire), 1);
    }

    #[test]
    fn queued_implicit_commands_keep_their_queue_time_target_after_focus_changes() {
        let scope = command_scope();
        let snapshot = targeted_snapshot();
        let state = Arc::new(Mutex::new(BlockingRecordingState {
            snapshot: snapshot.clone(),
            commands: Vec::new(),
        }));
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let backend = BlockingRecordingBackend {
            state: Arc::clone(&state),
            started: started_tx,
            release: Arc::new(Mutex::new(release_rx)),
        };
        let mut controller = MuxController::with_scope(scope);
        controller.sessions = snapshot.sessions.clone();
        controller.set_backend_factory(Arc::new(move |_| Box::new(backend.clone())));
        let repaint: RepaintHandle = Arc::new(|| {});
        let config = MultiplexerConfig {
            backend: MultiplexerBackendConfig::Tmux,
            ..Default::default()
        };

        let blocker = controller.execute_command_authoritatively(
            &repaint,
            &config,
            MuxCommand::RenameSession {
                session_id: "$1".to_owned(),
                name: "renamed".to_owned(),
            },
            Instant::now() + Duration::from_secs(5),
            CommandCancellation::new(),
        );
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first command started");
        let stale_window = controller.execute_command_authoritatively(
            &repaint,
            &config,
            MuxCommand::NewWindow {
                session_id: "$1".to_owned(),
                cwd: None,
            },
            Instant::now() + Duration::from_secs(5),
            CommandCancellation::new(),
        );

        let commands = [
            MuxCommand::MoveWindow {
                session_id: "$1".to_owned(),
                window_id: None,
                delta: 1,
            },
            MuxCommand::SelectPane {
                session_id: "$1".to_owned(),
                window_id: None,
                direction: MuxDirection::Right,
            },
            MuxCommand::SelectNextPane {
                session_id: "$1".to_owned(),
                window_id: None,
            },
            MuxCommand::SelectPreviousPane {
                session_id: "$1".to_owned(),
                window_id: None,
            },
            MuxCommand::SelectLastPane {
                session_id: "$1".to_owned(),
                window_id: None,
            },
            MuxCommand::SplitPane {
                session_id: "$1".to_owned(),
                pane_id: None,
                direction: MuxSplitDirection::Right,
            },
            MuxCommand::KillPane {
                session_id: "$1".to_owned(),
                pane_id: None,
            },
            MuxCommand::ClosePane {
                session_id: "$1".to_owned(),
                pane_id: None,
            },
            MuxCommand::TogglePaneZoom {
                session_id: "$1".to_owned(),
                pane_id: None,
            },
            MuxCommand::ResizePane {
                session_id: "$1".to_owned(),
                pane_id: None,
                adjustment: MuxPaneResize::Directional {
                    direction: MuxDirection::Right,
                    cells: 1,
                },
            },
        ];
        let queued = commands
            .into_iter()
            .map(|command| {
                controller.execute_command_authoritatively(
                    &repaint,
                    &config,
                    command,
                    Instant::now() + Duration::from_secs(5),
                    CommandCancellation::new(),
                )
            })
            .collect::<Vec<_>>();

        {
            let mut state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.snapshot.sessions[0].active_window_id = Some("@new".to_owned());
            state.snapshot.sessions[0].windows[0].active = false;
            state.snapshot.sessions[0].windows[1].active = true;
        }
        release_tx.send(()).expect("release first command");

        assert!(
            blocker
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .is_ok()
        );
        assert_eq!(
            stale_window.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(MuxCommandError::Stale)
        );
        for result in queued {
            assert!(result.recv_timeout(Duration::from_secs(1)).unwrap().is_ok());
        }

        let state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let commands = &state.commands;
        assert_eq!(commands.len(), 11);
        assert!(matches!(
            &commands[1],
            MuxCommand::MoveWindow {
                window_id: Some(window_id),
                ..
            } if window_id == "@old"
        ));
        for command in &commands[2..6] {
            assert!(matches!(
                command,
                MuxCommand::SelectPane {
                    window_id: Some(window_id),
                    ..
                } | MuxCommand::SelectNextPane {
                    window_id: Some(window_id),
                    ..
                } | MuxCommand::SelectPreviousPane {
                    window_id: Some(window_id),
                    ..
                } | MuxCommand::SelectLastPane {
                    window_id: Some(window_id),
                    ..
                } if window_id == "@old"
            ));
        }
        for command in &commands[6..] {
            assert!(matches!(
                command,
                MuxCommand::SplitPane {
                    pane_id: Some(pane_id),
                    ..
                } | MuxCommand::KillPane {
                    pane_id: Some(pane_id),
                    ..
                } | MuxCommand::ClosePane {
                    pane_id: Some(pane_id),
                    ..
                } | MuxCommand::TogglePaneZoom {
                    pane_id: Some(pane_id),
                    ..
                } | MuxCommand::ResizePane {
                    pane_id: Some(pane_id),
                    ..
                } if pane_id == "%old"
            ));
        }
    }

    #[test]
    fn pane_occupant_replacement_stales_only_pane_preconditions() {
        let scope = command_scope();
        let source = targeted_snapshot();
        let mut replacement = source.clone();
        replacement.sessions[0].windows[0].anchor.occupant_id =
            Some("occupant-replacement".to_owned());

        let pane_command = MuxCommand::ResizePane {
            session_id: "$1".to_owned(),
            pane_id: Some("%old".to_owned()),
            adjustment: MuxPaneResize::Directional {
                direction: MuxDirection::Right,
                cells: 1,
            },
        };
        let window_command = MuxCommand::RenameWindow {
            session_id: "$1".to_owned(),
            window_id: "@old".to_owned(),
            name: "renamed".to_owned(),
        };
        let session_command = MuxCommand::RenameSession {
            session_id: "$1".to_owned(),
            name: "renamed".to_owned(),
        };
        let pane_precondition =
            capture_execution_precondition(Some(scope), &source.sessions, &pane_command)
                .unwrap()
                .expect("pane precondition");
        let window_precondition =
            capture_execution_precondition(Some(scope), &source.sessions, &window_command)
                .unwrap()
                .expect("window precondition");
        let session_precondition =
            capture_execution_precondition(Some(scope), &source.sessions, &session_command)
                .unwrap()
                .expect("session precondition");

        assert_eq!(pane_precondition.target.pane_id.as_deref(), Some("%old"));
        assert_eq!(
            window_precondition.target.window_id.as_deref(),
            Some("@old")
        );
        assert!(window_precondition.target.pane_id.is_none());
        assert!(session_precondition.target.window_id.is_none());

        let mut backend = RecordingBackend {
            snapshot: replacement,
            commands: Vec::new(),
        };
        assert_eq!(
            execute_backend_command(
                &mut backend,
                Some(scope),
                pane_command,
                Some(&pane_precondition),
            ),
            Err(MuxCommandError::Stale)
        );
        assert_eq!(
            execute_backend_command(
                &mut backend,
                Some(scope),
                window_command.clone(),
                Some(&window_precondition),
            ),
            Ok(())
        );
        assert_eq!(
            execute_backend_command(
                &mut backend,
                Some(scope),
                session_command.clone(),
                Some(&session_precondition),
            ),
            Ok(())
        );
        assert_eq!(backend.commands, vec![window_command, session_command]);
    }

    #[test]
    fn window_navigation_rechecks_its_resolved_window_before_execution() {
        let scope = command_scope();
        let source = targeted_snapshot();
        let mut switched_focus = source.clone();
        switched_focus.sessions[0].active_window_id = Some("@new".to_owned());
        switched_focus.sessions[0].windows[0].active = false;
        switched_focus.sessions[0].windows[1].active = true;
        let mut backend = RecordingBackend {
            snapshot: switched_focus,
            commands: Vec::new(),
        };

        for command in [
            MuxCommand::ActivateNextWindow {
                session_id: "$1".to_owned(),
            },
            MuxCommand::ActivatePreviousWindow {
                session_id: "$1".to_owned(),
            },
            MuxCommand::ActivateLastWindow {
                session_id: "$1".to_owned(),
            },
        ] {
            let precondition =
                capture_execution_precondition(Some(scope), &source.sessions, &command)
                    .unwrap()
                    .expect("window precondition");
            assert_eq!(
                execute_backend_command(&mut backend, Some(scope), command, Some(&precondition)),
                Err(MuxCommandError::Stale)
            );
        }

        let explicit = MuxCommand::ActivateWindow {
            session_id: "$1".to_owned(),
            window_id: "@old".to_owned(),
        };
        let explicit_precondition =
            capture_execution_precondition(Some(scope), &source.sessions, &explicit)
                .unwrap()
                .expect("explicit window precondition");
        assert_eq!(
            execute_backend_command(
                &mut backend,
                Some(scope),
                explicit.clone(),
                Some(&explicit_precondition),
            ),
            Ok(())
        );

        let indexed = MuxCommand::ActivateWindowIndex {
            session_id: "$1".to_owned(),
            index: 1,
        };
        let indexed_precondition =
            capture_execution_precondition(Some(scope), &source.sessions, &indexed)
                .unwrap()
                .expect("indexed window precondition");
        assert_eq!(
            execute_backend_command(
                &mut backend,
                Some(scope),
                indexed.clone(),
                Some(&indexed_precondition),
            ),
            Ok(())
        );
        assert_eq!(backend.commands, vec![explicit, indexed]);
    }

    #[test]
    fn a_new_session_keeps_focus_until_the_backend_reports_it() {
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let config = MultiplexerConfig {
            backend: MultiplexerBackendConfig::Native,
            ..Default::default()
        };
        let work = session("$1", "work");
        let mut controller = controller_with_backend(vec![work.clone()]);
        controller.apply_refreshed_snapshot(
            MultiplexerBackendConfig::Native,
            snapshot_of(vec![work.clone()]),
        );

        controller.create_project_session(
            NewMuxSessionRequest {
                session_id: "agents/main".to_owned(),
                cwd: "/repo".to_owned(),
            },
            &repaint,
            &config,
        );

        assert_eq!(
            controller.selected_session(),
            Some("agents/main"),
            "the session just created takes focus even before the backend reports it"
        );
        controller.apply_session_order(&["work".to_owned(), "agents/main".to_owned()]);
        assert_eq!(
            controller.selected_session(),
            Some("agents/main"),
            "applying the session order must not snap focus back to an existing session"
        );

        let created = session("$2", "agents/main");
        controller.apply_refreshed_snapshot(
            MultiplexerBackendConfig::Native,
            snapshot_of(vec![work.clone(), created]),
        );
        assert_eq!(
            controller.selected_session(),
            Some("$2"),
            "once the backend reports it, the selection is its id: the sidebar marks the current row \
             by id, and a name stops resolving the moment the session is renamed"
        );

        // The expectation is spent: a session that disappears still hands focus back.
        controller
            .apply_refreshed_snapshot(MultiplexerBackendConfig::Native, snapshot_of(vec![work]));
        assert_eq!(controller.selected_session(), Some("$1"));
    }

    #[test]
    fn renaming_the_selected_session_keeps_it_selected() {
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let config = MultiplexerConfig {
            backend: MultiplexerBackendConfig::Tmux,
            ..Default::default()
        };
        let work = session("$1", "work");
        let agents = session("$2", "agents");
        let mut controller = controller_with_backend(vec![work.clone(), agents.clone()]);
        controller.apply_refreshed_snapshot(
            MultiplexerBackendConfig::Tmux,
            snapshot_of(vec![work.clone(), agents]),
        );
        // Focus tracked by name, as it is for a session bootty created this run.
        controller.activate_session("agents");

        controller.rename_session("$2", "agents/main".to_owned(), &repaint, &config);
        controller.apply_refreshed_snapshot(
            MultiplexerBackendConfig::Tmux,
            snapshot_of(vec![work, session("$2", "agents/main")]),
        );

        assert_eq!(
            controller.selected_session(),
            Some("$2"),
            "a rename must not hand focus to the backend's active session"
        );
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
            terminal_id: Some("t1".to_owned()),
            pane_pid: None,
            cwd: Some("/repo".to_owned()),
            process: Some("nvim".to_owned()),
            occupant_id: None,
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
            terminal_id: Some("t1".to_owned()),
            pane_pid: None,
            cwd: Some("/repo".to_owned()),
            process: Some("nvim".to_owned()),
            occupant_id: None,
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
    fn scoped_resource_generations_advance_on_replacement() {
        let pane = MuxPaneAnchor {
            session_id: "$1".to_owned(),
            pane_id: Some("%1".to_owned()),
            terminal_id: Some("t1".to_owned()),
            pane_pid: Some(10),
            cwd: None,
            process: Some("zsh".to_owned()),
            occupant_id: None,
        };
        let mut editor = window("@1", 1);
        editor.anchor = pane.clone();
        editor.panes = vec![pane];
        let mut work = session("$1", "work");
        work.active_window_id = Some("@1".to_owned());
        work.windows = vec![editor];
        let mut binding = BindingMuxController::default();
        binding.controller.sessions = vec![work.clone()];

        binding.record_resource_snapshot();

        assert_eq!(binding.session_generation("$1"), Some(1));
        assert_eq!(binding.window_generation("$1", "@1"), Some(1));
        assert_eq!(binding.pane_generation("$1", "@1", "%1"), Some(1));
        assert_eq!(binding.terminal_generation("$1", "@1", "t1"), Some(1));

        binding.controller.sessions[0].windows[0].panes[0].pane_pid = Some(11);
        binding.record_resource_snapshot();
        assert_eq!(binding.pane_generation("$1", "@1", "%1"), Some(2));
        assert_eq!(binding.terminal_generation("$1", "@1", "t1"), Some(2));

        binding.controller.sessions.clear();
        binding.record_resource_snapshot();
        binding.controller.sessions = vec![work];
        binding.record_resource_snapshot();

        assert_eq!(binding.session_generation("$1"), Some(3));
        assert_eq!(binding.window_generation("$1", "@1"), Some(3));
        assert_eq!(binding.pane_generation("$1", "@1", "%1"), Some(3));
        assert_eq!(binding.terminal_generation("$1", "@1", "t1"), Some(3));
    }

    #[test]
    fn resource_generations_bound_live_inventory_through_replacements_and_removals() {
        let pane = MuxPaneAnchor {
            session_id: "$1".to_owned(),
            pane_id: Some("%1".to_owned()),
            terminal_id: Some("t1".to_owned()),
            pane_pid: Some(10),
            cwd: None,
            process: Some("zsh".to_owned()),
            occupant_id: None,
        };
        let mut editor = window("@1", 1);
        editor.anchor = pane.clone();
        editor.panes = vec![pane];
        let mut work = session("$1", "work");
        work.active_window_id = Some("@1".to_owned());
        work.windows = vec![editor];
        let mut binding = BindingMuxController::default();
        binding.controller.sessions = vec![work];
        binding.record_resource_snapshot();

        let session_generation = binding
            .session_generation("$1")
            .expect("session generation");
        let window_generation = binding
            .window_generation("$1", "@1")
            .expect("window generation");
        let pane_generation = binding
            .pane_generation("$1", "@1", "%1")
            .expect("pane generation");
        assert_eq!(session_generation, 1);
        assert_eq!(window_generation, 1);
        assert_eq!(pane_generation, 1);
        assert_eq!(binding.resource_generations.len(), 4);

        binding.record_resource_snapshot();
        assert_eq!(binding.session_generation("$1"), Some(session_generation));
        assert_eq!(
            binding.window_generation("$1", "@1"),
            Some(window_generation)
        );
        assert_eq!(
            binding.pane_generation("$1", "@1", "%1"),
            Some(pane_generation)
        );
        assert_eq!(binding.resource_generations.len(), 4);

        binding.controller.sessions[0].windows[0].panes[0].pane_pid = Some(11);
        binding.record_resource_snapshot();
        let replaced_generation = binding
            .pane_generation("$1", "@1", "%1")
            .expect("replaced pane generation");
        assert!(replaced_generation > pane_generation);
        assert_eq!(binding.session_generation("$1"), Some(session_generation));
        assert_eq!(
            binding.window_generation("$1", "@1"),
            Some(window_generation)
        );
        assert_eq!(binding.resource_generations.len(), 4);

        for iteration in 2_u32..32 {
            let empty_anchor = MuxPaneAnchor {
                session_id: "$1".to_owned(),
                pane_id: None,
                terminal_id: None,
                pane_pid: None,
                cwd: None,
                process: None,
                occupant_id: None,
            };
            binding.controller.sessions[0].windows[0].anchor = empty_anchor;
            binding.controller.sessions[0].windows[0].panes.clear();
            binding.record_resource_snapshot();
            assert_eq!(binding.resource_generations.len(), 2);
            assert_eq!(binding.session_generation("$1"), Some(session_generation));
            assert_eq!(
                binding.window_generation("$1", "@1"),
                Some(window_generation)
            );

            let pane_id = format!("%{iteration}");
            let pane = MuxPaneAnchor {
                session_id: "$1".to_owned(),
                pane_id: Some(pane_id.clone()),
                terminal_id: Some(format!("t{iteration}")),
                pane_pid: Some(10 + iteration),
                cwd: None,
                process: Some(format!("shell-{iteration}")),
                occupant_id: None,
            };
            binding.controller.sessions[0].windows[0].anchor = pane.clone();
            binding.controller.sessions[0].windows[0].panes = vec![pane];
            binding.record_resource_snapshot();
            let generation = binding
                .pane_generation("$1", "@1", &pane_id)
                .expect("new pane generation");
            assert!(generation > replaced_generation);
            assert_eq!(binding.resource_generations.len(), 4);
            assert_eq!(binding.session_generation("$1"), Some(session_generation));
            assert_eq!(
                binding.window_generation("$1", "@1"),
                Some(window_generation)
            );
        }
    }

    #[test]
    fn unknown_target_state_replays_after_topology_refresh() {
        let scope = command_scope();
        let mut binding = BindingMuxController::new(scope);
        let occupant = MuxOccupantIdentity {
            backend_identity: "rmux:%1:generation:1".to_owned(),
            pid: Some(7),
            process: Some("shell".to_owned()),
        };
        let target = MuxEventTarget::pane("$new", "@1", "%1", "t1", Some(occupant.clone()));
        let topology = MuxEvent {
            backend_identity: "rmux:test".to_owned(),
            scope,
            revision: 1,
            cursor: None,
            topic: crate::backend::MuxEventTopic::TopologyChanged,
            provenance: crate::backend::MuxEventProvenance::RmuxSdk,
            target: None,
            payload: MuxEventPayload::Topology {
                change: crate::backend::MuxTopologyChange::Mutation,
            },
        };
        let state = MuxEvent {
            backend_identity: "rmux:test".to_owned(),
            scope,
            revision: 2,
            cursor: None,
            topic: crate::backend::MuxEventTopic::PaneStateChanged,
            provenance: crate::backend::MuxEventProvenance::RmuxSdk,
            target: Some(target.clone()),
            payload: MuxEventPayload::PaneState {
                state: crate::backend::MuxPaneState {
                    title: Some("ready".to_owned()),
                    options: vec![crate::backend::MuxPaneOption {
                        name: "mode".to_owned(),
                        value: "vi".to_owned(),
                    }],
                    foreground: None,
                },
            },
        };
        let observations = binding.observe_backend_events(vec![topology, state]);
        assert_eq!(observations.len(), 1);
        assert_eq!(
            observations[0].event.topic,
            crate::backend::MuxEventTopic::TopologyChanged
        );
        assert_eq!(binding.deferred_unknown_events.len(), 1);

        let pane = MuxPaneAnchor {
            session_id: "$new".to_owned(),
            pane_id: Some("%1".to_owned()),
            terminal_id: Some("t1".to_owned()),
            pane_pid: Some(7),
            cwd: None,
            process: Some("shell".to_owned()),
            occupant_id: Some(occupant.backend_identity),
        };
        let mut refreshed_window = window("@1", 0);
        refreshed_window.anchor = pane.clone();
        refreshed_window.panes = vec![pane];
        let mut refreshed_session = session("$new", "new");
        refreshed_session.active_window_id = Some("@1".to_owned());
        refreshed_session.windows = vec![refreshed_window];
        binding.controller.apply_refreshed_snapshot(
            MultiplexerBackendConfig::Rmux,
            snapshot_of(vec![refreshed_session]),
        );
        binding.record_resource_snapshot();
        binding.deferred_refresh_completed = true;

        let replayed = binding.observe_backend_events(Vec::new());
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].event.target, Some(target));
        match &replayed[0].event.payload {
            MuxEventPayload::PaneState { state } => {
                assert_eq!(state.title.as_deref(), Some("ready"));
                assert_eq!(state.options[0].name, "mode");
                assert_eq!(state.options[0].value, "vi");
            }
            payload => panic!("expected pane state replay, got {payload:?}"),
        }
        assert!(binding.deferred_unknown_events.is_empty());
    }

    #[test]
    fn authoritative_close_retires_session_and_window_across_limited_drains() {
        let scope = command_scope();
        let snapshot = targeted_snapshot();
        let mut binding = BindingMuxController::new(scope);
        binding.controller.sessions = snapshot.sessions.clone();
        binding.synchronize_resource_generations();

        let session_precondition = binding
            .controller
            .capture_execution_precondition(&MuxCommand::RenameSession {
                session_id: "$1".to_owned(),
                name: "replacement".to_owned(),
            })
            .expect("session precondition")
            .expect("session target");
        let window_precondition = binding
            .controller
            .capture_execution_precondition(&MuxCommand::RenameWindow {
                session_id: "$1".to_owned(),
                window_id: "@old".to_owned(),
                name: "replacement-window".to_owned(),
            })
            .expect("window precondition")
            .expect("window target");
        let pane_precondition = binding
            .controller
            .capture_execution_precondition(&MuxCommand::ResizePane {
                session_id: "$1".to_owned(),
                pane_id: Some("%old".to_owned()),
                adjustment: MuxPaneResize::Directional {
                    direction: MuxDirection::Right,
                    cells: 1,
                },
            })
            .expect("pane precondition")
            .expect("pane target");
        let pane_guard = binding
            .controller
            .execution_resource_generation_guard(Some(&pane_precondition))
            .expect("pane generation guard");
        let session_guard = binding
            .controller
            .execution_resource_generation_guard(Some(&session_precondition))
            .expect("session generation guard");
        let window_guard = binding
            .controller
            .execution_resource_generation_guard(Some(&window_precondition))
            .expect("window generation guard");

        struct CloseEventBackend {
            events: Vec<MuxEvent>,
        }

        impl MuxBackend for CloseEventBackend {
            fn snapshot(&self) -> anyhow::Result<MuxSnapshot> {
                Ok(MuxSnapshot::default())
            }

            fn execute(&mut self, _command: MuxCommand) -> anyhow::Result<()> {
                Ok(())
            }

            fn execute_checked(
                &mut self,
                scope: MuxScope,
                command: MuxCommand,
                _precondition: Option<&MuxScopedExecutionPrecondition>,
            ) -> BindingOperationOutcome<anyhow::Result<()>> {
                let descriptor = self.capabilities(scope);
                descriptor.invoke(
                    descriptor.request(command.operation()),
                    BindingOperationAvailability::Available,
                    || self.execute(command),
                )
            }

            fn drain_events(&mut self, _scope: MuxScope, maximum: usize) -> Vec<MuxEvent> {
                let count = self.events.len().min(maximum);
                self.events.drain(..count).collect()
            }
        }

        let close_event = |revision, target| MuxEvent {
            backend_identity: "tmux-control".to_owned(),
            scope,
            revision,
            cursor: None,
            topic: crate::backend::MuxEventTopic::PaneClosed,
            provenance: crate::backend::MuxEventProvenance::TmuxSnapshotFallback,
            target: Some(target),
            payload: MuxEventPayload::Closed {
                reason: "authoritative inventory close".to_owned(),
            },
        };
        let old_window = &snapshot.sessions[0].windows[0];
        let new_window = &snapshot.sessions[0].windows[1];
        let config = MultiplexerConfig {
            backend: MultiplexerBackendConfig::Tmux,
            ..Default::default()
        };
        binding.event_backend = Some((
            config.clone(),
            Box::new(CloseEventBackend {
                events: vec![
                    close_event(
                        1,
                        pane_event_target("$1", &old_window.id, &old_window.anchor),
                    ),
                    close_event(
                        2,
                        pane_event_target("$1", &new_window.id, &new_window.anchor),
                    ),
                ],
            }),
        ));

        let first = binding.drain_events(&config, 1);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].retired_target_generation, Some(1));
        assert_eq!(
            first[0]
                .event
                .target
                .as_ref()
                .and_then(|target| target.terminal_id.as_deref()),
            Some("t-old")
        );
        assert!(
            first[0]
                .event
                .target
                .as_ref()
                .and_then(|target| target.occupant.as_ref())
                .is_some()
        );
        assert_eq!(binding.session_generation("$1"), Some(1));
        assert_eq!(binding.window_generation("$1", "@old"), None);
        assert_eq!(binding.authoritative_closed_targets.len(), 1);
        assert_eq!(binding.pane_generation("$1", "@old", "%old"), None);
        assert_eq!(binding.terminal_generation("$1", "@old", "t-old"), None);
        assert!(!pane_guard.is_current());
        assert!(session_guard.is_current());
        assert!(!window_guard.is_current());

        let second = binding.drain_events(&config, 1);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].retired_target_generation, Some(1));
        assert_eq!(binding.session_generation("$1"), None);
        assert_eq!(binding.window_generation("$1", "@old"), None);
        assert_eq!(binding.authoritative_closed_targets.len(), 2);
        assert!(!session_guard.is_current());
        assert!(!window_guard.is_current());

        let mut replacement = snapshot;
        replacement.sessions[0].name = "replacement".to_owned();
        binding.controller.sessions = replacement.sessions;
        binding.synchronize_resource_generations();
        assert!(binding.authoritative_closed_targets.is_empty());
        assert!(!session_guard.is_current());
        assert!(!window_guard.is_current());
    }

    #[test]
    fn topology_invalidation_does_not_retire_session_or_window() {
        let scope = command_scope();
        let snapshot = targeted_snapshot();
        let mut binding = BindingMuxController::new(scope);
        binding.controller.sessions = snapshot.sessions;
        binding.synchronize_resource_generations();
        let precondition = binding
            .controller
            .capture_execution_precondition(&MuxCommand::RenameWindow {
                session_id: "$1".to_owned(),
                window_id: "@old".to_owned(),
                name: "renamed".to_owned(),
            })
            .expect("window precondition")
            .expect("window target");
        let guard = binding
            .controller
            .execution_resource_generation_guard(Some(&precondition))
            .expect("window generation guard");

        binding.observe_backend_events(vec![MuxEvent {
            backend_identity: "tmux-control".to_owned(),
            scope,
            revision: 1,
            cursor: None,
            topic: crate::backend::MuxEventTopic::TopologyChanged,
            provenance: crate::backend::MuxEventProvenance::TmuxControl,
            target: Some(MuxEventTarget {
                session_id: Some("$1".to_owned()),
                window_id: Some("@old".to_owned()),
                ..Default::default()
            }),
            payload: MuxEventPayload::Topology {
                change: crate::backend::MuxTopologyChange::Invalidated,
            },
        }]);

        assert!(guard.is_current());
        assert_eq!(binding.window_generation("$1", "@old"), Some(1));
    }

    #[test]
    fn authoritative_occupant_identity_advances_generation_when_pid_and_process_are_reused() {
        let mut binding = BindingMuxController::default();
        let initial = MuxEventTarget::pane(
            "$1",
            "@1",
            "%1",
            "terminal-1",
            Some(crate::backend::MuxOccupantIdentity {
                backend_identity: "daemon-generation-7".to_owned(),
                pid: Some(10),
                process: Some("zsh".to_owned()),
            }),
        );
        binding.record_authoritative_occupant(&initial);
        assert_eq!(binding.pane_generation("$1", "@1", "%1"), Some(1));
        assert_eq!(
            binding.terminal_generation("$1", "@1", "terminal-1"),
            Some(1)
        );

        let replacement = MuxEventTarget::pane(
            "$1",
            "@1",
            "%1",
            "terminal-1",
            Some(crate::backend::MuxOccupantIdentity {
                backend_identity: "daemon-generation-8".to_owned(),
                pid: Some(10),
                process: Some("zsh".to_owned()),
            }),
        );
        binding.record_authoritative_occupant(&replacement);

        assert_eq!(binding.pane_generation("$1", "@1", "%1"), Some(2));
        assert_eq!(
            binding.terminal_generation("$1", "@1", "terminal-1"),
            Some(2)
        );
    }

    #[test]
    fn batched_occupant_replacements_keep_each_events_generation() {
        let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(1));
        let mut binding = BindingMuxController::new(scope);
        binding.controller.sessions = vec![session("$1", "work")];
        let occupant = |identity: &str| MuxOccupantIdentity {
            backend_identity: identity.to_owned(),
            pid: Some(10),
            process: Some("zsh".to_owned()),
        };
        let replacement = |revision, identity: &str| MuxEvent {
            backend_identity: "test".to_owned(),
            scope,
            revision,
            cursor: None,
            topic: crate::backend::MuxEventTopic::PaneOccupantReplaced,
            provenance: crate::backend::MuxEventProvenance::Queue,
            target: Some(MuxEventTarget::pane(
                "$1",
                "@1",
                "%1",
                "t1",
                Some(occupant(identity)),
            )),
            payload: MuxEventPayload::OccupantReplaced {
                old_occupant: None,
                new_occupant: Some(occupant(identity)),
            },
        };

        let observations = binding.observe_backend_events(vec![
            replacement(1, "occupant-1"),
            replacement(2, "occupant-2"),
        ]);

        assert_eq!(
            observations
                .iter()
                .map(|observation| (
                    observation.target_generation,
                    observation.retired_target_generation,
                ))
                .collect::<Vec<_>>(),
            vec![(Some(1), None), (Some(2), Some(1))]
        );
    }

    #[test]
    fn stale_occupant_output_after_replacement_rebases_within_one_drained_batch() {
        struct EventBackend {
            events: Vec<MuxEvent>,
        }

        impl MuxBackend for EventBackend {
            fn snapshot(&self) -> anyhow::Result<MuxSnapshot> {
                Ok(MuxSnapshot::default())
            }

            fn execute(&mut self, _command: MuxCommand) -> anyhow::Result<()> {
                Ok(())
            }

            fn execute_checked(
                &mut self,
                scope: MuxScope,
                command: MuxCommand,
                _precondition: Option<&MuxScopedExecutionPrecondition>,
            ) -> BindingOperationOutcome<anyhow::Result<()>> {
                let descriptor = self.capabilities(scope);
                descriptor.invoke(
                    descriptor.request(command.operation()),
                    BindingOperationAvailability::Available,
                    || self.execute(command),
                )
            }

            fn drain_events(&mut self, _scope: MuxScope, maximum: usize) -> Vec<MuxEvent> {
                let count = self.events.len().min(maximum);
                self.events.drain(..count).collect()
            }
        }

        let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(1));
        let occupant = |identity: &str| MuxOccupantIdentity {
            backend_identity: identity.to_owned(),
            pid: Some(10),
            process: Some("zsh".to_owned()),
        };
        let target = |identity: &str| {
            MuxEventTarget::pane("$1", "@1", "%1", "terminal-1", Some(occupant(identity)))
        };
        let output = |revision, identity: &str| MuxEvent {
            backend_identity: "test".to_owned(),
            scope,
            revision,
            cursor: None,
            topic: crate::backend::MuxEventTopic::TerminalOutput,
            provenance: crate::backend::MuxEventProvenance::Queue,
            target: Some(target(identity)),
            payload: MuxEventPayload::Output {
                bytes: b"output".to_vec(),
            },
        };
        let replacement = MuxEvent {
            backend_identity: "test".to_owned(),
            scope,
            revision: 2,
            cursor: None,
            topic: crate::backend::MuxEventTopic::PaneOccupantReplaced,
            provenance: crate::backend::MuxEventProvenance::Queue,
            target: Some(target("occupant-new")),
            payload: MuxEventPayload::OccupantReplaced {
                old_occupant: Some(occupant("occupant-old")),
                new_occupant: Some(occupant("occupant-new")),
            },
        };
        let config = MultiplexerConfig {
            backend: MultiplexerBackendConfig::Tmux,
            ..Default::default()
        };
        let mut binding = BindingMuxController::new(scope);
        binding.controller.sessions = vec![session("$1", "work")];
        binding.event_backend = Some((
            config.clone(),
            Box::new(EventBackend {
                events: vec![
                    output(1, "occupant-old"),
                    replacement,
                    output(3, "occupant-old"),
                ],
            }),
        ));

        let initial = binding.drain_events(&config, 1);

        assert_eq!(initial.len(), 1);
        assert_eq!(
            initial[0].event.topic,
            crate::backend::MuxEventTopic::TerminalOutput
        );
        assert_eq!(initial[0].target_generation, Some(1));
        assert_eq!(binding.pane_generation("$1", "@1", "%1"), Some(1));
        assert_eq!(
            binding.terminal_generation("$1", "@1", "terminal-1"),
            Some(1)
        );

        binding.controller.current_backend = Some(MultiplexerBackendConfig::Tmux);
        let observations = binding.drain_events(&config, 2);

        assert_eq!(observations.len(), 2);
        assert_eq!(
            observations[0].event.topic,
            crate::backend::MuxEventTopic::PaneOccupantReplaced
        );
        assert_eq!(observations[0].target_generation, Some(2));
        assert_eq!(observations[0].retired_target_generation, Some(1));
        assert_eq!(
            observations[1].event.topic,
            crate::backend::MuxEventTopic::SnapshotRebased
        );
        assert_eq!(
            observations[1].event.payload,
            MuxEventPayload::Rebase {
                reason: MuxRebaseReason::SequenceGap,
            }
        );
        assert!(observations[1].event.target.is_none());
        assert_eq!(observations[1].event.revision, 3);
        assert_eq!(observations[1].target_generation, None);
        assert_eq!(observations[1].retired_target_generation, None);
        assert_eq!(binding.pane_generation("$1", "@1", "%1"), Some(2));
        assert_eq!(
            binding.terminal_generation("$1", "@1", "terminal-1"),
            Some(2)
        );
        assert_eq!(binding.controller.current_backend, None);
    }

    #[test]
    fn reconnect_rebase_clears_before_later_pane_observation() {
        let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(1));
        let mut binding = BindingMuxController::new(scope);
        binding.controller.sessions = vec![session("$1", "work")];
        let initial_binding_generation = binding.binding_generation();
        let reconnect = MuxEvent {
            backend_identity: "test".to_owned(),
            scope,
            revision: 1,
            cursor: None,
            topic: crate::backend::MuxEventTopic::SnapshotRebased,
            provenance: crate::backend::MuxEventProvenance::Queue,
            target: None,
            payload: MuxEventPayload::Rebase {
                reason: MuxRebaseReason::Reconnect,
            },
        };
        let pane = MuxEvent {
            backend_identity: "test".to_owned(),
            scope,
            revision: 2,
            cursor: None,
            topic: crate::backend::MuxEventTopic::PaneOccupantReplaced,
            provenance: crate::backend::MuxEventProvenance::Queue,
            target: Some(MuxEventTarget::pane(
                "$1",
                "@1",
                "%1",
                "t1",
                Some(MuxOccupantIdentity {
                    backend_identity: "fresh-occupant".to_owned(),
                    pid: Some(10),
                    process: Some("zsh".to_owned()),
                }),
            )),
            payload: MuxEventPayload::OccupantReplaced {
                old_occupant: None,
                new_occupant: None,
            },
        };

        let observations = binding.observe_backend_events(vec![reconnect, pane]);

        assert_eq!(
            observations[0].binding_generation,
            initial_binding_generation + 1
        );
        assert_eq!(
            observations[1].binding_generation,
            observations[0].binding_generation
        );
        assert_eq!(observations[1].target_generation, Some(1));
    }

    #[test]
    fn binding_event_observation_routes_targeted_drafts_to_their_member() {
        let first_scope =
            MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(1));
        let second_scope =
            MuxScope::new(SpaceId::from_persistence(2), BindingId::from_persistence(2));
        let event = |scope, target| MuxEvent {
            backend_identity: "shared-backend".to_owned(),
            scope,
            revision: 1,
            cursor: None,
            topic: crate::backend::MuxEventTopic::BackendDisconnected,
            provenance: crate::backend::MuxEventProvenance::Queue,
            target,
            payload: MuxEventPayload::Disconnected {
                reason: "test".to_owned(),
            },
        };
        let mut first = BindingMuxController::new(first_scope);
        first.controller.sessions = vec![session("$1", "first")];
        let mut second = BindingMuxController::new(second_scope);
        second.controller.sessions = vec![session("$2", "second")];

        let first_events = first.observe_backend_events(vec![
            event(first_scope, Some(MuxEventTarget::session("$2"))),
            event(first_scope, None),
        ]);
        let second_events = second.observe_backend_events(vec![
            event(second_scope, Some(MuxEventTarget::session("$2"))),
            event(second_scope, None),
        ]);

        assert_eq!(first_events.len(), 1);
        assert!(first_events[0].event.target.is_none());
        assert_eq!(second_events.len(), 2);
        assert_eq!(
            second_events[0]
                .event
                .target
                .as_ref()
                .and_then(|target| target.session_id.as_deref()),
            Some("$2")
        );
    }

    #[test]
    fn backend_event_session_id_does_not_match_local_session_name() {
        let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(1));
        let mut binding = BindingMuxController::new(scope);
        binding.controller.sessions = vec![session("local-backend-id", "foreign-backend-id")];

        let observations = binding.observe_backend_events(vec![MuxEvent {
            backend_identity: "test".to_owned(),
            scope,
            revision: 1,
            cursor: None,
            topic: crate::backend::MuxEventTopic::BackendDisconnected,
            provenance: crate::backend::MuxEventProvenance::Queue,
            target: Some(MuxEventTarget::session("foreign-backend-id")),
            payload: MuxEventPayload::Disconnected {
                reason: "foreign backend".to_owned(),
            },
        }]);

        assert!(observations.is_empty());
    }

    #[test]
    fn topology_event_for_external_session_is_observed_before_snapshot_refresh() {
        let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(1));
        let mut binding = BindingMuxController::new(scope);
        binding.controller.sessions = vec![session("$local", "local")];
        binding.controller.current_backend = Some(MultiplexerBackendConfig::Tmux);

        let observations = binding.observe_backend_events(vec![MuxEvent {
            backend_identity: "test".to_owned(),
            scope,
            revision: 1,
            cursor: None,
            topic: crate::backend::MuxEventTopic::TopologyChanged,
            provenance: crate::backend::MuxEventProvenance::Queue,
            target: Some(MuxEventTarget::session("$external")),
            payload: MuxEventPayload::Topology {
                change: crate::backend::MuxTopologyChange::Mutation,
            },
        }]);

        assert_eq!(observations.len(), 1);
        assert_eq!(
            observations[0]
                .event
                .target
                .as_ref()
                .and_then(|target| target.session_id.as_deref()),
            Some("$external")
        );
        assert_eq!(binding.controller.current_backend, None);
    }

    #[test]
    fn tombstoned_pane_closed_event_retains_its_retired_generation() {
        let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(1));
        let mut binding = BindingMuxController::new(scope);
        binding.controller.sessions = targeted_snapshot().sessions;
        binding.record_resource_snapshot();
        assert_eq!(binding.terminal_generation("$1", "@old", "t-old"), Some(1));

        binding.controller.sessions.clear();
        binding.record_resource_snapshot();
        assert_eq!(binding.terminal_generation("$1", "@old", "t-old"), None);

        let wrong_terminal = binding.observe_backend_events(vec![MuxEvent {
            backend_identity: "test".to_owned(),
            scope,
            revision: 1,
            cursor: None,
            topic: crate::backend::MuxEventTopic::PaneClosed,
            provenance: crate::backend::MuxEventProvenance::Queue,
            target: Some(MuxEventTarget::pane(
                "$1",
                "@old",
                "%old",
                "t-wrong",
                Some(MuxOccupantIdentity {
                    backend_identity: "occupant-old".to_owned(),
                    pid: None,
                    process: Some("shell".to_owned()),
                }),
            )),
            payload: MuxEventPayload::Closed {
                reason: "wrong terminal".to_owned(),
            },
        }]);
        assert!(wrong_terminal.is_empty());

        let observations = binding.observe_backend_events(vec![MuxEvent {
            backend_identity: "test".to_owned(),
            scope,
            revision: 1,
            cursor: None,
            topic: crate::backend::MuxEventTopic::PaneClosed,
            provenance: crate::backend::MuxEventProvenance::Queue,
            target: Some(MuxEventTarget::pane(
                "$1",
                "@old",
                "%old",
                "t-old",
                Some(MuxOccupantIdentity {
                    backend_identity: "occupant-old".to_owned(),
                    pid: None,
                    process: Some("shell".to_owned()),
                }),
            )),
            payload: MuxEventPayload::Closed {
                reason: "external close".to_owned(),
            },
        }]);

        assert_eq!(observations.len(), 1);
        assert_eq!(
            observations[0].event.topic,
            crate::backend::MuxEventTopic::PaneClosed
        );
        assert_eq!(observations[0].retired_target_generation, Some(1));
        assert_eq!(binding.pane_generation("$1", "@old", "%old"), None);
        assert_eq!(binding.terminal_generation("$1", "@old", "t-old"), None);
    }

    #[test]
    fn tombstoned_pane_replacement_advances_its_generation() {
        let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(1));
        let mut binding = BindingMuxController::new(scope);
        binding.controller.sessions = targeted_snapshot().sessions;
        binding.record_resource_snapshot();
        binding.controller.sessions.clear();
        binding.record_resource_snapshot();

        let observations = binding.observe_backend_events(vec![MuxEvent {
            backend_identity: "test".to_owned(),
            scope,
            revision: 1,
            cursor: None,
            topic: crate::backend::MuxEventTopic::PaneOccupantReplaced,
            provenance: crate::backend::MuxEventProvenance::Queue,
            target: Some(MuxEventTarget::pane(
                "$1",
                "@old",
                "%old",
                "t-old",
                Some(MuxOccupantIdentity {
                    backend_identity: "occupant-new".to_owned(),
                    pid: None,
                    process: Some("shell".to_owned()),
                }),
            )),
            payload: MuxEventPayload::OccupantReplaced {
                old_occupant: Some(MuxOccupantIdentity {
                    backend_identity: "occupant-old".to_owned(),
                    pid: None,
                    process: Some("shell".to_owned()),
                }),
                new_occupant: Some(MuxOccupantIdentity {
                    backend_identity: "occupant-new".to_owned(),
                    pid: None,
                    process: Some("shell".to_owned()),
                }),
            },
        }]);

        assert_eq!(observations.len(), 1);
        assert_eq!(
            observations[0].event.topic,
            crate::backend::MuxEventTopic::PaneOccupantReplaced
        );
        assert_eq!(observations[0].retired_target_generation, Some(1));
        assert_eq!(observations[0].target_generation, Some(2));
        assert_eq!(binding.pane_generation("$1", "@old", "%old"), Some(2));
        assert_eq!(binding.terminal_generation("$1", "@old", "t-old"), Some(2));
    }

    #[test]
    fn event_backend_rebuilds_when_remote_server_identity_changes() {
        use bootty_config::config::SshRemoteConfig;

        #[derive(Clone)]
        struct EventBackend {
            starts: Arc<std::sync::atomic::AtomicUsize>,
        }

        impl MuxBackend for EventBackend {
            fn snapshot(&self) -> anyhow::Result<MuxSnapshot> {
                Ok(MuxSnapshot::default())
            }

            fn execute(&mut self, _command: MuxCommand) -> anyhow::Result<()> {
                Ok(())
            }

            fn execute_checked(
                &mut self,
                scope: MuxScope,
                command: MuxCommand,
                _precondition: Option<&MuxScopedExecutionPrecondition>,
            ) -> BindingOperationOutcome<anyhow::Result<()>> {
                let descriptor = self.capabilities(scope);
                descriptor.invoke(
                    descriptor.request(command.operation()),
                    BindingOperationAvailability::Available,
                    || self.execute(command),
                )
            }

            fn start_event_stream(&mut self) {
                self.starts.fetch_add(1, Ordering::AcqRel);
            }
        }

        let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let backend = EventBackend {
            starts: Arc::clone(&starts),
        };
        let mut binding = BindingMuxController::new(MuxScope::new(
            SpaceId::from_persistence(1),
            BindingId::from_persistence(1),
        ));
        binding
            .controller
            .set_backend_factory(Arc::new(move |_| Box::new(backend.clone())));
        let first = MultiplexerConfig {
            backend: MultiplexerBackendConfig::Tmux,
            remote: Some(SshRemoteConfig {
                host: "first.example".to_owned(),
                user: None,
                port: None,
                program: "ssh".to_owned(),
                args: Vec::new(),
            }),
            ..Default::default()
        };
        let replacement = MultiplexerConfig {
            remote: Some(SshRemoteConfig {
                host: "second.example".to_owned(),
                user: None,
                port: None,
                program: "ssh".to_owned(),
                args: Vec::new(),
            }),
            ..first.clone()
        };

        binding.event_capabilities(&first);
        let initial_generation = binding.binding_generation();
        binding.controller.sessions = vec![session("$1", "stale")];
        binding.event_capabilities(&replacement);

        assert_eq!(starts.load(Ordering::Acquire), 2);
        assert_eq!(binding.binding_generation(), initial_generation + 1);
        assert!(
            binding.sessions().is_empty(),
            "a replacement backend must not inherit the prior backend snapshot"
        );
        assert!(binding.controller.last_session_refresh.is_none());
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
    fn failed_binding_reports_operations_as_unavailable() {
        let mut binding = BindingMuxController::new(MuxScope::new(
            SpaceId::from_persistence(1),
            BindingId::from_persistence(1),
        ));
        binding.set_availability_error(Some("SSH profile missing".to_owned()));

        assert_eq!(
            binding.operation_outcome(
                &MultiplexerConfig::default(),
                BindingOperation::CreateWindow,
            ),
            BindingOperationOutcome::Unavailable
        );
    }

    #[test]
    fn rmux_checked_mutations_are_advertised_unavailable() {
        let binding = BindingMuxController::new(MuxScope::new(
            SpaceId::from_persistence(1),
            BindingId::from_persistence(1),
        ));
        let config = MultiplexerConfig {
            backend: MultiplexerBackendConfig::Rmux,
            ..Default::default()
        };

        for operation in [
            BindingOperation::CreateWindow,
            BindingOperation::NavigateWindow,
            BindingOperation::MoveWindow,
            BindingOperation::SplitPane,
            BindingOperation::NavigatePane,
            BindingOperation::LastPane,
            BindingOperation::ResizePane,
            BindingOperation::ClosePane,
            BindingOperation::TogglePaneZoom,
            BindingOperation::RenameSession,
            BindingOperation::DitchSession,
        ] {
            assert_eq!(
                binding.operation_outcome(&config, operation),
                BindingOperationOutcome::Unavailable,
                "{operation:?}"
            );
        }
        assert_eq!(
            binding.operation_outcome(&config, BindingOperation::CreateProjectSession),
            BindingOperationOutcome::Supported(())
        );
    }

    #[test]
    fn prior_command_errors_do_not_make_the_binding_unavailable() {
        let mut binding = BindingMuxController::new(MuxScope::new(
            SpaceId::from_persistence(1),
            BindingId::from_persistence(1),
        ));
        binding.set_error(Some("previous command failed".to_owned()));

        assert_ne!(
            binding.operation_outcome(
                &MultiplexerConfig::default(),
                BindingOperation::CreateWindow,
            ),
            BindingOperationOutcome::Unavailable
        );
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
    fn forced_refresh_invalidates_an_in_flight_snapshot() {
        let mut binding = BindingMuxController::default();
        binding.controller.session_refresh_generation = 7;
        binding.controller.session_refresh_pending = true;
        binding.controller.last_session_refresh = Some(Instant::now());

        binding.refresh_on_next_frame();

        assert_eq!(binding.controller.session_refresh_generation, 8);
        assert!(!binding.controller.session_refresh_pending);
        assert!(binding.controller.last_session_refresh.is_none());
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

    #[test]
    fn authoritative_allocations_must_match_observed_window_and_pane_identities() {
        let plan = MuxSessionLaunchPlan {
            session_id: "requested".to_owned(),
            focus: true,
            default_cwd: "/repo".to_owned(),
            environment: std::collections::BTreeMap::new(),
            windows: vec![crate::command::MuxWindowLaunchPlan {
                name: None,
                focus: true,
                layout: MuxPaneLaunchPlan::Pane(crate::command::MuxPaneLaunch {
                    cwd: "/repo".to_owned(),
                    command: None,
                    argv: None,
                    environment: std::collections::BTreeMap::new(),
                    title: None,
                }),
            }],
            focused_window: 0,
        };
        let pane = MuxPaneAnchor {
            session_id: "$1".to_owned(),
            pane_id: Some("%1".to_owned()),
            terminal_id: Some("t1".to_owned()),
            cwd: Some("/repo".to_owned()),
            ..Default::default()
        };
        let snapshot = MuxSnapshot {
            sessions: vec![MuxSession {
                id: "$1".to_owned(),
                name: "requested".to_owned(),
                active: true,
                anchor: pane.clone(),
                active_window_id: Some("@1".to_owned()),
                windows: vec![MuxWindow {
                    id: "@1".to_owned(),
                    index: 0,
                    name: "window".to_owned(),
                    active: true,
                    anchor: pane.clone(),
                    panes: vec![pane],
                    layout: None,
                    progress: None,
                }],
            }],
            active_session_id: Some("$1".to_owned()),
        };
        let allocation = MuxAllocatedResources {
            session_id: "$1".to_owned(),
            windows: vec![MuxAllocatedWindow {
                window_id: "@1".to_owned(),
                pane_ids: vec!["%1".to_owned()],
            }],
        };

        assert!(validate_allocated_resources(&plan, &snapshot, allocation.clone()).is_ok());

        let mut wrong_window = allocation.clone();
        wrong_window.windows[0].window_id = "@wrong".to_owned();
        assert!(matches!(
            validate_allocated_resources(&plan, &snapshot, wrong_window),
            Err(MuxCommandError::Failed(message)) if message.contains("declaration index")
        ));

        let mut wrong_count = allocation.clone();
        wrong_count.windows[0].pane_ids.clear();
        assert!(matches!(
            validate_allocated_resources(&plan, &snapshot, wrong_count),
            Err(MuxCommandError::Failed(message)) if message.contains("allocated 0 panes")
        ));

        let mut wrong_pane = allocation;
        wrong_pane.windows[0].pane_ids = vec!["%wrong".to_owned()];
        assert!(matches!(
            validate_allocated_resources(&plan, &snapshot, wrong_pane),
            Err(MuxCommandError::Failed(message)) if message.contains("DFS declaration order")
        ));
    }

    #[test]
    fn authoritative_recursive_allocation_survives_a_flat_tmux_snapshot() {
        let pane = || crate::command::MuxPaneLaunch {
            cwd: "/repo".to_owned(),
            command: None,
            argv: None,
            environment: std::collections::BTreeMap::new(),
            title: None,
        };
        let plan = MuxSessionLaunchPlan {
            session_id: "requested".to_owned(),
            focus: true,
            default_cwd: "/repo".to_owned(),
            environment: std::collections::BTreeMap::new(),
            windows: vec![crate::command::MuxWindowLaunchPlan {
                name: None,
                focus: true,
                layout: MuxPaneLaunchPlan::Split(crate::command::MuxSplitLaunch {
                    direction: crate::command::MuxSplitDirection::Right,
                    ratio_millis: 600,
                    first: Box::new(MuxPaneLaunchPlan::Pane(pane())),
                    second: Box::new(MuxPaneLaunchPlan::Pane(pane())),
                }),
            }],
            focused_window: 0,
        };
        let active_pane = MuxPaneAnchor {
            session_id: "$1".to_owned(),
            pane_id: Some("%2".to_owned()),
            terminal_id: Some("t2".to_owned()),
            cwd: Some("/repo".to_owned()),
            ..Default::default()
        };
        let snapshot = MuxSnapshot {
            sessions: vec![MuxSession {
                id: "$1".to_owned(),
                name: "requested".to_owned(),
                active: true,
                anchor: active_pane.clone(),
                active_window_id: Some("@1".to_owned()),
                windows: vec![MuxWindow {
                    id: "@1".to_owned(),
                    index: 0,
                    name: "window".to_owned(),
                    active: true,
                    anchor: active_pane.clone(),
                    // tmux deliberately exposes one attach anchor rather than a durable split
                    // tree, even though the transaction created both panes.
                    panes: vec![active_pane],
                    layout: None,
                    progress: None,
                }],
            }],
            active_session_id: Some("$1".to_owned()),
        };
        let allocation = MuxAllocatedResources {
            session_id: "$1".to_owned(),
            windows: vec![MuxAllocatedWindow {
                window_id: "@1".to_owned(),
                pane_ids: vec!["%1".to_owned(), "%2".to_owned()],
            }],
        };
        let command = MuxCommand::CreateSession { plan: plan.clone() };

        let completion = MuxCommandCompletion::from_command_snapshot(
            MultiplexerConfig::default(),
            snapshot.clone(),
            &command,
            None,
            Some(MuxBackendCommandCompletion {
                allocated: Some(allocation.clone()),
                target: None,
            }),
        )
        .expect("authoritative recursive refs must not require a snapshot layout");
        assert_eq!(completion.allocated(), Some(&allocation));

        let mut binding = BindingMuxController::default();
        let completed = binding
            .complete_authoritative_command(Ok(completion.clone()), &MultiplexerConfig::default())
            .expect("complete authoritative allocation");
        assert_eq!(completed.allocated(), Some(&allocation));
        assert_eq!(binding.session_generation("$1"), Some(1));
        assert_eq!(binding.window_generation("$1", "@1"), Some(1));
        assert_eq!(binding.pane_generation("$1", "@1", "%1"), Some(1));
        assert_eq!(binding.pane_generation("$1", "@1", "%2"), Some(1));
        assert_eq!(binding.terminal_generation("$1", "@1", "%1"), None);
        assert_eq!(binding.terminal_generation("$1", "@1", "t2"), Some(1));

        binding
            .controller
            .apply_refreshed_snapshot(MultiplexerBackendConfig::Tmux, snapshot.clone());
        binding.record_resource_snapshot();
        assert_eq!(binding.pane_generation("$1", "@1", "%1"), Some(1));
        assert_eq!(binding.pane_generation("$1", "@1", "%2"), Some(1));

        let mut complete_snapshot = snapshot.clone();
        complete_snapshot.sessions[0].windows[0].layout =
            Some(MuxPaneLayout::Pane("%2".to_owned()));
        binding
            .controller
            .apply_refreshed_snapshot(MultiplexerBackendConfig::Tmux, complete_snapshot);
        binding.record_resource_snapshot();
        assert_eq!(binding.pane_generation("$1", "@1", "%1"), None);
        assert_eq!(binding.pane_generation("$1", "@1", "%2"), Some(1));

        let mut wrong_count = allocation;
        wrong_count.windows[0].pane_ids.pop();
        assert!(matches!(
            MuxCommandCompletion::from_command_snapshot(
                MultiplexerConfig::default(),
                snapshot,
                &command,
                None,
                Some(MuxBackendCommandCompletion {
                    allocated: Some(wrong_count),
                    target: None,
                }),
            ),
            Err(MuxCommandError::Failed(message)) if message.contains("allocated 1 panes")
        ));
    }

    #[test]
    fn authoritative_simple_session_allocations_are_retained_for_project_and_worktree_launches() {
        let pane = MuxPaneAnchor {
            session_id: "$project".to_owned(),
            pane_id: Some("%pane".to_owned()),
            terminal_id: Some("terminal-pane".to_owned()),
            cwd: Some("/repo".to_owned()),
            ..Default::default()
        };
        let snapshot = MuxSnapshot {
            sessions: vec![MuxSession {
                id: "$project".to_owned(),
                name: "project".to_owned(),
                active: true,
                anchor: pane.clone(),
                active_window_id: Some("@window".to_owned()),
                windows: vec![MuxWindow {
                    id: "@window".to_owned(),
                    index: 0,
                    name: "window".to_owned(),
                    active: true,
                    anchor: pane.clone(),
                    panes: vec![pane],
                    layout: None,
                    progress: None,
                }],
            }],
            active_session_id: Some("$project".to_owned()),
        };
        let allocation = MuxAllocatedResources {
            session_id: "$project".to_owned(),
            windows: vec![MuxAllocatedWindow {
                window_id: "@window".to_owned(),
                pane_ids: vec!["%pane".to_owned()],
            }],
        };

        for command in [
            MuxCommand::CreateProjectSession {
                session_id: "project".to_owned(),
                cwd: "/repo".to_owned(),
            },
            MuxCommand::CreateWorktreeSession {
                session_id: "project".to_owned(),
                cwd: "/repo".to_owned(),
            },
        ] {
            let completion = MuxCommandCompletion::from_command_snapshot(
                MultiplexerConfig::default(),
                snapshot.clone(),
                &command,
                None,
                Some(MuxBackendCommandCompletion {
                    allocated: Some(allocation.clone()),
                    target: None,
                }),
            )
            .expect("authoritative simple allocation");
            assert_eq!(completion.allocated(), Some(&allocation));
        }
    }

    #[test]
    fn snapshot_occupant_id_advances_generation_when_pid_and_process_are_reused() {
        let mut snapshot = targeted_snapshot();
        let pane = &mut snapshot.sessions[0].windows[0].anchor;
        pane.occupant_id = Some("snapshot-occupant-1".to_owned());
        pane.pane_pid = Some(10);
        pane.process = Some("zsh".to_owned());
        let mut binding = BindingMuxController::default();
        binding.controller.sessions = snapshot.sessions;
        binding.synchronize_resource_generations();

        binding.controller.sessions[0].windows[0].anchor.occupant_id =
            Some("snapshot-occupant-2".to_owned());
        binding.synchronize_resource_generations();

        assert_eq!(binding.pane_generation("$1", "@old", "%old"), Some(2));
        assert_eq!(binding.terminal_generation("$1", "@old", "t-old"), Some(2));
    }

    #[test]
    fn queued_pane_mutation_stales_on_authoritative_replacement_with_reused_snapshot_fingerprint() {
        let scope = command_scope();
        let mut snapshot = targeted_snapshot();
        let pane = &mut snapshot.sessions[0].windows[0].anchor;
        pane.occupant_id = None;
        pane.pane_pid = Some(10);
        pane.process = Some("zsh".to_owned());
        let state = Arc::new(Mutex::new(BlockingRecordingState {
            snapshot: snapshot.clone(),
            commands: Vec::new(),
        }));
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let backend = BlockingRecordingBackend {
            state: Arc::clone(&state),
            started: started_tx,
            release: Arc::new(Mutex::new(release_rx)),
        };
        let mut binding = BindingMuxController::new(scope);
        binding.controller.sessions = snapshot.sessions.clone();
        binding
            .controller
            .set_backend_factory(Arc::new(move |_| Box::new(backend.clone())));
        binding.synchronize_resource_generations();

        let original = MuxOccupantIdentity {
            backend_identity: "tmux-occupant-1".to_owned(),
            pid: Some(10),
            process: Some("zsh".to_owned()),
        };
        let replacement = MuxOccupantIdentity {
            backend_identity: "tmux-occupant-2".to_owned(),
            pid: Some(10),
            process: Some("zsh".to_owned()),
        };
        let occupant_event =
            |revision: u64,
             old_occupant: Option<MuxOccupantIdentity>,
             occupant: MuxOccupantIdentity| MuxEvent {
                backend_identity: "tmux-control".to_owned(),
                scope,
                revision,
                cursor: None,
                topic: crate::backend::MuxEventTopic::PaneOccupantReplaced,
                provenance: crate::backend::MuxEventProvenance::TmuxControl,
                target: Some(crate::backend::MuxEventTarget::pane(
                    "$1",
                    "@old",
                    "%old",
                    "t-old",
                    Some(occupant.clone()),
                )),
                payload: MuxEventPayload::OccupantReplaced {
                    old_occupant,
                    new_occupant: Some(occupant),
                },
            };
        binding.observe_backend_events(vec![occupant_event(1, None, original.clone())]);
        assert_eq!(binding.pane_generation("$1", "@old", "%old"), Some(1));

        let repaint: RepaintHandle = Arc::new(|| {});
        let config = MultiplexerConfig {
            backend: MultiplexerBackendConfig::Tmux,
            ..Default::default()
        };
        let blocker = binding.execute_command_authoritatively(
            &repaint,
            &config,
            MuxCommand::RenameSession {
                session_id: "$1".to_owned(),
                name: "blocked".to_owned(),
            },
            Instant::now() + Duration::from_secs(5),
            CommandCancellation::new(),
        );
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("blocker started");
        let queued = binding.execute_command_authoritatively(
            &repaint,
            &config,
            MuxCommand::ResizePane {
                session_id: "$1".to_owned(),
                pane_id: Some("%old".to_owned()),
                adjustment: MuxPaneResize::Directional {
                    direction: MuxDirection::Right,
                    cells: 1,
                },
            },
            Instant::now() + Duration::from_secs(5),
            CommandCancellation::new(),
        );

        binding.observe_backend_events(vec![occupant_event(2, Some(original), replacement)]);
        {
            let state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let snapshot_pane = &state.snapshot.sessions[0].windows[0].anchor;
            assert_eq!(snapshot_pane.occupant_id, None);
            assert_eq!(snapshot_pane.pane_pid, Some(10));
            assert_eq!(snapshot_pane.process.as_deref(), Some("zsh"));
        }
        assert_eq!(binding.pane_generation("$1", "@old", "%old"), Some(2));
        release_tx.send(()).expect("release blocker");

        assert!(
            blocker
                .recv_timeout(Duration::from_secs(1))
                .expect("blocker result")
                .is_ok()
        );
        assert_eq!(
            queued
                .recv_timeout(Duration::from_secs(1))
                .expect("queued result"),
            Err(MuxCommandError::Stale)
        );
        assert_eq!(
            state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .commands
                .len(),
            1
        );
    }

    #[test]
    fn queued_session_and_window_mutations_stale_on_reconnect_rebase_with_reused_ids() {
        let scope = command_scope();
        let snapshot = targeted_snapshot();
        let state = Arc::new(Mutex::new(BlockingRecordingState {
            snapshot: snapshot.clone(),
            commands: Vec::new(),
        }));
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let backend = BlockingRecordingBackend {
            state: Arc::clone(&state),
            started: started_tx,
            release: Arc::new(Mutex::new(release_rx)),
        };
        let mut binding = BindingMuxController::new(scope);
        binding.controller.sessions = snapshot.sessions;
        binding
            .controller
            .set_backend_factory(Arc::new(move |_| Box::new(backend.clone())));
        binding.synchronize_resource_generations();
        let generation = binding.binding_generation();
        let session_precondition = binding
            .controller
            .capture_execution_precondition(&MuxCommand::RenameSession {
                session_id: "$1".to_owned(),
                name: "stale-session".to_owned(),
            })
            .expect("capture session precondition")
            .expect("session precondition");
        let window_precondition = binding
            .controller
            .capture_execution_precondition(&MuxCommand::RenameWindow {
                session_id: "$1".to_owned(),
                window_id: "@old".to_owned(),
                name: "stale-window".to_owned(),
            })
            .expect("capture window precondition")
            .expect("window precondition");
        assert_eq!(session_precondition.binding_generation, Some(generation));
        assert_eq!(window_precondition.binding_generation, Some(generation));
        assert!(session_precondition.target.window_id.is_none());
        assert!(window_precondition.target.pane_id.is_none());
        assert!(
            binding
                .controller
                .execution_resource_generation_guard(Some(&session_precondition))
                .is_some()
        );
        assert!(
            binding
                .controller
                .execution_resource_generation_guard(Some(&window_precondition))
                .is_some()
        );
        let repaint: RepaintHandle = Arc::new(|| {});
        let config = MultiplexerConfig {
            backend: MultiplexerBackendConfig::Tmux,
            ..Default::default()
        };
        let blocker = binding.execute_command_authoritatively(
            &repaint,
            &config,
            MuxCommand::RenameSession {
                session_id: "$1".to_owned(),
                name: "blocker".to_owned(),
            },
            Instant::now() + Duration::from_secs(5),
            CommandCancellation::new(),
        );
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("blocker started");
        let queued_session = binding.execute_command_authoritatively(
            &repaint,
            &config,
            MuxCommand::RenameSession {
                session_id: "$1".to_owned(),
                name: "stale-session".to_owned(),
            },
            Instant::now() + Duration::from_secs(5),
            CommandCancellation::new(),
        );
        let queued_window = binding.execute_command_authoritatively(
            &repaint,
            &config,
            MuxCommand::RenameWindow {
                session_id: "$1".to_owned(),
                window_id: "@old".to_owned(),
                name: "stale-window".to_owned(),
            },
            Instant::now() + Duration::from_secs(5),
            CommandCancellation::new(),
        );

        binding.observe_backend_events(vec![MuxEvent {
            backend_identity: "tmux-control".to_owned(),
            scope,
            revision: 1,
            cursor: None,
            topic: crate::backend::MuxEventTopic::SnapshotRebased,
            provenance: crate::backend::MuxEventProvenance::TmuxControl,
            target: None,
            payload: MuxEventPayload::Rebase {
                reason: MuxRebaseReason::Reconnect,
            },
        }]);
        assert_eq!(binding.binding_generation(), generation + 1);
        {
            let state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert_eq!(state.snapshot.sessions[0].id, "$1");
            assert_eq!(state.snapshot.sessions[0].windows[0].id, "@old");
        }
        release_tx.send(()).expect("release blocker");

        assert!(
            blocker
                .recv_timeout(Duration::from_secs(1))
                .expect("blocker result")
                .is_ok()
        );
        assert_eq!(
            queued_session
                .recv_timeout(Duration::from_secs(1))
                .expect("queued session result"),
            Err(MuxCommandError::Stale)
        );
        assert_eq!(
            queued_window
                .recv_timeout(Duration::from_secs(1))
                .expect("queued window result"),
            Err(MuxCommandError::Stale)
        );
        assert_eq!(
            state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .commands
                .len(),
            1
        );
    }

    fn active_window(session_id: &str, window_id: &str) -> ActiveWindow {
        ActiveWindow {
            session_id: session_id.to_owned(),
            window_id: window_id.to_owned(),
        }
    }
}
