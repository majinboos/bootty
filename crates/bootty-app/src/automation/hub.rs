use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    sync::{Arc, Mutex, MutexGuard, Weak},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::commands::{CommandCancellation, CommandInvocation, CommandTarget};

pub const MAX_TASKS: usize = 64;
pub const MAX_SUBSCRIPTIONS: usize = 64;
pub const MAX_TOPICS_PER_SUBSCRIPTION: usize = 16;
pub const EVENT_QUEUE_LIMIT: usize = 64;
pub const EVENT_QUEUE_BYTE_LIMIT: usize = 512 * 1024;
pub const EVENT_TOPIC_LIMIT: usize = 128;
pub const MAX_REGISTERED_EVENT_TOPICS: usize = 256;
pub const TERMINAL_OUTPUT_STREAM_LIMIT: usize = 64;
const MAX_SNAPSHOT_SOURCES: usize = 16;
const MAX_SNAPSHOT_SOURCE_BYTES: usize = 128;
pub const TERMINAL_OUTPUT_BYTE_LIMIT: usize = 512 * 1024;
pub const COMMAND_COMPLETED_TOPIC: &str = "command.completed";
pub const TERMINAL_OUTPUT_TOPIC: &str = "terminal.output";
// A completed task is retained alongside up to `MAX_TASKS - 1` peers. Reserve
// half of the snapshot budget for task envelopes and the selected-topic map.
const TASK_OUTCOME_BYTE_LIMIT: usize = EVENT_QUEUE_BYTE_LIMIT / (MAX_TASKS * 2);
const TASK_OUTCOME_LIMIT_CODE: &str = "-32003";
const TASK_OUTCOME_LIMIT_MESSAGE: &str = "task outcome exceeds retained task result limit";
const TASK_COMPLETION_PUBLICATION_MESSAGE: &str = "task completion could not be published";

const BUILTIN_EVENT_TOPICS: &[&str] = &[
    COMMAND_COMPLETED_TOPIC,
    "topology.changed",
    TERMINAL_OUTPUT_TOPIC,
    "terminal.title_changed",
    "terminal.cwd_changed",
    "terminal.process_changed",
    "terminal.occupant_replaced",
    "terminal.options_changed",
    "terminal.foreground_changed",
    "terminal.closed",
    "metadata.changed",
    "metadata.expired",
    "task.changed",
    "backend.connection_changed",
    "backend.lagged",
    "backend.rebased",
    "worktree.changed",
    "directory.usage_changed",
    "command.registry_changed",
    "extension.reloaded",
];

/// An owner combines an OS process generation with an optional logical owner
/// inside that process. Liveness depends only on the OS identity; the logical
/// generation isolates multiple control servers hosted by one process.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OwnerIdentity {
    pid: u32,
    generation: u64,
    logical_generation: u64,
}

impl OwnerIdentity {
    pub const fn new(pid: u32, generation: u64) -> Self {
        Self {
            pid,
            generation,
            logical_generation: 0,
        }
    }

    pub fn for_process(pid: u32) -> Option<Self> {
        let system = sysinfo::System::new_all();
        system
            .process(sysinfo::Pid::from_u32(pid))
            .map(|process| Self::new(pid, process.start_time()))
    }

    pub fn for_process_logical_owner(pid: u32, logical_generation: u64) -> Option<Self> {
        Self::for_process(pid).map(|owner| Self {
            logical_generation,
            ..owner
        })
    }

    pub fn current_process() -> Option<Self> {
        Self::for_process(std::process::id())
    }

    pub const fn pid(&self) -> u32 {
        self.pid
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    fn is_live_in(&self, system: &sysinfo::System) -> bool {
        system
            .process(sysinfo::Pid::from_u32(self.pid))
            .is_some_and(|process| process.start_time() == self.generation)
    }
}

#[derive(Clone, Debug)]
pub struct AutomationError {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
}

impl AutomationError {
    pub(crate) fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    fn with_data(code: i32, message: impl Into<String>, data: Value) -> Self {
        Self {
            code,
            message: message.into(),
            data: Some(data),
        }
    }
}

impl fmt::Display for AutomationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for AutomationError {}

/// The immutable input to the event seam. The hub assigns the scope revision
/// and a separate sequence for each matching subscription.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventPublication {
    pub scope: String,
    pub topic: String,
    pub provenance: Value,
    pub target: Option<CommandTarget>,
    pub payload: Value,
}

impl EventPublication {
    pub fn new(
        scope: impl Into<String>,
        topic: impl Into<String>,
        provenance: Value,
        target: Option<CommandTarget>,
        payload: Value,
    ) -> Self {
        Self {
            scope: scope.into(),
            topic: topic.into(),
            provenance,
            target,
            payload,
        }
    }
}

/// A delivered event. `target` intentionally serializes as `null` when the
/// event has no command target, preserving one stable envelope shape.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub sequence: u64,
    pub scope: String,
    pub revision: u64,
    pub topic: String,
    pub provenance: Value,
    pub target: Option<CommandTarget>,
    pub payload: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct EventDelivery {
    pub subscription: String,
    pub scope: String,
    pub revision: u64,
    pub cursor: u64,
    pub events: Vec<EventEnvelope>,
}

/// Authoritative bootstrap state for selected registered topics in one scope.
#[derive(Clone, Debug, Serialize)]
pub struct EventSnapshot {
    pub scope: String,
    pub revision: u64,
    pub snapshots: BTreeMap<String, Value>,
}

/// The atomic gap recovery handshake. The returned cursor is the exact point
/// after which the caller may resume polling after replacing its local state.
#[derive(Clone, Debug, Serialize)]
pub struct EventRebase {
    pub subscription: String,
    pub scope: String,
    pub revision: u64,
    pub cursor: u64,
    pub snapshot: EventSnapshot,
}

#[derive(Clone, Debug, Serialize)]
pub struct EventUnsubscription {
    pub unsubscribed: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct TerminalOutputChunk {
    pub cursor: u64,
    pub payload: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct TerminalOutputRead {
    pub scope: String,
    pub target: CommandTarget,
    pub cursor: u64,
    pub chunks: Vec<TerminalOutputChunk>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TaskState {
    Running,
    Cancelling,
    Completed { outcome: Value },
}

#[derive(Clone, Debug, Serialize)]
pub struct TaskStatus {
    pub id: String,
    pub owner_pid: u32,
    pub state: TaskState,
}

pub const MAX_METADATA_RECORDS: usize = 256;
pub const METADATA_NAME_LIMIT: usize = 128;

/// A namespaced, scoped value owned by the generic automation substrate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MetadataRecord {
    pub scope: String,
    pub namespace: String,
    pub key: String,
    pub target: Option<CommandTarget>,
    pub value: Value,
    pub expires_at_ms: Option<u64>,
    pub provenance: Value,
    pub generation: u64,
}

/// Input for an authoritative metadata upsert. Expiry is an absolute Unix
/// timestamp so callers and deterministic tests do not depend on a hidden
/// clock abstraction.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetadataPublication {
    pub scope: String,
    pub namespace: String,
    pub key: String,
    pub target: Option<CommandTarget>,
    pub value: Value,
    pub expires_at_ms: Option<u64>,
    pub provenance: Value,
}

impl MetadataPublication {
    pub fn new(
        scope: impl Into<String>,
        namespace: impl Into<String>,
        key: impl Into<String>,
        target: Option<CommandTarget>,
        value: Value,
        expires_at_ms: Option<u64>,
        provenance: Value,
    ) -> Self {
        Self {
            scope: scope.into(),
            namespace: namespace.into(),
            key: key.into(),
            target,
            value,
            expires_at_ms,
            provenance,
        }
    }
}

/// Cloneable, thread-safe automation seam shared by control, the UI state, and
/// future backend or extension adapters.
#[derive(Clone)]
pub struct AutomationHub {
    events: EventHub,
    tasks: TaskHub,
    metadata: MetadataHub,
    instance_scope: Arc<Mutex<Option<String>>>,
}

impl Default for AutomationHub {
    fn default() -> Self {
        Self::new()
    }
}

impl AutomationHub {
    pub fn new() -> Self {
        let events = EventHub::new();
        for &topic in BUILTIN_EVENT_TOPICS {
            events
                .register_topic(topic)
                .expect("the built-in event topics are valid and bounded");
        }
        let tasks = TaskHub::new(events.clone());
        let metadata = MetadataHub::new(events.clone());
        Self {
            events,
            tasks,
            metadata,
            instance_scope: Arc::new(Mutex::new(None)),
        }
    }

    pub fn events(&self) -> &EventHub {
        &self.events
    }

    pub fn tasks(&self) -> &TaskHub {
        &self.tasks
    }

    pub fn metadata(&self) -> &MetadataHub {
        &self.metadata
    }

    pub fn register_event_topic(&self, topic: impl AsRef<str>) -> Result<(), AutomationError> {
        self.events.register_topic(topic)
    }
    /// Retires an extension-owned topic and all subscriptions that selected
    /// it. Built-in topics are never removed by extension cleanup.
    pub fn unregister_event_topic(&self, topic: &str) -> Result<(), AutomationError> {
        self.events.unregister_topic(topic)
    }

    pub fn publish_event(&self, publication: EventPublication) -> Result<u64, AutomationError> {
        self.events.publish(publication)
    }

    pub fn publish_event_with_snapshot(
        &self,
        publication: EventPublication,
        snapshot: Value,
    ) -> Result<u64, AutomationError> {
        self.events.publish_with_snapshot(publication, snapshot)
    }

    pub fn publish_event_with_snapshot_source(
        &self,
        publication: EventPublication,
        source: impl Into<String>,
        snapshot: Value,
    ) -> Result<u64, AutomationError> {
        self.events
            .publish_with_snapshot_source(publication, source, snapshot)
    }

    pub fn publish_events_with_snapshot_sources(
        &self,
        publications: impl IntoIterator<Item = (EventPublication, Vec<(String, String, Value)>)>,
    ) -> Result<Vec<u64>, AutomationError> {
        self.events
            .publish_batch_with_snapshot_sources(publications)
    }

    pub fn publish_terminal_output(
        &self,
        scope: impl Into<String>,
        provenance: Value,
        target: CommandTarget,
        payload: Value,
    ) -> Result<u64, AutomationError> {
        self.events
            .publish_terminal_output(scope, provenance, target, payload)
    }

    pub fn terminal_output_after(
        &self,
        scope: &str,
        target: &CommandTarget,
        cursor: u64,
    ) -> Result<TerminalOutputRead, AutomationError> {
        self.events.terminal_output_after(scope, target, cursor)
    }

    pub fn publish_command_completion(
        &self,
        scope: String,
        owner: &OwnerIdentity,
        invocation: &CommandInvocation,
        outcome: Value,
    ) -> Result<u64, AutomationError> {
        self.events.publish(EventPublication::new(
            scope,
            COMMAND_COMPLETED_TOPIC,
            json!({"caller": invocation.caller, "owner_pid": owner.pid()}),
            invocation.target.clone(),
            json!({"command": invocation.command, "outcome": outcome}),
        ))
    }

    /// Removes an exact process generation's subscriptions and cancels only its
    /// running tasks. A same-PID replacement cannot affect the old owner's work.
    pub fn disconnect_owner(&self, owner: &OwnerIdentity) {
        self.events.disconnect_owner(owner);
        self.tasks.cancel_owner(owner);
    }
    pub fn disconnect_owner_checked(
        &self,
        owner: &OwnerIdentity,
    ) -> Result<usize, AutomationError> {
        self.events.disconnect_owner(owner);
        self.tasks.cancel_owner_checked(owner)
    }

    pub fn reap_dead_owners(&self) {
        let owners = self
            .events
            .owners()
            .into_iter()
            .chain(self.tasks.owners())
            .collect::<BTreeSet<_>>();
        if owners.is_empty() {
            return;
        }
        let system = sysinfo::System::new_all();
        for owner in owners {
            if !owner.is_live_in(&system) {
                self.disconnect_owner(&owner);
            }
        }
    }
    pub fn cancel_all_tasks_checked(&self) -> Result<usize, AutomationError> {
        self.tasks.cancel_all_checked()
    }

    pub fn cancel_tasks_in_scope_checked(&self, scope: &str) -> Result<usize, AutomationError> {
        self.tasks.cancel_scope_checked(scope)
    }

    pub fn cancel_all_tasks(&self) {
        self.tasks.cancel_all();
    }

    pub fn cancel_tasks_in_scope(&self, scope: &str) -> usize {
        self.tasks.cancel_scope(scope)
    }

    pub fn reap_expired_metadata(&self) -> Result<usize, AutomationError> {
        self.metadata.reap_expired()
    }

    /// Binds this process-wide hub to the control instance that owns it.
    ///
    /// The binding is immutable so UI-side producers cannot accidentally emit
    /// events into a different control instance after startup.
    pub fn bind_instance_scope(&self, scope: impl Into<String>) -> Result<(), AutomationError> {
        let scope = scope.into();
        if scope.is_empty() {
            return Err(AutomationError::new(
                -32602,
                "instance event scope must not be empty",
            ));
        }
        let initialize = {
            let mut bound = lock(&self.instance_scope);
            match bound.as_deref() {
                Some(current) if current != scope => {
                    return Err(AutomationError::new(
                        -32603,
                        "automation hub is already bound to another instance scope",
                    ));
                }
                Some(_) => false,
                None => {
                    *bound = Some(scope.clone());
                    true
                }
            }
        };
        if initialize {
            self.events.replace_snapshot_fragments(
                scope.clone(),
                "extension.reloaded",
                [
                    (
                        "runtime".to_owned(),
                        json!({"modules": [], "commands": [], "events": []}),
                    ),
                    ("status".to_owned(), json!({"modules": []})),
                    ("sidebar".to_owned(), json!({"modules": []})),
                ],
            )?;
            self.metadata.install_snapshot(&scope)?;
            self.tasks.install_snapshot(&scope)?;
        }
        Ok(())
    }

    pub fn instance_scope(&self) -> Option<String> {
        lock(&self.instance_scope).clone()
    }

    pub fn shares_state_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.events.state, &other.events.state)
            && Arc::ptr_eq(&self.tasks.state, &other.tasks.state)
            && Arc::ptr_eq(&self.metadata.state, &other.metadata.state)
            && Arc::ptr_eq(&self.instance_scope, &other.instance_scope)
    }
}
#[derive(Clone)]
pub struct EventHub {
    state: Arc<Mutex<EventState>>,
    metadata_state: Arc<Mutex<Option<Weak<Mutex<MetadataState>>>>>,
    #[cfg(test)]
    before_publication_lock: Arc<Mutex<Option<std::sync::mpsc::Sender<()>>>>,
    #[cfg(test)]
    before_publication_notified: Arc<Mutex<bool>>,
}

struct EventState {
    registered_topics: BTreeSet<String>,
    next_subscription: u64,
    subscriptions: BTreeMap<String, SubscriptionRecord>,
    retired_subscriptions: BTreeSet<String>,
    retired_subscription_order: VecDeque<String>,
    retired_subscription_watermark: u64,
    revisions: BTreeMap<String, u64>,
    snapshots: BTreeMap<String, BTreeMap<String, Value>>,
    snapshot_fragments: BTreeMap<String, BTreeMap<String, BTreeMap<String, Value>>>,
    live_binding_scopes: BTreeSet<String>,
    live_extension_scopes: BTreeSet<String>,
    output_streams: BTreeMap<String, TerminalOutputStream>,
    output_stream_order: VecDeque<String>,
    output_tombstones: BTreeMap<String, u64>,
    output_tombstone_order: VecDeque<String>,
}

