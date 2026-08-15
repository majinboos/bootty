use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use rmux_ipc::{LocalEndpoint, LocalListener, connect_blocking, endpoint_for_label};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

use crate::commands::{
    AppCommandRequest, AppCommandSendError, BoundAppCommandSender, Caller, CommandCancellation,
    CommandInvocation, CommandRegistry,
};

pub const PROTOCOL_VERSION: u32 = 1;
const REQUEST_LIMIT: u64 = 1024 * 1024;
const RPC_ID_LIMIT: usize = 4096;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONNECTIONS: usize = 32;
const MAX_TASKS: usize = 64;
const MAX_SUBSCRIPTIONS: usize = 64;
const MAX_TOPICS_PER_SUBSCRIPTION: usize = 16;
const EVENT_QUEUE_LIMIT: usize = 64;
const EVENT_TOPIC_LIMIT: usize = 128;
const TASK_WAIT_INTERVAL: Duration = Duration::from_millis(50);
const COMMAND_COMPLETED_TOPIC: &str = "command.completed";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceDescriptor {
    pub instance_id: String,
    pub generation: u64,
    pub pid: u32,
    pub window_state_key: String,
    pub endpoint: PathBuf,
    pub started_at_ms: u128,
    pub protocol_version: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

pub struct ControlServer {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
    descriptor_path: PathBuf,
    endpoint_path: PathBuf,
    state: SharedControlState,
}

impl ControlServer {
    pub fn spawn(window_state_key: String, commands: BoundAppCommandSender) -> Result<Self> {
        let descriptor = new_instance_descriptor(&window_state_key)?;
        prepare_endpoint(&LocalEndpoint::from_path(descriptor.endpoint.clone()))?;
        let endpoint_path = descriptor.endpoint.clone();
        let endpoint = LocalEndpoint::from_path(endpoint_path.clone());
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let state = Arc::new(Mutex::new(ControlState::default()));
        let server_state = Arc::clone(&state);
        let server_descriptor = descriptor.clone();
        let server_thread = match thread::Builder::new()
            .name("bootty-control".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("control runtime");
                runtime.block_on(async move {
                    let listener = match LocalListener::bind(&endpoint).and_then(|listener| {
                        set_owner_only_file(endpoint.as_path())?;
                        Ok(listener)
                    }) {
                        Ok(listener) => {
                            let _ = ready_tx.send(Ok::<(), String>(()));
                            listener
                        }
                        Err(error) => {
                            let _ = ready_tx.send(Err(error.to_string()));
                            return;
                        }
                    };
                    let connections = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
                    tokio::pin!(shutdown_rx);
                    loop {
                        tokio::select! {
                            _ = &mut shutdown_rx => break,
                            accepted = listener.accept() => {
                                let Ok((stream, peer)) = accepted else { continue };
                                if !same_user(&peer) { continue; }
                                let Ok(permit) = Arc::clone(&connections).try_acquire_owned() else {
                                    continue;
                                };
                                let commands = commands.clone();
                                let descriptor = server_descriptor.clone();
                                let state = Arc::clone(&server_state);
                                tokio::spawn(async move {
                                    let _permit = permit;
                                    let _ = serve_connection(
                                        stream,
                                        descriptor,
                                        commands,
                                        state,
                                        peer.pid,
                                    )
                                    .await;
                                });
                            }
                        }
                    }
                    lock_control_state(&server_state).cancel_all_tasks();
                });
            }) {
            Ok(thread) => thread,
            Err(error) => {
                let _ = fs::remove_file(&endpoint_path);
                return Err(error).context("spawn control server");
            }
        };
        match ready_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(())) => match write_instance_descriptor(&descriptor) {
                Ok(descriptor_path) => Ok(Self {
                    shutdown: Some(shutdown_tx),
                    thread: Some(server_thread),
                    descriptor_path,
                    endpoint_path,
                    state,
                }),
                Err(error) => {
                    let _ = shutdown_tx.send(());
                    let _ = server_thread.join();
                    let _ = fs::remove_file(&endpoint_path);
                    Err(error)
                }
            },
            Ok(Err(error)) => {
                let _ = shutdown_tx.send(());
                let _ = server_thread.join();
                let _ = fs::remove_file(&endpoint_path);
                anyhow::bail!(error)
            }
            Err(error) => {
                let _ = shutdown_tx.send(());
                let _ = server_thread.join();
                let _ = fs::remove_file(&endpoint_path);
                anyhow::bail!("control server did not start: {error}")
            }
        }
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        lock_control_state(&self.state).cancel_all_tasks();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.descriptor_path);
        let _ = fs::remove_file(&self.endpoint_path);
    }
}

