use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use rmux_ipc::{LocalEndpoint, LocalListener, connect_blocking, endpoint_for_label};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

use crate::{
    automation::{
        AutomationError, AutomationHub, InstanceRef, OwnerIdentity,
        hub::{
            COMMAND_COMPLETED_TOPIC, EVENT_QUEUE_LIMIT, EVENT_TOPIC_LIMIT, MAX_SUBSCRIPTIONS,
            MAX_TASKS, MAX_TOPICS_PER_SUBSCRIPTION,
        },
    },
    commands::{
        AppCommandRequest, AppCommandSendError, BoundAppCommandSender, Caller, CommandCancellation,
        CommandCompletionContext, CommandInvocation, CommandRegistry,
    },
};

pub const PROTOCOL_VERSION: u32 = 1;
pub const REQUEST_LIMIT: u64 = 1024 * 1024;
const RPC_ID_LIMIT: usize = 4096;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONNECTIONS: usize = 32;
const TASK_WAIT_INTERVAL: Duration = Duration::from_millis(50);

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

impl InstanceDescriptor {
    /// The identity external callers discover and directory claims must share.
    #[must_use]
    pub(crate) fn directory_instance(&self) -> InstanceRef {
        InstanceRef {
            instance_id: self.instance_id.clone(),
            generation: self.generation,
        }
    }
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

/// A catalog command whose dynamic CLI invocation must call a control-plane
/// method directly rather than enter the application command dispatcher.
#[derive(Debug)]
pub enum DirectControlRequest {
    Rpc { method: &'static str, params: Value },
    CommandInvocation(CommandInvocation),
}

impl DirectControlRequest {
    #[must_use]
    pub fn method(&self) -> &'static str {
        match self {
            Self::Rpc { method, .. } => method,
            Self::CommandInvocation(_) => "command.invoke",
        }
    }
}

/// Returns whether a catalog command is implemented by an owner-local
/// control-plane RPC method rather than by `AppState`.
#[must_use]
pub fn is_direct_control_command(command: &str) -> bool {
    crate::commands::is_direct_control_command(command)
}

/// Converts dynamic CLI argument slots for a catalog direct-control command
/// into the exact JSON-RPC request shape its owner-local handler accepts.
///
/// Slots preserve omitted optional fields, unlike a positional command
/// invocation. `None` means the command is dispatched through the normal
/// application command channel.
pub fn direct_control_request(
    command: &str,
    arguments: &[Vec<String>],
) -> Result<Option<DirectControlRequest>> {
    let request = match command {
        "system.ping" => DirectControlRequest::Rpc {
            method: "system.ping",
            params: direct_system_ping_params(arguments)?,
        },
        "system.describe" | "instance.describe" | "command.list" => {
            direct_control_slots(command, arguments, 0)?;
            DirectControlRequest::Rpc {
                method: match command {
                    "system.describe" => "system.describe",
                    "instance.describe" => "instance.describe",
                    "command.list" => "command.list",
                    _ => unreachable!("matched direct control command"),
                },
                params: Value::Null,
            }
        }
        "command.describe" => {
            let [command_slot] = direct_control_slots(command, arguments, 1)? else {
                unreachable!("checked direct-control slot count")
            };
            DirectControlRequest::Rpc {
                method: "command.describe",
                params: json!({
                    "command": direct_control_required_argument(
                        command,
                        "command",
                        command_slot,
                    )?
                }),
            }
        }
        "command.invoke" => {
            let [command_slot, argument_batches] = direct_control_slots(command, arguments, 2)?
            else {
                unreachable!("checked direct-control slot count")
            };
            let command = direct_control_required_argument(command, "command", command_slot)?;
            let mut invocation_arguments = Vec::new();
            for batch in argument_batches {
                invocation_arguments.extend(direct_control_string_array(
                    "command.invoke",
                    "arguments",
                    batch,
                )?);
            }
            DirectControlRequest::CommandInvocation(CommandInvocation {
                command: command.to_owned(),
                arguments: invocation_arguments,
                caller: Caller::Cli,
                target: None,
                confirmation: None,
            })
        }
        "event.subscribe" => DirectControlRequest::Rpc {
            method: "event.subscribe",
            params: direct_event_subscribe_params(arguments)?,
        },
        "event.snapshot" | "event.rebase" | "event.unsubscribe" => {
            let [subscription] = direct_control_slots(command, arguments, 1)? else {
                unreachable!("checked direct-control slot count")
            };
            DirectControlRequest::Rpc {
                method: match command {
                    "event.snapshot" => "event.snapshot",
                    "event.rebase" => "event.rebase",
                    "event.unsubscribe" => "event.unsubscribe",
                    _ => unreachable!("matched direct control command"),
                },
                params: json!({
                    "subscription": direct_control_required_argument(
                        command,
                        "subscription",
                        subscription,
                    )?
                }),
            }
        }
        "task.status" | "task.cancel" => {
            let [task] = direct_control_slots(command, arguments, 1)? else {
                unreachable!("checked direct-control slot count")
            };
            DirectControlRequest::Rpc {
                method: match command {
                    "task.status" => "task.status",
                    "task.cancel" => "task.cancel",
                    _ => unreachable!("matched direct control command"),
                },
                params: json!({
                    "task": direct_control_required_argument(command, "task", task)?
                }),
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(request))
}

fn direct_system_ping_params(arguments: &[Vec<String>]) -> Result<Value> {
    let [minimum, maximum] = direct_control_slots("system.ping", arguments, 2)? else {
        unreachable!("checked direct-control slot count")
    };
    let minimum =
        direct_control_optional_argument("system.ping", "minimum_protocol_version", minimum)?
            .map(|value| direct_control_u32("system.ping", "minimum_protocol_version", value))
            .transpose()?;
    let maximum =
        direct_control_optional_argument("system.ping", "maximum_protocol_version", maximum)?
            .map(|value| direct_control_u32("system.ping", "maximum_protocol_version", value))
            .transpose()?;
    Ok(match (minimum, maximum) {
        (None, None) => json!({}),
        (Some(minimum), None) => json!({"minimum_protocol_version": minimum}),
        (None, Some(maximum)) => json!({"maximum_protocol_version": maximum}),
        (Some(minimum), Some(maximum)) => {
            json!({
                "minimum_protocol_version": minimum,
                "maximum_protocol_version": maximum,
            })
        }
    })
}

fn direct_event_subscribe_params(arguments: &[Vec<String>]) -> Result<Value> {
    let [topics, scope, subscription, cursor] =
        direct_control_slots("event.subscribe", arguments, 4)?
    else {
        unreachable!("checked direct-control slot count")
    };
    let topics = direct_control_optional_argument("event.subscribe", "topics", topics)?;
    let scope = direct_control_optional_argument("event.subscribe", "scope", scope)?;
    let subscription =
        direct_control_optional_argument("event.subscribe", "subscription", subscription)?;
    let cursor = direct_control_optional_argument("event.subscribe", "cursor", cursor)?;

    if let Some(subscription) = subscription {
        if topics.is_some() || scope.is_some() {
            anyhow::bail!(
                "command event.subscribe accepts either --topics/--scope or --subscription/--cursor"
            );
        }
        let mut params = json!({"subscription": subscription});
        if let Some(cursor) = cursor {
            params["cursor"] = json!(direct_control_u64("event.subscribe", "cursor", cursor)?);
        }
        return Ok(params);
    }
    if cursor.is_some() {
        anyhow::bail!("command event.subscribe requires --subscription with --cursor");
    }
    let topics = direct_control_string_array(
        "event.subscribe",
        "topics",
        topics.ok_or_else(|| {
            anyhow::anyhow!("command event.subscribe requires --topics or --subscription")
        })?,
    )?;
    let mut params = json!({"topics": topics});
    if let Some(scope) = scope {
        params["scope"] = json!(scope);
    }
    Ok(params)
}

fn direct_control_slots<'a>(
    command: &str,
    arguments: &'a [Vec<String>],
    expected: usize,
) -> Result<&'a [Vec<String>]> {
    if arguments.len() == expected {
        Ok(arguments)
    } else {
        anyhow::bail!("command {command} has an unexpected argument schema")
    }
}

fn direct_control_required_argument<'a>(
    command: &str,
    argument: &str,
    values: &'a [String],
) -> Result<&'a str> {
    direct_control_optional_argument(command, argument, values)?
        .ok_or_else(|| anyhow::anyhow!("command {command} requires --{argument}"))
}

fn direct_control_optional_argument<'a>(
    command: &str,
    argument: &str,
    values: &'a [String],
) -> Result<Option<&'a str>> {
    match values {
        [] => Ok(None),
        [value] => Ok(Some(value)),
        _ => anyhow::bail!("command {command} accepts exactly one --{argument}"),
    }
}

