use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{
    capability::{
        BindingCapabilityDescriptor, BindingOperationAvailability, BindingOperationOutcome,
    },
    command::{MuxCommand, MuxSessionLaunchPlan},
    controller::MuxScope,
    snapshot::{MuxPaneAnchor, MuxSnapshot},
};

pub use crate::operation::{
    MuxAllocatedResources, MuxAllocatedWindow, MuxBackendCommandCompletion,
    MuxBackendOperationError, MuxEventTarget, MuxOccupantIdentity,
};

/// The hard upper bound for queued backend observations. A full queue never silently drops an
/// observation: it replaces the stale tail with a gap and a rebase request.
pub const MUX_EVENT_QUEUE_MAX_EVENTS: usize = 256;
/// The byte budget is separate from the event count because terminal output is arbitrary binary.
pub const MUX_EVENT_QUEUE_MAX_BYTES: usize = 1_048_576;

/// Cursor assigned by an authoritative backend subscription.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct MuxEventCursor {
    pub stream: String,
    pub sequence: u64,
}

impl MuxEventCursor {
    pub fn new(stream: impl Into<String>, sequence: u64) -> Self {
        Self {
            stream: stream.into(),
            sequence,
        }
    }
}

/// The source that observed an event. This is data, not a capability claim.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MuxEventProvenance {
    Native,
    RmuxSdk,
    TmuxControl,
    TmuxSnapshotFallback,
    Queue,
}

/// Stable backend-neutral event topics. The payload carries the topic-specific facts.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MuxEventTopic {
    TopologyChanged,
    TerminalOutput,
    PaneStateChanged,
    PaneTitleChanged,
    PaneOptionsChanged,
    PaneForegroundChanged,
    PaneCwdChanged,
    PaneOccupantReplaced,
    PaneClosed,
    BackendDisconnected,
    BackendLagged,
    SnapshotRebased,
}