async fn serve_connection(
    mut stream: rmux_ipc::LocalStream,
    descriptor: InstanceDescriptor,
    commands: BoundAppCommandSender,
    state: SharedControlState,
    owner_pid: u32,
) -> io::Result<()> {
    let mut line = String::new();
    let read = {
        let mut reader = tokio::io::BufReader::new(&mut stream).take(REQUEST_LIMIT + 1);
        tokio::time::timeout(IO_TIMEOUT, reader.read_line(&mut line)).await
    };
    let response = match read {
        Err(_) => RpcResponse::error(Value::Null, -32003, "request read timed out", None),
        Ok(Err(error)) => return Err(error),
        Ok(Ok(_)) if line.len() as u64 > REQUEST_LIMIT => {
            RpcResponse::error(Value::Null, -32600, "request exceeds payload limit", None)
        }
        Ok(Ok(_)) => match serde_json::from_str::<RpcRequest>(line.trim_end()) {
            Ok(request)
                if serde_json::to_vec(&request.id)
                    .is_ok_and(|encoded| encoded.len() > RPC_ID_LIMIT) =>
            {
                RpcResponse::error(
                    Value::Null,
                    -32600,
                    "request ID exceeds payload limit",
                    None,
                )
            }
            Ok(request) => handle_request(request, descriptor, commands, state, owner_pid).await,
            Err(error) => RpcResponse::error(Value::Null, -32700, error.to_string(), None),
        },
    };
    let mut encoded = serde_json::to_vec(&response).map_err(io::Error::other)?;
    if encoded.len() as u64 > REQUEST_LIMIT {
        encoded = serde_json::to_vec(&RpcResponse::error(
            Value::Null,
            -32603,
            "response exceeds payload limit",
            None,
        ))
        .map_err(io::Error::other)?;
    }
    encoded.push(b'\n');
    tokio::time::timeout(IO_TIMEOUT, stream.write_all(&encoded))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "response write timed out"))?
}

async fn handle_request(
    request: RpcRequest,
    descriptor: InstanceDescriptor,
    commands: BoundAppCommandSender,
    state: SharedControlState,
    owner_pid: u32,
) -> RpcResponse {
    if request.jsonrpc != "2.0" {
        return RpcResponse::error(request.id, -32600, "invalid JSON-RPC version", None);
    }
    let result = match request.method.as_str() {
        "system.ping" => negotiate_protocol(&request.params),
        "system.describe" => Ok(json!({
            "protocol": {
                "minimum": PROTOCOL_VERSION,
                "maximum": PROTOCOL_VERSION,
                "current": PROTOCOL_VERSION
            },
            "framing": "newline-delimited-json",
            "limits": {
                "request_bytes": REQUEST_LIMIT,
                "response_bytes": REQUEST_LIMIT,
                "connections": MAX_CONNECTIONS,
                "tasks": MAX_TASKS,
                "subscriptions": MAX_SUBSCRIPTIONS,
                "topics_per_subscription": MAX_TOPICS_PER_SUBSCRIPTION,
                "events_per_subscription": EVENT_QUEUE_LIMIT,
                "command_timeout_ms": COMMAND_TIMEOUT.as_millis()
            },
            "methods": [
                "system.ping", "system.describe", "instance.describe", "command.list",
                "command.describe", "command.invoke", "event.subscribe", "event.unsubscribe",
                "task.status", "task.cancel"
            ],
            "event_topics": {
                "command.completed": {"snapshot": "instance.describe"}
            }
        })),
        "instance.describe" => serde_json::to_value(descriptor).map_err(internal_error),
        "command.list" => serde_json::to_value(CommandRegistry::core().list().collect::<Vec<_>>())
            .map_err(internal_error),
        "command.describe" => {
            let name = request.params.get("command").and_then(Value::as_str);
            match name.and_then(|name| CommandRegistry::core().describe(name)) {
                Some(command) => serde_json::to_value(command).map_err(internal_error),
                None => Err(RpcError::new(-32602, "unknown command")),
            }
        }
        "command.invoke" => {
            invoke_command(request.params, descriptor, commands, state, owner_pid).await
        }
        "event.subscribe" => subscribe_events(request.params, descriptor, state, owner_pid),
        "event.unsubscribe" => unsubscribe_events(request.params, state, owner_pid),
        "task.status" => task_status(request.params, state, owner_pid),
        "task.cancel" => task_cancel(request.params, state, owner_pid),
        _ => Err(RpcError::new(-32601, "method not found")),
    };
    match result {
        Ok(result) => RpcResponse::success(request.id, result),
        Err(error) => RpcResponse {
            jsonrpc: "2.0".to_owned(),
            id: request.id,
            result: None,
            error: Some(error),
        },
    }
}