fn direct_control_string_array(command: &str, argument: &str, value: &str) -> Result<Vec<String>> {
    serde_json::from_str(value).with_context(|| {
        format!("command {command} requires --{argument} as a JSON array of strings")
    })
}

fn direct_control_u32(command: &str, argument: &str, value: &str) -> Result<u32> {
    value.parse::<u32>().with_context(|| {
        format!("command {command} requires --{argument} as an unsigned 32-bit integer")
    })
}

fn direct_control_u64(command: &str, argument: &str, value: &str) -> Result<u64> {
    value.parse::<u64>().with_context(|| {
        format!("command {command} requires --{argument} as an unsigned 64-bit integer")
    })
}

#[derive(Clone, Default)]
struct ConnectionCancellationRegistry {
    next_id: Arc<AtomicU64>,
    tokens: Arc<Mutex<BTreeMap<u64, CommandCancellation>>>,
}

struct ConnectionCancellationRegistration {
    id: u64,
    token: CommandCancellation,
    registry: ConnectionCancellationRegistry,
}

impl ConnectionCancellationRegistry {
    fn register(&self) -> ConnectionCancellationRegistration {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let token = CommandCancellation::new();
        self.tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, token.clone());
        ConnectionCancellationRegistration {
            id,
            token,
            registry: self.clone(),
        }
    }

    fn cancel_all(&self) {
        let tokens = self
            .tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for token in tokens {
            let _ = token.cancel();
        }
    }

    fn unregister(&self, id: u64) {
        self.tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id);
    }
}

impl ConnectionCancellationRegistration {
    fn token(&self) -> CommandCancellation {
        self.token.clone()
    }
}

impl Drop for ConnectionCancellationRegistration {
    fn drop(&mut self) {
        self.registry.unregister(self.id);
    }
}

pub struct ControlServer {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
    descriptor_path: PathBuf,
    endpoint_path: PathBuf,
    automation: AutomationHub,
    event_owner: OwnerIdentity,
    instance_scope: String,
    connection_cancellations: ConnectionCancellationRegistry,
}

impl ControlServer {
    pub fn spawn(window_state_key: String, commands: BoundAppCommandSender) -> Result<Self> {
        Self::spawn_with_hub(window_state_key, commands, AutomationHub::new())
    }

    pub fn spawn_with_hub(
        window_state_key: String,
        commands: BoundAppCommandSender,
        automation: AutomationHub,
    ) -> Result<Self> {
        Self::spawn_with_descriptor(
            new_instance_descriptor(&window_state_key)?,
            commands,
            automation,
            CommandRegistry::core().clone(),
        )
    }

    pub(crate) fn spawn_with_descriptor(
        descriptor: InstanceDescriptor,
        commands: BoundAppCommandSender,
        automation: AutomationHub,
        registry: CommandRegistry,
    ) -> Result<Self> {
        let instance_scope = instance_scope(&descriptor);
        automation.bind_instance_scope(instance_scope.clone())?;
        let event_owner = instance_event_owner(&descriptor);
        prepare_endpoint(&LocalEndpoint::from_path(descriptor.endpoint.clone()))?;
        let endpoint_path = descriptor.endpoint.clone();
        let endpoint = LocalEndpoint::from_path(endpoint_path.clone());
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let server_automation = automation.clone();
        let connection_cancellations = ConnectionCancellationRegistry::default();
        let server_connection_cancellations = connection_cancellations.clone();
        let server_descriptor = descriptor.clone();
        let server_registry = registry.clone();
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
                    let mut owner_reap = tokio::time::interval(Duration::from_secs(1));
                    owner_reap.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    loop {
                        tokio::select! {
                            _ = &mut shutdown_rx => break,
                            _ = owner_reap.tick() => server_automation.reap_dead_owners(),
                            accepted = listener.accept() => {
                                let Ok((stream, peer)) = accepted else { continue };
                                if !same_user(&peer) { continue; }
                                let Some(owner) = OwnerIdentity::for_process(peer.pid) else {
                                    continue;
                                };
                                server_automation.reap_dead_owners();
                                let Ok(permit) = Arc::clone(&connections).try_acquire_owned() else {
                                    continue;
                                };
                                let commands = commands.clone();
                                let descriptor = server_descriptor.clone();
                                let automation = server_automation.clone();
                                let registry = server_registry.clone();
                                let registration = server_connection_cancellations.register();
                                let connection_cancellation = registration.token();
                                tokio::spawn(async move {
                                    let _permit = permit;
                                    let _registration = registration;
                                    let _ = serve_connection(
                                        stream,
                                        descriptor,
                                        commands,
                                        registry,
                                        automation,
                                        owner,
                                        connection_cancellation,
                                    )
                                    .await;
                                });
                            }
                        }
                    }
                    server_connection_cancellations.cancel_all();
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
                    automation,
                    event_owner,
                    instance_scope,
                    connection_cancellations,
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

    pub fn automation_hub(&self) -> AutomationHub {
        self.automation.clone()
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.connection_cancellations.cancel_all();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.automation.cancel_tasks_in_scope(&self.instance_scope);
        self.automation.disconnect_owner(&self.event_owner);
        let _ = fs::remove_file(&self.descriptor_path);
        let _ = fs::remove_file(&self.endpoint_path);
    }
}

async fn serve_connection(
    mut stream: rmux_ipc::LocalStream,
    descriptor: InstanceDescriptor,
    commands: BoundAppCommandSender,
    registry: CommandRegistry,
    automation: AutomationHub,
    owner: OwnerIdentity,
    connection_cancellation: CommandCancellation,
) -> io::Result<()> {
    let mut reader = tokio::io::BufReader::new(&mut stream);
    let mut line = String::new();
    let read = {
        let mut request = (&mut reader).take(REQUEST_LIMIT + 1);
        tokio::time::timeout(IO_TIMEOUT, request.read_line(&mut line)).await
    };
    drop(reader);
    let response = match read {
        Err(_) => RpcResponse::error(Value::Null, -32003, "request read timed out", None),
        Ok(Err(error)) => return Err(error),
        Ok(Ok(_)) if line.len() as u64 > REQUEST_LIMIT => {
            RpcResponse::error(Value::Null, -32600, "request exceeds payload limit", None)
        }
        Ok(Ok(_)) => match parse_rpc_request(line.trim_end()) {
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
            Ok(request) => {
                tokio::select! {
                    response = handle_request(
                        request,
                        descriptor,
                        commands,
                        registry,
                        automation,
                        owner,
                        connection_cancellation.clone(),
                    ) => response,
                    closed = rmux_ipc::wait_for_peer_close(&stream) => {
                        connection_cancellation.cancel();
                        closed?;
                        return Ok(());
                    }
                }
            }
            Err(response) => *response,
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

fn parse_rpc_request(line: &str) -> Result<RpcRequest, Box<RpcResponse>> {
    let value = serde_json::from_str::<Value>(line).map_err(|error| {
        Box::new(RpcResponse::error(
            Value::Null,
            -32700,
            error.to_string(),
            None,
        ))
    })?;
    let id = value
        .get("id")
        .filter(|id| matches!(id, Value::String(_) | Value::Number(_) | Value::Null))
        .cloned()
        .unwrap_or(Value::Null);
    let request = serde_json::from_value::<RpcRequest>(value).map_err(|error| {
        Box::new(RpcResponse::error(
            id.clone(),
            -32600,
            error.to_string(),
            None,
        ))
    })?;
    if request.method.is_empty()
        || !matches!(
            request.id,
            Value::String(_) | Value::Number(_) | Value::Null
        )
    {
        return Err(Box::new(RpcResponse::error(
            id,
            -32600,
            "invalid JSON-RPC request",
            None,
        )));
    }
    Ok(request)
}
async fn handle_request(
    request: RpcRequest,
    descriptor: InstanceDescriptor,
    commands: BoundAppCommandSender,
    registry: CommandRegistry,
    automation: AutomationHub,
    owner: OwnerIdentity,
    connection_cancellation: CommandCancellation,
) -> RpcResponse {
    if request.jsonrpc != "2.0" {
        return RpcResponse::error(request.id, -32600, "invalid JSON-RPC version", None);
    }
    // Event subscriptions belong to the control instance, not to a one-shot CLI process.
    // The process component keeps liveness OS-backed; the logical generation isolates multiple
    // owner-local control servers that share one process and AutomationHub.
    let event_owner = instance_event_owner(&descriptor);
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
                "command.describe", "command.invoke", "event.subscribe", "event.snapshot",
                "event.rebase", "event.unsubscribe", "task.status", "task.cancel"
            ],
            "event_topics": event_topic_descriptions(&automation)
        })),
        "instance.describe" => serde_json::to_value(descriptor).map_err(internal_error),
        "command.list" => {
            serde_json::to_value(registry.list().collect::<Vec<_>>()).map_err(internal_error)
        }
        "command.describe" => {
            let name = request.params.get("command").and_then(Value::as_str);
            match name.and_then(|name| registry.describe(name)) {
                Some(command) => serde_json::to_value(command).map_err(internal_error),
                None => Err(RpcError::new(-32602, "unknown command")),
            }
        }
        "command.invoke" => {
            invoke_command(
                request.params,
                descriptor,
                commands,
                automation,
                owner,
                event_owner.clone(),
                connection_cancellation,
            )
            .await
        }
        "event.subscribe" => {
            subscribe_events(request.params, descriptor, automation, event_owner.clone())
        }
        "event.snapshot" => event_snapshot(request.params, automation, event_owner.clone()),
        "event.rebase" => event_rebase(request.params, automation, event_owner.clone()),
        "event.unsubscribe" => unsubscribe_events(request.params, automation, event_owner),
        "task.status" => task_status(request.params, automation, owner),
        "task.cancel" => task_cancel(request.params, automation, owner),
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

fn event_topic_descriptions(automation: &AutomationHub) -> serde_json::Map<String, Value> {
    automation
        .events()
        .registered_topics()
        .into_iter()
        .map(|topic| {
            let snapshot = if topic == COMMAND_COMPLETED_TOPIC {
                "instance.describe"
            } else {
                "event.snapshot"
            };
            (topic, json!({"snapshot": snapshot}))
        })
        .collect()
}
async fn invoke_command(
    params: Value,
    descriptor: InstanceDescriptor,
    commands: BoundAppCommandSender,
    automation: AutomationHub,
    owner: OwnerIdentity,
    event_owner: OwnerIdentity,
    connection_cancellation: CommandCancellation,
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
    if let Some(outcome) = invoke_event_command(&invocation, &automation, &event_owner) {
        publish_command_completion(&automation, scope, &event_owner, &invocation, &outcome);
        return outcome;
    }
    if params
        .get("detached")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return start_task(invocation, commands, automation, owner, scope);
    }