impl MuxEventTopic {
    pub const ALL: [Self; 12] = [
        Self::TopologyChanged,
        Self::TerminalOutput,
        Self::PaneStateChanged,
        Self::PaneTitleChanged,
        Self::PaneOptionsChanged,
        Self::PaneForegroundChanged,
        Self::PaneCwdChanged,
        Self::PaneOccupantReplaced,
        Self::PaneClosed,
        Self::BackendDisconnected,
        Self::BackendLagged,
        Self::SnapshotRebased,
    ];
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MuxTopologyChange {
    Mutation,
    Invalidated,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct MuxPaneOption {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct MuxForegroundState {
    pub pid: Option<u32>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub executable: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct MuxPaneState {
    pub title: Option<String>,
    pub options: Vec<MuxPaneOption>,
    pub foreground: Option<MuxForegroundState>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MuxRebaseReason {
    Bootstrap,
    Reconnect,
    SequenceGap,
    QueueOverflow,
}

/// Event data kept separate from scope and queue revision so a backend can publish an observation
/// before a controller chooses the binding that owns it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum MuxEventPayload {
    Topology {
        change: MuxTopologyChange,
    },
    Output {
        bytes: Vec<u8>,
    },
    PaneState {
        state: MuxPaneState,
    },
    Title {
        old_title: Option<String>,
        new_title: Option<String>,
    },
    Option {
        name: String,
        old_value: Option<String>,
        new_value: Option<String>,
    },
    Cwd {
        old_cwd: Option<String>,
        new_cwd: Option<String>,
    },
    OccupantReplaced {
        old_occupant: Option<MuxOccupantIdentity>,
        new_occupant: Option<MuxOccupantIdentity>,
    },
    Foreground {
        old_state: Option<MuxForegroundState>,
        new_state: Option<MuxForegroundState>,
    },
    Closed {
        reason: String,
    },
    Disconnected {
        reason: String,
    },
    Gap {
        expected_sequence: u64,
        resume_sequence: u64,
        missed_events: u64,
    },
    Rebase {
        reason: MuxRebaseReason,
    },
}

/// A fully attributed event delivered to the UI/controller. `revision` is monotonic for this
/// binding's subscription, while `cursor` preserves the backend stream's own ordering.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct MuxEvent {
    /// Stable identity of the event-producing server/transport. Consumers must not merge events
    /// from distinct identities merely because their binding scopes happen to be the same.
    pub backend_identity: String,
    pub scope: MuxScope,
    pub revision: u64,
    pub cursor: Option<MuxEventCursor>,
    pub topic: MuxEventTopic,
    pub provenance: MuxEventProvenance,
    pub target: Option<MuxEventTarget>,
    pub payload: MuxEventPayload,
}

impl MuxEvent {
    pub fn requires_rebase(&self) -> bool {
        self.topic == MuxEventTopic::SnapshotRebased
    }
}

/// A backend-produced event before the controller binds it to an exact scope and revision.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct MuxEventDraft {
    pub cursor: Option<MuxEventCursor>,
    pub topic: MuxEventTopic,
    pub provenance: MuxEventProvenance,
    pub target: Option<MuxEventTarget>,
    pub payload: MuxEventPayload,
}

impl MuxEventDraft {
    pub fn new(
        topic: MuxEventTopic,
        provenance: MuxEventProvenance,
        target: Option<MuxEventTarget>,
        cursor: Option<MuxEventCursor>,
        payload: MuxEventPayload,
    ) -> Self {
        Self {
            cursor,
            topic,
            provenance,
            target,
            payload,
        }
    }

    pub fn rebase(provenance: MuxEventProvenance, reason: MuxRebaseReason) -> Self {
        Self::new(
            MuxEventTopic::SnapshotRebased,
            provenance,
            None,
            None,
            MuxEventPayload::Rebase { reason },
        )
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "availability")]
pub enum MuxEventAvailability {
    Available,
    BestEffort { reason: String },
    Unsupported { reason: String },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct MuxEventCapability {
    pub topic: MuxEventTopic,
    pub availability: MuxEventAvailability,
}

impl MuxEventCapability {
    pub fn available(topic: MuxEventTopic) -> Self {
        Self {
            topic,
            availability: MuxEventAvailability::Available,
        }
    }

    pub fn best_effort(topic: MuxEventTopic, reason: impl Into<String>) -> Self {
        Self {
            topic,
            availability: MuxEventAvailability::BestEffort {
                reason: reason.into(),
            },
        }
    }

    pub fn unsupported(topic: MuxEventTopic, reason: impl Into<String>) -> Self {
        Self {
            topic,
            availability: MuxEventAvailability::Unsupported {
                reason: reason.into(),
            },
        }
    }
}

/// A bounded, lock-protected fanout from backend workers to binding-specific event cursors.
///
/// Publishing never assigns a binding scope. Each scope owns an independent cursor, so one
/// binding cannot consume an observation before another one sees it, or relabel an observation
/// from a different backend instance.
#[derive(Clone, Debug)]
pub struct MuxEventQueue {
    state: Arc<Mutex<MuxEventQueueState>>,
}

#[derive(Debug)]
struct MuxEventQueueState {
    backend_identity: String,
    events: VecDeque<QueuedMuxEvent>,
    subscribers: HashMap<MuxScope, QueueCursor>,
    bytes: usize,
    next_event_id: u64,
    max_events: usize,
    max_bytes: usize,
}

#[derive(Debug)]
struct QueuedMuxEvent {
    id: u64,
    draft: MuxEventDraft,
}

#[derive(Debug)]
struct QueueCursor {
    next_event_id: u64,
    next_revision: u64,
}

impl Default for MuxEventQueue {
    fn default() -> Self {
        Self::with_limits(MUX_EVENT_QUEUE_MAX_EVENTS, MUX_EVENT_QUEUE_MAX_BYTES)
    }
}

impl MuxEventQueue {
    pub fn for_backend(backend_identity: impl Into<String>) -> Self {
        Self::with_backend_limits(
            backend_identity,
            MUX_EVENT_QUEUE_MAX_EVENTS,
            MUX_EVENT_QUEUE_MAX_BYTES,
        )
    }

    pub fn with_limits(max_events: usize, max_bytes: usize) -> Self {
        Self::with_backend_limits("unscoped", max_events, max_bytes)
    }

    pub fn with_backend_limits(
        backend_identity: impl Into<String>,
        max_events: usize,
        max_bytes: usize,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(MuxEventQueueState {
                backend_identity: backend_identity.into(),
                events: VecDeque::new(),
                subscribers: HashMap::new(),
                bytes: 0,
                next_event_id: 1,
                max_events: max_events.max(3),
                max_bytes: max_bytes.max(1),
            })),
        }
    }

    pub fn publish(&self, draft: MuxEventDraft) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.push(draft);
    }