impl EventHub {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(EventState {
                registered_topics: BTreeSet::new(),
                next_subscription: 1,
                subscriptions: BTreeMap::new(),
                retired_subscriptions: BTreeSet::new(),
                retired_subscription_order: VecDeque::new(),
                retired_subscription_watermark: 0,
                revisions: BTreeMap::new(),
                snapshots: BTreeMap::new(),
                snapshot_fragments: BTreeMap::new(),
                live_binding_scopes: BTreeSet::new(),
                live_extension_scopes: BTreeSet::new(),
                output_streams: BTreeMap::new(),
                output_stream_order: VecDeque::new(),
                output_tombstones: BTreeMap::new(),
                output_tombstone_order: VecDeque::new(),
            })),
            metadata_state: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            before_publication_lock: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            before_publication_notified: Arc::new(Mutex::new(false)),
        }
    }

    fn set_metadata_state(&self, state: Weak<Mutex<MetadataState>>) {
        *lock(&self.metadata_state) = Some(state);
    }

    fn reap_expired_metadata_before_poll(&self) -> Result<(), AutomationError> {
        let metadata_state = lock(&self.metadata_state).clone();
        let Some(metadata_state) = metadata_state.and_then(|state| state.upgrade()) else {
            return Ok(());
        };
        MetadataHub {
            state: metadata_state,
            events: self.clone(),
        }
        .reap_expired()
        .map(|_| ())
    }

    pub fn register_topic(&self, topic: impl AsRef<str>) -> Result<(), AutomationError> {
        let topic = topic.as_ref();
        validate_topic_name(topic)?;
        let mut state = lock(&self.state);
        if state.registered_topics.contains(topic) {
            return Ok(());
        }
        if state.registered_topics.len() >= MAX_REGISTERED_EVENT_TOPICS {
            return Err(AutomationError::new(-32001, "event topic limit reached"));
        }
        state.registered_topics.insert(topic.to_owned());
        Ok(())
    }

    pub fn unregister_topic(&self, topic: &str) -> Result<(), AutomationError> {
        validate_topic_name(topic)?;
        if BUILTIN_EVENT_TOPICS.contains(&topic) {
            return Err(AutomationError::new(
                -32603,
                "built-in event topics cannot be unregistered",
            ));
        }
        let mut state = lock(&self.state);
        if !state.registered_topics.remove(topic) {
            return Ok(());
        }
        let removed = state
            .subscriptions
            .iter()
            .filter(|(_, subscription)| subscription.topics.contains(topic))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in removed {
            state.subscriptions.remove(&id);
            retire_subscription_locked(&mut state, id);
        }
        for snapshots in state.snapshots.values_mut() {
            snapshots.remove(topic);
        }
        for fragments in state.snapshot_fragments.values_mut() {
            fragments.remove(topic);
        }
        Ok(())
    }

    #[cfg(test)]
    fn set_before_publication_lock(&self, sender: Option<std::sync::mpsc::Sender<()>>) {
        *lock(&self.before_publication_notified) = false;
        *lock(&self.before_publication_lock) = sender;
    }

    #[cfg(test)]
    fn notify_before_publication_lock(&self) {
        let should_notify = {
            let mut notified = lock(&self.before_publication_notified);
            if *notified {
                false
            } else {
                *notified = true;
                true
            }
        };
        if should_notify && let Some(sender) = lock(&self.before_publication_lock).clone() {
            let _ = sender.send(());
        }
    }

    #[cfg(test)]
    fn clear_before_publication_notification(&self) {
        *lock(&self.before_publication_notified) = false;
    }

    pub fn topic_registered(&self, topic: &str) -> bool {
        lock(&self.state).registered_topics.contains(topic)
    }

    pub fn registered_topics(&self) -> BTreeSet<String> {
        lock(&self.state).registered_topics.clone()
    }

    /// Whether a source has installed authoritative bootstrap state for a
    /// scope. Control-plane callers use this to reject syntactically valid but
    /// nonexistent binding scopes.
    pub fn scope_has_snapshot(&self, scope: &str) -> bool {
        lock(&self.state).snapshots.contains_key(scope)
    }
    /// Binding scopes are authorized independently from their snapshots so a
    /// retired scope cannot stay subscribable merely because stale bootstrap
    /// state was retained.
    pub fn binding_scope_is_live(&self, scope: &str) -> bool {
        lock(&self.state).live_binding_scopes.contains(scope)
    }

    /// Replaces the owner-local set of live binding scopes and atomically
    /// purges every event artifact for scopes that were retired.
    pub fn replace_live_binding_scopes(&self, scopes: impl IntoIterator<Item = String>) {
        let live_binding_scopes = scopes.into_iter().collect::<BTreeSet<_>>();
        let mut state = lock(&self.state);
        let retired = state
            .live_binding_scopes
            .difference(&live_binding_scopes)
            .cloned()
            .collect::<Vec<_>>();
        state.live_binding_scopes = live_binding_scopes;
        for scope in retired {
            purge_retired_scope_locked(&mut state, &scope);
        }
    }

    /// Returns whether an extension generation is still authorized to expose
    /// event subscriptions through the control plane.
    pub fn extension_scope_is_live(&self, scope: &str) -> bool {
        lock(&self.state).live_extension_scopes.contains(scope)
    }

    /// Atomically replaces live extension generations and purges all retained
    /// event/output state for generations that have been retired.
    pub fn replace_live_extension_scopes(&self, scopes: impl IntoIterator<Item = String>) {
        let live_extension_scopes = scopes.into_iter().collect::<BTreeSet<_>>();
        let mut state = lock(&self.state);
        let retired = state
            .live_extension_scopes
            .difference(&live_extension_scopes)
            .cloned()
            .collect::<Vec<_>>();
        state.live_extension_scopes = live_extension_scopes;
        for scope in retired {
            purge_retired_scope_locked(&mut state, &scope);
        }
    }

    /// Marks one extension generation live or retired without replacing the
    /// other generation registrations.
    pub fn set_extension_scope_live(&self, scope: impl Into<String>, live: bool) {
        let scope = scope.into();
        let mut state = lock(&self.state);
        if live {
            state.live_extension_scopes.insert(scope);
        } else {
            state.live_extension_scopes.remove(&scope);
            purge_retired_scope_locked(&mut state, &scope);
        }
    }

    pub fn subscribe(
        &self,
        owner: OwnerIdentity,
        topics: BTreeSet<String>,
        scope: String,
    ) -> Result<EventDelivery, AutomationError> {
        if scope.is_empty() {
            return Err(AutomationError::new(
                -32602,
                "event scope must not be empty",
            ));
        }
        let mut state = lock(&self.state);
        validate_subscription_topics(&state, &topics)?;
        if is_binding_scope(&scope) && !state.live_binding_scopes.contains(&scope) {
            return Err(AutomationError::new(
                -32006,
                "binding event scope is not live",
            ));
        }
        if is_extension_scope(&scope) && !state.live_extension_scopes.contains(&scope) {
            return Err(AutomationError::new(
                -32006,
                "extension event scope is not live",
            ));
        }
        reap_dead_subscriptions(&mut state);
        if state.subscriptions.len() >= MAX_SUBSCRIPTIONS {
            return Err(AutomationError::new(-32001, "subscription limit reached"));
        }
        let next_subscription = state.next_subscription;
        state.next_subscription = next_subscription
            .checked_add(1)
            .ok_or_else(|| AutomationError::new(-32003, "subscription ID exhausted"))?;
        let id = format!("subscription-{next_subscription}");
        let revision = *state.revisions.get(&scope).unwrap_or(&0);
        state.subscriptions.insert(
            id.clone(),
            SubscriptionRecord {
                owner,
                topics,
                scope: scope.clone(),
                sequence: 0,
                cursor: 0,
                queued_bytes: 0,
                events: VecDeque::new(),
                gap: None,
            },
        );
        Ok(EventDelivery {
            subscription: id,
            scope,
            revision,
            cursor: 0,
            events: Vec::new(),
        })
    }

    pub fn poll(
        &self,
        subscription: &str,
        owner: &OwnerIdentity,
        cursor: u64,
    ) -> Result<EventDelivery, AutomationError> {
        self.reap_expired_metadata_before_poll()?;
        let mut state = lock(&self.state);
        let (scope, cursor, events) = {
            let record = owned_subscription(&mut state, subscription, owner)?;
            if let Some(gap) = &record.gap {
                return Err(rebase_error(
                    subscription,
                    &record.scope,
                    record.cursor,
                    gap.sequence,
                ));
            }
            if cursor != record.cursor {
                return Err(rebase_error(
                    subscription,
                    &record.scope,
                    record.cursor,
                    record.sequence,
                ));
            }
            let mut events = Vec::new();
            let mut bytes = 0;
            while let Some(queued) = record.events.front() {
                if bytes + queued.bytes > EVENT_QUEUE_BYTE_LIMIT {
                    break;
                }
                let queued = record.events.pop_front().expect("front event exists");
                bytes += queued.bytes;
                record.queued_bytes = record.queued_bytes.saturating_sub(queued.bytes);
                events.push(queued.event);
            }
            if events.is_empty() && !record.events.is_empty() {
                record.events.clear();
                record.queued_bytes = 0;
                record.gap = Some(SubscriptionGap {
                    sequence: record.sequence,
                });
                return Err(rebase_error(
                    subscription,
                    &record.scope,
                    record.cursor,
                    record.sequence,
                ));
            }
            if let Some(event) = events.last() {
                record.cursor = event.sequence;
            }
            (record.scope.clone(), record.cursor, events)
        };
        let revision = *state.revisions.get(&scope).unwrap_or(&0);
        Ok(EventDelivery {
            subscription: subscription.to_owned(),
            scope,
            revision,
            cursor,
            events,
        })
    }

    pub fn unsubscribe(
        &self,
        subscription: &str,
        owner: &OwnerIdentity,
    ) -> Result<EventUnsubscription, AutomationError> {
        let mut state = lock(&self.state);
        owned_subscription(&mut state, subscription, owner)?;
        state.subscriptions.remove(subscription);
        Ok(EventUnsubscription {
            unsubscribed: subscription.to_owned(),
        })
    }

    /// Atomically replaces a set of authoritative source snapshots for one
    /// scope. Existing snapshots from unrelated sources remain intact.
    pub fn replace_snapshots(
        &self,
        scope: impl Into<String>,
        snapshots: impl IntoIterator<Item = (String, Value)>,
    ) -> Result<(), AutomationError> {
        let scope = scope.into();
        if scope.is_empty() {
            return Err(AutomationError::new(
                -32602,
                "event scope must not be empty",
            ));
        }
        let snapshots = snapshots.into_iter().collect::<BTreeMap<_, _>>();
        let mut state = lock(&self.state);
        replace_snapshots_locked(&mut state, &scope, snapshots)?;
        Ok(())
    }

    /// Replaces all source fragments for one authoritative topic and stores
    /// their merged view for subscriptions and rebases.
    pub fn replace_snapshot_fragments(
        &self,
        scope: impl Into<String>,
        topic: impl Into<String>,
        fragments: impl IntoIterator<Item = (String, Value)>,
    ) -> Result<(), AutomationError> {
        let scope = scope.into();
        let topic = topic.into();
        if scope.is_empty() {
            return Err(AutomationError::new(
                -32602,
                "event scope must not be empty",
            ));
        }
        let fragments = fragments.into_iter().collect::<BTreeMap<_, _>>();
        let mut state = lock(&self.state);
        validate_snapshot_fragments_locked(&state, &topic, &fragments)?;
        let merged = merge_snapshot_fragments(&fragments);
        state
            .snapshot_fragments
            .entry(scope.clone())
            .or_default()
            .insert(topic.clone(), fragments);
        state
            .snapshots
            .entry(scope)
            .or_default()
            .insert(topic, merged);
        Ok(())
    }

    /// Atomically replaces authoritative source snapshots and refreshes the
    /// terminal-output index from the streams retained for this scope.
    pub fn replace_snapshots_with_terminal_output(
        &self,
        scope: impl Into<String>,
        snapshots: impl IntoIterator<Item = (String, Value)>,
    ) -> Result<(), AutomationError> {
        let scope = scope.into();
        if scope.is_empty() {
            return Err(AutomationError::new(
                -32602,
                "event scope must not be empty",
            ));
        }
        let mut snapshots = snapshots.into_iter().collect::<BTreeMap<_, _>>();
        let mut state = lock(&self.state);
        validate_registered_topic(&state, TERMINAL_OUTPUT_TOPIC)?;
        let output_snapshot = terminal_output_snapshot(&state, &scope);
        validate_snapshot_size(&output_snapshot)?;
        snapshots.insert(TERMINAL_OUTPUT_TOPIC.to_owned(), output_snapshot);
        replace_snapshots_locked(&mut state, &scope, snapshots)?;
        Ok(())
    }

    pub fn set_snapshot(
        &self,
        scope: impl Into<String>,
        topic: impl AsRef<str>,
        snapshot: Value,
    ) -> Result<(), AutomationError> {
        self.replace_snapshots(scope, [(topic.as_ref().to_owned(), snapshot)])
    }

    /// Installs an authoritative empty/current output-stream index before a
    /// client subscribes. Output bytes remain cursor-addressed through
    /// [`Self::terminal_output_after`].
    pub fn set_terminal_output_snapshot(
        &self,
        scope: impl Into<String>,
    ) -> Result<(), AutomationError> {
        self.replace_snapshots_with_terminal_output(scope, std::iter::empty::<(String, Value)>())
    }

    /// Removes output streams and reread tombstones that no longer identify a
    /// live resource. The caller supplies the exact retired-target predicate;
    /// no target is ever rewritten to a replacement generation.
    pub fn purge_terminal_output(
        &self,
        scope: &str,
        mut is_retired: impl FnMut(&CommandTarget) -> bool,
    ) -> Result<usize, AutomationError> {
        let mut state = lock(&self.state);
        let stream_keys = state
            .output_streams
            .iter()
            .filter(|(_, stream)| stream.scope == scope && is_retired(&stream.target))
            .map(|(key, _)| key.clone())
            .collect::<BTreeSet<_>>();
        let tombstone_keys = state
            .output_tombstones
            .keys()
            .filter_map(|key| {
                terminal_output_key_target(key)
                    .filter(|(key_scope, target)| key_scope == scope && is_retired(target))
                    .map(|_| key.clone())
            })
            .collect::<Vec<_>>();

        let snapshot = if stream_keys.is_empty() && tombstone_keys.is_empty() {
            None
        } else {
            validate_registered_topic(&state, TERMINAL_OUTPUT_TOPIC)?;
            let snapshot = terminal_output_snapshot_excluding(&state, scope, &stream_keys);
            validate_snapshot_size(&snapshot)?;
            Some(snapshot)
        };

        for key in &stream_keys {
            state.output_streams.remove(key);
        }
        state
            .output_stream_order
            .retain(|key| !stream_keys.contains(key));
        for key in &tombstone_keys {
            state.output_tombstones.remove(key);
        }
        state
            .output_tombstone_order
            .retain(|key| !tombstone_keys.contains(key));

        if let Some(snapshot) = snapshot {
            state
                .snapshots
                .entry(scope.to_owned())
                .or_default()
                .insert(TERMINAL_OUTPUT_TOPIC.to_owned(), snapshot);
        }
        Ok(stream_keys.len() + tombstone_keys.len())
    }

    pub fn snapshot(
        &self,
        scope: &str,
        topics: &BTreeSet<String>,
    ) -> Result<EventSnapshot, AutomationError> {
        let state = lock(&self.state);
        validate_subscription_topics(&state, topics)?;
        snapshot_locked(&state, scope, topics)
    }

    /// Reads bootstrap state for an owned subscription under the same mutex
    /// that protects its scope, topic set, and source revisions.
    pub fn snapshot_for_subscription(
        &self,
        subscription: &str,
        owner: &OwnerIdentity,
    ) -> Result<EventSnapshot, AutomationError> {
        let mut state = lock(&self.state);
        let (scope, topics) = {
            let record = owned_subscription(&mut state, subscription, owner)?;
            (record.scope.clone(), record.topics.clone())
        };
        snapshot_locked(&state, &scope, &topics)
    }

    /// Clears a stale subscription and reads its snapshot under one mutex. No
    /// event can land between the snapshot revision and replacement cursor.
    pub fn rebase(
        &self,
        subscription: &str,
        owner: &OwnerIdentity,
    ) -> Result<EventRebase, AutomationError> {
        let mut state = lock(&self.state);
        let (scope, topics) = {
            let record = owned_subscription(&mut state, subscription, owner)?;
            (record.scope.clone(), record.topics.clone())
        };
        let snapshot = snapshot_locked(&state, &scope, &topics)?;
        let cursor = {
            let record = owned_subscription(&mut state, subscription, owner)?;
            record.events.clear();
            record.queued_bytes = 0;
            record.gap = None;
            record.cursor = record.sequence;
            record.cursor
        };
        Ok(EventRebase {
            subscription: subscription.to_owned(),
            scope,
            revision: snapshot.revision,
            cursor,
            snapshot,
        })
    }

    pub fn publish(&self, publication: EventPublication) -> Result<u64, AutomationError> {
        let mut batch = self.begin_publication_batch();
        let prepared = batch.prepare(publication, std::iter::empty())?;
        Ok(batch.commit_one(prepared))
    }

    pub fn publish_with_snapshot(
        &self,
        publication: EventPublication,
        snapshot: Value,
    ) -> Result<u64, AutomationError> {
        let topic = publication.topic.clone();
        self.publish_with_snapshots(publication, [(topic, snapshot)])
    }

    /// Installs every affected source snapshot and publishes one event under a
    /// single event-state mutex, so a rebase cannot observe a half-updated
    /// source bundle.
    pub fn publish_with_snapshots(
        &self,
        publication: EventPublication,
        snapshots: impl IntoIterator<Item = (String, Value)>,
    ) -> Result<u64, AutomationError> {
        if publication.scope.is_empty() {
            return Err(AutomationError::new(
                -32602,
                "event scope must not be empty",
            ));
        }
        let mut batch = self.begin_publication_batch();
        let prepared = batch.prepare(publication, snapshots)?;
        Ok(batch.commit_one(prepared))
    }

    pub fn publish_with_snapshot_source(
        &self,
        publication: EventPublication,
        source: impl Into<String>,
        snapshot: Value,
    ) -> Result<u64, AutomationError> {
        let topic = publication.topic.clone();
        let mut revisions = self.publish_batch_with_snapshot_sources([(
            publication,
            vec![(topic, source.into(), snapshot)],
        )])?;
        Ok(revisions
            .pop()
            .expect("single publication yields one revision"))
    }

    /// Preflights and commits every event and source-fragment update while
    /// holding one event-state lock. No event or snapshot is committed if any
    /// publication fails validation.
    pub fn publish_batch_with_snapshot_sources(
        &self,
        publications: impl IntoIterator<Item = (EventPublication, Vec<(String, String, Value)>)>,
    ) -> Result<Vec<u64>, AutomationError> {
        let mut batch = self.begin_publication_batch();
        let prepared = publications
            .into_iter()
            .map(|(publication, sources)| {
                batch.prepare_with_sources(publication, std::iter::empty(), sources)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(batch.commit_all(prepared))
    }

    fn begin_publication_batch(&self) -> EventPublicationBatch<'_> {
        #[cfg(test)]
        self.notify_before_publication_lock();
        EventPublicationBatch {
            #[cfg(test)]
            hub: self,
            state: lock(&self.state),
            revisions: BTreeMap::new(),
            sequences: BTreeMap::new(),
            pending_fragments: BTreeMap::new(),
        }
    }

    pub fn publish_terminal_output(
        &self,
        scope: impl Into<String>,
        provenance: Value,
        target: CommandTarget,
        payload: Value,
    ) -> Result<u64, AutomationError> {
        let scope = scope.into();
        if scope.is_empty() {
            return Err(AutomationError::new(
                -32602,
                "event scope must not be empty",
            ));
        }
        let mut state = lock(&self.state);
        validate_registered_topic(&state, TERMINAL_OUTPUT_TOPIC)?;
        let projection = terminal_output_projection(&state, &scope, &target, &payload)?;
        let publication = EventPublication::new(
            scope,
            TERMINAL_OUTPUT_TOPIC,
            provenance,
            Some(target),
            json!({"cursor": projection.stream.cursor, "data": payload}),
        );
        validate_publication_locked(&state, &publication)?;

        let new_stream = !state.output_streams.contains_key(&projection.key);
        if new_stream {
            for _ in 0..projection.order_pops {
                let Some(oldest) = state.output_stream_order.pop_front() else {
                    break;
                };
                if let Some(removed) = state.output_streams.remove(&oldest) {
                    remember_output_tombstone(&mut state, oldest, removed.cursor);
                }
            }
            state.output_tombstones.remove(&projection.key);
            state
                .output_tombstone_order
                .retain(|known| known != &projection.key);
            state.output_stream_order.push_back(projection.key.clone());
        }
        state
            .output_streams
            .insert(projection.key, projection.stream);
        for (affected_scope, snapshot) in projection.snapshots {
            state
                .snapshots
                .entry(affected_scope)
                .or_default()
                .insert(TERMINAL_OUTPUT_TOPIC.to_owned(), snapshot);
        }
        Ok(publish_locked_prevalidated(&mut state, publication))
    }

    pub fn terminal_output_after(
        &self,
        scope: &str,
        target: &CommandTarget,
        cursor: u64,
    ) -> Result<TerminalOutputRead, AutomationError> {
        let state = lock(&self.state);
        let key = terminal_output_key(scope, target);
        let Some(stream) = state.output_streams.get(&key) else {
            // A bounded tombstone index can eventually forget an old key. An
            // empty reply would then falsely claim that a formerly observed
            // stream had no output, so absence is always a reread boundary.
            let stream_cursor = state.output_tombstones.get(&key).copied().unwrap_or(0);
            return Err(output_rebase_error(scope, target, cursor, stream_cursor));
        };
        if cursor > stream.cursor {
            return Err(output_rebase_error(scope, target, cursor, stream.cursor));
        }
        if cursor < stream.cursor {
            let expected = cursor.saturating_add(1);
            let available = stream.chunks.front().map(|chunk| chunk.cursor);
            if available.is_none_or(|available| available > expected) {
                return Err(output_rebase_error(scope, target, cursor, stream.cursor));
            }
        }
        Ok(TerminalOutputRead {
            scope: stream.scope.clone(),
            target: stream.target.clone(),
            cursor: stream.cursor,
            chunks: stream
                .chunks
                .iter()
                .filter(|chunk| chunk.cursor > cursor)
                .cloned()
                .collect(),
        })
    }

    fn disconnect_owner(&self, owner: &OwnerIdentity) {
        lock(&self.state)
            .subscriptions
            .retain(|_, subscription| &subscription.owner != owner);
    }

    fn owners(&self) -> BTreeSet<OwnerIdentity> {
        lock(&self.state)
            .subscriptions
            .values()
            .map(|subscription| subscription.owner.clone())
            .collect()
    }
}
struct EventPublicationBatch<'a> {
    #[cfg(test)]
    hub: &'a EventHub,
    state: MutexGuard<'a, EventState>,
    revisions: BTreeMap<String, u64>,
    sequences: BTreeMap<String, u64>,
    pending_fragments: BTreeMap<(String, String), BTreeMap<String, Value>>,
}