fn negotiate_protocol(params: &Value) -> Result<Value, RpcError> {
    let minimum = params
        .get("minimum_protocol_version")
        .and_then(Value::as_u64)
        .unwrap_or(u64::from(PROTOCOL_VERSION));
    let maximum = params
        .get("maximum_protocol_version")
        .and_then(Value::as_u64)
        .unwrap_or(u64::from(PROTOCOL_VERSION));
    let version = u64::from(PROTOCOL_VERSION);
    if minimum > version || maximum < version || minimum > maximum {
        let mut error = RpcError::new(-32007, "no compatible protocol version");
        error.data = Some(json!({
            "server_minimum": PROTOCOL_VERSION,
            "server_maximum": PROTOCOL_VERSION,
            "client_minimum": minimum,
            "client_maximum": maximum
        }));
        return Err(error);
    }
    Ok(json!({
        "protocol_version": PROTOCOL_VERSION,
        "minimum_protocol_version": PROTOCOL_VERSION,
        "maximum_protocol_version": PROTOCOL_VERSION
    }))
}

async fn invoke_command(
    params: Value,
    descriptor: InstanceDescriptor,
    commands: BoundAppCommandSender,
    state: SharedControlState,
    owner_pid: u32,
) -> Result<Value, RpcError> {
    let mut invocation: CommandInvocation = serde_json::from_value(
        params
            .get("invocation")
            .cloned()
            .ok_or_else(|| RpcError::new(-32602, "missing invocation"))?,
    )
    .map_err(|error| RpcError::new(-32602, error.to_string()))?;
    invocation.caller = Caller::Socket;
    let scope = instance_scope(&descriptor);
    if params
        .get("detached")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return start_task(invocation, commands, state, owner_pid, scope);
    }

    let completion = invocation.clone();
    let outcome = await_command(invocation, commands, CommandCancellation::new()).await;
    publish_command_completion(&state, scope, owner_pid, &completion, &outcome);
    outcome
}

fn start_task(
    invocation: CommandInvocation,
    commands: BoundAppCommandSender,
    state: SharedControlState,
    owner_pid: u32,
    scope: String,
) -> Result<Value, RpcError> {
    let cancellation = CommandCancellation::new();
    let task_id = lock_control_state(&state).start_task(owner_pid, cancellation.clone())?;
    let response_rx = match enqueue_command(invocation.clone(), commands, cancellation.clone()) {
        Ok(response_rx) => response_rx,
        Err(error) => {
            lock_control_state(&state).remove_task(&task_id);
            return Err(error);
        }
    };
    let worker_state = Arc::clone(&state);
    let worker_task_id = task_id.clone();
    let worker_cancellation = cancellation.clone();
    let worker_scope = scope.clone();
    let worker_invocation = invocation.clone();
    let worker = thread::Builder::new()
        .name(format!("bootty-control-{task_id}"))
        .spawn(move || {
            let outcome = wait_for_task(response_rx, &worker_cancellation);
            let mut state = lock_control_state(&worker_state);
            state.finish_task(&worker_task_id, outcome.clone());
            state.publish_command_completion(worker_scope, owner_pid, &worker_invocation, outcome);
        });
    if let Err(error) = worker {
        cancellation.cancel();
        let outcome = failed_outcome(-32603, format!("start task worker: {error}"));
        let mut state = lock_control_state(&state);
        state.finish_task(&task_id, outcome.clone());
        state.publish_command_completion(scope, owner_pid, &invocation, outcome);
    }
    lock_control_state(&state).task_value(&task_id, owner_pid)
}

fn enqueue_command(
    invocation: CommandInvocation,
    commands: BoundAppCommandSender,
    cancellation: CommandCancellation,
) -> Result<mpsc::Receiver<crate::commands::CommandOutcome>, RpcError> {
    let (response_tx, response_rx) = mpsc::channel();
    commands
        .try_send(AppCommandRequest {
            invocation,
            deadline: Instant::now() + COMMAND_TIMEOUT,
            cancellation,
            response: response_tx,
        })
        .map_err(command_send_error)?;
    Ok(response_rx)
}

async fn await_command(
    invocation: CommandInvocation,
    commands: BoundAppCommandSender,
    cancellation: CommandCancellation,
) -> Result<Value, RpcError> {
    let response_rx = enqueue_command(invocation, commands, cancellation.clone())?;
    let outcome = tokio::task::spawn_blocking(move || response_rx.recv_timeout(COMMAND_TIMEOUT))
        .await
        .map_err(|error| RpcError::new(-32603, error.to_string()))?
        .map_err(|error| {
            cancellation.cancel();
            RpcError::new(-32003, error.to_string())
        })?;
    serde_json::to_value(outcome).map_err(internal_error)
}