    pub fn publish_gap(
        &self,
        provenance: MuxEventProvenance,
        target: Option<MuxEventTarget>,
        cursor: Option<MuxEventCursor>,
        expected_sequence: u64,
        resume_sequence: u64,
        missed_events: u64,
    ) {
        let gap = MuxEventDraft::new(
            MuxEventTopic::BackendLagged,
            provenance,
            target,
            cursor,
            MuxEventPayload::Gap {
                expected_sequence,
                resume_sequence,
                missed_events,
            },
        );
        let rebase = MuxEventDraft::rebase(provenance, MuxRebaseReason::SequenceGap);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.replace_with_gap(gap, rebase);
    }

    /// Drains this scope's cursor without I/O. Other scopes retain their cursors and observe the
    /// same drafts with revisions monotonic for their own binding.
    pub fn drain(&self, scope: MuxScope, maximum: usize) -> Vec<MuxEvent> {
        if maximum == 0 {
            return Vec::new();
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let first_available = state
            .events
            .front()
            .map_or(state.next_event_id, |event| event.id);
        let (next_event_id, next_revision) = {
            let cursor = state.subscribers.entry(scope).or_insert(QueueCursor {
                next_event_id: first_available,
                next_revision: 1,
            });
            (cursor.next_event_id, cursor.next_revision)
        };
        let queued = state
            .events
            .iter()
            .filter(|event| event.id >= next_event_id)
            .take(maximum)
            .map(|event| (event.id, event.draft.clone()))
            .collect::<Vec<_>>();
        let backend_identity = state.backend_identity.clone();
        let mut revision = next_revision;
        let mut events = Vec::with_capacity(queued.len());
        let mut resume_at = next_event_id;
        for (id, draft) in queued {
            events.push(MuxEvent {
                backend_identity: backend_identity.clone(),
                scope,
                revision,
                cursor: draft.cursor,
                topic: draft.topic,
                provenance: draft.provenance,
                target: draft.target,
                payload: draft.payload,
            });
            revision = revision.saturating_add(1);
            resume_at = id.saturating_add(1);
        }
        let cursor = state
            .subscribers
            .get_mut(&scope)
            .expect("cursor was inserted before draining");
        cursor.next_event_id = resume_at;
        cursor.next_revision = revision;
        // Retain until the bounded source history rolls over. A binding can begin draining after
        // another binding and must still receive retained observations rather than inherit that
        // binding's destructive queue position.
        events
    }
}

impl MuxEventQueueState {
    fn push(&mut self, draft: MuxEventDraft) {
        let bytes = draft.approximate_bytes();
        if bytes > self.max_bytes
            || self.events.len() >= self.max_events
            || self.bytes.saturating_add(bytes) > self.max_bytes
        {
            self.replace_with_rebase();
            if bytes > self.max_bytes
                || self.events.len() >= self.max_events
                || self.bytes.saturating_add(bytes) > self.max_bytes
            {
                return;
            }
        }
        self.push_unchecked(draft, bytes);
    }

    fn replace_with_rebase(&mut self) {
        let expected_sequence = self
            .events
            .front()
            .map(|event| event.id)
            .unwrap_or(self.next_event_id);
        let missed_events = self.events.len() as u64;
        let resume_sequence = self.next_event_id;
        self.events.clear();
        self.bytes = 0;
        for cursor in self.subscribers.values_mut() {
            cursor.next_event_id = resume_sequence;
        }
        self.push_unchecked(
            MuxEventDraft::new(
                MuxEventTopic::BackendLagged,
                MuxEventProvenance::Queue,
                None,
                Some(MuxEventCursor::new("backend-event-queue", resume_sequence)),
                MuxEventPayload::Gap {
                    expected_sequence,
                    resume_sequence,
                    missed_events,
                },
            ),
            0,
        );
        self.push_unchecked(
            MuxEventDraft::rebase(MuxEventProvenance::Queue, MuxRebaseReason::QueueOverflow),
            0,
        );
    }