struct PreparedEventPublication {
    publication: EventPublication,
    snapshots: BTreeMap<String, Value>,
    snapshot_fragments: Vec<(String, String, Value)>,
}

impl EventPublicationBatch<'_> {
    fn prepare(
        &mut self,
        publication: EventPublication,
        snapshots: impl IntoIterator<Item = (String, Value)>,
    ) -> Result<PreparedEventPublication, AutomationError> {
        self.prepare_with_sources(publication, snapshots, std::iter::empty())
    }

    fn prepare_with_sources(
        &mut self,
        publication: EventPublication,
        snapshots: impl IntoIterator<Item = (String, Value)>,
        source_snapshots: impl IntoIterator<Item = (String, String, Value)>,
    ) -> Result<PreparedEventPublication, AutomationError> {
        let snapshots = snapshots.into_iter().collect::<BTreeMap<_, _>>();
        let source_snapshots = source_snapshots.into_iter().collect::<Vec<_>>();
        validate_snapshots_locked(&self.state, &snapshots)?;
        let mut candidates = BTreeMap::<(String, String), BTreeMap<String, Value>>::new();
        for (topic, source, snapshot) in &source_snapshots {
            let key = (publication.scope.clone(), topic.clone());
            let fragments = candidates.entry(key.clone()).or_insert_with(|| {
                self.pending_fragments
                    .get(&key)
                    .cloned()
                    .or_else(|| {
                        self.state
                            .snapshot_fragments
                            .get(&publication.scope)
                            .and_then(|topics| topics.get(topic))
                            .cloned()
                    })
                    .unwrap_or_default()
            });
            fragments.insert(source.clone(), snapshot.clone());
        }
        for ((_, topic), fragments) in &candidates {
            validate_snapshot_fragments_locked(&self.state, topic, fragments)?;
        }
        validate_publication_shape_locked(&self.state, &publication)?;
        let revision = self
            .revisions
            .get(&publication.scope)
            .copied()
            .or_else(|| self.state.revisions.get(&publication.scope).copied())
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| AutomationError::new(-32003, "event revision exhausted"))?;
        let mut sequences = Vec::new();
        for (subscription_id, subscription) in &self.state.subscriptions {
            if subscription.scope != publication.scope
                || !subscription.topics.contains(&publication.topic)
            {
                continue;
            }
            let sequence = self
                .sequences
                .get(subscription_id)
                .copied()
                .unwrap_or(subscription.sequence)
                .checked_add(1)
                .ok_or_else(|| AutomationError::new(-32003, "event sequence exhausted"))?;
            let event = EventEnvelope {
                sequence,
                scope: publication.scope.clone(),
                revision,
                topic: publication.topic.clone(),
                provenance: publication.provenance.clone(),
                target: publication.target.clone(),
                payload: publication.payload.clone(),
            };
            value_size(&event)?;
            sequences.push((subscription_id.clone(), sequence));
        }
        self.revisions.insert(publication.scope.clone(), revision);
        for (subscription_id, sequence) in sequences {
            self.sequences.insert(subscription_id, sequence);
        }
        for (key, fragments) in candidates {
            self.pending_fragments.insert(key, fragments);
        }
        Ok(PreparedEventPublication {
            publication,
            snapshots,
            snapshot_fragments: source_snapshots,
        })
    }

    fn commit_one(&mut self, prepared: PreparedEventPublication) -> u64 {
        let PreparedEventPublication {
            publication,
            snapshots,
            snapshot_fragments,
        } = prepared;
        replace_snapshots_prevalidated(&mut self.state, &publication.scope, snapshots);
        for (topic, source, snapshot) in snapshot_fragments {
            replace_snapshot_fragment_prevalidated(
                &mut self.state,
                &publication.scope,
                &topic,
                &source,
                snapshot,
            );
        }
        publish_locked_prevalidated(&mut self.state, publication)
    }

    fn commit_all(&mut self, prepared: Vec<PreparedEventPublication>) -> Vec<u64> {
        prepared
            .into_iter()
            .map(|prepared| self.commit_one(prepared))
            .collect()
    }

    fn replace_snapshots(
        &mut self,
        scope: &str,
        snapshots: impl IntoIterator<Item = (String, Value)>,
    ) -> Result<(), AutomationError> {
        let snapshots = snapshots.into_iter().collect::<BTreeMap<_, _>>();
        validate_snapshots_locked(&self.state, &snapshots)?;
        replace_snapshots_prevalidated(&mut self.state, scope, snapshots);
        Ok(())
    }
}

impl Drop for EventPublicationBatch<'_> {
    fn drop(&mut self) {
        #[cfg(test)]
        self.hub.clear_before_publication_notification();
    }
}

struct SubscriptionRecord {
    owner: OwnerIdentity,
    topics: BTreeSet<String>,
    scope: String,
    sequence: u64,
    cursor: u64,
    queued_bytes: usize,
    events: VecDeque<QueuedEvent>,
    gap: Option<SubscriptionGap>,
}

struct QueuedEvent {
    bytes: usize,
    event: EventEnvelope,
}

struct SubscriptionGap {
    sequence: u64,
}

#[derive(Clone)]
struct TerminalOutputStream {
    scope: String,
    target: CommandTarget,
    cursor: u64,
    retained_bytes: usize,
    chunks: VecDeque<TerminalOutputChunk>,
}

struct TerminalOutputProjection {
    key: String,
    stream: TerminalOutputStream,
    order_pops: usize,
    evicted_keys: BTreeSet<String>,
    snapshots: BTreeMap<String, Value>,
}

fn terminal_output_snapshot(state: &EventState, scope: &str) -> Value {
    terminal_output_snapshot_excluding(state, scope, &BTreeSet::new())
}

fn terminal_output_snapshot_excluding(
    state: &EventState,
    scope: &str,
    excluded: &BTreeSet<String>,
) -> Value {
    let streams = state
        .output_streams
        .iter()
        .filter(|(key, stream)| stream.scope == scope && !excluded.contains(key.as_str()))
        .map(|(key, stream)| (key.clone(), terminal_output_snapshot_entry(stream)))
        .collect::<Vec<_>>();
    terminal_output_snapshot_value(streams)
}

fn terminal_output_snapshot_with_projection(
    state: &EventState,
    scope: &str,
    projection: &TerminalOutputProjection,
) -> Value {
    let mut streams = state
        .output_streams
        .iter()
        .filter(|(key, stream)| key.as_str() != projection.key.as_str() && stream.scope == scope)
        .map(|(key, stream)| (key.clone(), terminal_output_snapshot_entry(stream)))
        .collect::<Vec<_>>();
    streams.retain(|(key, _)| !projection.evicted_keys.contains(key));
    if projection.stream.scope == scope {
        streams.push((
            projection.key.clone(),
            terminal_output_snapshot_entry(&projection.stream),
        ));
    }
    terminal_output_snapshot_value(streams)
}

fn terminal_output_snapshot_value(mut streams: Vec<(String, Value)>) -> Value {
    streams.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    json!({
        "streams": streams
            .into_iter()
            .map(|(_, stream)| stream)
            .collect::<Vec<_>>(),
    })
}

fn terminal_output_snapshot_entry(stream: &TerminalOutputStream) -> Value {
    json!({
        "target": &stream.target,
        "cursor": stream.cursor,
        "retained_from": stream
            .chunks
            .front()
            .map(|chunk| chunk.cursor)
            .unwrap_or_else(|| stream.cursor.saturating_add(1)),
    })
}

