use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use bootty_command::{
    AppCommandSendError, BoundAppCommandSender, Caller, CommandCancellation, CommandInvocation,
};
use rmux_ipc::{LocalEndpoint, LocalListener};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

use crate::{
    ControlCatalog,
    lease::{ControlInstanceLease, InstanceDescriptor, same_user, set_owner_only_file},
    plane::ControlPlane,
    protocol::{
        COMMAND_COMPLETED_TOPIC, COMMAND_TIMEOUT, EVENT_QUEUE_LIMIT, EVENT_TOPIC_LIMIT, IO_TIMEOUT,
        MAX_CONNECTIONS, MAX_SUBSCRIPTIONS, MAX_TASKS, MAX_TOPICS_PER_SUBSCRIPTION,
        PROTOCOL_VERSION, REQUEST_LIMIT, RPC_ID_LIMIT, RpcError, RpcRequest, RpcResponse,
        TASK_WAIT_INTERVAL, internal_error, negotiate_protocol,
    },
    state::{ControlState, SharedControlState, lock_control_state},
};

pub struct ControlServer {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
    lease: Option<ControlInstanceLease>,
    state: SharedControlState,
}

impl ControlServer {
    pub fn spawn(
        window_state_key: &str,
        commands: BoundAppCommandSender,
        catalog: Arc<ControlCatalog>,
        plane: &ControlPlane,
    ) -> Result<Self> {
        let mut lease = ControlInstanceLease::claim(window_state_key)?;
        let descriptor = lease.descriptor().clone();
        let endpoint = LocalEndpoint::from_path(descriptor.endpoint.clone());
        let state = Arc::clone(&plane.state);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (published_tx, published_rx) = mpsc::sync_channel(1);
        let server_state = Arc::clone(&state);
        let server_descriptor = descriptor.clone();
        let server_plane = (*plane).clone();
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
                    if published_rx.recv().is_err() {
                        return;
                    }
                    let connections = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
                    tokio::pin!(shutdown_rx);
                    loop {
                        tokio::select! {
                            _ = &mut shutdown_rx => break,
                            () = tokio::time::sleep(Duration::from_millis(5)) => {
                                server_plane.process_extension_events(catalog.extensions());
                            },
                            accepted = listener.accept() => {
                                let Ok((stream, peer)) = accepted else { continue };
                                if !same_user(&peer) { continue; }
                                let Ok(permit) = Arc::clone(&connections).try_acquire_owned() else {
                                    continue;
                                };
                                let commands = commands.clone();
                                let descriptor = server_descriptor.clone();
                                let state = Arc::clone(&server_state);
                                let catalog = Arc::clone(&catalog);
                                tokio::spawn(async move {
                                    let _permit = permit;
                                    let _ = serve_connection(
                                        stream,
                                        descriptor,
                                        commands,
                                        catalog,
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
                lease.abort();
                return Err(error).context("spawn control server");
            }
        };
        match ready_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(())) => {
                if let Err(error) = plane
                    .instance_scope
                    .lock()
                    .map_err(|_| anyhow::anyhow!("control plane scope is unavailable"))
                    .map(|mut scope| *scope = Some(instance_scope(&descriptor)))
                {
                    drop(published_tx);
                    let _ = server_thread.join();
                    lease.release();
                    return Err(error);
                }
                if let Err(error) = lease.publish() {
                    drop(published_tx);
                    let _ = server_thread.join();
                    lease.abort();
                    return Err(error).context("publish control descriptor");
                }
                if published_tx.send(()).is_err() {
                    let _ = server_thread.join();
                    lease.release();
                    anyhow::bail!("control server stopped before descriptor publication");
                }
                Ok(Self {
                    shutdown: Some(shutdown_tx),
                    thread: Some(server_thread),
                    lease: Some(lease),
                    state,
                })
            }
            Ok(Err(error)) => {
                let _ = shutdown_tx.send(());
                let _ = server_thread.join();
                lease.abort();
                anyhow::bail!(error)
            }
            Err(error) => {
                drop(published_tx);
                let _ = shutdown_tx.send(());
                let _ = server_thread.join();
                lease.abort();
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
        if let Some(lease) = self.lease.take() {
            lease.release();
        }
    }
}

async fn serve_connection(
    mut stream: rmux_ipc::LocalStream,
    descriptor: InstanceDescriptor,
    commands: BoundAppCommandSender,
    catalog: Arc<ControlCatalog>,
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
            Ok(request) => {
                handle_request(request, descriptor, commands, catalog, state, owner_pid).await
            }
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
    catalog: Arc<ControlCatalog>,
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
            "event_topics": available_event_topics(&state, &catalog)
                .into_iter()
                .map(|topic| (topic.clone(), json!({
                    "snapshot": if topic == COMMAND_COMPLETED_TOPIC {
                        Some("instance.describe")
                    } else {
                        None
                    }
                })))
                .collect::<BTreeMap<_, _>>()
        })),
        "instance.describe" => {
            serde_json::to_value(descriptor).map_err(|error| internal_error(&error))
        }
        "command.list" => {
            serde_json::to_value(catalog.list()).map_err(|error| internal_error(&error))
        }
        "command.describe" => {
            let name = request.params.get("command").and_then(Value::as_str);
            match name.and_then(|name| catalog.describe(name)) {
                Some(command) => {
                    serde_json::to_value(command).map_err(|error| internal_error(&error))
                }
                None => Err(RpcError::new(-32602, "unknown command")),
            }
        }
        "command.invoke" => {
            invoke_command(request.params, descriptor, commands, state, owner_pid).await
        }
        "event.subscribe" => subscribe_events(&request.params, &descriptor, &state, &catalog),
        "event.unsubscribe" => unsubscribe_events(&request.params, &state),
        "task.status" => task_status(&request.params, &state),
        "task.cancel" => task_cancel(&request.params, &state),
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
        return start_task(&invocation, &commands, &state, owner_pid, &scope);
    }