    fn replace_with_gap(&mut self, gap: MuxEventDraft, rebase: MuxEventDraft) {
        let resume_sequence = self.next_event_id;
        self.events.clear();
        self.bytes = 0;
        for cursor in self.subscribers.values_mut() {
            cursor.next_event_id = resume_sequence;
        }
        self.push_unchecked(gap, 0);
        self.push_unchecked(rebase, 0);
    }

    fn push_unchecked(&mut self, draft: MuxEventDraft, bytes: usize) {
        let id = self.next_event_id;
        self.next_event_id = self.next_event_id.saturating_add(1);
        self.bytes = self.bytes.saturating_add(bytes);
        self.events.push_back(QueuedMuxEvent { id, draft });
    }
}

impl MuxEventDraft {
    fn approximate_bytes(&self) -> usize {
        let target = self.target.as_ref().map_or(0, |target| {
            target
                .session_id
                .as_ref()
                .map_or(0, String::len)
                .saturating_add(target.window_id.as_ref().map_or(0, String::len))
                .saturating_add(target.pane_id.as_ref().map_or(0, String::len))
                .saturating_add(target.terminal_id.as_ref().map_or(0, String::len))
                .saturating_add(
                    target
                        .occupant
                        .as_ref()
                        .map_or(0, |occupant| occupant.backend_identity.len()),
                )
        });
        let payload = match &self.payload {
            MuxEventPayload::Topology { .. } | MuxEventPayload::Rebase { .. } => 0,
            MuxEventPayload::Output { bytes } => bytes.len(),
            MuxEventPayload::PaneState { state } => {
                state.title.as_ref().map_or(0, String::len).saturating_add(
                    state
                        .options
                        .iter()
                        .map(|option| option.name.len().saturating_add(option.value.len()))
                        .sum::<usize>(),
                )
            }
            MuxEventPayload::Title {
                old_title,
                new_title,
            } => old_title
                .as_ref()
                .map_or(0, String::len)
                .saturating_add(new_title.as_ref().map_or(0, String::len)),
            MuxEventPayload::Option {
                name,
                old_value,
                new_value,
            } => name
                .len()
                .saturating_add(old_value.as_ref().map_or(0, String::len))
                .saturating_add(new_value.as_ref().map_or(0, String::len)),
            MuxEventPayload::Cwd { old_cwd, new_cwd } => old_cwd
                .as_ref()
                .map_or(0, String::len)
                .saturating_add(new_cwd.as_ref().map_or(0, String::len)),
            MuxEventPayload::OccupantReplaced {
                old_occupant,
                new_occupant,
            } => old_occupant
                .as_ref()
                .map_or(0, |occupant| occupant.backend_identity.len())
                .saturating_add(
                    new_occupant
                        .as_ref()
                        .map_or(0, |occupant| occupant.backend_identity.len()),
                ),
            MuxEventPayload::Foreground {
                old_state,
                new_state,
            } => foreground_bytes(old_state).saturating_add(foreground_bytes(new_state)),
            MuxEventPayload::Closed { reason } | MuxEventPayload::Disconnected { reason } => {
                reason.len()
            }
            MuxEventPayload::Gap { .. } => 0,
        };
        target.saturating_add(payload)
    }
}

fn foreground_bytes(state: &Option<MuxForegroundState>) -> usize {
    state.as_ref().map_or(0, |state| {
        state
            .command
            .as_ref()
            .map_or(0, String::len)
            .saturating_add(state.cwd.as_ref().map_or(0, String::len))
            .saturating_add(state.executable.as_ref().map_or(0, String::len))
    })
}

/// A fully resolved binding-scoped resource identity captured before queueing a mutation.
///
/// Workers snapshot immediately before mutation and compare this token. A resource that vanished
/// or whose pane occupant changed is stale; it must never be retargeted by a reused ID.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct MuxScopedExecutionPrecondition {
    pub scope: MuxScope,
    pub target: MuxEventTarget,
    pub occupant_fingerprint: Option<String>,
    /// Binding generation captured with this target. The controller's queue guard rejects a
    /// mutation when reconnect or backend replacement advances the binding, even if backend IDs
    /// have been reused.
    #[serde(default)]
    pub binding_generation: Option<u64>,
    /// Binding-local generation of the exact target resource. Pane and terminal target
    /// generations advance on authoritative occupant replacement even when the snapshot
    /// fingerprint is reused; the controller rechecks it immediately before mutation.
    #[serde(default)]
    pub occupant_generation: Option<u64>,
}