fn wait_for_task(
    response_rx: mpsc::Receiver<crate::commands::CommandOutcome>,
    cancellation: &CommandCancellation,
) -> Value {
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    loop {
        match response_rx.try_recv() {
            Ok(outcome) => return serde_json::to_value(outcome).unwrap_or_else(internal_outcome),
            Err(mpsc::TryRecvError::Disconnected) => {
                return failed_outcome("-32003", "command response channel closed");
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if cancellation.is_cancelled() {
            return match response_rx.try_recv() {
                Ok(outcome) => serde_json::to_value(outcome).unwrap_or_else(internal_outcome),
                Err(_) => failed_outcome("-32003", "command was cancelled"),
            };
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            cancellation.cancel();
            return failed_outcome("-32003", "command deadline expired");
        }
        match response_rx.recv_timeout(remaining.min(TASK_WAIT_INTERVAL)) {
            Ok(outcome) => return serde_json::to_value(outcome).unwrap_or_else(internal_outcome),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return failed_outcome("-32003", "command response channel closed");
            }
        }
    }
}

fn command_send_error(error: AppCommandSendError) -> RpcError {
    match error {
        AppCommandSendError::Overloaded => RpcError::new(-32001, "command queue overloaded"),
        AppCommandSendError::Shutdown => RpcError::new(-32002, "instance is shutting down"),
    }
}

fn internal_outcome(error: serde_json::Error) -> Value {
    failed_outcome("-32603", error.to_string())
}

fn failed_outcome(code: impl ToString, message: impl Into<String>) -> Value {
    json!({"status": "failed", "code": code.to_string(), "message": message.into()})
}

fn publish_command_completion(
    state: &SharedControlState,
    scope: String,
    owner_pid: u32,
    invocation: &CommandInvocation,
    outcome: &Result<Value, RpcError>,
) {
    let outcome = outcome
        .as_ref()
        .cloned()
        .unwrap_or_else(|error| failed_outcome(error.code.to_string(), error.message.clone()));
    lock_control_state(state).publish_command_completion(scope, owner_pid, invocation, outcome);
}

fn subscribe_events(
    params: Value,
    descriptor: InstanceDescriptor,
    state: SharedControlState,
    owner_pid: u32,
) -> Result<Value, RpcError> {
    let mut state = lock_control_state(&state);
    if let Some(subscription) = params.get("subscription").and_then(Value::as_str) {
        let cursor = params
            .get("cursor")
            .and_then(Value::as_u64)
            .ok_or_else(|| RpcError::new(-32602, "missing subscription cursor"))?;
        return state.poll_subscription(subscription, owner_pid, cursor);
    }
    let topics = event_topics(&params)?;
    let scope = event_scope(&params, &descriptor)?;
    state.create_subscription(owner_pid, topics, scope)
}

fn unsubscribe_events(
    params: Value,
    state: SharedControlState,
    owner_pid: u32,
) -> Result<Value, RpcError> {
    let subscription = subscription_id(&params)?;
    lock_control_state(&state).unsubscribe(subscription, owner_pid)
}

fn task_status(
    params: Value,
    state: SharedControlState,
    owner_pid: u32,
) -> Result<Value, RpcError> {
    let task = task_id(&params)?;
    lock_control_state(&state).task_value(task, owner_pid)
}

fn task_cancel(
    params: Value,
    state: SharedControlState,
    owner_pid: u32,
) -> Result<Value, RpcError> {
    let task = task_id(&params)?;
    lock_control_state(&state).cancel_task(task, owner_pid)
}

fn task_id(params: &Value) -> Result<&str, RpcError> {
    params
        .get("task")
        .or_else(|| params.get("task_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(-32602, "missing task"))
}

fn subscription_id(params: &Value) -> Result<&str, RpcError> {
    params
        .get("subscription")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(-32602, "missing subscription"))
}

fn event_topics(params: &Value) -> Result<BTreeSet<String>, RpcError> {
    let topics = params
        .get("topics")
        .and_then(Value::as_array)
        .ok_or_else(|| RpcError::new(-32602, "missing event topics"))?;
    if topics.is_empty() || topics.len() > MAX_TOPICS_PER_SUBSCRIPTION {
        return Err(RpcError::new(-32602, "invalid event topic count"));
    }
    topics
        .iter()
        .map(|topic| {
            let Some(topic) = topic.as_str() else {
                return Err(RpcError::new(-32602, "event topic must be a string"));
            };
            if topic.is_empty()
                || topic.len() > EVENT_TOPIC_LIMIT
                || topic != COMMAND_COMPLETED_TOPIC
            {
                return Err(RpcError::new(-32602, "unsupported event topic"));
            }
            Ok(topic.to_owned())
        })
        .collect()
}

fn event_scope(params: &Value, descriptor: &InstanceDescriptor) -> Result<String, RpcError> {
    let expected = instance_scope(descriptor);
    let scope = params
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or(&expected);
    if scope != expected {
        return Err(RpcError::new(-32602, "unsupported event scope"));
    }
    Ok(expected)
}

fn instance_scope(descriptor: &InstanceDescriptor) -> String {
    format!("instance:{}", descriptor.instance_id)
}

type SharedControlState = Arc<Mutex<ControlState>>;

struct ControlState {
    next_task: u64,
    tasks: BTreeMap<String, TaskRecord>,
    completed_tasks: VecDeque<String>,
    next_subscription: u64,
    subscriptions: BTreeMap<String, SubscriptionRecord>,
    revisions: BTreeMap<String, u64>,
}

impl Default for ControlState {
    fn default() -> Self {
        Self {
            next_task: 1,
            tasks: BTreeMap::new(),
            completed_tasks: VecDeque::new(),
            next_subscription: 1,
            subscriptions: BTreeMap::new(),
            revisions: BTreeMap::new(),
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
    owner_pid: u32,
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
    fn start_task(
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
        let id = format!("task-{}", self.next_task);
        self.next_task += 1;
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

    fn remove_task(&mut self, task: &str) {
        self.tasks.remove(task);
    }

    fn finish_task(&mut self, task: &str, outcome: Value) {
        let Some(record) = self.tasks.get_mut(task) else {
            return;
        };
        if !matches!(record.state, TaskState::Completed { .. }) {
            record.state = TaskState::Completed { outcome };
            self.completed_tasks.push_back(task.to_owned());
        }
    }

    fn task_value(&self, task: &str, owner_pid: u32) -> Result<Value, RpcError> {
        self.owned_task(task, owner_pid)?;
        let record = self.tasks.get(task).expect("owned task exists");
        Ok(json!({
            "task": {
                "id": task,
                "owner_pid": record.owner_pid,
                "state": &record.state,
            }
        }))
    }

    fn cancel_task(&mut self, task: &str, owner_pid: u32) -> Result<Value, RpcError> {
        self.owned_task(task, owner_pid)?;
        let record = self.tasks.get_mut(task).expect("owned task exists");
        if matches!(record.state, TaskState::Running) && record.cancellation.cancel() {
            record.state = TaskState::Cancelling;
        }
        self.task_value(task, owner_pid)
    }

    fn cancel_all_tasks(&mut self) {
        for task in self.tasks.values_mut() {
            if matches!(task.state, TaskState::Running) {
                task.cancellation.cancel();
                task.state = TaskState::Cancelling;
            }
        }
    }

    fn owned_task(&self, task: &str, owner_pid: u32) -> Result<(), RpcError> {
        let Some(record) = self.tasks.get(task) else {
            return Err(RpcError::new(-32602, "unknown task"));
        };
        if record.owner_pid != owner_pid {
            return Err(RpcError::new(-32006, "task is owned by another process"));
        }
        Ok(())
    }

    fn create_subscription(
        &mut self,
        owner_pid: u32,
        topics: BTreeSet<String>,
        scope: String,
    ) -> Result<Value, RpcError> {
        self.reap_dead_subscriptions();
        if self.subscriptions.len() >= MAX_SUBSCRIPTIONS {
            return Err(RpcError::new(-32001, "subscription limit reached"));
        }
        let id = format!("subscription-{}", self.next_subscription);
        self.next_subscription += 1;
        let revision = *self.revisions.get(&scope).unwrap_or(&0);
        self.subscriptions.insert(
            id.clone(),
            SubscriptionRecord {
                owner_pid,
                topics,
                scope: scope.clone(),
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

    fn poll_subscription(
        &mut self,
        subscription: &str,
        owner_pid: u32,
        cursor: u64,
    ) -> Result<Value, RpcError> {
        let (scope, cursor, events) = {
            let record = self.owned_subscription(subscription, owner_pid)?;
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
                let event_bytes = serde_json::to_vec(event).map_err(internal_error)?.len();
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

    fn reap_dead_subscriptions(&mut self) {
        let system = sysinfo::System::new_all();
        self.subscriptions.retain(|_, subscription| {
            system
                .process(sysinfo::Pid::from_u32(subscription.owner_pid))
                .is_some()
        });
    }

    fn unsubscribe(&mut self, subscription: &str, owner_pid: u32) -> Result<Value, RpcError> {
        self.owned_subscription(subscription, owner_pid)?;
        self.subscriptions.remove(subscription);
        Ok(json!({"unsubscribed": subscription}))
    }

    fn owned_subscription(
        &mut self,
        subscription: &str,
        owner_pid: u32,
    ) -> Result<&mut SubscriptionRecord, RpcError> {
        let Some(record) = self.subscriptions.get_mut(subscription) else {
            return Err(RpcError::new(-32602, "unknown subscription"));
        };
        if record.owner_pid != owner_pid {
            return Err(RpcError::new(
                -32006,
                "subscription is owned by another process",
            ));
        }
        Ok(record)
    }

    fn publish_command_completion(
        &mut self,
        scope: String,
        owner_pid: u32,
        invocation: &CommandInvocation,
        outcome: Value,
    ) {
        let provenance = json!({"caller": invocation.caller, "owner_pid": owner_pid});
        let target = serde_json::to_value(&invocation.target).unwrap_or(Value::Null);
        self.publish_event(
            scope,
            COMMAND_COMPLETED_TOPIC,
            provenance,
            target,
            json!({"command": invocation.command, "outcome": outcome}),
        );
    }

    fn publish_event(
        &mut self,
        scope: String,
        topic: &str,
        provenance: Value,
        target: Value,
        payload: Value,
    ) {
        let revision = self.revisions.entry(scope.clone()).or_default();
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
                scope: scope.clone(),
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

fn lock_control_state(state: &SharedControlState) -> std::sync::MutexGuard<'_, ControlState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl RpcResponse {
    fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Value, code: i32, message: impl Into<String>, data: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
                data,
            }),
        }
    }
}

impl RpcError {
    fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

fn internal_error(error: serde_json::Error) -> RpcError {
    RpcError::new(-32603, error.to_string())
}

pub fn invoke(instance: Option<&str>, method: &str, params: Value) -> Result<RpcResponse> {
    let descriptor = select_instance(instance)?;
    invoke_instance(&descriptor, method, params)
}
pub fn invoke_or_start(
    instance: Option<&str>,
    start: bool,
    method: &str,
    params: Value,
) -> Result<RpcResponse> {
    let descriptor = select_or_start(instance, start)?;
    invoke_instance(&descriptor, method, params)
}

pub fn select_or_start(instance: Option<&str>, start: bool) -> Result<InstanceDescriptor> {
    let selected = instance
        .map(str::to_owned)
        .or_else(|| std::env::var("BOOTTY_INSTANCE").ok());
    if let Some(selected) = selected {
        return select_instance(Some(&selected));
    }
    if !start {
        return select_instance(None);
    }
    let instances = discover_instances()?;
    match instances.as_slice() {
        [] => start_instance(),
        [instance] => Ok(instance.clone()),
        _ => select_instance(None),
    }
}

fn start_instance() -> Result<InstanceDescriptor> {
    let existing = discover_instances()?
        .into_iter()
        .map(|instance| instance.instance_id)
        .collect::<BTreeSet<_>>();
    let executable = std::env::current_exe().context("find Bootty executable")?;
    let mut child = ProcessCommand::new(executable)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("start Bootty instance")?;
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    loop {
        if let Some(status) = child
            .try_wait()
            .context("inspect started Bootty instance")?
        {
            anyhow::bail!("started Bootty instance exited with {status}");
        }
        let started = discover_instances()?
            .into_iter()
            .filter(|instance| !existing.contains(&instance.instance_id))
            .collect::<Vec<_>>();
        match started.as_slice() {
            [instance] => return Ok(instance.clone()),
            [] if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            [] => anyhow::bail!("started Bootty instance did not become ready"),
            _ => anyhow::bail!("multiple Bootty instances started; pass --instance"),
        }
    }
}

pub fn invoke_instance(
    descriptor: &InstanceDescriptor,
    method: &str,
    params: Value,
) -> Result<RpcResponse> {
    if descriptor.protocol_version != PROTOCOL_VERSION {
        anyhow::bail!(
            "unsupported Bootty protocol version {}; expected {}",
            descriptor.protocol_version,
            PROTOCOL_VERSION
        );
    }
    let endpoint = LocalEndpoint::from_path(descriptor.endpoint.clone());
    let mut stream = connect_blocking(&endpoint, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let request = RpcRequest {
        jsonrpc: "2.0".to_owned(),
        id: json!(1),
        method: method.to_owned(),
        params,
    };
    serde_json::to_writer(&mut stream, &request)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut response = String::new();
    BufReader::new(stream)
        .take(REQUEST_LIMIT + 1)
        .read_line(&mut response)?;
    if response.len() as u64 > REQUEST_LIMIT {
        anyhow::bail!("control response exceeds payload limit");
    }
    serde_json::from_str(&response).context("decode control response")
}

pub fn select_instance(explicit: Option<&str>) -> Result<InstanceDescriptor> {
    let selected = explicit
        .map(str::to_owned)
        .or_else(|| std::env::var("BOOTTY_INSTANCE").ok());
    let mut instances = discover_instances()?;
    if let Some(selected) = selected {
        return instances
            .into_iter()
            .find(|instance| instance.instance_id == selected)
            .with_context(|| format!("Bootty instance {selected} was not found"));
    }
    match instances.len() {
        0 => anyhow::bail!("no running Bootty instance was found"),
        1 => Ok(instances.remove(0)),
        _ => {
            let candidates = instances
                .iter()
                .map(|instance| instance.instance_id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!(
                "multiple Bootty instances are running ({candidates}); pass --instance or set BOOTTY_INSTANCE"
            )
        }
    }
}

pub fn discover_instances() -> Result<Vec<InstanceDescriptor>> {
    let directory = instance_directory()?;
    let mut instances = Vec::new();
    let Ok(entries) = fs::read_dir(&directory) else {
        return Ok(instances);
    };
    if set_owner_only_directory(&directory).is_err() {
        return Ok(instances);
    }
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(bytes) = fs::read(&path) else { continue };
        let Ok(instance) = serde_json::from_slice::<InstanceDescriptor>(&bytes) else {
            continue;
        };
        let Ok(expected_endpoint) =
            endpoint_for_label(format!("bootty-control-{}", instance.instance_id))
                .map(LocalEndpoint::into_path)
        else {
            let _ = fs::remove_file(path);
            continue;
        };
        if instance.endpoint != expected_endpoint
            || path.file_stem().and_then(|stem| stem.to_str()) != Some(&instance.instance_id)
        {
            let _ = fs::remove_file(path);
            continue;
        }
        let live = invoke_instance(&instance, "instance.describe", Value::Null)
            .ok()
            .and_then(|response| response.result)
            .and_then(|value| serde_json::from_value::<InstanceDescriptor>(value).ok())
            .is_some_and(|descriptor| descriptor == instance);
        if live {
            instances.push(instance);
        } else if instance_process_is_dead(&instance) {
            let _ = fs::remove_file(path);
            let _ = fs::remove_file(expected_endpoint);
        }
    }
    instances.sort_by_key(|instance| instance.started_at_ms);
    Ok(instances)
}

fn instance_process_is_dead(instance: &InstanceDescriptor) -> bool {
    let system = sysinfo::System::new_all();
    system
        .process(sysinfo::Pid::from_u32(instance.pid))
        .is_none_or(|process| u128::from(process.start_time()) * 1000 > instance.started_at_ms)
}

fn new_instance_descriptor(window_state_key: &str) -> Result<InstanceDescriptor> {
    let started_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before epoch")?
        .as_millis();
    let pid = std::process::id();
    let instance_id = format!("{pid}-{started_at_ms}");
    let label = format!("bootty-control-{instance_id}");
    let endpoint = endpoint_for_label(label)?.into_path();
    Ok(InstanceDescriptor {
        instance_id,
        generation: 1,
        pid,
        window_state_key: window_state_key.to_owned(),
        endpoint,
        started_at_ms,
        protocol_version: PROTOCOL_VERSION,
    })
}

fn write_instance_descriptor(descriptor: &InstanceDescriptor) -> Result<PathBuf> {
    let directory = instance_directory()?;
    fs::create_dir_all(&directory)?;
    set_owner_only_directory(&directory)?;
    let path = directory.join(format!("{}.json", descriptor.instance_id));
    let temporary = directory.join(format!(".{}.json.tmp", descriptor.instance_id));
    fs::write(&temporary, serde_json::to_vec(descriptor)?)?;
    set_owner_only_file(&temporary)?;
    fs::rename(&temporary, &path)?;
    Ok(path)
}

fn instance_directory() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .or_else(|| std::env::var_os("LOCALAPPDATA"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .context("no user-private runtime directory is available")?;
    Ok(base.join("bootty"))
}

fn prepare_endpoint(endpoint: &LocalEndpoint) -> Result<()> {
    if let Some(parent) = endpoint.as_path().parent() {
        fs::create_dir_all(parent)?;
        set_owner_only_directory(parent)?;
    }
    if endpoint.as_path().exists() {
        fs::remove_file(endpoint.as_path())?;
    }
    Ok(())
}

#[cfg(unix)]
fn same_user(peer: &rmux_ipc::PeerIdentity) -> bool {
    peer.uid == rmux_os::identity::real_user_id()
}

#[cfg(windows)]
fn same_user(peer: &rmux_ipc::PeerIdentity) -> bool {
    rmux_os::identity::IdentityResolver::current().is_ok_and(|identity| identity == peer.user)
}

#[cfg(unix)]
fn set_owner_only_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(windows)]
fn set_owner_only_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_owner_only_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(windows)]
fn set_owner_only_file(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{CommandTarget, ResourceKind, app_command_channel};

    #[test]
    fn ping_request_uses_json_rpc_envelope() {
        let request = RpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: json!(7),
            method: "system.ping".to_owned(),
            params: Value::Null,
        };
        let encoded = serde_json::to_value(request).unwrap();
        assert_eq!(encoded["jsonrpc"], "2.0");
        assert_eq!(encoded["method"], "system.ping");
    }

    #[test]
    fn unknown_method_returns_stable_error_code() {
        let (commands, _receiver) = app_command_channel(1);
        let response = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(handle_request(
                RpcRequest {
                    jsonrpc: "2.0".to_owned(),
                    id: json!(1),
                    method: "missing".to_owned(),
                    params: Value::Null,
                },
                new_instance_descriptor("test").unwrap(),
                commands.for_caller(Caller::Socket),
                Arc::new(Mutex::new(ControlState::default())),
                7,
            ));
        assert_eq!(response.error.unwrap().code, -32601);
    }

    #[test]
    fn task_cancellation_is_owned_and_shutdown_cancels() {
        let mut state = ControlState::default();
        let cancellation = CommandCancellation::new();
        let task = state.start_task(7, cancellation.clone()).unwrap();

        assert_eq!(state.cancel_task(&task, 8).unwrap_err().code, -32006);
        let value = state.cancel_task(&task, 7).unwrap();
        assert_eq!(value["task"]["state"]["status"], "cancelling");
        assert!(cancellation.is_cancelled());

        let other_cancellation = CommandCancellation::new();
        state.start_task(7, other_cancellation.clone()).unwrap();
        state.cancel_all_tasks();
        assert!(other_cancellation.is_cancelled());
    }

    #[test]
    fn detached_task_reports_completion() {
        let state = Arc::new(Mutex::new(ControlState::default()));
        let (commands, receiver) = app_command_channel(1);
        let task = start_task(
            CommandInvocation::from_action("new_tab", Caller::Socket),
            commands.for_caller(Caller::Socket),
            Arc::clone(&state),
            7,
            "instance:test".to_owned(),
        )
        .unwrap()["task"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let request = receiver.try_recv().unwrap();
        request
            .response
            .send(crate::commands::CommandOutcome::success())
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let status = task_status(json!({"task": task}), Arc::clone(&state), 7).unwrap();
            if status["task"]["state"]["status"] == "completed" {
                assert_eq!(status["task"]["state"]["outcome"]["status"], "success");
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn completion_events_include_target_and_provenance() {
        let mut state = ControlState::default();
        let scope = "instance:test".to_owned();
        let subscription = state
            .create_subscription(
                7,
                [COMMAND_COMPLETED_TOPIC.to_owned()].into_iter().collect(),
                scope.clone(),
            )
            .unwrap()["subscription"]
            .as_str()
            .unwrap()
            .to_owned();
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

        state.publish_command_completion(
            scope,
            7,
            &invocation,
            json!({"status": "success", "value": null}),
        );
        let events = state.poll_subscription(&subscription, 7, 0).unwrap();

        assert_eq!(events["revision"], 1);
        assert_eq!(events["events"][0]["sequence"], 1);
        assert_eq!(events["events"][0]["provenance"]["caller"], "socket");
        assert_eq!(events["events"][0]["provenance"]["owner_pid"], 7);
        assert_eq!(events["events"][0]["target"]["handle"], "pane-4");
    }

    #[test]
    fn event_overflow_requires_snapshot_rebase() {
        let mut state = ControlState::default();
        let scope = "instance:test".to_owned();
        let subscription = state
            .create_subscription(
                7,
                [COMMAND_COMPLETED_TOPIC.to_owned()].into_iter().collect(),
                scope.clone(),
            )
            .unwrap()["subscription"]
            .as_str()
            .unwrap()
            .to_owned();

        for _ in 0..=EVENT_QUEUE_LIMIT {
            state.publish_event(
                scope.clone(),
                COMMAND_COMPLETED_TOPIC,
                Value::Null,
                Value::Null,
                Value::Null,
            );
        }

        let error = state.poll_subscription(&subscription, 7, 0).unwrap_err();
        assert_eq!(error.code, -32005);
        assert_eq!(error.data.unwrap()["rebase"], "snapshot");
    }

    #[test]
    fn protocol_negotiation_rejects_incompatible_clients() {
        let error = negotiate_protocol(&json!({
            "minimum_protocol_version": PROTOCOL_VERSION + 1,
            "maximum_protocol_version": PROTOCOL_VERSION + 1,
        }))
        .unwrap_err();

        assert_eq!(error.code, -32007);
        assert_eq!(
            error.data.unwrap()["server_maximum"],
            json!(PROTOCOL_VERSION)
        );
    }

    #[test]
    fn current_process_descriptor_is_not_stale() {
        let descriptor = new_instance_descriptor("test").unwrap();

        assert!(!instance_process_is_dead(&descriptor));
    }
}