    let completion = invocation.clone();
    let outcome = await_command(invocation, &commands, CommandCancellation::new()).await;
    publish_command_completion(&state, &scope, owner_pid, &completion, &outcome);
    outcome
}

fn start_task(
    invocation: &CommandInvocation,
    commands: &BoundAppCommandSender,
    state: &SharedControlState,
    owner_pid: u32,
    scope: &str,
) -> Result<Value, RpcError> {
    let cancellation = CommandCancellation::new();
    let task_id = lock_control_state(state).start_task(owner_pid, cancellation.clone())?;
    let response_rx = match enqueue_command(invocation.clone(), commands, cancellation.clone()) {
        Ok(response_rx) => response_rx,
        Err(error) => {
            lock_control_state(state).remove_task(&task_id);
            return Err(error);
        }
    };
    let worker_state = Arc::clone(state);
    let worker_task_id = task_id.clone();
    let worker_cancellation = cancellation.clone();
    let worker_scope = scope.to_owned();
    let worker_invocation = invocation.clone();
    let worker = thread::Builder::new()
        .name(format!("bootty-control-{task_id}"))
        .spawn(move || {
            let outcome = wait_for_task(&response_rx, &worker_cancellation);
            let mut state = lock_control_state(&worker_state);
            state.finish_task(&worker_task_id, outcome.clone());
            state.publish_command_completion(
                &worker_scope,
                owner_pid,
                &worker_invocation,
                &outcome,
            );
        });
    if let Err(error) = worker {
        cancellation.cancel();
        let outcome = failed_outcome("-32603", format!("start task worker: {error}"));
        let mut state = lock_control_state(state);
        state.finish_task(&task_id, outcome.clone());
        state.publish_command_completion(scope, owner_pid, invocation, &outcome);
    }
    lock_control_state(state).task_value(&task_id)
}

fn enqueue_command(
    invocation: CommandInvocation,
    commands: &BoundAppCommandSender,
    cancellation: CommandCancellation,
) -> Result<mpsc::Receiver<bootty_command::CommandOutcome>, RpcError> {
    commands
        .submit(invocation, Instant::now() + COMMAND_TIMEOUT, cancellation)
        .map_err(command_send_error)
}