impl MuxScopedExecutionPrecondition {
    pub fn matches_snapshot(&self, snapshot: &MuxSnapshot) -> bool {
        let Some(session_id) = self.target.session_id.as_deref() else {
            return false;
        };
        let Some(session) = snapshot
            .sessions
            .iter()
            .find(|session| session.id == session_id)
        else {
            return false;
        };
        let Some(window_id) = self.target.window_id.as_deref() else {
            return self.target.pane_id.is_none() && self.target.terminal_id.is_none();
        };
        let Some(window) = session.windows.iter().find(|window| window.id == window_id) else {
            return false;
        };
        let Some(pane_id) = self.target.pane_id.as_deref() else {
            return self.target.terminal_id.is_none();
        };
        let pane = std::iter::once(&window.anchor)
            .chain(window.panes.iter())
            .find(|pane| pane.pane_id.as_deref() == Some(pane_id));
        let Some(pane) = pane else {
            return false;
        };
        if self
            .target
            .terminal_id
            .as_deref()
            .is_some_and(|terminal_id| pane.terminal_id.as_deref() != Some(terminal_id))
        {
            return false;
        }
        self.occupant_fingerprint
            .as_ref()
            .is_none_or(|fingerprint| {
                snapshot_occupant_fingerprint(pane).as_deref() == Some(fingerprint)
            })
    }
}

pub fn snapshot_occupant_fingerprint(pane: &MuxPaneAnchor) -> Option<String> {
    pane.occupant_id.clone().or_else(|| {
        (pane.pane_pid.is_some() || pane.process.is_some())
            .then(|| format!("{:?}\u{1f}{:?}", pane.pane_pid, pane.process))
    })
}

pub trait MuxBackend {
    fn snapshot(&self) -> Result<MuxSnapshot>;
    fn execute(&mut self, command: MuxCommand) -> Result<()>;

    /// Session launch is opt-in. A backend must override both this and
    /// [`Self::session_launch_capability`] rather than inheriting a fake successful create.
    fn execute_session_launch(
        &mut self,
        _plan: MuxSessionLaunchPlan,
    ) -> BindingOperationOutcome<Result<()>> {
        BindingOperationOutcome::Unsupported
    }

    /// Preflight for an immutable recursive launch. This performs no mutation.
    fn session_launch_capability(
        &self,
        _plan: &MuxSessionLaunchPlan,
    ) -> BindingOperationOutcome<()> {
        BindingOperationOutcome::Unsupported
    }

    /// Compares a queue-time binding-scoped identity with a snapshot taken immediately before a
    /// mutation. Concrete adapters may strengthen this with an opaque backend occupant identity.
    fn validate_execution_precondition(
        &self,
        precondition: &MuxScopedExecutionPrecondition,
        snapshot: &MuxSnapshot,
    ) -> Result<bool> {
        Ok(precondition.matches_snapshot(snapshot))
    }

    /// Returns exact mutation facts that cannot be reconstructed from a lossy snapshot.
    fn take_authoritative_completion(&mut self) -> Option<MuxBackendCommandCompletion> {
        None
    }

    fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor {
        BindingCapabilityDescriptor::new(scope, [])
    }

    /// Describes which notifications this backend can observe authoritatively. A missing stream
    /// is explicit rather than silently represented as an empty event queue.
    fn event_capabilities(&self) -> Vec<MuxEventCapability> {
        MuxEventTopic::ALL
            .into_iter()
            .map(|topic| {
                MuxEventCapability::unsupported(
                    topic,
                    "backend does not expose an authoritative event stream",
                )
            })
            .collect()
    }

    /// Starts any persistent observer owned by this backend. It is intentionally separate from
    /// `drain_events`: draining is a pure cursor operation and must never perform backend I/O.
    fn start_event_stream(&mut self) {}