    let completion_context = CommandCompletionContext {
        caller: Caller::Socket,
        owner_pid: owner.pid(),
        owner_generation: owner.generation(),
        target: invocation.target.clone(),
    };
    await_command(
        invocation,
        commands,
        connection_cancellation,
        completion_context,
    )
    .await
}

fn invoke_event_command(
    invocation: &CommandInvocation,
    automation: &AutomationHub,
    owner: &OwnerIdentity,
) -> Option<Result<Value, RpcError>> {
    let subscription = || event_command_subscription(invocation);
    match invocation.command.as_str() {
        "event.snapshot" => Some(subscription().and_then(|subscription| {
            event_snapshot_for_subscription(subscription, automation, owner)
        })),
        "event.rebase" => {
            Some(subscription().and_then(|subscription| {
                event_rebase_subscription(subscription, automation, owner)
            }))
        }
        _ => None,
    }
}

fn event_command_subscription(invocation: &CommandInvocation) -> Result<&str, RpcError> {
    match invocation.arguments.as_slice() {
        [subscription] if !subscription.is_empty() => Ok(subscription),
        _ => Err(RpcError::new(
            -32602,
            "event command requires exactly one subscription argument",
        )),
    }
}

fn start_task(
    invocation: CommandInvocation,
    commands: BoundAppCommandSender,
    automation: AutomationHub,
    owner: OwnerIdentity,
    scope: String,
) -> Result<Value, RpcError> {
    let cancellation = CommandCancellation::new();
    let task = automation
        .tasks()
        .start(owner.clone(), cancellation.clone(), scope.clone())
        .map_err(automation_error)?;
    let task_id = task.id.clone();
    let completion_context = CommandCompletionContext {
        caller: Caller::Socket,
        owner_pid: owner.pid(),
        owner_generation: owner.generation(),
        target: invocation.target.clone(),
    };
    let response_rx = match enqueue_command(
        invocation.clone(),
        commands,
        cancellation.clone(),
        completion_context,
    ) {
        Ok(response_rx) => response_rx,
        Err(error) => {
            automation.tasks().remove(&task_id);
            return Err(error);
        }
    };
    let worker_automation = automation.clone();
    let worker_task_id = task_id.clone();
    let worker_cancellation = cancellation.clone();
    let worker = thread::Builder::new()
        .name(format!("bootty-control-{task_id}"))
        .spawn(move || {
            let outcome = match wait_for_task(response_rx, &worker_cancellation) {
                CommandWaitResult::Completed(outcome) => {
                    serde_json::to_value(outcome).unwrap_or_else(internal_outcome)
                }
                CommandWaitResult::Cancelled => failed_outcome("-32003", "command was cancelled"),
                CommandWaitResult::DeadlineExceeded => {
                    failed_outcome("-32003", "command deadline expired")
                }
                CommandWaitResult::Indeterminate => completion_indeterminate_value(),
                CommandWaitResult::ChannelClosed => {
                    failed_outcome("-32003", "command response channel closed")
                }
            };
            if let Err(error) = worker_automation.tasks().finish(&worker_task_id, &outcome) {
                eprintln!("task {worker_task_id} completion publication failed: {error}");
            }
        });
    if let Err(error) = worker {
        cancellation.cancel();
        let outcome = failed_outcome(-32603, format!("start task worker: {error}"));
        if let Err(error) = automation.tasks().finish(&task_id, &outcome) {
            eprintln!("task {task_id} completion publication failed: {error}");
        }
    }
    task_value(
        automation
            .tasks()
            .status(&task_id, &owner)
            .map_err(automation_error)?,
    )
}

fn enqueue_command(
    invocation: CommandInvocation,
    commands: BoundAppCommandSender,
    cancellation: CommandCancellation,
    completion: CommandCompletionContext,
) -> Result<mpsc::Receiver<crate::commands::CommandOutcome>, RpcError> {
    let (response_tx, response_rx) = mpsc::channel();
    commands
        .try_send(AppCommandRequest {
            invocation,
            deadline: Instant::now() + COMMAND_TIMEOUT,
            cancellation,
            response: response_tx,
            completion: Some(completion),
        })
        .map_err(command_send_error)?;
    Ok(response_rx)
}

enum CommandWaitResult {
    Completed(crate::commands::CommandOutcome),
    Cancelled,
    DeadlineExceeded,
    Indeterminate,
    ChannelClosed,
}

fn completion_indeterminate_outcome() -> crate::commands::CommandOutcome {
    crate::commands::CommandOutcome::completion_indeterminate()
}

fn completion_indeterminate_value() -> Value {
    serde_json::to_value(completion_indeterminate_outcome()).unwrap_or_else(internal_outcome)
}

async fn await_command(
    invocation: CommandInvocation,
    commands: BoundAppCommandSender,
    cancellation: CommandCancellation,
    completion: CommandCompletionContext,
) -> Result<Value, RpcError> {
    let response_rx = enqueue_command(invocation, commands, cancellation.clone(), completion)?;
    let outcome = wait_for_attached_command_async(
        response_rx,
        &cancellation,
        Instant::now() + COMMAND_TIMEOUT,
    )
    .await;
    match outcome {
        CommandWaitResult::Completed(outcome) => {
            serde_json::to_value(outcome).map_err(internal_error)
        }
        CommandWaitResult::Cancelled => {
            cancellation.cancel();
            Err(RpcError::new(-32003, "command was cancelled"))
        }
        CommandWaitResult::DeadlineExceeded => {
            cancellation.cancel();
            Err(RpcError::new(-32003, "command deadline expired"))
        }
        CommandWaitResult::Indeterminate => Ok(completion_indeterminate_value()),
        CommandWaitResult::ChannelClosed => {
            cancellation.cancel();
            Err(RpcError::new(-32003, "command response channel closed"))
        }
    }
}

