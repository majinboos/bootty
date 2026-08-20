use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex},
};

use bootty_command::{CommandCancellation, CommandInvocation};
use serde::Serialize;
use serde_json::{Value, json};

use crate::protocol::{
    COMMAND_COMPLETED_TOPIC, EVENT_QUEUE_LIMIT, MAX_SUBSCRIPTIONS, MAX_TASKS, REQUEST_LIMIT,
    RpcError, internal_error,
};

pub(crate) type SharedControlState = Arc<Mutex<ControlState>>;

pub(crate) struct ControlState {
    tasks: BTreeMap<String, TaskRecord>,
    completed_tasks: VecDeque<String>,
    subscriptions: BTreeMap<String, SubscriptionRecord>,
    revisions: BTreeMap<String, u64>,
    pub(crate) topics: BTreeSet<String>,
}

impl Default for ControlState {
    fn default() -> Self {
        Self {
            tasks: BTreeMap::new(),
            completed_tasks: VecDeque::new(),
            subscriptions: BTreeMap::new(),
            revisions: BTreeMap::new(),
            topics: BTreeSet::from([COMMAND_COMPLETED_TOPIC.to_owned()]),
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum TaskState {
    Running,
    Cancelling,
    Completed { outcome: Value },
}

struct TaskRecord {
    owner_pid: u32,
    cancellation: CommandCancellation,
    state: TaskState,
}

struct SubscriptionRecord {
    topics: BTreeSet<String>,
    scope: String,
    sequence: u64,
    cursor: u64,
    events: VecDeque<SubscriptionEvent>,
    gap: Option<SubscriptionGap>,
}

#[derive(Clone)]
struct SubscriptionGap {
    sequence: u64,
}

#[derive(Clone, Serialize)]
struct SubscriptionEvent {
    sequence: u64,
    scope: String,
    revision: u64,
    topic: String,
    provenance: Value,
    target: Value,
    payload: Value,
}

impl ControlState {
    pub(crate) fn start_task(
        &mut self,
        owner_pid: u32,
        cancellation: CommandCancellation,
    ) -> Result<String, RpcError> {
        while self.tasks.len() >= MAX_TASKS {
            let Some(completed) = self.completed_tasks.pop_front() else {
                return Err(RpcError::new(-32001, "task limit reached"));
            };
            self.tasks.remove(&completed);
        }
        let id = loop {
            let candidate = capability_id("task")?;
            if !self.tasks.contains_key(&candidate) {
                break candidate;
            }
        };
        self.tasks.insert(
            id.clone(),
            TaskRecord {
                owner_pid,
                cancellation,
                state: TaskState::Running,
            },
        );
        Ok(id)
    }

    pub(crate) fn remove_task(&mut self, task: &str) {
        self.tasks.remove(task);
    }

    pub(crate) fn finish_task(&mut self, task: &str, outcome: Value) {
        let Some(record) = self.tasks.get_mut(task) else {
            return;
        };
        if !matches!(record.state, TaskState::Completed { .. }) {
            record.state = TaskState::Completed { outcome };
            self.completed_tasks.push_back(task.to_owned());
        }
    }

    pub(crate) fn task_value(&self, task: &str) -> Result<Value, RpcError> {
        let record = self
            .tasks
            .get(task)
            .ok_or_else(|| RpcError::new(-32602, "unknown task"))?;
        Ok(json!({
            "task": {
                "id": task,
                "owner_pid": record.owner_pid,
                "state": &record.state,
            }
        }))
    }

    pub(crate) fn cancel_task(&mut self, task: &str) -> Result<Value, RpcError> {
        let record = self
            .tasks
            .get_mut(task)
            .ok_or_else(|| RpcError::new(-32602, "unknown task"))?;
        if matches!(record.state, TaskState::Running) && record.cancellation.cancel() {
            record.state = TaskState::Cancelling;
        }
        self.task_value(task)
    }

    pub(crate) fn cancel_all_tasks(&mut self) {
        for task in self.tasks.values_mut() {
            if matches!(task.state, TaskState::Running) {
                task.cancellation.cancel();
                task.state = TaskState::Cancelling;
            }
        }
    }

    pub(crate) fn create_subscription(
        &mut self,
        topics: BTreeSet<String>,
        scope: &str,
    ) -> Result<Value, RpcError> {
        if self.subscriptions.len() >= MAX_SUBSCRIPTIONS {
            return Err(RpcError::new(-32001, "subscription limit reached"));
        }
        let id = loop {
            let candidate = capability_id("subscription")?;
            if !self.subscriptions.contains_key(&candidate) {
                break candidate;
            }
        };
        let revision = *self.revisions.get(scope).unwrap_or(&0);
        self.subscriptions.insert(
            id.clone(),
            SubscriptionRecord {
                topics,
                scope: scope.to_owned(),
                sequence: 0,
                cursor: 0,
                events: VecDeque::new(),
                gap: None,
            },
        );
        Ok(json!({
            "subscription": id,
            "scope": scope,
            "revision": revision,
            "cursor": 0,
            "events": [],
        }))
    }

    pub(crate) fn poll_subscription(
        &mut self,
        subscription: &str,
        cursor: u64,
    ) -> Result<Value, RpcError> {
        let (scope, cursor, events) = {
            let record = self.subscription(subscription)?;
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
            let mut events = VecDeque::new();
            let mut bytes = 0;
            while let Some(event) = record.events.front() {
                let event_bytes = serde_json::to_vec(event)
                    .map_err(|error| internal_error(&error))?
                    .len();
                if bytes + event_bytes > REQUEST_LIMIT as usize / 2 {
                    break;
                }
                bytes += event_bytes;
                events.push_back(record.events.pop_front().expect("front event"));
            }
            if events.is_empty() && !record.events.is_empty() {
                record.events.clear();
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
            if let Some(event) = events.back() {
                record.cursor = event.sequence;
            }
            (record.scope.clone(), record.cursor, events)
        };
        let revision = *self.revisions.get(&scope).unwrap_or(&0);
        Ok(json!({
            "subscription": subscription,
            "scope": scope,
            "revision": revision,
            "cursor": cursor,
            "events": events,
        }))
    }

    pub(crate) fn unsubscribe(&mut self, subscription: &str) -> Result<Value, RpcError> {
        self.subscription(subscription)?;
        self.subscriptions.remove(subscription);
        Ok(json!({"unsubscribed": subscription}))
    }

    fn subscription(&mut self, subscription: &str) -> Result<&mut SubscriptionRecord, RpcError> {
        self.subscriptions
            .get_mut(subscription)
            .ok_or_else(|| RpcError::new(-32602, "unknown subscription"))
    }

    pub(crate) fn publish_command_completion(
        &mut self,
        scope: &str,
        owner_pid: u32,
        invocation: &CommandInvocation,
        outcome: &Value,
    ) {
        let provenance = json!({"caller": invocation.caller, "owner_pid": owner_pid});
        let target = serde_json::to_value(&invocation.target).unwrap_or(Value::Null);
        self.publish_event(
            scope,
            COMMAND_COMPLETED_TOPIC,
            &provenance,
            &target,
            &json!({"command": invocation.command, "outcome": outcome}),
        );
    }

    pub(crate) fn publish_event(
        &mut self,
        scope: &str,
        topic: &str,
        provenance: &Value,
        target: &Value,
        payload: &Value,
    ) {
        let revision = self.revisions.entry(scope.to_owned()).or_default();
        *revision += 1;
        let revision = *revision;
        for subscription in self.subscriptions.values_mut() {
            if subscription.scope != scope || !subscription.topics.contains(topic) {
                continue;
            }
            subscription.sequence += 1;
            if subscription.gap.is_some() || subscription.events.len() >= EVENT_QUEUE_LIMIT {
                subscription.events.clear();
                subscription.gap = Some(SubscriptionGap {
                    sequence: subscription.sequence,
                });
                continue;
            }
            subscription.events.push_back(SubscriptionEvent {
                sequence: subscription.sequence,
                scope: scope.to_owned(),
                revision,
                topic: topic.to_owned(),
                provenance: provenance.clone(),
                target: target.clone(),
                payload: payload.clone(),
            });
        }
    }
}

fn rebase_error(subscription: &str, scope: &str, cursor: u64, sequence: u64) -> RpcError {
    let mut error = RpcError::new(-32005, "event rebase required");
    error.data = Some(json!({
        "subscription": subscription,
        "scope": scope,
        "cursor": cursor,
        "sequence": sequence,
        "rebase": "snapshot",
    }));
    error
}

fn capability_id(prefix: &str) -> Result<String, RpcError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| RpcError::new(-32603, format!("generate capability ID: {error}")))?;
    let digits = b"0123456789abcdef";
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        token.push(digits[(byte >> 4) as usize] as char);
        token.push(digits[(byte & 0x0f) as usize] as char);
    }
    Ok(format!("{prefix}-{token}"))
}

pub(crate) fn lock_control_state(
    state: &SharedControlState,
) -> std::sync::MutexGuard<'_, ControlState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