fn remember_output_tombstone(state: &mut EventState, key: String, cursor: u64) {
    if !state.output_tombstones.contains_key(&key) {
        while state.output_tombstones.len() >= TERMINAL_OUTPUT_STREAM_LIMIT {
            let Some(oldest) = state.output_tombstone_order.pop_front() else {
                break;
            };
            state.output_tombstones.remove(&oldest);
        }
        state.output_tombstone_order.push_back(key.clone());
    }
    state.output_tombstones.insert(key, cursor);
}
fn terminal_output_projection(
    state: &EventState,
    scope: &str,
    target: &CommandTarget,
    payload: &Value,
) -> Result<TerminalOutputProjection, AutomationError> {
    value_size(target)?;
    let payload_bytes = value_size(payload)?;
    let key = terminal_output_key(scope, target);
    let existing = state.output_streams.get(&key);
    let mut affected_scopes = BTreeSet::from([scope.to_owned()]);
    let mut evicted_keys = BTreeSet::new();
    let mut order_pops = 0;
    if existing.is_none() {
        let mut remaining = state.output_streams.len();
        for oldest in &state.output_stream_order {
            if remaining < TERMINAL_OUTPUT_STREAM_LIMIT {
                break;
            }
            order_pops += 1;
            if let Some(removed) = state.output_streams.get(oldest) {
                remaining -= 1;
                evicted_keys.insert(oldest.clone());
                affected_scopes.insert(removed.scope.clone());
            }
        }
    }

    let previous_cursor = existing
        .map(|stream| stream.cursor)
        .or_else(|| state.output_tombstones.get(&key).copied())
        .unwrap_or(0);
    let cursor = previous_cursor
        .checked_add(1)
        .ok_or_else(|| AutomationError::new(-32003, "terminal output cursor exhausted"))?;
    let (retained_bytes, chunks) = if payload_bytes > TERMINAL_OUTPUT_BYTE_LIMIT {
        (0, VecDeque::new())
    } else {
        let mut chunks = existing
            .map(|stream| stream.chunks.clone())
            .unwrap_or_default();
        let mut retained_bytes = existing.map(|stream| stream.retained_bytes).unwrap_or(0);
        retained_bytes = retained_bytes
            .checked_add(payload_bytes)
            .ok_or_else(|| AutomationError::new(-32003, "terminal output size exhausted"))?;
        chunks.push_back(TerminalOutputChunk {
            cursor,
            payload: payload.clone(),
        });
        while retained_bytes > TERMINAL_OUTPUT_BYTE_LIMIT {
            let Some(removed) = chunks.pop_front() else {
                break;
            };
            retained_bytes = retained_bytes.saturating_sub(value_size(&removed.payload)?);
        }
        (retained_bytes, chunks)
    };
    let projection = TerminalOutputProjection {
        key,
        stream: TerminalOutputStream {
            scope: scope.to_owned(),
            target: target.clone(),
            cursor,
            retained_bytes,
            chunks,
        },
        order_pops,
        evicted_keys,
        snapshots: BTreeMap::new(),
    };
    let mut projection = projection;
    for affected_scope in affected_scopes {
        let snapshot =
            terminal_output_snapshot_with_projection(state, &affected_scope, &projection);
        validate_snapshot_size(&snapshot)?;
        projection.snapshots.insert(affected_scope, snapshot);
    }
    Ok(projection)
}

fn is_binding_scope(scope: &str) -> bool {
    scope.starts_with("binding:")
}

fn is_extension_scope(scope: &str) -> bool {
    scope.starts_with("extension:")
}

fn subscription_sequence(subscription: &str) -> Option<u64> {
    subscription.strip_prefix("subscription-")?.parse().ok()
}

fn retire_subscription_locked(state: &mut EventState, subscription: String) {
    if let Some(sequence) = subscription_sequence(&subscription) {
        state.retired_subscription_watermark = state.retired_subscription_watermark.max(sequence);
    }
    if state.retired_subscriptions.insert(subscription.clone()) {
        state.retired_subscription_order.push_back(subscription);
    }
    while state.retired_subscription_order.len() > MAX_SUBSCRIPTIONS {
        let Some(oldest) = state.retired_subscription_order.pop_front() else {
            break;
        };
        state.retired_subscriptions.remove(&oldest);
    }
}

fn validate_topic_name(topic: &str) -> Result<(), AutomationError> {
    if topic.is_empty() || topic.len() > EVENT_TOPIC_LIMIT {
        return Err(AutomationError::new(-32602, "invalid event topic"));
    }
    Ok(())
}

fn validate_registered_topic(state: &EventState, topic: &str) -> Result<(), AutomationError> {
    if state.registered_topics.contains(topic) {
        Ok(())
    } else {
        Err(AutomationError::new(-32602, "unsupported event topic"))
    }
}

fn validate_subscription_topics(
    state: &EventState,
    topics: &BTreeSet<String>,
) -> Result<(), AutomationError> {
    if topics.is_empty() || topics.len() > MAX_TOPICS_PER_SUBSCRIPTION {
        return Err(AutomationError::new(-32602, "invalid event topic count"));
    }
    for topic in topics {
        validate_topic_name(topic)?;
        validate_registered_topic(state, topic)?;
    }
    Ok(())
}

fn validate_snapshots_locked(
    state: &EventState,
    snapshots: &BTreeMap<String, Value>,
) -> Result<(), AutomationError> {
    for (topic, snapshot) in snapshots {
        validate_registered_topic(state, topic)?;
        validate_snapshot_size(snapshot)?;
    }
    Ok(())
}

fn validate_snapshot_fragments_locked(
    state: &EventState,
    topic: &str,
    fragments: &BTreeMap<String, Value>,
) -> Result<(), AutomationError> {
    validate_registered_topic(state, topic)?;
    if fragments.is_empty() || fragments.len() > MAX_SNAPSHOT_SOURCES {
        return Err(AutomationError::new(
            -32602,
            "invalid snapshot source count",
        ));
    }
    for source in fragments {
        if source.0.is_empty() || source.0.len() > MAX_SNAPSHOT_SOURCE_BYTES {
            return Err(AutomationError::new(-32602, "invalid snapshot source name"));
        }
        validate_snapshot_size(source.1)?;
    }
    validate_snapshot_size(&merge_snapshot_fragments(fragments))
}

fn merge_snapshot_fragments(fragments: &BTreeMap<String, Value>) -> Value {
    let mut merged = Map::new();
    merged.insert(
        "runtime".to_owned(),
        fragments.get("runtime").cloned().unwrap_or(Value::Null),
    );
    let mut hosts = Map::new();
    for (source, snapshot) in fragments {
        if source != "runtime" {
            hosts.insert(source.clone(), snapshot.clone());
        }
    }
    merged.insert("hosts".to_owned(), Value::Object(hosts));
    Value::Object(merged)
}

fn replace_snapshots_locked(
    state: &mut EventState,
    scope: &str,
    snapshots: BTreeMap<String, Value>,
) -> Result<(), AutomationError> {
    validate_snapshots_locked(state, &snapshots)?;
    replace_snapshots_prevalidated(state, scope, snapshots);
    Ok(())
}

fn replace_snapshots_prevalidated(
    state: &mut EventState,
    scope: &str,
    snapshots: BTreeMap<String, Value>,
) {
    if let Some(fragments) = state.snapshot_fragments.get_mut(scope) {
        for topic in snapshots.keys() {
            fragments.remove(topic);
        }
    }
    let state_snapshots = state.snapshots.entry(scope.to_owned()).or_default();
    for (topic, snapshot) in snapshots {
        state_snapshots.insert(topic, snapshot);
    }
}

fn replace_snapshot_fragment_prevalidated(
    state: &mut EventState,
    scope: &str,
    topic: &str,
    source: &str,
    snapshot: Value,
) {
    let fragments = state
        .snapshot_fragments
        .entry(scope.to_owned())
        .or_default()
        .entry(topic.to_owned())
        .or_default();
    fragments.insert(source.to_owned(), snapshot);
    let merged = merge_snapshot_fragments(fragments);
    state
        .snapshots
        .entry(scope.to_owned())
        .or_default()
        .insert(topic.to_owned(), merged);
}

fn purge_retired_scope_locked(state: &mut EventState, scope: &str) {
    state.snapshots.remove(scope);
    state.snapshot_fragments.remove(scope);
    state.revisions.remove(scope);
    let retired_subscriptions = state
        .subscriptions
        .iter()
        .filter(|(_, subscription)| subscription.scope == scope)
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    state
        .subscriptions
        .retain(|_, subscription| subscription.scope != scope);
    for subscription in retired_subscriptions {
        retire_subscription_locked(state, subscription);
    }

    let stream_keys = state
        .output_streams
        .iter()
        .filter(|(_, stream)| stream.scope == scope)
        .map(|(key, _)| key.clone())
        .collect::<BTreeSet<_>>();
    state
        .output_streams
        .retain(|key, _| !stream_keys.contains(key));
    state
        .output_stream_order
        .retain(|key| !stream_keys.contains(key));

    let tombstone_keys = state
        .output_tombstones
        .keys()
        .filter(|key| {
            terminal_output_key_target(key).is_some_and(|(key_scope, _)| key_scope == scope)
        })
        .cloned()
        .collect::<BTreeSet<_>>();
    state
        .output_tombstones
        .retain(|key, _| !tombstone_keys.contains(key));
    state
        .output_tombstone_order
        .retain(|key| !tombstone_keys.contains(key));
}
fn terminal_output_key_target(key: &str) -> Option<(String, CommandTarget)> {
    serde_json::from_str(key).ok()
}

fn validate_publication_shape_locked(
    state: &EventState,
    publication: &EventPublication,
) -> Result<(), AutomationError> {
    if publication.scope.is_empty() {
        return Err(AutomationError::new(
            -32602,
            "event scope must not be empty",
        ));
    }
    validate_registered_topic(state, &publication.topic)
}

fn validate_publication_locked(
    state: &EventState,
    publication: &EventPublication,
) -> Result<(), AutomationError> {
    validate_publication_shape_locked(state, publication)?;
    let revision = state
        .revisions
        .get(&publication.scope)
        .copied()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| AutomationError::new(-32003, "event revision exhausted"))?;
    for subscription in state.subscriptions.values() {
        if subscription.scope != publication.scope
            || !subscription.topics.contains(&publication.topic)
        {
            continue;
        }
        let sequence = subscription
            .sequence
            .checked_add(1)
            .ok_or_else(|| AutomationError::new(-32003, "event sequence exhausted"))?;
        let event = EventEnvelope {
            sequence,
            scope: publication.scope.clone(),
            revision,
            topic: publication.topic.clone(),
            provenance: publication.provenance.clone(),
            target: publication.target.clone(),
            payload: publication.payload.clone(),
        };
        value_size(&event)?;
    }
    Ok(())
}

fn publish_locked_prevalidated(state: &mut EventState, publication: EventPublication) -> u64 {
    let revision = state
        .revisions
        .entry(publication.scope.clone())
        .or_default();
    *revision = (*revision)
        .checked_add(1)
        .expect("publication revision was prevalidated");
    let revision = *revision;
    for subscription in state.subscriptions.values_mut() {
        if subscription.scope != publication.scope
            || !subscription.topics.contains(&publication.topic)
        {
            continue;
        }
        subscription.sequence = subscription
            .sequence
            .checked_add(1)
            .expect("publication sequence was prevalidated");
        if subscription.gap.is_some() || subscription.events.len() >= EVENT_QUEUE_LIMIT {
            subscription.events.clear();
            subscription.queued_bytes = 0;
            subscription.gap = Some(SubscriptionGap {
                sequence: subscription.sequence,
            });
            continue;
        }
        let event = EventEnvelope {
            sequence: subscription.sequence,
            scope: publication.scope.clone(),
            revision,
            topic: publication.topic.clone(),
            provenance: publication.provenance.clone(),
            target: publication.target.clone(),
            payload: publication.payload.clone(),
        };
        let bytes = value_size(&event).expect("publication event was prevalidated");
        if bytes > EVENT_QUEUE_BYTE_LIMIT
            || subscription
                .queued_bytes
                .checked_add(bytes)
                .expect("event queue bytes were prevalidated")
                > EVENT_QUEUE_BYTE_LIMIT
        {
            subscription.events.clear();
            subscription.queued_bytes = 0;
            subscription.gap = Some(SubscriptionGap {
                sequence: subscription.sequence,
            });
            continue;
        }
        subscription.queued_bytes += bytes;
        subscription.events.push_back(QueuedEvent { bytes, event });
    }
    revision
}

fn snapshot_locked(
    state: &EventState,
    scope: &str,
    topics: &BTreeSet<String>,
) -> Result<EventSnapshot, AutomationError> {
    let snapshots = topics
        .iter()
        .map(|topic| {
            (
                topic.clone(),
                state
                    .snapshots
                    .get(scope)
                    .and_then(|snapshots| snapshots.get(topic))
                    .cloned()
                    .unwrap_or(Value::Null),
            )
        })
        .collect::<BTreeMap<_, _>>();
    validate_snapshot_size(&snapshots)?;
    Ok(EventSnapshot {
        scope: scope.to_owned(),
        revision: *state.revisions.get(scope).unwrap_or(&0),
        snapshots,
    })
}

fn value_size(value: &impl Serialize) -> Result<usize, AutomationError> {
    serde_json::to_vec(value)
        .map(|value| value.len())
        .map_err(|error| AutomationError::new(-32603, error.to_string()))
}

fn validate_snapshot_size(snapshot: &impl Serialize) -> Result<(), AutomationError> {
    if value_size(snapshot)? > EVENT_QUEUE_BYTE_LIMIT {
        Err(AutomationError::new(
            -32003,
            "event snapshot exceeds payload limit",
        ))
    } else {
        Ok(())
    }
}

fn rebase_error(subscription: &str, scope: &str, cursor: u64, sequence: u64) -> AutomationError {
    AutomationError::with_data(
        -32005,
        "event rebase required",
        json!({
            "subscription": subscription,
            "scope": scope,
            "cursor": cursor,
            "sequence": sequence,
            "rebase": "snapshot",
        }),
    )
}

fn output_rebase_error(
    scope: &str,
    target: &CommandTarget,
    cursor: u64,
    stream_cursor: u64,
) -> AutomationError {
    AutomationError::with_data(
        -32005,
        "terminal output reread required",
        json!({
            "scope": scope,
            "target": target,
            "cursor": cursor,
            "stream_cursor": stream_cursor,
            "rebase": "reread",
        }),
    )
}

fn terminal_output_key(scope: &str, target: &CommandTarget) -> String {
    serde_json::to_string(&(scope, target)).expect("serializing a command target cannot fail")
}

fn owned_subscription<'a>(
    state: &'a mut EventState,
    subscription: &str,
    owner: &OwnerIdentity,
) -> Result<&'a mut SubscriptionRecord, AutomationError> {
    if let Some(record) = state.subscriptions.get_mut(subscription) {
        if &record.owner != owner {
            return Err(AutomationError::new(
                -32006,
                "subscription is owned by another process",
            ));
        }
        return Ok(record);
    }
    let retired = state.retired_subscriptions.contains(subscription)
        || subscription_sequence(subscription).is_some_and(|sequence| {
            sequence != 0 && sequence <= state.retired_subscription_watermark
        });
    if retired {
        Err(AutomationError::new(
            -32006,
            "subscription event scope is not live",
        ))
    } else {
        Err(AutomationError::new(-32602, "unknown subscription"))
    }
}

fn reap_dead_subscriptions(state: &mut EventState) {
    let system = sysinfo::System::new_all();
    state
        .subscriptions
        .retain(|_, subscription| subscription.owner.is_live_in(&system));
}

#[derive(Clone)]
pub struct MetadataHub {
    state: Arc<Mutex<MetadataState>>,
    events: EventHub,
}

struct MetadataState {
    next_generation: u64,
    records: BTreeMap<MetadataKey, MetadataRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct MetadataKey {
    scope: String,
    namespace: String,

    key: String,
    target: String,
}

impl MetadataHub {
    fn new(events: EventHub) -> Self {
        let state = Arc::new(Mutex::new(MetadataState {
            next_generation: 1,
            records: BTreeMap::new(),
        }));
        events.set_metadata_state(Arc::downgrade(&state));
        Self { state, events }
    }
    pub fn install_snapshot(&self, scope: &str) -> Result<(), AutomationError> {
        if scope.is_empty() {
            return Err(AutomationError::new(
                -32602,
                "metadata scope must not be empty",
            ));
        }
        self.reap_expired()?;
        let mut batch = self.events.begin_publication_batch();
        let state = lock(&self.state);
        let snapshot = metadata_snapshot(&state, scope);
        batch.replace_snapshots(scope, metadata_source_snapshots(snapshot))
    }