    /// Removes at most `maximum` already-observed events. This method must never perform I/O or
    /// wait for a backend; stream workers enqueue observations before the UI thread drains them.
    fn drain_events(&mut self, _scope: MuxScope, _maximum: usize) -> Vec<MuxEvent> {
        Vec::new()
    }

    fn execute_checked(
        &mut self,
        scope: MuxScope,
        command: MuxCommand,
    ) -> BindingOperationOutcome<Result<()>> {
        let descriptor = self.capabilities(scope);
        descriptor.invoke(
            descriptor.request(command.operation()),
            BindingOperationAvailability::Available,
            || self.execute(command),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capability::{BINDING_CAPABILITY_DESCRIPTOR_VERSION, BindingOperation},
        controller::{BindingId, SpaceId},
        snapshot::{MuxPaneAnchor, MuxSession, MuxWindow},
    };

    #[derive(Default)]
    struct FakeBackend {
        sessions: Vec<MuxSession>,
        commands: Vec<MuxCommand>,
    }

    impl MuxBackend for FakeBackend {
        fn snapshot(&self) -> Result<MuxSnapshot> {
            Ok(MuxSnapshot {
                active_session_id: self
                    .sessions
                    .iter()
                    .find(|session| session.active)
                    .map(|session| session.id.clone()),
                sessions: self.sessions.clone(),
            })
        }

        fn execute(&mut self, command: MuxCommand) -> Result<()> {
            self.commands.push(command);
            Ok(())
        }
    }

    #[test]
    fn fake_backend_contract_covers_session_lifecycle_and_anchors() {
        let mut backend = FakeBackend {
            sessions: vec![MuxSession {
                id: "project".to_owned(),
                name: "project".to_owned(),
                active: true,
                anchor: MuxPaneAnchor {
                    session_id: "project".to_owned(),
                    pane_id: Some("pane-1".to_owned()),
                    terminal_id: Some("pane-1".to_owned()),
                    occupant_id: None,
                    pane_pid: None,
                    cwd: Some("/repo".to_owned()),
                    process: Some("zsh".to_owned()),
                },
                active_window_id: None,
                windows: Vec::new(),
            }],
            commands: Vec::new(),
        };

        let snapshot = backend.snapshot().unwrap();
        assert_eq!(snapshot.active_session_id.as_deref(), Some("project"));
        assert_eq!(snapshot.sessions[0].anchor.cwd.as_deref(), Some("/repo"));

        let commands = [
            MuxCommand::ActivateWindow {
                session_id: "project".to_owned(),
                window_id: "@1".to_owned(),
            },
            MuxCommand::CreateProjectSession {
                session_id: "next".to_owned(),
                cwd: "/next".to_owned(),
            },
            MuxCommand::CreateWorktreeSession {
                session_id: "worktree".to_owned(),
                cwd: "/repo-worktree".to_owned(),
            },
            MuxCommand::RenameSession {
                session_id: "project".to_owned(),
                name: "renamed".to_owned(),
            },
            MuxCommand::DitchSession {
                session_id: "renamed".to_owned(),
            },
        ];
        for command in commands.clone() {
            backend.execute(command).unwrap();
        }

        assert_eq!(backend.commands, commands);
    }

    #[test]
    fn every_backend_has_a_scoped_default_capability_descriptor() {
        let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(2));
        let descriptor = FakeBackend::default().capabilities(scope);

        assert_eq!(descriptor.version(), BINDING_CAPABILITY_DESCRIPTOR_VERSION);
        assert_eq!(descriptor.scope(), scope);
        assert!(!descriptor.supports(BindingOperation::SplitPane));
    }

    #[test]
    fn checked_execution_does_not_mutate_an_unsupported_backend() {
        let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(2));
        let mut backend = FakeBackend::default();
        let outcome = backend.execute_checked(
            scope,
            MuxCommand::DitchSession {
                session_id: "project".to_owned(),
            },
        );