async fn wait_for_attached_command_async(
    response_rx: mpsc::Receiver<crate::commands::CommandOutcome>,
    cancellation: &CommandCancellation,
    deadline: Instant,
) -> CommandWaitResult {
    loop {
        match response_rx.try_recv() {
            Ok(outcome) => return CommandWaitResult::Completed(outcome),
            Err(mpsc::TryRecvError::Disconnected) => return CommandWaitResult::ChannelClosed,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if cancellation.is_cancelled() {
            return match response_rx.try_recv() {
                Ok(outcome) => CommandWaitResult::Completed(outcome),
                Err(mpsc::TryRecvError::Disconnected) => CommandWaitResult::ChannelClosed,
                Err(mpsc::TryRecvError::Empty) => CommandWaitResult::Cancelled,
            };
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return if cancellation.cancel() {
                CommandWaitResult::DeadlineExceeded
            } else {
                CommandWaitResult::Indeterminate
            };
        }
        tokio::time::sleep(remaining.min(TASK_WAIT_INTERVAL)).await;
    }
}

fn wait_for_task(
    response_rx: mpsc::Receiver<crate::commands::CommandOutcome>,
    cancellation: &CommandCancellation,
) -> CommandWaitResult {
    wait_for_task_until(response_rx, cancellation, Instant::now() + COMMAND_TIMEOUT)
}

fn wait_for_task_until(
    response_rx: mpsc::Receiver<crate::commands::CommandOutcome>,
    cancellation: &CommandCancellation,
    deadline: Instant,
) -> CommandWaitResult {
    loop {
        match response_rx.try_recv() {
            Ok(outcome) => return CommandWaitResult::Completed(outcome),
            Err(mpsc::TryRecvError::Disconnected) => return CommandWaitResult::ChannelClosed,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if cancellation.is_cancelled() {
            return match response_rx.try_recv() {
                Ok(outcome) => CommandWaitResult::Completed(outcome),
                Err(mpsc::TryRecvError::Disconnected) => CommandWaitResult::ChannelClosed,
                Err(mpsc::TryRecvError::Empty) => CommandWaitResult::Cancelled,
            };
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return if cancellation.cancel() {
                CommandWaitResult::DeadlineExceeded
            } else {
                CommandWaitResult::Indeterminate
            };
        }
        match response_rx.recv_timeout(remaining.min(TASK_WAIT_INTERVAL)) {
            Ok(outcome) => return CommandWaitResult::Completed(outcome),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return CommandWaitResult::ChannelClosed;
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
    automation: &AutomationHub,
    scope: String,
    owner: &OwnerIdentity,
    invocation: &CommandInvocation,
    outcome: &Result<Value, RpcError>,
) {
    let outcome = outcome
        .as_ref()
        .cloned()
        .unwrap_or_else(|error| failed_outcome(error.code.to_string(), error.message.clone()));
    let _ = automation.publish_command_completion(scope, owner, invocation, outcome);
}

fn subscribe_events(
    params: Value,
    descriptor: InstanceDescriptor,
    automation: AutomationHub,
    owner: OwnerIdentity,
) -> Result<Value, RpcError> {
    let subscription = match params.get("subscription") {
        Some(subscription) => Some(
            subscription
                .as_str()
                .ok_or_else(|| RpcError::new(-32602, "invalid subscription"))?,
        ),
        None => None,
    };
    if let Some(subscription) = subscription {
        if params.get("topics").is_some() || params.get("scope").is_some() {
            return Err(RpcError::new(
                -32602,
                "event subscription request must use either topics/scope or subscription/cursor",
            ));
        }
        let cursor = match params.get("cursor") {
            Some(cursor) => cursor
                .as_u64()
                .ok_or_else(|| RpcError::new(-32602, "invalid subscription cursor"))?,
            // A fresh subscription begins at zero; later polls must echo the
            // cursor returned by the preceding delivery.
            None => 0,
        };
        return serde_json::to_value(
            automation
                .events()
                .poll(subscription, &owner, cursor)
                .map_err(automation_error)?,
        )
        .map_err(internal_error);
    }
    if params.get("cursor").is_some() {
        return Err(RpcError::new(
            -32602,
            "subscription cursor requires subscription",
        ));
    }
    let topics = event_topics(&params, &automation)?;
    let scope = event_scope(&params, &descriptor, &automation)?;
    serde_json::to_value(
        automation
            .events()
            .subscribe(owner, topics, scope)
            .map_err(automation_error)?,
    )
    .map_err(internal_error)
}

fn event_snapshot(
    params: Value,
    automation: AutomationHub,
    owner: OwnerIdentity,
) -> Result<Value, RpcError> {
    event_snapshot_for_subscription(subscription_id(&params)?, &automation, &owner)
}

fn event_snapshot_for_subscription(
    subscription: &str,
    automation: &AutomationHub,
    owner: &OwnerIdentity,
) -> Result<Value, RpcError> {
    serde_json::to_value(
        automation
            .events()
            .snapshot_for_subscription(subscription, owner)
            .map_err(automation_error)?,
    )
    .map_err(internal_error)
}

fn event_rebase(
    params: Value,
    automation: AutomationHub,
    owner: OwnerIdentity,
) -> Result<Value, RpcError> {
    event_rebase_subscription(subscription_id(&params)?, &automation, &owner)
}

fn event_rebase_subscription(
    subscription: &str,
    automation: &AutomationHub,
    owner: &OwnerIdentity,
) -> Result<Value, RpcError> {
    serde_json::to_value(
        automation
            .events()
            .rebase(subscription, owner)
            .map_err(automation_error)?,
    )
    .map_err(internal_error)
}

fn unsubscribe_events(
    params: Value,
    automation: AutomationHub,
    owner: OwnerIdentity,
) -> Result<Value, RpcError> {
    let subscription = subscription_id(&params)?;
    serde_json::to_value(
        automation
            .events()
            .unsubscribe(subscription, &owner)
            .map_err(automation_error)?,
    )
    .map_err(internal_error)
}

fn task_status(
    params: Value,
    automation: AutomationHub,
    owner: OwnerIdentity,
) -> Result<Value, RpcError> {
    let task = task_id(&params)?;
    task_value(
        automation
            .tasks()
            .status(task, &owner)
            .map_err(automation_error)?,
    )
}

fn task_cancel(
    params: Value,
    automation: AutomationHub,
    owner: OwnerIdentity,
) -> Result<Value, RpcError> {
    let task = task_id(&params)?;
    task_value(
        automation
            .tasks()
            .cancel(task, &owner)
            .map_err(automation_error)?,
    )
}

fn task_value(task: crate::automation::TaskStatus) -> Result<Value, RpcError> {
    Ok(json!({"task": task}))
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

fn event_topics(params: &Value, automation: &AutomationHub) -> Result<BTreeSet<String>, RpcError> {
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
                || !automation.events().topic_registered(topic)
            {
                return Err(RpcError::new(-32602, "unsupported event topic"));
            }
            Ok(topic.to_owned())
        })
        .collect()
}

fn event_scope(
    params: &Value,
    descriptor: &InstanceDescriptor,
    automation: &AutomationHub,
) -> Result<String, RpcError> {
    let instance = instance_scope(descriptor);
    let scope = params
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or(&instance);
    if scope == instance {
        return Ok(scope.to_owned());
    }
    if is_owner_local_binding_scope(scope) {
        return if automation.events().binding_scope_is_live(scope) {
            Ok(scope.to_owned())
        } else {
            Err(RpcError::new(-32006, "binding event scope is not live"))
        };
    }
    Err(RpcError::new(-32602, "unsupported event scope"))
}

/// Binding scopes are accepted only by the owner-local server that owns the
/// hub. A scope cannot cross into another Bootty process because hubs are not
/// shared between processes.
fn is_owner_local_binding_scope(scope: &str) -> bool {
    let mut parts = scope.split(':');
    matches!(parts.next(), Some("binding"))
        && parts
            .next()
            .is_some_and(|space| space.parse::<i64>().is_ok())
        && parts
            .next()
            .is_some_and(|binding| binding.parse::<i64>().is_ok())
        && parts.next().is_none()
}

pub(crate) fn instance_scope(descriptor: &InstanceDescriptor) -> String {
    format!("instance:{}", descriptor.instance_id)
}

fn instance_event_owner(descriptor: &InstanceDescriptor) -> OwnerIdentity {
    let logical_generation =
        u64::try_from(descriptor.started_at_ms).expect("instance start time fits in 64 bits");
    OwnerIdentity::for_process_logical_owner(descriptor.pid, logical_generation)
        .expect("the running control server has a process identity")
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

fn automation_error(error: AutomationError) -> RpcError {
    RpcError {
        code: error.code,
        message: error.message,
        data: error.data,
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

struct SpawnedChildGuard {
    child: Option<Child>,
}

impl SpawnedChildGuard {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn id(&self) -> u32 {
        self.child
            .as_ref()
            .expect("spawned child guard must be armed")
            .id()
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child
            .as_mut()
            .expect("spawned child guard must be armed")
            .try_wait()
    }

    fn disarm(&mut self) -> Option<Child> {
        self.child.take()
    }

    fn terminate(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
    }
}

impl Drop for SpawnedChildGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn wait_for_started_instance(
    mut child: SpawnedChildGuard,
    existing: &BTreeSet<String>,
    deadline: Instant,
    mut discover: impl FnMut() -> Result<Vec<InstanceDescriptor>>,
) -> Result<InstanceDescriptor> {
    let child_pid = child.id();
    loop {
        if let Some(status) = child
            .try_wait()
            .context("inspect started Bootty instance")?
        {
            anyhow::bail!("started Bootty instance exited with {status}");
        }
        let started = discover()?
            .into_iter()
            .filter(|instance| {
                !existing.contains(&instance.instance_id) && instance.pid == child_pid
            })
            .collect::<Vec<_>>();
        match started.as_slice() {
            [instance] => {
                let _ = child.disarm();
                return Ok(instance.clone());
            }
            [] if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            [] => anyhow::bail!("started Bootty instance did not become ready"),
            _ => anyhow::bail!("started Bootty child registered multiple instances"),
        }
    }
}

fn start_instance() -> Result<InstanceDescriptor> {
    let existing = discover_instances()?
        .into_iter()
        .map(|instance| instance.instance_id)
        .collect::<BTreeSet<_>>();
    let executable = std::env::current_exe().context("find Bootty executable")?;
    let child = ProcessCommand::new(executable)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("start Bootty instance")?;
    wait_for_started_instance(
        SpawnedChildGuard::new(child),
        &existing,
        Instant::now() + COMMAND_TIMEOUT,
        discover_instances,
    )
}

pub fn invoke_instance(
    descriptor: &InstanceDescriptor,
    method: &str,
    params: Value,
) -> Result<RpcResponse> {
    if descriptor.protocol_version != PROTOCOL_VERSION && method != "system.ping" {
        negotiate_instance(descriptor)?;
    }
    invoke_instance_raw(descriptor, method, params)
}

fn negotiate_instance(descriptor: &InstanceDescriptor) -> Result<()> {
    let response = invoke_instance_raw(
        descriptor,
        "system.ping",
        json!({
            "minimum_protocol_version": PROTOCOL_VERSION,
            "maximum_protocol_version": PROTOCOL_VERSION,
        }),
    )?;
    let result = response
        .result
        .context("protocol negotiation returned no result")?;
    let minimum = result
        .get("minimum_protocol_version")
        .and_then(Value::as_u64)
        .context("protocol negotiation omitted minimum version")?;
    let maximum = result
        .get("maximum_protocol_version")
        .and_then(Value::as_u64)
        .context("protocol negotiation omitted maximum version")?;
    let version = u64::from(PROTOCOL_VERSION);
    if minimum > version || maximum < version {
        anyhow::bail!(
            "unsupported Bootty protocol range {minimum}..={maximum}; expected {PROTOCOL_VERSION}"
        );
    }
    Ok(())
}

fn invoke_instance_raw(
    descriptor: &InstanceDescriptor,
    method: &str,
    params: Value,
) -> Result<RpcResponse> {
    let request = RpcRequest {
        jsonrpc: "2.0".to_owned(),
        id: json!(1),
        method: method.to_owned(),
        params,
    };
    let encoded_request = serde_json::to_vec(&request)?;
    validate_request_size(&encoded_request)?;

    let endpoint = LocalEndpoint::from_path(descriptor.endpoint.clone());
    let mut stream = connect_blocking(&endpoint, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    stream.write_all(&encoded_request)?;
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

fn validate_request_size(encoded_request: &[u8]) -> Result<()> {
    if encoded_request.len() as u64 + 1 > REQUEST_LIMIT {
        anyhow::bail!("control request exceeds the {REQUEST_LIMIT} byte payload limit");
    }
    Ok(())
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
        let expected_endpoint =
            endpoint_for_label(format!("bootty-control-{}", instance.instance_id))
                .map(LocalEndpoint::into_path)
                .ok();
        let descriptor_path_matches =
            path.file_stem().and_then(|stem| stem.to_str()) == Some(&instance.instance_id);
        let endpoint_matches = expected_endpoint
            .as_ref()
            .is_some_and(|expected| instance.endpoint.as_path() == expected.as_path());
        if !descriptor_path_matches || !endpoint_matches {
            if instance_process_is_dead(&instance) {
                let _ = fs::remove_file(path);
            }
            continue;
        }
        let expected_endpoint = expected_endpoint.expect("a matching endpoint was derived");
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

pub(crate) fn new_instance_descriptor(window_state_key: &str) -> Result<InstanceDescriptor> {
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
    use crate::{
        automation::{ClaimOwner, EventPublication},
        commands::{CommandTarget, ExtensionCommandRegistry, ResourceKind, app_command_channel},
    };

    fn owner() -> OwnerIdentity {
        OwnerIdentity::new(7, 11)
    }

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
    fn request_limit_includes_json_rpc_envelope() {
        let mut payload = "x".repeat(REQUEST_LIMIT as usize);
        let encoded = loop {
            let request = RpcRequest {
                jsonrpc: "2.0".to_owned(),
                id: json!(1),
                method: "command.invoke".to_owned(),
                params: json!({"payload": &payload}),
            };
            let encoded = serde_json::to_vec(&request).unwrap();
            if (encoded.len() as u64) < REQUEST_LIMIT {
                break encoded;
            }
            payload.pop();
        };

        assert_eq!(encoded.len() as u64 + 1, REQUEST_LIMIT);
        validate_request_size(&encoded).unwrap();

        payload.push('x');
        let request = RpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: json!(1),
            method: "command.invoke".to_owned(),
            params: json!({"payload": &payload}),
        };
        let encoded = serde_json::to_vec(&request).unwrap();
        let error = validate_request_size(&encoded).unwrap_err();
        assert!(error.to_string().contains("control request exceeds"));
    }

    #[test]
    fn control_server_holds_connection_permits_until_requests_finish() {
        let (commands, receiver) = app_command_channel(MAX_CONNECTIONS + 1);
        let descriptor = new_instance_descriptor("connection-limit").unwrap();
        let server = ControlServer::spawn_with_descriptor(
            descriptor.clone(),
            commands.for_caller(Caller::Socket),
            AutomationHub::new(),
            CommandRegistry::core().clone(),
        )
        .unwrap();
        let request = serde_json::to_vec(&RpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: json!(1),
            method: "command.invoke".to_owned(),
            params: json!({
                "invocation": {
                    "command": "pane.focus",
                    "arguments": [],
                    "caller": "socket"
                }
            }),
        })
        .unwrap();
        let endpoint = LocalEndpoint::from_path(descriptor.endpoint.clone());
        let mut streams = Vec::new();
        for _ in 0..=MAX_CONNECTIONS {
            let mut stream = connect_blocking(&endpoint, Duration::from_secs(2)).unwrap();
            stream.write_all(&request).unwrap();
            stream.write_all(b"\n").unwrap();
            stream.flush().unwrap();
            streams.push(stream);
        }

        let mut requests = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && requests.len() < MAX_CONNECTIONS {
            while let Ok(request) = receiver.try_recv() {
                requests.push(request);
            }
            if requests.len() < MAX_CONNECTIONS {
                thread::sleep(Duration::from_millis(5));
            }
        }
        assert_eq!(requests.len(), MAX_CONNECTIONS);
        thread::sleep(Duration::from_millis(50));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        drop(requests);
        drop(streams);
        drop(server);
    }

    #[test]
    fn direct_control_requests_cover_each_catalog_control_method() {
        let cases: &[(&str, &[&[&str]], &str)] = &[
            ("system.ping", &[&["1"], &["1"]], "system.ping"),
            ("system.describe", &[], "system.describe"),
            ("instance.describe", &[], "instance.describe"),
            ("command.list", &[], "command.list"),
            ("command.describe", &[&["pane.focus"]], "command.describe"),
            (
                "command.invoke",
                &[&["pane.focus"], &["[]"]],
                "command.invoke",
            ),
            (
                "event.subscribe",
                &[&[r#"["extension.reloaded"]"#], &["binding:1:2"], &[], &[]],
                "event.subscribe",
            ),
            ("event.snapshot", &[&["subscription-1"]], "event.snapshot"),
            ("event.rebase", &[&["subscription-1"]], "event.rebase"),
            (
                "event.unsubscribe",
                &[&["subscription-1"]],
                "event.unsubscribe",
            ),
            ("task.status", &[&["task-1"]], "task.status"),
            ("task.cancel", &[&["task-1"]], "task.cancel"),
        ];

        for (command, arguments, method) in cases {
            let arguments = arguments
                .iter()
                .map(|slot| {
                    slot.iter()
                        .map(|argument| (*argument).to_owned())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let request = direct_control_request(command, &arguments)
                .unwrap()
                .expect("catalog direct-control request");
            assert!(is_direct_control_command(command));
            assert_eq!(request.method(), *method);
        }
        assert!(!is_direct_control_command("pane.focus"));
        assert!(direct_control_request("pane.focus", &[]).unwrap().is_none());

        let subscription = direct_control_request(
            "event.subscribe",
            &[
                vec![r#"["terminal.output"]"#.to_owned()],
                vec!["binding:1:2".to_owned()],
                Vec::new(),
                Vec::new(),
            ],
        )
        .unwrap()
        .expect("event request");
        let DirectControlRequest::Rpc { params, .. } = subscription else {
            panic!("event.subscribe must be a direct RPC");
        };
        assert_eq!(
            params,
            json!({
                "topics": ["terminal.output"],
                "scope": "binding:1:2",
            })
        );

        let poll = direct_control_request(
            "event.subscribe",
            &[
                Vec::new(),
                Vec::new(),
                vec!["subscription-1".to_owned()],
                vec!["42".to_owned()],
            ],
        )
        .unwrap()
        .expect("event poll request");
        let DirectControlRequest::Rpc { params, .. } = poll else {
            panic!("event.subscribe polling must be a direct RPC");
        };
        assert_eq!(
            params,
            json!({"subscription": "subscription-1", "cursor": 42})
        );

        let task = direct_control_request("task.cancel", &[vec!["task-1".to_owned()]])
            .unwrap()
            .expect("task request");
        let DirectControlRequest::Rpc { params, .. } = task else {
            panic!("task.cancel must be a direct RPC");
        };
        assert_eq!(params, json!({"task": "task-1"}));

        let ping = direct_control_request("system.ping", &[Vec::new(), vec!["7".to_owned()]])
            .unwrap()
            .expect("maximum-only ping request");
        let DirectControlRequest::Rpc { params, .. } = ping else {
            panic!("system.ping must be a direct RPC");
        };
        assert_eq!(params, json!({"maximum_protocol_version": 7}));

        let invocation = direct_control_request(
            "command.invoke",
            &[
                vec!["pane.focus".to_owned()],
                vec![r#"["argument", "{\"nested\":true}"]"#.to_owned()],
            ],
        )
        .unwrap()
        .expect("command invocation");
        let DirectControlRequest::CommandInvocation(invocation) = invocation else {
            panic!("command.invoke must retain its nested invocation");
        };
        assert_eq!(invocation.command, "pane.focus");
        assert_eq!(
            invocation.arguments,
            vec!["argument".to_owned(), r#"{"nested":true}"#.to_owned()]
        );
        assert_eq!(invocation.caller, Caller::Cli);
    }

    #[test]
    fn valid_json_with_an_invalid_request_is_not_a_parse_error() {
        let invalid = parse_rpc_request(r#"{"jsonrpc":"2.0","id":7}"#).unwrap_err();
        assert_eq!(invalid.error.unwrap().code, -32600);

        let malformed = parse_rpc_request("{").unwrap_err();
        assert_eq!(malformed.error.unwrap().code, -32700);
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
                CommandRegistry::core().clone(),
                AutomationHub::new(),
                owner(),
                CommandCancellation::new(),
            ));
        assert_eq!(response.error.unwrap().code, -32601);
    }
    #[test]
    fn command_list_and_describe_use_the_instance_extension_registry() {
        let overlay = ExtensionCommandRegistry::new();
        let registry = CommandRegistry::core().with_extension_registry(overlay);
        let mut descriptor = registry.describe("pane.focus").expect("core descriptor");
        descriptor.id = "sample.extension.echo".to_owned();
        descriptor.aliases = vec!["sample.extension.e".to_owned()];
        descriptor.arguments = Default::default();
        registry
            .register_extension_command(descriptor.clone(), "sample.extension", 1)
            .unwrap();
        let resolved = registry
            .resolve(CommandInvocation {
                command: "sample.extension.e".to_owned(),
                arguments: Vec::new(),
                caller: Caller::Cli,
                target: None,
                confirmation: None,
            })
            .unwrap();
        assert_eq!(resolved.descriptor.id, "sample.extension.echo");
        let (commands, _receiver) = app_command_channel(1);
        let commands = commands.for_caller(Caller::Socket);
        let instance = new_instance_descriptor("extension-registry").unwrap();
        let automation = AutomationHub::new();
        let runtime = tokio::runtime::Runtime::new().unwrap();

        let listed = runtime.block_on(handle_request(
            RpcRequest {
                jsonrpc: "2.0".to_owned(),
                id: json!(1),
                method: "command.list".to_owned(),
                params: Value::Null,
            },
            instance.clone(),
            commands.clone(),
            registry.clone(),
            automation.clone(),
            owner(),
            CommandCancellation::new(),
        ));
        let listed = listed.result.expect("command list result");
        assert!(
            listed
                .as_array()
                .expect("command list array")
                .iter()
                .any(|command| command["id"] == "sample.extension.echo")
        );

        let described = runtime.block_on(handle_request(
            RpcRequest {
                jsonrpc: "2.0".to_owned(),
                id: json!(2),
                method: "command.describe".to_owned(),
                params: json!({"command": "sample.extension.e"}),
            },
            instance.clone(),
            commands.clone(),
            registry.clone(),
            automation.clone(),
            owner(),
            CommandCancellation::new(),
        ));
        let described = described.result.expect("command describe result");
        assert_eq!(described["id"], "sample.extension.echo");
        assert_eq!(described["origin"]["extension_id"], "sample.extension");
        assert_eq!(described["origin"]["generation"], 1);

        assert_eq!(
            registry.unregister_extension_commands("sample.extension", 1),
            1
        );
        registry
            .register_extension_command(descriptor, "sample.extension", 2)
            .unwrap();
        let replaced = runtime.block_on(handle_request(
            RpcRequest {
                jsonrpc: "2.0".to_owned(),
                id: json!(3),
                method: "command.describe".to_owned(),
                params: json!({"command": "sample.extension.echo"}),
            },
            instance,
            commands,
            registry,
            automation,
            owner(),
            CommandCancellation::new(),
        ));
        let replaced = replaced.result.expect("replacement command result");
        assert_eq!(replaced["origin"]["generation"], 2);
    }

    #[test]
    fn event_subscription_polling_survives_one_shot_cli_processes() {
        let descriptor = new_instance_descriptor("event-cli").unwrap();
        let scope = instance_scope(&descriptor);
        let automation = AutomationHub::new();
        automation.bind_instance_scope(scope.clone()).unwrap();
        let (commands, _receiver) = app_command_channel(1);
        let commands = commands.for_caller(Caller::Socket);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let registry = CommandRegistry::core().clone();

        let subscribed = runtime.block_on(handle_request(
            RpcRequest {
                jsonrpc: "2.0".to_owned(),
                id: json!(1),
                method: "event.subscribe".to_owned(),
                params: json!({"topics": [COMMAND_COMPLETED_TOPIC]}),
            },
            descriptor.clone(),
            commands.clone(),
            registry.clone(),
            automation.clone(),
            OwnerIdentity::new(41, 1),
            CommandCancellation::new(),
        ));
        assert!(subscribed.error.is_none(), "{:?}", subscribed.error);
        let subscription_response = subscribed.result.expect("event subscription response");
        assert_eq!(subscription_response["revision"], json!(0));
        assert_eq!(subscription_response["cursor"], json!(0));
        assert_eq!(subscription_response["events"], json!([]));
        let subscription = subscription_response["subscription"]
            .as_str()
            .expect("event subscription id")
            .to_owned();
        let cursor = subscription_response["cursor"]
            .as_u64()
            .expect("event subscription cursor");

        let initial_poll = runtime.block_on(handle_request(
            RpcRequest {
                jsonrpc: "2.0".to_owned(),
                id: json!(2),
                method: "event.subscribe".to_owned(),
                params: json!({"subscription": subscription.clone()}),
            },
            descriptor.clone(),
            commands.clone(),
            registry.clone(),
            automation.clone(),
            OwnerIdentity::new(42, 1),
            CommandCancellation::new(),
        ));
        assert!(initial_poll.error.is_none(), "{:?}", initial_poll.error);
        let initial_poll = initial_poll.result.expect("initial event poll response");
        assert_eq!(initial_poll["subscription"], json!(subscription));
        assert_eq!(initial_poll["revision"], json!(0));
        assert_eq!(initial_poll["cursor"], json!(0));
        assert_eq!(initial_poll["events"], json!([]));

        automation
            .publish_event(EventPublication::new(
                scope.clone(),
                COMMAND_COMPLETED_TOPIC,
                Value::Null,
                None,
                json!({"ready": true}),
            ))
            .unwrap();
        let polled = runtime.block_on(handle_request(
            RpcRequest {
                jsonrpc: "2.0".to_owned(),
                id: json!(3),
                method: "event.subscribe".to_owned(),
                params: json!({"subscription": subscription.clone(), "cursor": cursor}),
            },
            descriptor.clone(),
            commands.clone(),
            registry.clone(),
            automation.clone(),
            OwnerIdentity::new(43, 1),
            CommandCancellation::new(),
        ));
        assert!(polled.error.is_none(), "{:?}", polled.error);
        let polled = polled.result.expect("event poll response");
        assert_eq!(polled["subscription"], json!(subscription));
        assert_eq!(polled["revision"], json!(1));
        assert_eq!(polled["cursor"], json!(1));
        let events = polled["events"].as_array().expect("event delivery array");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["sequence"], json!(1));
        assert_eq!(events[0]["revision"], json!(1));
        assert_eq!(events[0]["topic"], json!(COMMAND_COMPLETED_TOPIC));
        let cursor = polled["cursor"].as_u64().expect("event poll cursor");

        let snapshot = runtime.block_on(handle_request(
            RpcRequest {
                jsonrpc: "2.0".to_owned(),
                id: json!(4),
                method: "event.snapshot".to_owned(),
                params: json!({"subscription": subscription.clone()}),
            },
            descriptor.clone(),
            commands.clone(),
            registry.clone(),
            automation.clone(),
            OwnerIdentity::new(44, 1),
            CommandCancellation::new(),
        ));
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        assert_eq!(
            snapshot.result.expect("event snapshot response")["scope"],
            json!(scope)
        );

        let invoked_snapshot = runtime.block_on(handle_request(
            RpcRequest {
                jsonrpc: "2.0".to_owned(),
                id: json!(5),
                method: "command.invoke".to_owned(),
                params: json!({
                    "invocation": CommandInvocation::from_action(
                        &format!("event.snapshot:{subscription}"),
                        Caller::Cli,
                    )
                }),
            },
            descriptor.clone(),
            commands.clone(),
            registry.clone(),
            automation.clone(),
            OwnerIdentity::new(45, 1),
            CommandCancellation::new(),
        ));
        assert!(
            invoked_snapshot.error.is_none(),
            "{:?}",
            invoked_snapshot.error
        );
        assert_eq!(
            invoked_snapshot
                .result
                .expect("invoked event snapshot response")["scope"],
            json!(scope)
        );

        let completion_poll = runtime.block_on(handle_request(
            RpcRequest {
                jsonrpc: "2.0".to_owned(),
                id: json!(6),
                method: "event.subscribe".to_owned(),
                params: json!({"subscription": subscription.clone(), "cursor": cursor}),
            },
            descriptor.clone(),
            commands.clone(),
            registry.clone(),
            automation.clone(),
            OwnerIdentity::new(46, 1),
            CommandCancellation::new(),
        ));
        assert!(
            completion_poll.error.is_none(),
            "{:?}",
            completion_poll.error
        );
        let completion_poll = completion_poll
            .result
            .expect("command completion poll response");
        assert_eq!(completion_poll["subscription"], json!(subscription));
        assert_eq!(completion_poll["revision"], json!(2));
        assert_eq!(completion_poll["cursor"], json!(2));
        let events = completion_poll["events"]
            .as_array()
            .expect("command completion delivery array");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["sequence"], json!(2));
        assert_eq!(events[0]["revision"], json!(2));
        assert_eq!(events[0]["topic"], json!(COMMAND_COMPLETED_TOPIC));

        let rebased = runtime.block_on(handle_request(
            RpcRequest {
                jsonrpc: "2.0".to_owned(),
                id: json!(7),
                method: "event.rebase".to_owned(),
                params: json!({"subscription": subscription.clone()}),
            },
            descriptor.clone(),
            commands.clone(),
            registry.clone(),
            automation.clone(),
            OwnerIdentity::new(47, 1),
            CommandCancellation::new(),
        ));
        assert!(rebased.error.is_none(), "{:?}", rebased.error);
        let rebased = rebased.result.expect("event rebase response");
        assert_eq!(rebased["subscription"], json!(subscription));
        assert_eq!(rebased["cursor"], json!(2));
        assert_eq!(rebased["revision"], json!(2));
        assert_eq!(rebased["snapshot"]["revision"], json!(2));

        let invalid_cursor = runtime.block_on(handle_request(
            RpcRequest {
                jsonrpc: "2.0".to_owned(),
                id: json!(8),
                method: "event.subscribe".to_owned(),
                params: json!({"subscription": subscription.clone(), "cursor": "not-a-cursor"}),
            },
            descriptor.clone(),
            commands.clone(),
            registry.clone(),
            automation.clone(),
            OwnerIdentity::new(48, 1),
            CommandCancellation::new(),
        ));
        let invalid_cursor = invalid_cursor.error.expect("invalid cursor error");
        assert_eq!(invalid_cursor.code, -32602);
        assert_eq!(invalid_cursor.message, "invalid subscription cursor");

        let unsubscribed = runtime.block_on(handle_request(
            RpcRequest {
                jsonrpc: "2.0".to_owned(),
                id: json!(9),
                method: "event.unsubscribe".to_owned(),
                params: json!({"subscription": subscription.clone()}),
            },
            descriptor,
            commands,
            registry,
            automation.clone(),
            OwnerIdentity::new(49, 1),
            CommandCancellation::new(),
        ));
        assert!(unsubscribed.error.is_none(), "{:?}", unsubscribed.error);
        assert_eq!(
            unsubscribed.result.expect("event unsubscribe response")["unsubscribed"],
            json!(subscription)
        );
    }

    #[test]
    fn task_cancellation_is_owned_and_shutdown_cancels() {
        let automation = AutomationHub::new();
        let cancellation = CommandCancellation::new();
        let task = automation
            .tasks()
            .start(owner(), cancellation.clone(), "instance:test".to_owned())
            .unwrap();

        assert_eq!(
            automation
                .tasks()
                .cancel(&task.id, &OwnerIdentity::new(8, 11))
                .unwrap_err()
                .code,
            -32006
        );
        let value = automation.tasks().cancel(&task.id, &owner()).unwrap();
        assert_eq!(
            serde_json::to_value(value).unwrap()["state"]["status"],
            "cancelling"
        );
        assert!(cancellation.is_cancelled());

        let other_cancellation = CommandCancellation::new();
        automation
            .tasks()
            .start(
                owner(),
                other_cancellation.clone(),
                "instance:test".to_owned(),
            )
            .unwrap();
        automation.cancel_all_tasks();
        assert!(other_cancellation.is_cancelled());
    }

    #[test]
    fn control_runtime_cancels_registered_connection_requests() {
        let registry = ConnectionCancellationRegistry::default();
        let first = registry.register();
        let second = registry.register();
        let first_token = first.token();
        let second_token = second.token();

        assert!(!first_token.is_cancelled());
        assert!(!second_token.is_cancelled());
        registry.cancel_all();
        assert!(first_token.is_cancelled());
        assert!(second_token.is_cancelled());
    }

    #[test]
    fn attached_command_drop_releases_response_after_started() {
        let (commands, receiver) = app_command_channel(1);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let cancellation = CommandCancellation::new();
            let waiter = tokio::spawn(await_command(
                CommandInvocation::from_action("new_tab", Caller::Socket),
                commands.for_caller(Caller::Socket),
                cancellation,
                CommandCompletionContext {
                    caller: Caller::Socket,
                    owner_pid: owner().pid(),
                    owner_generation: owner().generation(),
                    target: None,
                },
            ));
            let request = loop {
                match receiver.try_recv() {
                    Ok(request) => break request,
                    Err(mpsc::TryRecvError::Empty) => tokio::task::yield_now().await,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        panic!("command receiver disconnected")
                    }
                }
            };
            assert!(request.cancellation.try_start());
            tokio::time::sleep(Duration::from_millis(20)).await;
            waiter.abort();
            assert!(waiter.await.unwrap_err().is_cancelled());
            assert!(
                request
                    .response
                    .send(crate::commands::CommandOutcome::success())
                    .is_err()
            );
        });
    }

    #[test]
    fn command_waiters_return_indeterminate_after_started_deadline() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let cancellation = CommandCancellation::new();
            assert!(cancellation.try_start());
            let (response_tx, response_rx) = mpsc::channel();
            let outcome = wait_for_attached_command_async(
                response_rx,
                &cancellation,
                Instant::now() - Duration::from_millis(1),
            )
            .await;
            assert!(matches!(outcome, CommandWaitResult::Indeterminate));
            assert!(
                response_tx
                    .send(crate::commands::CommandOutcome::success())
                    .is_err()
            );

            let cancellation = CommandCancellation::new();
            assert!(cancellation.try_start());
            let (response_tx, response_rx) = mpsc::channel();
            let outcome = wait_for_task_until(
                response_rx,
                &cancellation,
                Instant::now() - Duration::from_millis(1),
            );
            assert!(matches!(outcome, CommandWaitResult::Indeterminate));
            assert!(
                response_tx
                    .send(crate::commands::CommandOutcome::success())
                    .is_err()
            );
        });
    }

    #[test]
    fn detached_task_reports_completion() {
        let automation = AutomationHub::new();
        let (commands, receiver) = app_command_channel(1);
        let task = start_task(
            CommandInvocation::from_action("new_tab", Caller::Socket),
            commands.for_caller(Caller::Socket),
            automation.clone(),
            owner(),
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
            let status = task_status(json!({"task": task}), automation.clone(), owner()).unwrap();
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
        let automation = AutomationHub::new();
        let scope = "instance:test".to_owned();
        let subscription = automation
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

        automation
            .publish_command_completion(
                scope,
                &owner(),
                &invocation,
                json!({"status": "success", "value": null}),
            )
            .unwrap();
        let events = automation
            .events()
            .poll(&subscription, &owner(), 0)
            .unwrap();

        assert_eq!(events.revision, 1);
        assert_eq!(events.events[0].sequence, 1);
        assert_eq!(events.events[0].provenance["caller"], "socket");
        assert_eq!(events.events[0].provenance["owner_pid"], 7);
        assert_eq!(events.events[0].target.as_ref().unwrap().handle, "pane-4");
    }

    #[test]
    fn event_overflow_requires_snapshot_rebase() {
        let automation = AutomationHub::new();
        let scope = "instance:test".to_owned();
        let subscription = automation
            .events()
            .subscribe(
                owner(),
                [COMMAND_COMPLETED_TOPIC.to_owned()].into_iter().collect(),
                scope.clone(),
            )
            .unwrap()
            .subscription;

        for _ in 0..=EVENT_QUEUE_LIMIT {
            automation
                .publish_event(EventPublication::new(
                    scope.clone(),
                    COMMAND_COMPLETED_TOPIC,
                    Value::Null,
                    None,
                    Value::Null,
                ))
                .unwrap();
        }

        let error = automation
            .events()
            .poll(&subscription, &owner(), 0)
            .unwrap_err();
        assert_eq!(error.code, -32005);
        assert_eq!(error.data.unwrap()["rebase"], "snapshot");
    }

    #[test]
    fn event_snapshot_and_rebase_are_owned_rpc_operations() {
        let automation = AutomationHub::new();
        let scope = "binding:1:2".to_owned();
        automation
            .events()
            .replace_live_binding_scopes([scope.clone()]);
        automation
            .events()
            .set_snapshot(
                scope.clone(),
                COMMAND_COMPLETED_TOPIC,
                json!({"authoritative": true}),
            )
            .unwrap();
        let subscription = automation
            .events()
            .subscribe(
                owner(),
                [COMMAND_COMPLETED_TOPIC.to_owned()].into_iter().collect(),
                scope.clone(),
            )
            .unwrap()
            .subscription;
        automation
            .publish_event(EventPublication::new(
                scope,
                COMMAND_COMPLETED_TOPIC,
                Value::Null,
                None,
                Value::Null,
            ))
            .unwrap();

        let snapshot = event_snapshot(
            json!({"subscription": subscription.clone()}),
            automation.clone(),
            owner(),
        )
        .unwrap();
        assert_eq!(
            snapshot["snapshots"][COMMAND_COMPLETED_TOPIC]["authoritative"],
            true
        );

        let rebase = event_rebase(
            json!({"subscription": subscription.clone()}),
            automation.clone(),
            owner(),
        )
        .unwrap();
        assert_eq!(rebase["cursor"], 1);
        assert_eq!(
            rebase["snapshot"]["snapshots"][COMMAND_COMPLETED_TOPIC]["authoritative"],
            true
        );
        assert_eq!(
            event_snapshot(
                json!({"subscription": subscription}),
                automation,
                OwnerIdentity::new(8, 11),
            )
            .unwrap_err()
            .code,
            -32006
        );
    }

    #[test]
    fn owner_local_binding_event_scope_requires_a_live_registry_entry() {
        let descriptor = new_instance_descriptor("test").unwrap();
        let automation = AutomationHub::new();
        let scope = "binding:1:-2".to_owned();
        automation
            .events()
            .replace_live_binding_scopes([scope.clone()]);
        automation
            .events()
            .set_snapshot(
                scope.clone(),
                "topology.changed",
                json!({"authoritative": true}),
            )
            .unwrap();
        let owner = owner();
        let subscription = automation
            .events()
            .subscribe(
                owner.clone(),
                ["topology.changed".to_owned()].into_iter().collect(),
                scope.clone(),
            )
            .unwrap()
            .subscription;

        assert_eq!(
            event_scope(&json!({"scope": scope}), &descriptor, &automation,).unwrap(),
            "binding:1:-2"
        );

        automation
            .events()
            .replace_live_binding_scopes(std::iter::empty());

        assert!(!automation.events().scope_has_snapshot("binding:1:-2"));
        assert_eq!(
            event_scope(&json!({"scope": "binding:1:-2"}), &descriptor, &automation,)
                .unwrap_err()
                .code,
            -32006
        );
        assert_eq!(
            automation
                .events()
                .poll(&subscription, &owner, 0)
                .unwrap_err()
                .code,
            -32006
        );
        assert_eq!(
            event_scope(&json!({"scope": "binding:1:-3"}), &descriptor, &automation,)
                .unwrap_err()
                .code,
            -32006
        );
        assert_eq!(
            event_scope(
                &json!({"scope": "binding:not-a-number:2"}),
                &descriptor,
                &automation,
            )
            .unwrap_err()
            .code,
            -32602
        );
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

    #[cfg(unix)]
    fn assert_startup_failure_reaps(
        deadline: Instant,
        expected: &str,
        mut discover: impl FnMut(u32) -> Result<Vec<InstanceDescriptor>>,
    ) {
        let child = ProcessCommand::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn hanging child");
        let pid = child.id();
        let error = wait_for_started_instance(
            SpawnedChildGuard::new(child),
            &BTreeSet::new(),
            deadline,
            || discover(pid),
        )
        .expect_err("startup should fail");

        assert_eq!(error.to_string(), expected);
        assert!(
            sysinfo::System::new_all()
                .process(sysinfo::Pid::from_u32(pid))
                .is_none(),
            "startup child {pid} was not reaped"
        );
    }

    #[cfg(unix)]
    fn descriptor_for_pid(pid: u32, instance_id: &str) -> InstanceDescriptor {
        InstanceDescriptor {
            instance_id: instance_id.to_owned(),
            generation: 1,
            pid,
            window_state_key: "test".to_owned(),
            endpoint: PathBuf::new(),
            started_at_ms: 1,
            protocol_version: PROTOCOL_VERSION,
        }
    }

    #[cfg(unix)]
    #[test]
    fn exited_startup_child_is_reaped() {
        let child = ProcessCommand::new("true")
            .spawn()
            .expect("spawn short-lived child");
        let pid = child.id();
        let error = wait_for_started_instance(
            SpawnedChildGuard::new(child),
            &BTreeSet::new(),
            Instant::now() + Duration::from_secs(1),
            || Ok(Vec::new()),
        )
        .expect_err("exited startup child should fail selection");

        assert!(
            error
                .to_string()
                .starts_with("started Bootty instance exited")
        );
        assert!(
            sysinfo::System::new_all()
                .process(sysinfo::Pid::from_u32(pid))
                .is_none(),
            "short-lived startup child {pid} was not reaped"
        );
    }

    #[cfg(unix)]
    #[test]
    fn startup_errors_reap_spawned_child_and_preserve_error() {
        assert_startup_failure_reaps(
            Instant::now() + Duration::from_secs(1),
            "malformed descriptor",
            |_| Err(anyhow::anyhow!("malformed descriptor")),
        );
        assert_startup_failure_reaps(
            Instant::now() + Duration::from_secs(1),
            "started Bootty child registered multiple instances",
            |pid| {
                Ok(vec![
                    descriptor_for_pid(pid, "first"),
                    descriptor_for_pid(pid, "second"),
                ])
            },
        );
        assert_startup_failure_reaps(
            Instant::now(),
            "started Bootty instance did not become ready",
            |_| Ok(Vec::new()),
        );
    }

    #[cfg(unix)]
    #[test]
    fn startup_success_disarms_child_guard() {
        let child = ProcessCommand::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn hanging child");
        let mut guard = SpawnedChildGuard::new(child);
        let mut child = guard.disarm().expect("armed child");

        assert!(
            child.try_wait().expect("inspect disarmed child").is_none(),
            "disarming must leave the child running"
        );
        child.kill().expect("stop test child");
        child.wait().expect("reap test child");
    }

    #[test]
    fn descriptor_identity_is_the_directory_claim_identity() {
        let descriptor = new_instance_descriptor("test").expect("descriptor");
        let instance = descriptor.directory_instance();
        let owner = ClaimOwner::current(instance.instance_id.clone()).expect("claim owner");

        assert_eq!(instance.instance_id, descriptor.instance_id);
        assert_eq!(instance.generation, descriptor.generation);
        assert_eq!(owner.instance_id, descriptor.instance_id);
    }

    #[test]
    fn current_process_descriptor_is_not_stale() {
        let descriptor = new_instance_descriptor("test").unwrap();

        assert!(!instance_process_is_dead(&descriptor));
    }
}