    pub fn publish(
        &self,
        publication: MetadataPublication,
    ) -> Result<MetadataRecord, AutomationError> {
        validate_metadata_publication(&publication)?;
        let now = unix_time_ms()?;
        if publication
            .expires_at_ms
            .is_some_and(|expires_at| expires_at <= now)
        {
            return Err(AutomationError::new(
                -32602,
                "metadata expiry must be in the future",
            ));
        }
        self.reap_expired_at(now)?;

        let key = metadata_key(
            &publication.scope,
            &publication.namespace,
            &publication.key,
            publication.target.as_ref(),
        );
        let mut batch = self.events.begin_publication_batch();
        let mut state = lock(&self.state);
        let previous = state.records.get(&key);
        if previous.is_none() && state.records.len() >= MAX_METADATA_RECORDS {
            return Err(AutomationError::new(
                -32001,
                "metadata record limit reached",
            ));
        }
        let generation = state.next_generation;
        let next_generation = generation
            .checked_add(1)
            .ok_or_else(|| AutomationError::new(-32003, "metadata generation exhausted"))?;
        let record = MetadataRecord {
            scope: publication.scope.clone(),
            namespace: publication.namespace.clone(),
            key: publication.key.clone(),
            target: publication.target.clone(),
            value: publication.value.clone(),
            expires_at_ms: publication.expires_at_ms,
            provenance: publication.provenance.clone(),
            generation,
        };
        let snapshot = metadata_snapshot_with_record(&state, &key, &record);
        let event = EventPublication::new(
            record.scope.clone(),
            "metadata.changed",
            record.provenance.clone(),
            record.target.clone(),
            json!({"operation": "published", "metadata": record.clone()}),
        );
        let sources = metadata_source_snapshots(snapshot);
        let prepared = batch.prepare(event, sources.iter().cloned())?;
        state.next_generation = next_generation;
        state.records.insert(key, record.clone());
        batch.commit_one(prepared);
        Ok(record)
    }
    pub fn clear(
        &self,
        scope: &str,
        namespace: &str,
        key: &str,
        target: Option<&CommandTarget>,
        provenance: Value,
    ) -> Result<Option<MetadataRecord>, AutomationError> {
        validate_metadata_identity(scope, namespace, key)?;
        self.reap_expired()?;
        let metadata_key = metadata_key(scope, namespace, key, target);
        let mut batch = self.events.begin_publication_batch();
        let mut state = lock(&self.state);
        let Some(record) = state.records.get(&metadata_key).cloned() else {
            return Ok(None);
        };
        let excluded = BTreeSet::from([metadata_key.clone()]);
        let snapshot = metadata_snapshot_excluding(&state, scope, &excluded);
        let event = EventPublication::new(
            scope.to_owned(),
            "metadata.changed",
            provenance,
            record.target.clone(),
            json!({"operation": "cleared", "metadata": record.clone()}),
        );
        let sources = metadata_source_snapshots(snapshot);
        let prepared = batch.prepare(event, sources.iter().cloned())?;
        state.records.remove(&metadata_key);
        batch.commit_one(prepared);
        Ok(Some(record))
    }
    pub fn get(
        &self,
        scope: &str,
        namespace: &str,
        key: &str,
        target: Option<&CommandTarget>,
    ) -> Result<Option<MetadataRecord>, AutomationError> {
        validate_metadata_identity(scope, namespace, key)?;
        self.reap_expired()?;
        Ok(lock(&self.state)
            .records
            .get(&metadata_key(scope, namespace, key, target))
            .cloned())
    }

    pub fn list(&self, scope: &str) -> Result<Vec<MetadataRecord>, AutomationError> {
        if scope.is_empty() {
            return Err(AutomationError::new(
                -32602,
                "metadata scope must not be empty",
            ));
        }
        self.reap_expired()?;
        Ok(lock(&self.state)
            .records
            .values()
            .filter(|record| record.scope == scope)
            .cloned()
            .collect())
    }

    pub fn reap_expired(&self) -> Result<usize, AutomationError> {
        self.reap_expired_at(unix_time_ms()?)
    }

