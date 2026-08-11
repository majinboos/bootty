use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandCancellation, CommandInvocation, CommandTarget};

pub const MAX_TASKS: usize = 64;
pub const MAX_SUBSCRIPTIONS: usize = 64;
pub const MAX_TOPICS_PER_SUBSCRIPTION: usize = 16;
pub const EVENT_QUEUE_LIMIT: usize = 64;
pub const EVENT_QUEUE_BYTE_LIMIT: usize = 512 * 1024;
pub const EVENT_TOPIC_LIMIT: usize = 128;
pub const MAX_REGISTERED_EVENT_TOPICS: usize = 256;
pub const TERMINAL_OUTPUT_STREAM_LIMIT: usize = 64;
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
            self.events.set_snapshot(
                scope.clone(),
                "extension.reloaded",
                json!({"modules": []}),
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
    #[cfg(test)]
    before_publication_lock: Arc<Mutex<Option<std::sync::mpsc::Sender<()>>>>,
}

struct EventState {
    registered_topics: BTreeSet<String>,
    next_subscription: u64,
    subscriptions: BTreeMap<String, SubscriptionRecord>,
    retired_subscriptions: BTreeSet<String>,
    retired_subscription_order: VecDeque<String>,
    revisions: BTreeMap<String, u64>,
    snapshots: BTreeMap<String, BTreeMap<String, Value>>,
    live_binding_scopes: BTreeSet<String>,
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
                revisions: BTreeMap::new(),
                retired_subscriptions: BTreeSet::new(),
                retired_subscription_order: VecDeque::new(),
                snapshots: BTreeMap::new(),
                live_binding_scopes: BTreeSet::new(),
                output_streams: BTreeMap::new(),
                output_stream_order: VecDeque::new(),
                output_tombstones: BTreeMap::new(),
                output_tombstone_order: VecDeque::new(),
            })),
            #[cfg(test)]
            before_publication_lock: Arc::new(Mutex::new(None)),
        }
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

    #[cfg(test)]
    fn set_before_publication_lock(&self, sender: Option<std::sync::mpsc::Sender<()>>) {
        *lock(&self.before_publication_lock) = sender;
    }

    #[cfg(test)]
    fn notify_before_publication_lock(&self) {
        if let Some(sender) = lock(&self.before_publication_lock).clone() {
            let _ = sender.send(());
        }
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
            .collect::<Vec<_>>();
        let tombstone_keys = state
            .output_tombstones
            .keys()
            .filter_map(|key| {
                terminal_output_key_target(key)
                    .filter(|(key_scope, target)| key_scope == scope && is_retired(target))
                    .map(|_| key.clone())
            })
            .collect::<Vec<_>>();

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

        if !stream_keys.is_empty() || !tombstone_keys.is_empty() {
            validate_registered_topic(&state, TERMINAL_OUTPUT_TOPIC)?;
            let snapshot = terminal_output_snapshot(&state, scope);
            validate_snapshot_size(&snapshot)?;
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
        let (scope, topics, cursor) = {
            let record = owned_subscription(&mut state, subscription, owner)?;
            record.events.clear();
            record.queued_bytes = 0;
            record.gap = None;
            record.cursor = record.sequence;
            (record.scope.clone(), record.topics.clone(), record.cursor)
        };
        let snapshot = snapshot_locked(&state, &scope, &topics)?;
        Ok(EventRebase {
            subscription: subscription.to_owned(),
            scope,
            revision: snapshot.revision,
            cursor,
            snapshot,
        })
    }

    pub fn publish(&self, publication: EventPublication) -> Result<u64, AutomationError> {
        let mut state = lock(&self.state);
        publish_locked(&mut state, publication)
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
        #[cfg(test)]
        self.notify_before_publication_lock();
        let snapshots = snapshots.into_iter().collect::<BTreeMap<_, _>>();
        let mut state = lock(&self.state);
        validate_registered_topic(&state, &publication.topic)?;
        replace_snapshots_locked(&mut state, &publication.scope, snapshots)?;
        publish_locked(&mut state, publication)
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
        let key = terminal_output_key(&scope, &target);
        let mut affected_scopes = BTreeSet::from([scope.clone()]);
        if !state.output_streams.contains_key(&key) {
            while state.output_streams.len() >= TERMINAL_OUTPUT_STREAM_LIMIT {
                let Some(oldest) = state.output_stream_order.pop_front() else {
                    break;
                };
                if let Some(removed) = state.output_streams.remove(&oldest) {
                    affected_scopes.insert(removed.scope.clone());
                    remember_output_tombstone(&mut state, oldest, removed.cursor);
                }
            }
            let previous_cursor = state.output_tombstones.remove(&key).unwrap_or(0);
            state.output_tombstone_order.retain(|known| known != &key);
            state.output_stream_order.push_back(key.clone());
            state.output_streams.insert(
                key.clone(),
                TerminalOutputStream {
                    scope: scope.clone(),
                    target: target.clone(),
                    cursor: previous_cursor,
                    retained_bytes: 0,
                    chunks: VecDeque::new(),
                },
            );
        }
        let cursor = {
            let stream = state
                .output_streams
                .get_mut(&key)
                .expect("terminal output stream was inserted");
            stream.cursor = stream
                .cursor
                .checked_add(1)
                .ok_or_else(|| AutomationError::new(-32003, "terminal output cursor exhausted"))?;
            let cursor = stream.cursor;
            let bytes = value_size(&payload)?;
            if bytes > TERMINAL_OUTPUT_BYTE_LIMIT {
                stream.chunks.clear();
                stream.retained_bytes = 0;
            } else {
                stream.retained_bytes += bytes;
                stream.chunks.push_back(TerminalOutputChunk {
                    cursor,
                    payload: payload.clone(),
                });
                while stream.retained_bytes > TERMINAL_OUTPUT_BYTE_LIMIT {
                    let Some(removed) = stream.chunks.pop_front() else {
                        break;
                    };
                    stream.retained_bytes = stream
                        .retained_bytes
                        .saturating_sub(value_size(&removed.payload)?);
                }
            }
            cursor
        };
        for affected_scope in affected_scopes {
            let snapshot = terminal_output_snapshot(&state, &affected_scope);
            validate_snapshot_size(&snapshot)?;
            state
                .snapshots
                .entry(affected_scope)
                .or_default()
                .insert(TERMINAL_OUTPUT_TOPIC.to_owned(), snapshot);
        }
        publish_locked(
            &mut state,
            EventPublication::new(
                scope,
                TERMINAL_OUTPUT_TOPIC,
                provenance,
                Some(target),
                json!({"cursor": cursor, "data": payload}),
            ),
        )
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

struct TerminalOutputStream {
    scope: String,
    target: CommandTarget,
    cursor: u64,
    retained_bytes: usize,
    chunks: VecDeque<TerminalOutputChunk>,
}

fn terminal_output_snapshot(state: &EventState, scope: &str) -> Value {
    let streams = state
        .output_streams
        .values()
        .filter(|stream| stream.scope == scope)
        .map(|stream| {
            json!({
                "target": &stream.target,
                "cursor": stream.cursor,
                "retained_from": stream
                    .chunks
                    .front()
                    .map(|chunk| chunk.cursor)
                    .unwrap_or_else(|| stream.cursor.saturating_add(1)),
            })
        })
        .collect::<Vec<_>>();
    json!({"streams": streams})
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

fn replace_snapshots_locked(
    state: &mut EventState,
    scope: &str,
    snapshots: BTreeMap<String, Value>,
) -> Result<(), AutomationError> {
    for (topic, snapshot) in &snapshots {
        validate_registered_topic(state, topic)?;
        validate_snapshot_size(snapshot)?;
    }
    let state_snapshots = state.snapshots.entry(scope.to_owned()).or_default();
    for (topic, snapshot) in snapshots {
        state_snapshots.insert(topic, snapshot);
    }
    Ok(())
}

fn purge_retired_scope_locked(state: &mut EventState, scope: &str) {
    state.snapshots.remove(scope);
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
        if state.retired_subscriptions.insert(subscription.clone()) {
            state.retired_subscription_order.push_back(subscription);
        }
    }
    while state.retired_subscription_order.len() > MAX_SUBSCRIPTIONS {
        if let Some(subscription) = state.retired_subscription_order.pop_front() {
            state.retired_subscriptions.remove(&subscription);
        }
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

fn publish_locked(
    state: &mut EventState,
    publication: EventPublication,
) -> Result<u64, AutomationError> {
    if publication.scope.is_empty() {
        return Err(AutomationError::new(
            -32602,
            "event scope must not be empty",
        ));
    }
    validate_registered_topic(state, &publication.topic)?;
    let revision = state
        .revisions
        .entry(publication.scope.clone())
        .or_default();
    *revision += 1;
    let revision = *revision;
    for subscription in state.subscriptions.values_mut() {
        if subscription.scope != publication.scope
            || !subscription.topics.contains(&publication.topic)
        {
            continue;
        }
        subscription.sequence += 1;
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
        let bytes = value_size(&event)?;
        if bytes > EVENT_QUEUE_BYTE_LIMIT
            || subscription.queued_bytes + bytes > EVENT_QUEUE_BYTE_LIMIT
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
    Ok(revision)
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
    if state.retired_subscriptions.contains(subscription) {
        return Err(AutomationError::new(
            -32006,
            "subscription event scope is not live",
        ));
    }
    let Some(record) = state.subscriptions.get_mut(subscription) else {
        return Err(AutomationError::new(-32602, "unknown subscription"));
    };
    if &record.owner != owner {
        return Err(AutomationError::new(
            -32006,
            "subscription is owned by another process",
        ));
    }
    Ok(record)
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
        Self {
            state: Arc::new(Mutex::new(MetadataState {
                next_generation: 1,
                records: BTreeMap::new(),
            })),
            events,
        }
    }
    /// Installs the current metadata store as bootstrap state for both
    /// metadata lifecycle topics without manufacturing a lifecycle event.
    pub fn install_snapshot(&self, scope: &str) -> Result<(), AutomationError> {
        if scope.is_empty() {
            return Err(AutomationError::new(
                -32602,
                "metadata scope must not be empty",
            ));
        }
        self.reap_expired()?;
        let state = lock(&self.state);
        let snapshot = metadata_snapshot(&state, scope);
        self.events
            .replace_snapshots(scope, metadata_source_snapshots(snapshot))
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
        let mut state = lock(&self.state);
        let previous = state.records.get(&key).cloned();
        if previous.is_none() && state.records.len() >= MAX_METADATA_RECORDS {
            return Err(AutomationError::new(
                -32001,
                "metadata record limit reached",
            ));
        }
        let generation = state.next_generation;
        state.next_generation = generation
            .checked_add(1)
            .ok_or_else(|| AutomationError::new(-32003, "metadata generation exhausted"))?;
        let record = MetadataRecord {
            scope: publication.scope,
            namespace: publication.namespace,
            key: publication.key,
            target: publication.target,
            value: publication.value,
            expires_at_ms: publication.expires_at_ms,
            provenance: publication.provenance,
            generation,
        };
        state.records.insert(key.clone(), record.clone());
        let snapshot = metadata_snapshot(&state, &record.scope);
        if let Err(error) = validate_snapshot_size(&snapshot) {
            match previous {
                Some(previous) => {
                    state.records.insert(key, previous);
                }
                None => {
                    state.records.remove(&key);
                }
            }
            return Err(error);
        }
        self.events.publish_with_snapshots(
            EventPublication::new(
                record.scope.clone(),
                "metadata.changed",
                record.provenance.clone(),
                record.target.clone(),
                json!({"operation": "published", "metadata": record.clone()}),
            ),
            metadata_source_snapshots(snapshot),
        )?;
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
        let mut state = lock(&self.state);
        let Some(record) = state.records.remove(&metadata_key) else {
            return Ok(None);
        };
        let snapshot = metadata_snapshot(&state, scope);
        self.events.publish_with_snapshots(
            EventPublication::new(
                scope.to_owned(),
                "metadata.changed",
                provenance,
                record.target.clone(),
                json!({"operation": "cleared", "metadata": record.clone()}),
            ),
            metadata_source_snapshots(snapshot),
        )?;
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
        let records = expired_keys
            .into_iter()
            .filter_map(|key| state.records.remove(&key))
            .collect::<Vec<_>>();
        let snapshots = records
            .iter()
            .map(|record| record.scope.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|scope| {
                let snapshot = metadata_snapshot(&state, &scope);
                (scope, snapshot)
            })
            .collect::<BTreeMap<_, _>>();
        let count = records.len();
        for record in records {
            let snapshot = snapshots
                .get(&record.scope)
                .expect("every expired metadata scope has a snapshot")
                .clone();
            self.events.publish_with_snapshots(
                EventPublication::new(
                    record.scope.clone(),
                    "metadata.expired",
                    json!({
                        "source": "metadata",
                        "operation": "expired",
                        "published_by": record.provenance.clone(),
                    }),
                    record.target.clone(),
                    json!({"metadata": record}),
                ),
                metadata_source_snapshots(snapshot),
            )?;
        }
        Ok(count)
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
    let records = state
        .records
        .values()
        .filter(|record| record.scope == scope)
        .cloned()
        .collect::<Vec<_>>();
    json!({"records": records})
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

struct TaskHubState {
    next_task: u64,
    tasks: BTreeMap<String, TaskRecord>,
    completed_tasks: VecDeque<String>,
}

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

    pub fn remove(&self, task: &str) {
        let mut state = lock(&self.state);
        let Some(record) = state.tasks.remove(task) else {
            return;
        };
        let completed_tasks = state.completed_tasks.clone();
        state.completed_tasks.retain(|completed| completed != task);
        let change = TaskChange {
            scope: record.scope.clone(),
            task: task_status(task, &record),
            operation: "removed",
        };
        if self.publish_task_change_locked(&state, change).is_err() {
            let _ = state.tasks.insert(task.to_owned(), record);
            state.completed_tasks = completed_tasks;
        }
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
        let (status, change) = {
            let record = owned_task_mut(&mut state, task, owner)?;
            if !matches!(record.state, TaskState::Running) {
                return Ok(task_status(task, record));
            }
            record.state = TaskState::Cancelling;
            let status = task_status(task, record);
            let change = TaskChange {
                scope: record.scope.clone(),
                task: status.clone(),
                operation: "cancelling",
            };
            (status, change)
        };
        if let Err(error) = task_change_snapshot(&state, &change) {
            state
                .tasks
                .get_mut(task)
                .expect("task exists while its state lock is held")
                .state = TaskState::Running;
            return Err(error);
        }
        if !state
            .tasks
            .get(task)
            .expect("task exists while its state lock is held")
            .cancellation
            .cancel()
        {
            let record = state
                .tasks
                .get_mut(task)
                .expect("task exists while its state lock is held");
            record.state = TaskState::Running;
            return Ok(task_status(task, record));
        }
        self.publish_task_change_locked(&state, change)?;
        Ok(status)
    }

    pub fn cancel_owner(&self, owner: &OwnerIdentity) -> usize {
        self.cancel_matching(|record| &record.owner == owner)
    }

    pub fn cancel_scope(&self, scope: &str) -> usize {
        self.cancel_matching(|record| record.scope.as_str() == scope)
    }

    pub fn cancel_all(&self) {
        self.cancel_matching(|_| true);
    }

    fn cancel_matching(&self, mut matches: impl FnMut(&TaskRecord) -> bool) -> usize {
        let mut state = lock(&self.state);
        let tasks = state
            .tasks
            .iter()
            .filter(|(_, record)| matches!(record.state, TaskState::Running) && matches(record))
            .map(|(task, _)| task.clone())
            .collect::<Vec<_>>();
        if tasks.is_empty() {
            return 0;
        }

        for task in &tasks {
            state
                .tasks
                .get_mut(task)
                .expect("task ID came from the task index")
                .state = TaskState::Cancelling;
        }
        let candidates = tasks
            .iter()
            .map(|task| {
                let record = state
                    .tasks
                    .get(task)
                    .expect("task ID came from the task index");
                TaskChange {
                    scope: record.scope.clone(),
                    task: task_status(task, record),
                    operation: "cancelling",
                }
            })
            .collect::<Vec<_>>();
        if self
            .validate_task_changes_locked(&state, &candidates)
            .is_err()
        {
            for task in tasks {
                state
                    .tasks
                    .get_mut(&task)
                    .expect("task ID came from the task index")
                    .state = TaskState::Running;
            }
            return 0;
        }

        let changes = tasks
            .into_iter()
            .filter_map(|task| {
                let record = state
                    .tasks
                    .get_mut(&task)
                    .expect("task ID came from the task index");
                if !record.cancellation.cancel() {
                    record.state = TaskState::Running;
                    return None;
                }
                Some(TaskChange {
                    scope: record.scope.clone(),
                    task: task_status(&task, record),
                    operation: "cancelling",
                })
            })
            .collect::<Vec<_>>();
        let count = changes.len();
        if count == 0 {
            return 0;
        }
        // The first preflight bounded every candidate snapshot; tasks whose
        // cancellation raced a start were restored to the smaller `running`
        // representation above.
        self.publish_task_changes_locked(&state, changes)
            .expect("validated task changes use the permanently registered task.changed topic");
        count
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
        for change in changes {
            self.publish_task_change_locked(state, change)?;
        }
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
        let snapshot = task_change_snapshot(state, &change)?;
        let TaskChange {
            scope,
            task,
            operation,
        } = change;
        self.events
            .publish_with_snapshot(
                EventPublication::new(
                    scope,
                    "task.changed",
                    json!({"owner_pid": task.owner_pid}),
                    None,
                    json!({"operation": operation, "task": task}),
                ),
                snapshot,
            )
            .map(|_| ())
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
    fn reconnect_replaces_binding_snapshot_before_gap_rebase() {
        let hub = AutomationHub::new();
        let scope = "binding:space:binding".to_owned();
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