async fn await_command(
    invocation: CommandInvocation,
    commands: &BoundAppCommandSender,
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
    serde_json::to_value(outcome).map_err(|error| internal_error(&error))
}

fn wait_for_task(
    response_rx: &mpsc::Receiver<bootty_command::CommandOutcome>,
    cancellation: &CommandCancellation,
) -> Value {
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    loop {
        match response_rx.try_recv() {
            Ok(outcome) => {
                return serde_json::to_value(outcome)
                    .unwrap_or_else(|error| internal_outcome(&error));
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                return failed_outcome("-32003", "command response channel closed");
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if cancellation.is_cancelled() {
            return match response_rx.try_recv() {
                Ok(outcome) => {
                    serde_json::to_value(outcome).unwrap_or_else(|error| internal_outcome(&error))
                }
                Err(_) => failed_outcome("-32003", "command was cancelled"),
            };
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            cancellation.cancel();
            return failed_outcome("-32003", "command deadline expired");
        }
        match response_rx.recv_timeout(remaining.min(TASK_WAIT_INTERVAL)) {
            Ok(outcome) => {
                return serde_json::to_value(outcome)
                    .unwrap_or_else(|error| internal_outcome(&error));
            }
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

fn internal_outcome(error: &serde_json::Error) -> Value {
    failed_outcome("-32603", error.to_string())
}

fn failed_outcome(code: &str, message: impl Into<String>) -> Value {
    json!({"status": "failed", "code": code, "message": message.into()})
}

fn publish_command_completion(
    state: &SharedControlState,
    scope: &str,
    owner_pid: u32,
    invocation: &CommandInvocation,
    outcome: &Result<Value, RpcError>,
) {
    let outcome = outcome
        .as_ref()
        .cloned()
        .unwrap_or_else(|error| failed_outcome(&error.code.to_string(), error.message.clone()));
    lock_control_state(state).publish_command_completion(scope, owner_pid, invocation, &outcome);
}

fn subscribe_events(
    params: &Value,
    descriptor: &InstanceDescriptor,
    state: &SharedControlState,
    catalog: &ControlCatalog,
) -> Result<Value, RpcError> {
    if let Some(subscription) = params.get("subscription").and_then(Value::as_str) {
        let cursor = params
            .get("cursor")
            .and_then(Value::as_u64)
            .ok_or_else(|| RpcError::new(-32602, "missing subscription cursor"))?;
        return lock_control_state(state).poll_subscription(subscription, cursor);
    }
    let extension_topics = catalog.extensions().topics();
    let mut state = lock_control_state(state);
    let topics = event_topics(params, &state, &extension_topics)?;
    let scope = event_scope(params, descriptor)?;
    state.create_subscription(topics, &scope)
}

fn unsubscribe_events(params: &Value, state: &SharedControlState) -> Result<Value, RpcError> {
    let subscription = subscription_id(params)?;
    lock_control_state(state).unsubscribe(subscription)
}

fn task_status(params: &Value, state: &SharedControlState) -> Result<Value, RpcError> {
    let task = task_id(params)?;
    lock_control_state(state).task_value(task)
}

fn task_cancel(params: &Value, state: &SharedControlState) -> Result<Value, RpcError> {
    let task = task_id(params)?;
    lock_control_state(state).cancel_task(task)
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

fn event_topics(
    params: &Value,
    state: &ControlState,
    extension_topics: &BTreeSet<String>,
) -> Result<BTreeSet<String>, RpcError> {
    let available = state
        .topics
        .iter()
        .cloned()
        .chain(extension_topics.iter().cloned())
        .collect::<BTreeSet<_>>();
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
            if topic.is_empty() || topic.len() > EVENT_TOPIC_LIMIT || !available.contains(topic) {
                return Err(RpcError::new(-32602, "unsupported event topic"));
            }
            Ok(topic.to_owned())
        })
        .collect()
}

fn available_event_topics(
    state: &SharedControlState,
    catalog: &ControlCatalog,
) -> BTreeSet<String> {
    let extension_topics = catalog.extensions().topics();
    lock_control_state(state)
        .topics
        .iter()
        .cloned()
        .chain(extension_topics)
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