        assert!(matches!(outcome, BindingOperationOutcome::Unsupported));
        assert!(backend.commands.is_empty());
    }

    #[test]
    fn scoped_precondition_requires_the_exact_pane_and_terminal_identity() {
        let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(2));
        let pane = MuxPaneAnchor {
            session_id: "$1".to_owned(),
            pane_id: Some("%p".to_owned()),
            terminal_id: Some("t1".to_owned()),
            pane_pid: Some(42),
            cwd: None,
            process: Some("shell".to_owned()),
            occupant_id: Some("occupant-1".to_owned()),
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
                    index: 1,
                    name: "one".to_owned(),
                    active: true,
                    anchor: pane,
                    panes: Vec::new(),
                    layout: None,
                    progress: None,
                }],
            }],
        };
        let precondition = |terminal_id| MuxScopedExecutionPrecondition {
            scope,
            target: MuxEventTarget::pane("$1", "@1", "%p", terminal_id, None),
            occupant_fingerprint: Some("occupant-1".to_owned()),
            binding_generation: Some(7),
            occupant_generation: Some(3),
        };

        assert!(precondition("t1").matches_snapshot(&snapshot));
        assert!(!precondition("%p").matches_snapshot(&snapshot));
        let wrong_pane = MuxScopedExecutionPrecondition {
            target: MuxEventTarget::pane("$1", "@1", "%other", "t1", None),
            ..precondition("t1")
        };
        assert!(!wrong_pane.matches_snapshot(&snapshot));
    }

    fn output_event(sequence: u64) -> MuxEventDraft {
        MuxEventDraft::new(
            MuxEventTopic::TerminalOutput,
            MuxEventProvenance::Native,
            None,
            Some(MuxEventCursor::new("pane:%1", sequence)),
            MuxEventPayload::Output {
                bytes: vec![sequence as u8],
            },
        )
    }

    #[test]
    fn bounded_drain_preserves_output_cursors_and_queue_revisions() {
        let queue = MuxEventQueue::with_limits(3, 1024);
        let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(2));
        queue.publish(output_event(41));
        queue.publish(output_event(42));

        let first = queue.drain(scope, 1);
        assert_eq!(first.len(), 1);
        assert_eq!(
            first[0].cursor.as_ref().map(|cursor| cursor.sequence),
            Some(41)
        );

        let second = queue.drain(scope, 8);
        assert_eq!(second.len(), 1);
        assert_eq!(
            second[0].cursor.as_ref().map(|cursor| cursor.sequence),
            Some(42)
        );
        assert!(first[0].revision < second[0].revision);
    }

    #[test]
    fn gaps_and_overflow_require_a_rebase() {
        let queue = MuxEventQueue::with_limits(3, 1024);
        let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(2));
        queue.publish(output_event(1));
        queue.publish(output_event(2));
        queue.publish(output_event(3));
        queue.publish(output_event(4));

        let overflow = queue.drain(scope, 8);
        assert!(
            overflow
                .iter()
                .any(|event| event.topic == MuxEventTopic::BackendLagged)
        );
        assert!(overflow.iter().any(MuxEvent::requires_rebase));

        queue.publish_gap(
            MuxEventProvenance::RmuxSdk,
            None,
            Some(MuxEventCursor::new("pane:%1", 9)),
            7,
            9,
            2,
        );
        let gap = queue.drain(scope, 8);
        assert!(gap.iter().any(MuxEvent::requires_rebase));
        assert!(gap.iter().any(|event| {
            matches!(
                &event.payload,
                MuxEventPayload::Gap {
                    expected_sequence: 7,
                    resume_sequence: 9,
                    missed_events: 2,
                }
            )
        }));
    }
    #[test]
    fn fanout_cursors_keep_bindings_isolated_and_backend_partitioned() {
        let queue = MuxEventQueue::with_backend_limits("rmux:server-a", 8, 1024);
        let first_scope =
            MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(2));
        let second_scope =
            MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(3));
        queue.publish(output_event(7));

        let first = queue.drain(first_scope, 8);
        let second = queue.drain(second_scope, 8);

        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_eq!(first[0].scope, first_scope);
        assert_eq!(second[0].scope, second_scope);
        assert_eq!(first[0].backend_identity, "rmux:server-a");
        assert_eq!(second[0].backend_identity, "rmux:server-a");
        assert_eq!(first[0].cursor, second[0].cursor);
        assert_eq!(first[0].revision, 1);
        assert_eq!(second[0].revision, 1);
    }
}