    pub fn reap_expired_at(&self, now_ms: u64) -> Result<usize, AutomationError> {
        let mut batch = self.events.begin_publication_batch();
        let mut state = lock(&self.state);
        let expired_keys = state
            .records
            .iter()
            .filter(|(_, record)| {
                record
                    .expires_at_ms
                    .is_some_and(|expires_at| expires_at <= now_ms)
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let expired_key_set = expired_keys.iter().cloned().collect::<BTreeSet<_>>();
        let records = expired_keys
            .iter()
            .filter_map(|key| state.records.get(key).cloned())
            .collect::<Vec<_>>();
        if records.is_empty() {
            return Ok(0);
        }
        let snapshots = records
            .iter()
            .map(|record| record.scope.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|scope| {
                (
                    scope.clone(),
                    metadata_snapshot_excluding(&state, &scope, &expired_key_set),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut prepared = Vec::with_capacity(records.len());
        for record in &records {
            let snapshot = snapshots
                .get(&record.scope)
                .expect("every expired metadata scope has a snapshot")
                .clone();
            let event = EventPublication::new(
                record.scope.clone(),
                "metadata.expired",
                json!({
                    "source": "metadata",
                    "operation": "expired",
                    "published_by": record.provenance.clone(),
                }),
                record.target.clone(),
                json!({"metadata": record.clone()}),
            );
            let sources = metadata_source_snapshots(snapshot);
            prepared.push(batch.prepare(event, sources.iter().cloned())?);
        }

        for key in &expired_keys {
            state.records.remove(key);
        }
        batch.commit_all(prepared);
        Ok(records.len())
    }
}

fn validate_metadata_publication(publication: &MetadataPublication) -> Result<(), AutomationError> {
    validate_metadata_identity(&publication.scope, &publication.namespace, &publication.key)?;
    if value_size(&publication.value)? > EVENT_QUEUE_BYTE_LIMIT {
        return Err(AutomationError::new(
            -32003,
            "metadata value exceeds payload limit",
        ));
    }
    Ok(())
}

fn validate_metadata_identity(
    scope: &str,
    namespace: &str,
    key: &str,
) -> Result<(), AutomationError> {
    if scope.is_empty() {
        return Err(AutomationError::new(
            -32602,
            "metadata scope must not be empty",
        ));
    }
    for name in [namespace, key] {
        if name.is_empty() || name.len() > METADATA_NAME_LIMIT {
            return Err(AutomationError::new(-32602, "invalid metadata name"));
        }
    }
    Ok(())
}

fn metadata_key(
    scope: &str,
    namespace: &str,
    key: &str,
    target: Option<&CommandTarget>,
) -> MetadataKey {
    MetadataKey {
        scope: scope.to_owned(),
        namespace: namespace.to_owned(),
        key: key.to_owned(),
        target: serde_json::to_string(&target)
            .expect("serializing an optional command target cannot fail"),
    }
}

fn metadata_snapshot(state: &MetadataState, scope: &str) -> Value {
    metadata_snapshot_excluding(state, scope, &BTreeSet::new())
}

fn metadata_snapshot_excluding(
    state: &MetadataState,
    scope: &str,
    excluded: &BTreeSet<MetadataKey>,
) -> Value {
    let records = state
        .records
        .iter()
        .filter(|(key, record)| record.scope == scope && !excluded.contains(key))
        .map(|(_, record)| record.clone())
        .collect::<Vec<_>>();
    json!({"records": records})
}
fn metadata_snapshot_with_record(
    state: &MetadataState,
    key: &MetadataKey,
    replacement: &MetadataRecord,
) -> Value {
    let mut records = BTreeMap::new();
    for (existing_key, record) in &state.records {
        if record.scope == replacement.scope {
            records.insert(existing_key, record);
        }
    }
    records.insert(key, replacement);
    json!({"records": records.values().collect::<Vec<_>>()})
}
fn metadata_source_snapshots(snapshot: Value) -> [(String, Value); 2] {
    [
        ("metadata.changed".to_owned(), snapshot.clone()),
        ("metadata.expired".to_owned(), snapshot),
    ]
}

fn unix_time_ms() -> Result<u64, AutomationError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .map_err(|error| AutomationError::new(-32603, error.to_string()))
}
#[derive(Clone)]
pub struct TaskHub {
    state: Arc<Mutex<TaskHubState>>,
    events: EventHub,
}

#[derive(Clone)]
struct TaskHubState {
    next_task: u64,
    tasks: BTreeMap<String, TaskRecord>,
    completed_tasks: VecDeque<String>,
}

#[derive(Clone)]
struct TaskRecord {
    owner: OwnerIdentity,
    scope: String,
    cancellation: CommandCancellation,
    state: TaskState,
}

struct TaskChange {
    scope: String,
    task: TaskStatus,
    operation: &'static str,
}

impl TaskHub {
    fn new(events: EventHub) -> Self {
        Self {
            state: Arc::new(Mutex::new(TaskHubState {
                next_task: 1,
                tasks: BTreeMap::new(),
                completed_tasks: VecDeque::new(),
            })),
            events,
        }
    }

    /// Installs the scoped task index as bootstrap state without emitting a
    /// synthetic task transition.
    pub fn install_snapshot(&self, scope: &str) -> Result<(), AutomationError> {
        if scope.is_empty() {
            return Err(AutomationError::new(-32602, "task scope must not be empty"));
        }
        let state = lock(&self.state);
        self.events
            .set_snapshot(scope, "task.changed", task_snapshot(&state, scope))
    }

    pub fn start(
        &self,
        owner: OwnerIdentity,
        cancellation: CommandCancellation,
        scope: String,
    ) -> Result<TaskStatus, AutomationError> {
        if scope.is_empty() {
            return Err(AutomationError::new(-32602, "task scope must not be empty"));
        }
        let mut state = lock(&self.state);
        let next_task = state.next_task;
        let next_task_after = next_task
            .checked_add(1)
            .ok_or_else(|| AutomationError::new(-32003, "task ID exhausted"))?;
        let evictions_needed = state
            .tasks
            .len()
            .saturating_add(1)
            .saturating_sub(MAX_TASKS);
        let evicted_ids = state
            .completed_tasks
            .iter()
            .filter(|task| state.tasks.contains_key(task.as_str()))
            .take(evictions_needed)
            .cloned()
            .collect::<Vec<_>>();
        if evicted_ids.len() != evictions_needed {
            return Err(AutomationError::new(-32001, "task limit reached"));
        }

        let completed_tasks = state.completed_tasks.clone();
        let mut evicted = Vec::with_capacity(evicted_ids.len());
        for task in &evicted_ids {
            let record = state
                .tasks
                .remove(task)
                .expect("completed task selected for eviction exists");
            evicted.push((task.clone(), record));
        }
        state
            .completed_tasks
            .retain(|task| !evicted_ids.contains(task));
        state.next_task = next_task_after;

        let id = format!("task-{next_task}");
        let record = TaskRecord {
            owner,
            scope: scope.clone(),
            cancellation,
            state: TaskState::Running,
        };
        let status = task_status(&id, &record);
        let _ = state.tasks.insert(id.clone(), record);

        let mut changes = evicted
            .iter()
            .map(|(task, record)| TaskChange {
                scope: record.scope.clone(),
                task: task_status(task, record),
                operation: "removed",
            })
            .collect::<Vec<_>>();
        changes.push(TaskChange {
            scope,
            task: status.clone(),
            operation: "started",
        });
        let result = match self.validate_task_changes_locked(&state, &changes) {
            Ok(()) => self.publish_task_changes_locked(&state, changes),
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            let _ = state.tasks.remove(&id);
            state.next_task = next_task;
            state.completed_tasks = completed_tasks;
            for (task, record) in evicted {
                let _ = state.tasks.insert(task, record);
            }
            return Err(error);
        }
        Ok(status)
    }

    pub fn remove_checked(&self, task: &str) -> Result<(), AutomationError> {
        let mut state = lock(&self.state);
        let Some(record) = state.tasks.remove(task) else {
            return Ok(());
        };
        let completed_tasks = state.completed_tasks.clone();
        state.completed_tasks.retain(|completed| completed != task);
        let change = TaskChange {
            scope: record.scope.clone(),
            task: task_status(task, &record),
            operation: "removed",
        };
        if let Err(error) = self.publish_task_change_locked(&state, change) {
            let _ = state.tasks.insert(task.to_owned(), record);
            state.completed_tasks = completed_tasks;
            return Err(error);
        }
        Ok(())
    }
    /// Cancels and removes a task only after the authoritative lifecycle
    /// publication batch has committed. A failed publication leaves the task
    /// and its cancellation token untouched for a later retry.
    pub fn terminate_checked(&self, task: &str) -> Result<(), AutomationError> {
        let mut state = lock(&self.state);
        let mut cancelling = state.clone();
        let Some(record) = cancelling.tasks.get_mut(task) else {
            return Ok(());
        };
        let was_running = matches!(record.state, TaskState::Running);
        if was_running {
            record.state = TaskState::Cancelling;
        }
        let mut removed = cancelling.clone();
        let Some(removed_record) = removed.tasks.remove(task) else {
            return Ok(());
        };
        removed
            .completed_tasks
            .retain(|completed| completed != task);
        let removed_change = TaskChange {
            scope: removed_record.scope.clone(),
            task: task_status(task, &removed_record),
            operation: "removed",
        };
        if was_running {
            let Some(cancelling_record) = cancelling.tasks.get(task) else {
                return Ok(());
            };
            let cancelling_change = TaskChange {
                scope: cancelling_record.scope.clone(),
                task: task_status(task, cancelling_record),
                operation: "cancelling",
            };
            self.publish_task_change_sequence_locked(vec![
                (&cancelling, cancelling_change),
                (&removed, removed_change),
            ])?;
        } else {
            self.publish_task_change_locked(&removed, removed_change)?;
        }

        if let Some(actual) = state.tasks.get_mut(task) {
            let _ = actual.cancellation.cancel();
        }
        state.tasks.remove(task);
        state.completed_tasks.retain(|completed| completed != task);
        Ok(())
    }
    /// Shutdown-only task teardown. Ownership is checked before mutation, but
    /// external publication cannot veto cancellation/removal because shutdown
    /// has no retry worker to preserve a live task.
    pub fn terminate_force_checked(
        &self,
        task: &str,
        owner: &OwnerIdentity,
    ) -> Result<(), AutomationError> {
        let mut state = lock(&self.state);
        let _ = owned_task(&state, task, owner)?;
        let mut candidate = state.clone();
        let Some(removed_record) = candidate.tasks.remove(task) else {
            return Ok(());
        };
        candidate
            .completed_tasks
            .retain(|completed| completed != task);
        let change = TaskChange {
            scope: removed_record.scope.clone(),
            task: task_status(task, &removed_record),
            operation: "removed",
        };
        let publication = self.publish_task_change_locked(&candidate, change);

        if let Some(actual) = state.tasks.get_mut(task) {
            let _ = actual.cancellation.cancel();
        }
        state.tasks.remove(task);
        state.completed_tasks.retain(|completed| completed != task);
        publication
    }

    pub fn remove(&self, task: &str) {
        let _ = self.remove_checked(task);
    }

    /// Completes a task without allowing an unretainable result to restore its
    /// prior running state. The full command result remains available through
    /// `command.completed`; task state retains at most a bounded summary.
    pub fn finish(&self, task: &str, outcome: &Value) -> Result<(), AutomationError> {
        let (outcome, outcome_bytes) = stored_task_outcome(outcome);
        let mut state = lock(&self.state);
        let change = {
            let Some(record) = state.tasks.get_mut(task) else {
                return Ok(());
            };
            if matches!(record.state, TaskState::Completed { .. }) {
                return Ok(());
            }
            record.state = TaskState::Completed { outcome };
            TaskChange {
                scope: record.scope.clone(),
                task: task_status(task, record),
                operation: "completed",
            }
        };
        if self.publish_task_change_locked(&state, change).is_ok() {
            state.completed_tasks.push_back(task.to_owned());
            return Ok(());
        }

        let failure_change = {
            let record = state
                .tasks
                .get_mut(task)
                .expect("task exists while its state lock is held");
            record.state = TaskState::Completed {
                outcome: task_outcome_failure(TASK_COMPLETION_PUBLICATION_MESSAGE, outcome_bytes),
            };
            TaskChange {
                scope: record.scope.clone(),
                task: task_status(task, record),
                operation: "completed",
            }
        };
        state.completed_tasks.push_back(task.to_owned());
        self.publish_task_change_locked(&state, failure_change)
    }

    pub fn status(&self, task: &str, owner: &OwnerIdentity) -> Result<TaskStatus, AutomationError> {
        let state = lock(&self.state);
        let record = owned_task(&state, task, owner)?;
        Ok(task_status(task, record))
    }

    pub fn cancel(&self, task: &str, owner: &OwnerIdentity) -> Result<TaskStatus, AutomationError> {
        let mut state = lock(&self.state);
        if !matches!(owned_task(&state, task, owner)?.state, TaskState::Running) {
            return Ok(task_status(task, owned_task(&state, task, owner)?));
        }
        let mut candidate = state.clone();
        let (status, change) = {
            let record = owned_task_mut(&mut candidate, task, owner)?;
            record.state = TaskState::Cancelling;
            let status = task_status(task, record);
            let change = TaskChange {
                scope: record.scope.clone(),
                task: status.clone(),
                operation: "cancelling",
            };
            (status, change)
        };
        self.publish_task_change_locked(&candidate, change)?;
        let record = owned_task_mut(&mut state, task, owner)?;
        record.state = TaskState::Cancelling;
        let _ = record.cancellation.cancel();
        Ok(status)
    }

    pub fn cancel_owner_checked(&self, owner: &OwnerIdentity) -> Result<usize, AutomationError> {
        self.cancel_matching(|record| &record.owner == owner)
    }

    pub fn cancel_owner(&self, owner: &OwnerIdentity) -> usize {
        self.cancel_owner_checked(owner).unwrap_or(0)
    }

    pub fn cancel_scope_checked(&self, scope: &str) -> Result<usize, AutomationError> {
        self.cancel_matching(|record| record.scope.as_str() == scope)
    }

    pub fn cancel_scope(&self, scope: &str) -> usize {
        self.cancel_scope_checked(scope).unwrap_or(0)
    }

    pub fn cancel_all_checked(&self) -> Result<usize, AutomationError> {
        self.cancel_matching(|_| true)
    }

    pub fn cancel_all(&self) {
        let _ = self.cancel_all_checked();
    }

    fn cancel_matching(
        &self,
        mut matches: impl FnMut(&TaskRecord) -> bool,
    ) -> Result<usize, AutomationError> {
        let mut state = lock(&self.state);
        let tasks = state
            .tasks
            .iter()
            .filter(|(_, record)| matches!(record.state, TaskState::Running) && matches(record))
            .map(|(task, _)| task.clone())
            .collect::<Vec<_>>();
        if tasks.is_empty() {
            return Ok(0);
        }

        let mut candidate = state.clone();
        for task in &tasks {
            if let Some(record) = candidate.tasks.get_mut(task) {
                record.state = TaskState::Cancelling;
            }
        }
        let changes = tasks
            .iter()
            .filter_map(|task| {
                candidate.tasks.get(task).map(|record| TaskChange {
                    scope: record.scope.clone(),
                    task: task_status(task, record),
                    operation: "cancelling",
                })
            })
            .collect::<Vec<_>>();
        self.validate_task_changes_locked(&candidate, &changes)?;
        self.publish_task_changes_locked(&candidate, changes)?;
        for task in &tasks {
            if let Some(record) = state.tasks.get_mut(task) {
                record.state = TaskState::Cancelling;
                let _ = record.cancellation.cancel();
            }
        }
        Ok(tasks.len())
    }
    fn validate_task_changes_locked(
        &self,
        state: &TaskHubState,
        changes: &[TaskChange],
    ) -> Result<(), AutomationError> {
        for change in changes {
            let _ = task_change_snapshot(state, change)?;
        }
        Ok(())
    }

    fn publish_task_changes_locked(
        &self,
        state: &TaskHubState,
        changes: Vec<TaskChange>,
    ) -> Result<(), AutomationError> {
        self.publish_task_change_sequence_locked(
            changes.into_iter().map(|change| (state, change)).collect(),
        )
    }

    fn publish_task_change_sequence_locked(
        &self,
        changes: Vec<(&TaskHubState, TaskChange)>,
    ) -> Result<(), AutomationError> {
        let mut batch = self.events.begin_publication_batch();
        let mut prepared = Vec::with_capacity(changes.len());
        for (state, change) in changes {
            let snapshot = task_change_snapshot(state, &change)?;
            let TaskChange {
                scope,
                task,
                operation,
            } = change;
            prepared.push(batch.prepare(
                EventPublication::new(
                    scope.clone(),
                    "task.changed",
                    json!({"owner_pid": task.owner_pid}),
                    None,
                    json!({"operation": operation, "task": task}),
                ),
                [("task.changed".to_owned(), snapshot)],
            )?);
        }
        let _ = batch.commit_all(prepared);
        Ok(())
    }

    fn owners(&self) -> BTreeSet<OwnerIdentity> {
        lock(&self.state)
            .tasks
            .values()
            .map(|task| task.owner.clone())
            .collect()
    }

    /// `state` remains locked until the event has replaced its source snapshot,
    /// so a later transition cannot publish an older task index.
    fn publish_task_change_locked(
        &self,
        state: &TaskHubState,
        change: TaskChange,
    ) -> Result<(), AutomationError> {
        self.publish_task_changes_locked(state, vec![change])
    }
}

fn stored_task_outcome(outcome: &Value) -> (Value, Option<usize>) {
    match value_size(outcome) {
        Ok(bytes) if bytes <= TASK_OUTCOME_BYTE_LIMIT => (outcome.clone(), Some(bytes)),
        Ok(bytes) => (
            task_outcome_failure(TASK_OUTCOME_LIMIT_MESSAGE, Some(bytes)),
            Some(bytes),
        ),
        Err(_) => (
            task_outcome_failure("task outcome could not be serialized", None),
            None,
        ),
    }
}

fn task_outcome_failure(message: &'static str, outcome_bytes: Option<usize>) -> Value {
    match outcome_bytes {
        Some(outcome_bytes) => json!({
            "status": "failed",
            "code": TASK_OUTCOME_LIMIT_CODE,
            "message": message,
            "outcome_bytes": outcome_bytes,
            "limit_bytes": TASK_OUTCOME_BYTE_LIMIT,
        }),
        None => json!({
            "status": "failed",
            "code": TASK_OUTCOME_LIMIT_CODE,
            "message": message,
        }),
    }
}
fn task_snapshot(state: &TaskHubState, scope: &str) -> Value {
    let tasks = state
        .tasks
        .iter()
        .filter(|(_, record)| record.scope == scope)
        .map(|(id, record)| (id.clone(), task_status(id, record)))
        .collect::<BTreeMap<_, _>>();
    json!({"tasks": tasks})
}

fn task_change_snapshot(
    state: &TaskHubState,
    change: &TaskChange,
) -> Result<Value, AutomationError> {
    if change.scope.is_empty() {
        return Err(AutomationError::new(-32602, "task scope must not be empty"));
    }
    let snapshot = task_snapshot(state, &change.scope);
    // Rebase bounds its selected-topic map, not only an individual source.
    validate_snapshot_size(&BTreeMap::from([("task.changed", &snapshot)]))?;
    Ok(snapshot)
}

fn owned_task<'a>(
    state: &'a TaskHubState,
    task: &str,
    owner: &OwnerIdentity,
) -> Result<&'a TaskRecord, AutomationError> {
    let Some(record) = state.tasks.get(task) else {
        return Err(AutomationError::new(-32602, "unknown task"));
    };
    if &record.owner != owner {
        return Err(AutomationError::new(
            -32006,
            "task is owned by another process",
        ));
    }
    Ok(record)
}

fn owned_task_mut<'a>(
    state: &'a mut TaskHubState,
    task: &str,
    owner: &OwnerIdentity,
) -> Result<&'a mut TaskRecord, AutomationError> {
    let Some(record) = state.tasks.get_mut(task) else {
        return Err(AutomationError::new(-32602, "unknown task"));
    };
    if &record.owner != owner {
        return Err(AutomationError::new(
            -32006,
            "task is owned by another process",
        ));
    }
    Ok(record)
}

fn task_status(id: &str, record: &TaskRecord) -> TaskStatus {
    TaskStatus {
        id: id.to_owned(),
        owner_pid: record.owner.pid(),
        state: record.state.clone(),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Caller, ResourceKind};
    use std::{sync::mpsc, thread, time::Duration};

    fn owner() -> OwnerIdentity {
        OwnerIdentity::new(7, 11)
    }

    #[test]
    fn rebase_snapshots_and_resets_are_atomic() {
        let hub = AutomationHub::new();
        let scope = "instance:test".to_owned();
        hub.events()
            .set_snapshot(
                scope.clone(),
                COMMAND_COMPLETED_TOPIC,
                json!({"panes": ["pane-4"]}),
            )
            .unwrap();
        let subscription = hub
            .events()
            .subscribe(
                owner(),
                [COMMAND_COMPLETED_TOPIC.to_owned()].into_iter().collect(),
                scope.clone(),
            )
            .unwrap()
            .subscription;
        for sequence in 0..=EVENT_QUEUE_LIMIT {
            hub.publish_event(EventPublication::new(
                scope.clone(),
                COMMAND_COMPLETED_TOPIC,
                Value::Null,
                None,
                json!({"sequence": sequence}),
            ))
            .unwrap();
        }
        assert_eq!(
            hub.events()
                .poll(&subscription, &owner(), 0)
                .unwrap_err()
                .code,
            -32005
        );
        hub.events()
            .replace_snapshots(
                scope.clone(),
                [(
                    COMMAND_COMPLETED_TOPIC.to_owned(),
                    json!({"panes": ["pane-5"]}),
                )],
            )
            .unwrap();

        let rebase = hub.events().rebase(&subscription, &owner()).unwrap();
        assert_eq!(rebase.cursor, EVENT_QUEUE_LIMIT as u64 + 1);
        assert_eq!(
            rebase.snapshot.snapshots[COMMAND_COMPLETED_TOPIC]["panes"][0],
            "pane-5"
        );
        hub.publish_event(EventPublication::new(
            scope,
            COMMAND_COMPLETED_TOPIC,
            Value::Null,
            None,
            Value::Null,
        ))
        .unwrap();
        let delivery = hub
            .events()
            .poll(&subscription, &owner(), rebase.cursor)
            .unwrap();
        assert_eq!(delivery.events[0].sequence, rebase.cursor + 1);
    }

    #[test]
    fn extension_snapshot_fragments_merge_and_rebase_as_one_union() {
        let hub = AutomationHub::new();
        let scope = "instance:extension-fragments".to_owned();
        hub.bind_instance_scope(scope.clone()).unwrap();
        let owner = owner();
        let subscription = hub
            .events()
            .subscribe(
                owner.clone(),
                BTreeSet::from(["extension.reloaded".to_owned()]),
                scope.clone(),
            )
            .unwrap()
            .subscription;
        let publish = |source: &str, snapshot: Value, generation: u64| {
            hub.publish_event_with_snapshot_source(
                EventPublication::new(
                    scope.clone(),
                    "extension.reloaded",
                    json!({"source": source}),
                    None,
                    json!({"source": source, "generation": generation}),
                ),
                source,
                snapshot,
            )
            .unwrap();
        };
        publish(
            "runtime",
            json!({"modules": [{"extension_id": "agents", "generation": 1}]}),
            1,
        );
        publish(
            "status",
            json!({"modules": [{"extension_id": "status", "generation": 2}]}),
            2,
        );
        publish(
            "sidebar",
            json!({"modules": [{"extension_id": "sidebar", "generation": 3}]}),
            3,
        );
        let topics = BTreeSet::from(["extension.reloaded".to_owned()]);
        let snapshot = hub.events().snapshot(&scope, &topics).unwrap();
        assert_eq!(
            snapshot.snapshots["extension.reloaded"]["runtime"]["modules"][0]["extension_id"],
            "agents"
        );
        assert_eq!(
            snapshot.snapshots["extension.reloaded"]["hosts"]["status"]["modules"][0]["extension_id"],
            "status"
        );
        assert_eq!(
            snapshot.snapshots["extension.reloaded"]["hosts"]["sidebar"]["modules"][0]["extension_id"],
            "sidebar"
        );

        for generation in 0..=EVENT_QUEUE_LIMIT {
            publish(
                "runtime",
                json!({"modules": [{"extension_id": "agents", "generation": generation}]}),
                generation as u64,
            );
        }
        assert_eq!(
            hub.events()
                .poll(&subscription, &owner, 0)
                .unwrap_err()
                .code,
            -32005
        );
        let rebase = hub.events().rebase(&subscription, &owner).unwrap();
        assert_eq!(
            rebase.snapshot.snapshots["extension.reloaded"]["runtime"]["modules"][0]["generation"],
            EVENT_QUEUE_LIMIT as u64
        );
        assert_eq!(
            rebase.snapshot.snapshots["extension.reloaded"]["hosts"]["status"]["modules"][0]["extension_id"],
            "status"
        );
        assert_eq!(
            rebase.snapshot.snapshots["extension.reloaded"]["hosts"]["sidebar"]["modules"][0]["extension_id"],
            "sidebar"
        );
    }

    #[test]
    fn snapshot_source_batch_preflights_without_partial_delivery() {
        let hub = AutomationHub::new();
        let scope = "instance:extension-batch".to_owned();
        hub.bind_instance_scope(scope.clone()).unwrap();
        let owner = owner();
        let subscription = hub
            .events()
            .subscribe(
                owner.clone(),
                BTreeSet::from(["extension.reloaded".to_owned()]),
                scope.clone(),
            )
            .unwrap()
            .subscription;
        let oversized = Value::String("x".repeat(EVENT_QUEUE_BYTE_LIMIT));
        let result = hub.events().publish_batch_with_snapshot_sources(vec![
            (
                EventPublication::new(
                    scope.clone(),
                    "extension.reloaded",
                    Value::Null,
                    None,
                    json!({"first": true}),
                ),
                vec![(
                    "extension.reloaded".to_owned(),
                    "status".to_owned(),
                    json!({"modules": [{"extension_id": "status"}]}),
                )],
            ),
            (
                EventPublication::new(
                    scope.clone(),
                    "extension.reloaded",
                    Value::Null,
                    None,
                    json!({"second": true}),
                ),
                vec![(
                    "extension.reloaded".to_owned(),
                    "sidebar".to_owned(),
                    oversized,
                )],
            ),
        ]);
        assert_eq!(result.unwrap_err().code, -32003);
        let snapshot = hub
            .events()
            .snapshot(&scope, &BTreeSet::from(["extension.reloaded".to_owned()]))
            .unwrap();
        assert_eq!(
            snapshot.snapshots["extension.reloaded"]["hosts"]["status"]["modules"],
            json!([])
        );
        assert!(
            hub.events()
                .poll(&subscription, &owner, 0)
                .unwrap()
                .events
                .is_empty()
        );
    }

    #[test]
    fn oversized_rebase_snapshot_is_non_destructive() {
        let hub = AutomationHub::new();
        let scope = "instance:oversized-rebase".to_owned();
        let topics = BTreeSet::from([
            COMMAND_COMPLETED_TOPIC.to_owned(),
            "topology.changed".to_owned(),
        ]);
        let oversized = Value::String("x".repeat(EVENT_QUEUE_BYTE_LIMIT / 2 - 16));
        hub.events()
            .replace_snapshots(
                scope.clone(),
                [
                    (COMMAND_COMPLETED_TOPIC.to_owned(), oversized.clone()),
                    ("topology.changed".to_owned(), oversized),
                ],
            )
            .unwrap();
        let subscription = hub
            .events()
            .subscribe(owner(), topics.clone(), scope.clone())
            .unwrap()
            .subscription;
        for sequence in 0..=EVENT_QUEUE_LIMIT {
            hub.publish_event(EventPublication::new(
                scope.clone(),
                COMMAND_COMPLETED_TOPIC,
                Value::Null,
                None,
                json!({"sequence": sequence}),
            ))
            .unwrap();
        }
        let before = hub.events().poll(&subscription, &owner(), 0).unwrap_err();
        let before_data = before.data.unwrap();
        let sequence = before_data["sequence"].as_u64().unwrap();
        assert_eq!(before_data["cursor"], 0);

        let error = hub.events().rebase(&subscription, &owner()).unwrap_err();
        assert_eq!(error.code, -32003);
        let after = hub.events().poll(&subscription, &owner(), 0).unwrap_err();
        let after_data = after.data.unwrap();
        assert_eq!(after.code, -32005);
        assert_eq!(after_data["cursor"], 0);
        assert_eq!(after_data["sequence"], sequence);

        hub.events()
            .replace_snapshots(
                scope.clone(),
                [
                    (COMMAND_COMPLETED_TOPIC.to_owned(), json!({"ready": true})),
                    ("topology.changed".to_owned(), json!({"ready": true})),
                ],
            )
            .unwrap();
        let rebase = hub.events().rebase(&subscription, &owner()).unwrap();
        assert_eq!(rebase.cursor, sequence);
    }

    #[test]
    fn oversized_terminal_output_snapshot_does_not_advance_stream() {
        let hub = AutomationHub::new();
        let scope = "instance:oversized-output";
        let target = CommandTarget {
            kind: ResourceKind::Terminal,
            handle: "terminal".to_owned(),
            generation: 1,
        };
        hub.publish_terminal_output(scope, Value::Null, target.clone(), Value::Null)
            .unwrap();
        let oversized = CommandTarget {
            kind: ResourceKind::Terminal,
            handle: "x".repeat(EVENT_QUEUE_BYTE_LIMIT),
            generation: 1,
        };
        let error = hub
            .publish_terminal_output(scope, Value::Null, oversized.clone(), Value::Null)
            .unwrap_err();
        assert_eq!(error.code, -32003);
        let retained = hub.terminal_output_after(scope, &target, 0).unwrap();
        assert_eq!(retained.cursor, 1);
        assert_eq!(retained.chunks.len(), 1);
        let missing = hub.terminal_output_after(scope, &oversized, 0).unwrap_err();
        assert_eq!(missing.data.unwrap()["stream_cursor"], 0);
    }

    #[test]
    fn binding_scope_retirement_wins_and_replacement_is_distinct() {
        let hub = AutomationHub::new();
        let old_scope = "binding:1:1".to_owned();
        let replacement_scope = old_scope.clone();
        hub.events()
            .replace_live_binding_scopes([old_scope.clone()]);
        hub.events()
            .set_snapshot(
                old_scope.clone(),
                "topology.changed",
                json!({"generation": 1}),
            )
            .unwrap();
        let old = hub
            .events()
            .subscribe(
                owner(),
                ["topology.changed".to_owned()].into_iter().collect(),
                old_scope.clone(),
            )
            .unwrap();
        hub.events().replace_live_binding_scopes(std::iter::empty());
        let retired = hub
            .events()
            .subscribe(
                owner(),
                ["topology.changed".to_owned()].into_iter().collect(),
                old_scope,
            )
            .unwrap_err();
        assert_eq!(retired.code, -32006);

        hub.events()
            .replace_live_binding_scopes([replacement_scope.clone()]);
        hub.events()
            .set_snapshot(
                replacement_scope.clone(),
                "topology.changed",
                json!({"generation": 2}),
            )
            .unwrap();
        let replacement = hub
            .events()
            .subscribe(
                owner(),
                ["topology.changed".to_owned()].into_iter().collect(),
                replacement_scope.clone(),
            )
            .unwrap();
        assert_ne!(old.subscription, replacement.subscription);
        let replacement_snapshot = hub
            .events()
            .snapshot(
                &replacement_scope,
                &["topology.changed".to_owned()].into_iter().collect(),
            )
            .unwrap();
        assert_eq!(
            replacement_snapshot.snapshots["topology.changed"]["generation"],
            2
        );
    }

    #[test]
    fn reconnect_replaces_binding_snapshot_before_gap_rebase() {
        let hub = AutomationHub::new();
        let scope = "binding:space:binding".to_owned();
        hub.events().replace_live_binding_scopes([scope.clone()]);
        let topic = "backend.rebased".to_owned();
        hub.events()
            .replace_snapshots_with_terminal_output(
                scope.clone(),
                [(
                    topic.clone(),
                    json!({"binding_generation": 1, "sessions": ["before"]}),
                )],
            )
            .unwrap();
        let subscription = hub
            .events()
            .subscribe(
                owner(),
                [topic.clone()].into_iter().collect(),
                scope.clone(),
            )
            .unwrap()
            .subscription;
        for sequence in 0..=EVENT_QUEUE_LIMIT {
            hub.publish_event(EventPublication::new(
                scope.clone(),
                topic.clone(),
                Value::Null,
                None,
                json!({"sequence": sequence}),
            ))
            .unwrap();
        }
        assert_eq!(
            hub.events()
                .poll(&subscription, &owner(), 0)
                .unwrap_err()
                .code,
            -32005
        );

        hub.events()
            .replace_snapshots_with_terminal_output(
                scope,
                [(
                    topic.clone(),
                    json!({"binding_generation": 2, "sessions": ["after"]}),
                )],
            )
            .unwrap();
        let rebase = hub.events().rebase(&subscription, &owner()).unwrap();
        assert_eq!(rebase.snapshot.snapshots[&topic]["binding_generation"], 2);
        assert_eq!(rebase.snapshot.snapshots[&topic]["sessions"][0], "after");
    }

    #[test]
    fn byte_bound_queue_requires_rebase_before_count_bound() {
        let hub = AutomationHub::new();
        let scope = "instance:test".to_owned();
        let subscription = hub
            .events()
            .subscribe(
                owner(),
                [COMMAND_COMPLETED_TOPIC.to_owned()].into_iter().collect(),
                scope.clone(),
            )
            .unwrap()
            .subscription;
        hub.publish_event(EventPublication::new(
            scope,
            COMMAND_COMPLETED_TOPIC,
            Value::Null,
            None,
            Value::String("x".repeat(EVENT_QUEUE_BYTE_LIMIT)),
        ))
        .unwrap();

        let error = hub.events().poll(&subscription, &owner(), 0).unwrap_err();
        assert_eq!(error.code, -32005);
        assert_eq!(error.data.unwrap()["rebase"], "snapshot");
    }

    #[test]
    fn owner_generation_prevents_pid_reuse_access() {
        let hub = AutomationHub::new();
        let cancellation = CommandCancellation::new();
        let task = hub
            .tasks()
            .start(owner(), cancellation, "instance:test".to_owned())
            .unwrap();
        let reused_pid = OwnerIdentity::new(7, 12);
        assert_eq!(
            hub.tasks().status(&task.id, &reused_pid).unwrap_err().code,
            -32006
        );
    }
    #[test]
    fn unregister_topic_retired_subscriptions_remain_bounded_and_stale() {
        let hub = AutomationHub::new();
        let scope = "instance:topic-churn".to_owned();
        let mut first_subscription = None;
        for index in 0..(MAX_SUBSCRIPTIONS * 2) {
            let topic = format!("extension.churn_{index}");
            hub.events().register_topic(&topic).unwrap();
            let subscription = hub
                .events()
                .subscribe(
                    owner(),
                    [topic.clone()].into_iter().collect(),
                    scope.clone(),
                )
                .unwrap()
                .subscription;
            if first_subscription.is_none() {
                first_subscription = Some(subscription);
            }
            hub.events().unregister_topic(&topic).unwrap();
        }
        let state = lock(&hub.events().state);
        assert!(state.retired_subscriptions.len() <= MAX_SUBSCRIPTIONS);
        assert!(state.retired_subscription_watermark >= (MAX_SUBSCRIPTIONS * 2) as u64);
        drop(state);
        assert_eq!(
            hub.events()
                .poll(&first_subscription.unwrap(), &owner(), 0)
                .unwrap_err()
                .code,
            -32006
        );
    }

    #[test]
    fn terminal_output_requires_reread_after_retention_gap() {
        let hub = AutomationHub::new();
        let target = CommandTarget {
            kind: ResourceKind::Terminal,
            handle: "term-4".to_owned(),
            generation: 3,
        };
        hub.publish_terminal_output(
            "instance:test",
            Value::Null,
            target.clone(),
            Value::String("x".repeat(TERMINAL_OUTPUT_BYTE_LIMIT + 1)),
        )
        .unwrap();
        let error = hub
            .terminal_output_after("instance:test", &target, 0)
            .unwrap_err();
        assert_eq!(error.code, -32005);
        assert_eq!(error.data.unwrap()["rebase"], "reread");
    }

    #[test]
    fn evicted_terminal_output_stream_requires_reread() {
        let hub = AutomationHub::new();
        let scope = "instance:test";
        let first = CommandTarget {
            kind: ResourceKind::Terminal,
            handle: "term-0".to_owned(),
            generation: 1,
        };
        for index in 0..=(TERMINAL_OUTPUT_STREAM_LIMIT * 2) {
            hub.publish_terminal_output(
                scope,
                Value::Null,
                CommandTarget {
                    kind: ResourceKind::Terminal,
                    handle: format!("term-{index}"),
                    generation: 1,
                },
                Value::Null,
            )
            .unwrap();
        }

        let error = hub.terminal_output_after(scope, &first, 0).unwrap_err();
        assert_eq!(error.code, -32005);
        assert_eq!(error.data.unwrap()["rebase"], "reread");
    }

    #[test]
    fn retiring_binding_generation_purges_output_streams_and_tombstones() {
        let hub = AutomationHub::new();
        let scope = "binding:space:binding";
        let retired_stream = CommandTarget {
            kind: ResourceKind::Terminal,
            handle: "retired-binding/stream".to_owned(),
            generation: 1,
        };
        hub.publish_terminal_output(scope, Value::Null, retired_stream.clone(), Value::Null)
            .unwrap();
        for index in 0..TERMINAL_OUTPUT_STREAM_LIMIT {
            hub.publish_terminal_output(
                scope,
                Value::Null,
                CommandTarget {
                    kind: ResourceKind::Terminal,
                    handle: format!("live-binding/{index}"),
                    generation: 1,
                },
                Value::Null,
            )
            .unwrap();
        }
        let retired_live = CommandTarget {
            kind: ResourceKind::Terminal,
            handle: "retired-binding/live".to_owned(),
            generation: 2,
        };
        hub.publish_terminal_output(scope, Value::Null, retired_live.clone(), Value::Null)
            .unwrap();

        assert_eq!(
            hub.events()
                .purge_terminal_output(scope, |target| {
                    target.handle.starts_with("retired-binding/")
                })
                .unwrap(),
            2
        );
        for target in [&retired_stream, &retired_live] {
            let error = hub.terminal_output_after(scope, target, 0).unwrap_err();
            assert_eq!(error.code, -32005);
            assert_eq!(error.data.unwrap()["stream_cursor"], 0);
        }
        let topics = [TERMINAL_OUTPUT_TOPIC.to_owned()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let snapshot = hub.events().snapshot(scope, &topics).unwrap();
        let streams = snapshot.snapshots[TERMINAL_OUTPUT_TOPIC]["streams"]
            .as_array()
            .unwrap();
        assert!(streams.iter().all(|stream| {
            !stream["target"]["handle"]
                .as_str()
                .unwrap()
                .starts_with("retired-binding/")
        }));
    }

    #[test]
    fn disconnect_owner_cancels_only_its_running_tasks() {
        let hub = AutomationHub::new();
        let cancellation = CommandCancellation::new();
        let task = hub
            .tasks()
            .start(owner(), cancellation.clone(), "instance:test".to_owned())
            .unwrap();
        hub.disconnect_owner(&owner());
        assert!(cancellation.is_cancelled());
        assert_eq!(
            serde_json::to_value(hub.tasks().status(&task.id, &owner()).unwrap()).unwrap()["state"]
                ["status"],
            "cancelling"
        );
    }

    #[test]
    fn cancel_scope_cancels_only_tasks_in_its_scope() {
        let hub = AutomationHub::new();
        let matching_cancellation = CommandCancellation::new();
        let matching = hub
            .tasks()
            .start(
                owner(),
                matching_cancellation.clone(),
                "instance:one".to_owned(),
            )
            .unwrap();
        let other_cancellation = CommandCancellation::new();
        let other = hub
            .tasks()
            .start(
                owner(),
                other_cancellation.clone(),
                "instance:two".to_owned(),
            )
            .unwrap();

        assert_eq!(hub.tasks().cancel_scope("instance:one"), 1);
        assert!(matching_cancellation.is_cancelled());
        assert!(!other_cancellation.is_cancelled());
        assert_eq!(
            serde_json::to_value(hub.tasks().status(&matching.id, &owner()).unwrap()).unwrap()["state"]
                ["status"],
            "cancelling"
        );
        assert_eq!(
            serde_json::to_value(hub.tasks().status(&other.id, &owner()).unwrap()).unwrap()["state"]
                ["status"],
            "running"
        );
    }

    #[test]
    fn oversized_task_completion_is_terminal_and_rebase_is_bounded() {
        let hub = AutomationHub::new();
        let scope = "instance:test".to_owned();
        let task = hub
            .tasks()
            .start(owner(), CommandCancellation::new(), scope.clone())
            .unwrap();
        let topics = ["task.changed".to_owned()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let subscription = hub
            .events()
            .subscribe(owner(), topics.clone(), scope.clone())
            .unwrap()
            .subscription;
        let outcome = json!({
            "status": "success",
            "value": "x".repeat(crate::control::REQUEST_LIMIT as usize - 1024),
        });
        let outcome_bytes = value_size(&outcome).unwrap();
        assert!(outcome_bytes < crate::control::REQUEST_LIMIT as usize);
        assert!(outcome_bytes > EVENT_QUEUE_BYTE_LIMIT);
        let before = hub.events().snapshot(&scope, &topics).unwrap();

        hub.tasks().finish(&task.id, &outcome).unwrap();

        let status = serde_json::to_value(hub.tasks().status(&task.id, &owner()).unwrap()).unwrap();
        assert_eq!(status["state"]["status"], "completed");
        assert_eq!(status["state"]["outcome"]["status"], "failed");
        assert_eq!(status["state"]["outcome"]["code"], TASK_OUTCOME_LIMIT_CODE);
        assert_eq!(
            status["state"]["outcome"]["message"],
            TASK_OUTCOME_LIMIT_MESSAGE
        );
        assert_eq!(
            status["state"]["outcome"]["outcome_bytes"],
            json!(outcome_bytes)
        );
        assert_eq!(
            status["state"]["outcome"]["limit_bytes"],
            json!(TASK_OUTCOME_BYTE_LIMIT)
        );
        let delivery = hub.events().poll(&subscription, &owner(), 0).unwrap();
        assert_eq!(delivery.events.len(), 1);
        assert_eq!(delivery.events[0].payload["operation"], "completed");
        assert_eq!(delivery.events[0].payload["task"], status);

        let rebase = hub.events().rebase(&subscription, &owner()).unwrap();
        assert_eq!(rebase.revision, before.revision + 1);
        assert!(value_size(&rebase.snapshot).unwrap() <= EVENT_QUEUE_BYTE_LIMIT);
        assert_eq!(
            rebase.snapshot.snapshots["task.changed"]["tasks"][task.id.as_str()],
            status
        );
    }

    #[test]
    fn retained_task_outcomes_keep_a_full_task_index_rebasable() {
        let hub = AutomationHub::new();
        let scope = "instance:test".to_owned();
        let outcome = Value::String("x".repeat(TASK_OUTCOME_BYTE_LIMIT - 2));
        assert_eq!(value_size(&outcome).unwrap(), TASK_OUTCOME_BYTE_LIMIT);

        for _ in 0..MAX_TASKS {
            let task = hub
                .tasks()
                .start(owner(), CommandCancellation::new(), scope.clone())
                .unwrap();
            hub.tasks().finish(&task.id, &outcome).unwrap();
        }

        let topics = ["task.changed".to_owned()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let subscription = hub
            .events()
            .subscribe(owner(), topics, scope)
            .unwrap()
            .subscription;
        let rebase = hub.events().rebase(&subscription, &owner()).unwrap();
        assert!(value_size(&rebase.snapshot).unwrap() <= EVENT_QUEUE_BYTE_LIMIT);
        let tasks = rebase.snapshot.snapshots["task.changed"]["tasks"]
            .as_object()
            .unwrap();
        assert_eq!(tasks.len(), MAX_TASKS);
        assert!(
            tasks
                .values()
                .all(|task| task["state"]["status"] == "completed")
        );
    }

    #[test]
    fn oversized_task_completion_rebases_to_terminal_state_after_gap() {
        let hub = AutomationHub::new();
        let scope = "instance:test".to_owned();
        let task = hub
            .tasks()
            .start(owner(), CommandCancellation::new(), scope.clone())
            .unwrap();
        let topics = ["task.changed".to_owned()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let subscription = hub
            .events()
            .subscribe(owner(), topics.clone(), scope.clone())
            .unwrap()
            .subscription;
        let queued_transitions = EVENT_QUEUE_LIMIT / 2 + 1;
        for _ in 0..queued_transitions {
            let queued = hub
                .tasks()
                .start(owner(), CommandCancellation::new(), scope.clone())
                .unwrap();
            hub.tasks().finish(&queued.id, &Value::Null).unwrap();
        }
        assert_eq!(
            hub.events()
                .poll(&subscription, &owner(), 0)
                .unwrap_err()
                .code,
            -32005
        );
        let before = hub.events().snapshot(&scope, &topics).unwrap();
        let outcome = Value::String("x".repeat(EVENT_QUEUE_BYTE_LIMIT));

        hub.tasks().finish(&task.id, &outcome).unwrap();

        let status = serde_json::to_value(hub.tasks().status(&task.id, &owner()).unwrap()).unwrap();
        assert_eq!(status["state"]["status"], "completed");
        assert_eq!(status["state"]["outcome"]["status"], "failed");
        let rebase = hub.events().rebase(&subscription, &owner()).unwrap();
        assert_eq!(rebase.revision, before.revision + 1);
        assert_eq!(rebase.cursor, (queued_transitions * 2 + 1) as u64);
        assert!(value_size(&rebase.snapshot).unwrap() <= EVENT_QUEUE_BYTE_LIMIT);
        assert_eq!(
            rebase.snapshot.snapshots["task.changed"]["tasks"][task.id.as_str()],
            status
        );
        let delivery = hub
            .events()
            .poll(&subscription, &owner(), rebase.cursor)
            .unwrap();
        assert_eq!(delivery.revision, rebase.revision);
        assert!(delivery.events.is_empty());
    }

    #[test]
    fn task_completion_publication_failure_remains_terminal_and_is_returned() {
        let tasks = TaskHub::new(EventHub::new());
        let task = "task-1";
        {
            let mut state = lock(&tasks.state);
            state.tasks.insert(
                task.to_owned(),
                TaskRecord {
                    owner: owner(),
                    scope: "instance:test".to_owned(),
                    cancellation: CommandCancellation::new(),
                    state: TaskState::Running,
                },
            );
        }

        let error = tasks.finish(task, &Value::Null).unwrap_err();

        assert_eq!(error.code, -32602);
        let status = serde_json::to_value(tasks.status(task, &owner()).unwrap()).unwrap();
        assert_eq!(status["state"]["status"], "completed");
        assert_eq!(status["state"]["outcome"]["status"], "failed");
        assert_eq!(
            status["state"]["outcome"]["message"],
            TASK_COMPLETION_PUBLICATION_MESSAGE
        );
        assert_eq!(
            lock(&tasks.state)
                .completed_tasks
                .back()
                .map(String::as_str),
            Some(task)
        );
    }

    #[test]
    fn force_termination_removes_task_when_publication_fails() {
        let tasks = TaskHub::new(EventHub::new());
        let task = "task-force";
        let cancellation = CommandCancellation::new();
        {
            let mut state = lock(&tasks.state);
            state.tasks.insert(
                task.to_owned(),
                TaskRecord {
                    owner: owner(),
                    scope: "instance:test".to_owned(),
                    cancellation: cancellation.clone(),
                    state: TaskState::Running,
                },
            );
        }

        let error = tasks
            .terminate_force_checked(task, &owner())
            .expect_err("unregistered task topic should reject publication");

        assert_eq!(error.code, -32602);
        assert!(cancellation.is_cancel_requested());
        assert_eq!(tasks.status(task, &owner()).unwrap_err().code, -32602);
    }

    #[test]
    fn command_completion_keeps_target_and_provenance() {
        let hub = AutomationHub::new();
        let scope = "instance:test".to_owned();
        let subscription = hub
            .events()
            .subscribe(
                owner(),
                [COMMAND_COMPLETED_TOPIC.to_owned()].into_iter().collect(),
                scope.clone(),
            )
            .unwrap()
            .subscription;
        let invocation = CommandInvocation {
            command: "new_tab".to_owned(),
            arguments: Vec::new(),
            caller: Caller::Socket,
            target: Some(CommandTarget {
                kind: ResourceKind::Pane,
                handle: "pane-4".to_owned(),
                generation: 3,
            }),
            confirmation: None,
        };
        hub.publish_command_completion(
            scope,
            &owner(),
            &invocation,
            json!({"status": "success", "value": null}),
        )
        .unwrap();
        let event = hub
            .events()
            .poll(&subscription, &owner(), 0)
            .unwrap()
            .events
            .remove(0);
        assert_eq!(event.provenance["caller"], "socket");
        assert_eq!(event.provenance["owner_pid"], 7);
        assert_eq!(event.target.unwrap().handle, "pane-4");
    }

    #[test]
    fn metadata_clear_rejects_oversized_result_without_removing_record() {
        let hub = AutomationHub::new();
        let scope = "instance:metadata-clear".to_owned();
        let clear_key = metadata_key(&scope, "test", "clear", None);
        let remaining_key = metadata_key(&scope, "test", "remaining", None);
        let clear_record = MetadataRecord {
            scope: scope.clone(),
            namespace: "test".to_owned(),
            key: "clear".to_owned(),
            target: None,
            value: Value::Null,
            expires_at_ms: None,
            provenance: Value::Null,
            generation: 1,
        };
        let remaining_record = MetadataRecord {
            scope: scope.clone(),
            namespace: "test".to_owned(),
            key: "remaining".to_owned(),
            target: None,
            value: Value::String("x".repeat(EVENT_QUEUE_BYTE_LIMIT)),
            expires_at_ms: None,
            provenance: Value::Null,
            generation: 2,
        };
        {
            let mut metadata_state = lock(&hub.metadata().state);
            metadata_state
                .records
                .insert(clear_key.clone(), clear_record.clone());
            metadata_state
                .records
                .insert(remaining_key, remaining_record);
        }
        let error = hub
            .metadata()
            .clear(&scope, "test", "clear", None, Value::Null)
            .unwrap_err();
        assert_eq!(error.code, -32003);
        let metadata_state = lock(&hub.metadata().state);
        assert_eq!(metadata_state.records.get(&clear_key), Some(&clear_record));
    }

    #[test]
    fn metadata_expiry_preflights_all_scopes_before_removing_any_record() {
        let hub = AutomationHub::new();
        let first_scope = "instance:expiry-a".to_owned();
        let second_scope = "instance:expiry-b".to_owned();
        let first_key = metadata_key(&first_scope, "test", "expired", None);
        let second_key = metadata_key(&second_scope, "test", "expired", None);
        let remaining_key = metadata_key(&second_scope, "test", "remaining", None);
        {
            let mut metadata_state = lock(&hub.metadata().state);
            metadata_state.records.insert(
                first_key.clone(),
                MetadataRecord {
                    scope: first_scope,
                    namespace: "test".to_owned(),
                    key: "expired".to_owned(),
                    target: None,
                    value: Value::Null,
                    expires_at_ms: Some(10),
                    provenance: Value::Null,
                    generation: 1,
                },
            );
            metadata_state.records.insert(
                second_key.clone(),
                MetadataRecord {
                    scope: second_scope.clone(),
                    namespace: "test".to_owned(),
                    key: "expired".to_owned(),
                    target: None,
                    value: Value::Null,
                    expires_at_ms: Some(10),
                    provenance: Value::Null,
                    generation: 2,
                },
            );
            metadata_state.records.insert(
                remaining_key.clone(),
                MetadataRecord {
                    scope: second_scope,
                    namespace: "test".to_owned(),
                    key: "remaining".to_owned(),
                    target: None,
                    value: Value::String("x".repeat(EVENT_QUEUE_BYTE_LIMIT)),
                    expires_at_ms: None,
                    provenance: Value::Null,
                    generation: 3,
                },
            );
        }
        let error = hub.metadata().reap_expired_at(10).unwrap_err();
        assert_eq!(error.code, -32003);
        let metadata_state = lock(&hub.metadata().state);
        assert!(metadata_state.records.contains_key(&first_key));
        assert!(metadata_state.records.contains_key(&second_key));
        assert!(metadata_state.records.contains_key(&remaining_key));
    }

    #[test]
    fn polling_reaps_metadata_ttl_without_an_app_frame() {
        let hub = AutomationHub::new();
        let scope = "instance:metadata-poll-ttl".to_owned();
        let topics = ["metadata.changed".to_owned(), "metadata.expired".to_owned()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let subscription = hub
            .events()
            .subscribe(owner(), topics, scope.clone())
            .unwrap()
            .subscription;
        let expires_at_ms = unix_time_ms().unwrap().saturating_add(5);
        hub.metadata()
            .publish(MetadataPublication::new(
                scope,
                "test",
                "ttl",
                None,
                Value::Null,
                Some(expires_at_ms),
                Value::Null,
            ))
            .unwrap();
        while unix_time_ms().unwrap() < expires_at_ms {
            thread::sleep(Duration::from_millis(1));
        }
        let delivery = hub.events().poll(&subscription, &owner(), 0).unwrap();
        assert_eq!(
            delivery
                .events
                .iter()
                .map(|event| event.topic.as_str())
                .collect::<Vec<_>>(),
            vec!["metadata.changed", "metadata.expired"]
        );
    }

    #[test]
    fn metadata_expiry_publishes_authoritative_source_snapshots() {
        let hub = AutomationHub::new();
        let scope = "instance:test".to_owned();
        let topics = ["metadata.changed".to_owned(), "metadata.expired".to_owned()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let subscription = hub
            .events()
            .subscribe(owner(), topics.clone(), scope.clone())
            .unwrap()
            .subscription;
        let record = hub
            .metadata()
            .publish(MetadataPublication::new(
                scope.clone(),
                "extension.example",
                "status",
                None,
                json!({"ready": true}),
                Some(u64::MAX),
                json!({"source": "test"}),
            ))
            .unwrap();
        assert_eq!(record.generation, 1);
        assert_eq!(
            hub.events().snapshot(&scope, &topics).unwrap().snapshots["metadata.changed"]["records"]
                [0]["value"]["ready"],
            true
        );

        assert_eq!(hub.metadata().reap_expired_at(u64::MAX).unwrap(), 1);
        assert!(
            hub.metadata()
                .get(&scope, "extension.example", "status", None)
                .unwrap()
                .is_none()
        );
        let delivery = hub.events().poll(&subscription, &owner(), 0).unwrap();
        assert_eq!(
            delivery
                .events
                .iter()
                .map(|event| event.topic.as_str())
                .collect::<Vec<_>>(),
            vec!["metadata.changed", "metadata.expired"]
        );
        let snapshot = hub.events().snapshot(&scope, &topics).unwrap();
        assert_eq!(snapshot.snapshots["metadata.changed"]["records"], json!([]));
        assert_eq!(snapshot.snapshots["metadata.expired"]["records"], json!([]));
    }

    #[test]
    fn metadata_mutation_keeps_its_source_snapshot_publication_ordered() {
        let hub = AutomationHub::new();
        let scope = "instance:test".to_owned();
        let (publication_tx, publication_rx) = mpsc::channel();
        hub.events()
            .set_before_publication_lock(Some(publication_tx));
        let event_state = lock(&hub.events().state);

        let first_hub = hub.clone();
        let first_scope = scope.clone();
        let first = thread::spawn(move || {
            first_hub
                .metadata()
                .publish(MetadataPublication::new(
                    first_scope,
                    "extension.example",
                    "first",
                    None,
                    Value::Null,
                    None,
                    Value::Null,
                ))
                .unwrap();
        });
        publication_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first metadata publication reached the event boundary");

        let (second_started_tx, second_started_rx) = mpsc::channel();
        let second_hub = hub.clone();
        let second_scope = scope.clone();
        let second = thread::spawn(move || {
            second_started_tx.send(()).unwrap();
            second_hub
                .metadata()
                .publish(MetadataPublication::new(
                    second_scope,
                    "extension.example",
                    "second",
                    None,
                    Value::Null,
                    None,
                    Value::Null,
                ))
                .unwrap();
        });
        second_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("second metadata mutation started");
        let second_reached_event_early = publication_rx.recv_timeout(Duration::from_millis(100));
        drop(event_state);
        first.join().unwrap();
        second.join().unwrap();
        assert!(
            second_reached_event_early.is_err(),
            "a later metadata mutation reached event publication before the first completed"
        );

        let topics = ["metadata.changed".to_owned()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let snapshot = hub.events().snapshot(&scope, &topics).unwrap();
        let records = snapshot.snapshots["metadata.changed"]["records"]
            .as_array()
            .unwrap();
        assert_eq!(
            records
                .iter()
                .map(|record| record["key"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
    }

    #[test]
    fn task_transition_keeps_its_source_snapshot_publication_ordered() {
        let hub = AutomationHub::new();
        let scope = "instance:test".to_owned();
        let (publication_tx, publication_rx) = mpsc::channel();
        hub.events()
            .set_before_publication_lock(Some(publication_tx));
        let event_state = lock(&hub.events().state);

        let first_hub = hub.clone();
        let first_scope = scope.clone();
        let first = thread::spawn(move || {
            first_hub
                .tasks()
                .start(
                    OwnerIdentity::new(1, 1),
                    CommandCancellation::new(),
                    first_scope,
                )
                .unwrap();
        });
        publication_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first task transition reached the event boundary");

        let (second_started_tx, second_started_rx) = mpsc::channel();
        let second_hub = hub.clone();
        let second_scope = scope.clone();
        let second = thread::spawn(move || {
            second_started_tx.send(()).unwrap();
            second_hub
                .tasks()
                .start(
                    OwnerIdentity::new(2, 1),
                    CommandCancellation::new(),
                    second_scope,
                )
                .unwrap();
        });
        second_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("second task transition started");
        let second_reached_event_early = publication_rx.recv_timeout(Duration::from_millis(100));
        drop(event_state);
        first.join().unwrap();
        second.join().unwrap();
        assert!(
            second_reached_event_early.is_err(),
            "a later task transition reached event publication before the first completed"
        );

        let topics = ["task.changed".to_owned()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            hub.events().snapshot(&scope, &topics).unwrap().snapshots["task.changed"]["tasks"]
                .as_object()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn instance_scope_binding_is_shared_and_immutable() {
        let hub = AutomationHub::new();
        let clone = hub.clone();

        hub.bind_instance_scope("instance:one").unwrap();
        assert_eq!(clone.instance_scope().as_deref(), Some("instance:one"));
        assert!(clone.bind_instance_scope("instance:one").is_ok());
        assert_eq!(
            clone.bind_instance_scope("instance:two").unwrap_err().code,
            -32603
        );
    }
}
