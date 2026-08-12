//! Runs Bootty's embedded rmux backend through the small remote Bootty daemon.
//!
//! The remote host never resolves or executes an `rmux` binary. Bootty serializes backend requests,
//! sends them through SSH, and handles them with the same embedded rmux SDK path used locally.

#[cfg(feature = "app")]
use std::{
    io::BufReader,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use std::{
    io::{BufRead, BufWriter, Write},
    thread,
};

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rmux_sdk::{PaneOutputChunk, TerminalSizeSpec};
#[cfg(feature = "app")]
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
#[cfg(feature = "app")]
use tokio::sync::mpsc as tokio_mpsc;

use crate::{
    backend::MuxBackendOperationError,
    command::MuxCommand,
    operation::{MuxBackendCommandCompletion, MuxEventTarget},
    rmux_bridge::{
        RmuxPaneEvent, RmuxPaneTarget, open_rmux_pane_io, resize_rmux_pane,
        resolve_rmux_pane_target, rmux_execute, rmux_launch_session, rmux_snapshot,
        supports_rmux_session_launch_plan,
    },
    write_remote_operation_completion, write_remote_operation_error,
};
#[cfg(feature = "app")]
use crate::{
    backend::{
        MuxBackend, MuxEvent, MuxEventCapability, MuxEventDraft, MuxEventPayload,
        MuxEventProvenance, MuxEventQueue, MuxEventTopic, MuxRebaseReason,
        MuxScopedExecutionPrecondition,
    },
    capability::{
        BindingCapabilityDescriptor, BindingOperationAvailability, BindingOperationOutcome,
    },
    command::MuxSessionLaunchPlan,
    controller::{BindingId, MuxScope, SpaceId},
    process::{CommandOutput, CommandRunner, SystemCommandRunner},
    remote_operation_protocol::{decode_remote_operation_completion, remote_operation_failure},
    rmux::rmux_capabilities,
    rmux_bridge::RmuxPaneIo,
    snapshot::MuxSnapshot,
    ssh::SshRemote,
};

#[cfg(feature = "app")]
const REMOTE_RMUX_SUBCOMMAND: &str = "remote-rmux";
const MAX_REMOTE_RMUX_PAYLOAD: usize = 1024 * 1024;
const MAX_REMOTE_RMUX_ERROR_MESSAGE: usize = 1024;

#[cfg(feature = "app")]
const REMOTE_RMUX_EVENT_RETRY_DELAY: Duration = Duration::from_millis(250);
#[cfg(feature = "app")]
const REMOTE_RMUX_EVENT_POLL_INTERVAL: Duration = Duration::from_millis(20);
#[cfg(feature = "app")]
const REMOTE_RMUX_EVENT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
#[cfg(feature = "app")]
const REMOTE_RMUX_EVENT_BATCH: usize = 32;
const MAX_REMOTE_RMUX_EVENT_FRAME: usize = 8 * 1024 * 1024;
const MAX_REMOTE_RMUX_INPUT_FRAME: usize = 16 * 1024;

#[derive(Debug, Deserialize, Serialize)]
enum RemoteRmuxRequest {
    Snapshot,
    Execute {
        command: MuxCommand,
    },
    ExecuteChecked {
        command: MuxCommand,
        precondition: Box<MuxScopedExecutionPrecondition>,
    },
    ResolvePane {
        session: String,
        pane: Option<String>,
    },
    EventStream,
    PaneStream {
        session: String,
        pane: String,
        max_scrollback: usize,
    },
    PaneInput {
        session: String,
        pane: String,
    },
    Resize {
        session: String,
        pane: String,
        cols: u16,
        rows: u16,
    },
}

#[derive(Debug, Deserialize, Serialize)]
enum RemoteRmuxPaneResolution {
    Resolved { session: String, pane: String },
    Error(RemoteRmuxOperationError),
}

#[derive(Debug, Deserialize, Serialize)]
enum RemoteRmuxOperationError {
    Unsupported(String),
    Unavailable(String),
    Denied(String),
    Stale(String),
    Failed(String),
}
impl RemoteRmuxOperationError {
    fn from_error(error: &anyhow::Error) -> Self {
        let backend_error = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<MuxBackendOperationError>());
        match backend_error {
            Some(MuxBackendOperationError::Unsupported(message)) => {
                Self::Unsupported(bounded_remote_rmux_error_message(message))
            }
            Some(MuxBackendOperationError::Unavailable(message)) => {
                Self::Unavailable(bounded_remote_rmux_error_message(message))
            }
            Some(MuxBackendOperationError::Denied(message)) => {
                Self::Denied(bounded_remote_rmux_error_message(message))
            }
            Some(MuxBackendOperationError::Stale(message)) => {
                Self::Stale(bounded_remote_rmux_error_message(message))
            }
            Some(MuxBackendOperationError::Failed(message)) => {
                Self::Failed(bounded_remote_rmux_error_message(message))
            }
            None => Self::Failed(bounded_remote_rmux_error_message(&error.to_string())),
        }
    }

    #[cfg(feature = "app")]
    fn into_error(self) -> anyhow::Error {
        match self {
            Self::Unsupported(message) => MuxBackendOperationError::Unsupported(message).into(),
            Self::Unavailable(message) => MuxBackendOperationError::Unavailable(message).into(),
            Self::Denied(message) => MuxBackendOperationError::Denied(message).into(),
            Self::Stale(message) => MuxBackendOperationError::Stale(message).into(),
            Self::Failed(message) => MuxBackendOperationError::Failed(message).into(),
        }
    }
}

fn bounded_remote_rmux_error_message(message: &str) -> String {
    const SUFFIX: &str = "...";
    if message.len() <= MAX_REMOTE_RMUX_ERROR_MESSAGE {
        return message.to_owned();
    }
    let mut end = MAX_REMOTE_RMUX_ERROR_MESSAGE - SUFFIX.len();
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    let mut bounded = message[..end].to_owned();
    bounded.push_str(SUFFIX);
    bounded
}

fn resolve_remote_pane(
    session: String,
    pane: Option<String>,
    resolve: impl FnOnce(RmuxPaneTarget) -> Result<RmuxPaneTarget>,
) -> RemoteRmuxPaneResolution {
    match resolve(RmuxPaneTarget::new(session, pane)) {
        Ok(target) => match target.pane_selector() {
            Some(pane) => RemoteRmuxPaneResolution::Resolved {
                session: target.session_selector().to_owned(),
                pane: pane.to_owned(),
            },
            None => RemoteRmuxPaneResolution::Error(RemoteRmuxOperationError::Failed(
                "rmux pane resolver returned no pane".to_owned(),
            )),
        },
        Err(error) => RemoteRmuxPaneResolution::Error(RemoteRmuxOperationError::from_error(&error)),
    }
}

#[derive(Debug, Deserialize, Serialize)]
enum RemotePaneFrame {
    Restore {
        capture: String,
        buffered_chunks: Vec<String>,
    },
    Chunks(Vec<String>),
    KeyboardProtocol(String),
    Error(RemoteRmuxOperationError),
}

#[derive(Debug, Deserialize, Serialize)]
enum RemotePaneInputFrame {
    Ack,
    Error(RemoteRmuxOperationError),
}

#[cfg(feature = "app")]
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
enum RemoteRmuxEventFrame {
    Event { event: Box<MuxEventDraft> },
    Heartbeat,
}

#[cfg(feature = "app")]
pub struct RemoteRmuxBackend {
    remote: SshRemote,
    completion: Option<MuxBackendCommandCompletion>,
    events: MuxEventQueue,
    event_worker: Option<RemoteRmuxEventWorker>,
}

#[cfg(feature = "app")]
impl RemoteRmuxBackend {
    pub fn new(remote: SshRemote) -> Self {
        let events =
            MuxEventQueue::for_backend(format!("rmux:remote:{}", remote.transport_identity()));
        Self {
            remote,
            completion: None,
            events,
            event_worker: None,
        }
    }

    fn run(&self, request: &RemoteRmuxRequest) -> Result<CommandOutput> {
        run_remote_rmux_request(&self.remote, request)
    }

    fn start_remote_event_worker(&mut self) {
        if self.event_worker.is_some() {
            return;
        }
        match RemoteRmuxEventWorker::start(self.remote.clone(), self.events.clone()) {
            Ok(worker) => self.event_worker = Some(worker),
            Err(error) => self.events.publish(MuxEventDraft::new(
                MuxEventTopic::BackendDisconnected,
                MuxEventProvenance::RmuxSdk,
                None,
                None,
                MuxEventPayload::Disconnected {
                    reason: format!("start remote rmux event stream: {error}"),
                },
            )),
        }
    }
}

#[cfg(feature = "app")]
struct RemoteRmuxEventWorker {
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

#[cfg(feature = "app")]
impl RemoteRmuxEventWorker {
    fn start(remote: SshRemote, events: MuxEventQueue) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));
        let worker_stop = Arc::clone(&stop);
        let worker_child = Arc::clone(&child);
        thread::Builder::new()
            .name("bootty-remote-rmux-events".to_owned())
            .spawn(move || run_remote_rmux_event_worker(remote, events, worker_stop, worker_child))
            .context("start remote rmux event worker")?;
        Ok(Self { stop, child })
    }
}

#[cfg(feature = "app")]
impl Drop for RemoteRmuxEventWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        terminate_remote_rmux_event_child(&self.child);
    }
}

#[cfg(feature = "app")]
fn run_remote_rmux_event_worker(
    remote: SshRemote,
    events: MuxEventQueue,
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
) {
    let mut established = false;
    let mut disconnected = false;
    while !stop.load(Ordering::Acquire) {
        let result = remote
            .ensure_daemon()
            .and_then(|_| spawn_remote_rmux_event_process(&remote, &child))
            .and_then(|stdout| {
                consume_remote_rmux_event_stream(
                    stdout,
                    &events,
                    &stop,
                    if established {
                        MuxRebaseReason::Reconnect
                    } else {
                        MuxRebaseReason::Bootstrap
                    },
                    &mut established,
                    &mut disconnected,
                )
            });
        terminate_remote_rmux_event_child(&child);
        if stop.load(Ordering::Acquire) {
            break;
        }
        if let Err(error) = result {
            publish_remote_rmux_transport_disconnect(
                &events,
                &mut disconnected,
                format!("remote rmux event stream stopped: {error}"),
            );
        }
        thread::sleep(REMOTE_RMUX_EVENT_RETRY_DELAY);
    }
    terminate_remote_rmux_event_child(&child);
}

#[cfg(feature = "app")]
fn spawn_remote_rmux_event_process(
    remote: &SshRemote,
    child_slot: &Arc<Mutex<Option<Child>>>,
) -> Result<ChildStdout> {
    let (program, args) = remote_rmux_argv(remote, &RemoteRmuxRequest::EventStream)?;
    let mut child = Command::new(&program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("start remote rmux event stream")?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            bail!("remote rmux event stream has no stdout");
        }
    };
    let mut slot = child_slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(mut previous) = slot.replace(child) {
        let _ = previous.kill();
        let _ = previous.wait();
    }
    Ok(stdout)
}

#[cfg(feature = "app")]
fn terminate_remote_rmux_event_child(child_slot: &Arc<Mutex<Option<Child>>>) {
    let child = child_slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(mut child) = child {
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(feature = "app")]
fn consume_remote_rmux_event_stream(
    stdout: ChildStdout,
    events: &MuxEventQueue,
    stop: &AtomicBool,
    expected_rebase: MuxRebaseReason,
    established: &mut bool,
    disconnected: &mut bool,
) -> Result<()> {
    let mut reader = BufReader::new(stdout);
    let mut awaiting_rebase = true;
    while !stop.load(Ordering::Acquire) {
        let Some(frame) = read_remote_rmux_frame(
            &mut reader,
            MAX_REMOTE_RMUX_EVENT_FRAME,
            "remote rmux event",
        )?
        else {
            bail!("remote rmux event stream ended");
        };
        let event = match frame {
            RemoteRmuxEventFrame::Event { event } => *event,
            RemoteRmuxEventFrame::Heartbeat => continue,
        };
        if awaiting_rebase {
            match event.topic {
                MuxEventTopic::BackendDisconnected => {
                    *disconnected = true;
                    events.publish(event);
                    continue;
                }
                MuxEventTopic::BackendLagged => {
                    events.publish(event);
                    continue;
                }
                MuxEventTopic::SnapshotRebased => {
                    for event in normalize_remote_event_stream_rebase(event, expected_rebase)? {
                        events.publish(event);
                    }
                    awaiting_rebase = false;
                    *established = true;
                    *disconnected = false;
                    continue;
                }
                _ => bail!("remote rmux event stream did not begin with a snapshot rebase"),
            }
        }
        match event.topic {
            MuxEventTopic::BackendDisconnected => *disconnected = true,
            MuxEventTopic::SnapshotRebased => *disconnected = false,
            _ => {}
        }
        events.publish(event);
    }
    Ok(())
}

#[cfg(feature = "app")]
fn normalize_remote_event_stream_rebase(
    mut event: MuxEventDraft,
    expected: MuxRebaseReason,
) -> Result<Vec<MuxEventDraft>> {
    if event.topic != MuxEventTopic::SnapshotRebased {
        bail!("remote rmux event stream did not begin with a snapshot rebase");
    }
    let MuxEventPayload::Rebase { reason } = &mut event.payload else {
        bail!("remote rmux snapshot rebase carried a non-rebase payload");
    };
    if *reason == MuxRebaseReason::Bootstrap && expected == MuxRebaseReason::Reconnect {
        *reason = MuxRebaseReason::Reconnect;
        return Ok(vec![event]);
    }
    if *reason == expected {
        return Ok(vec![event]);
    }
    let provenance = event.provenance;
    Ok(vec![event, MuxEventDraft::rebase(provenance, expected)])
}

#[cfg(feature = "app")]
fn publish_remote_rmux_transport_disconnect(
    events: &MuxEventQueue,
    disconnected: &mut bool,
    reason: String,
) {
    if *disconnected {
        return;
    }
    *disconnected = true;
    events.publish(MuxEventDraft::new(
        MuxEventTopic::BackendDisconnected,
        MuxEventProvenance::RmuxSdk,
        None,
        None,
        MuxEventPayload::Disconnected { reason },
    ));
}

#[cfg(feature = "app")]
fn run_remote_rmux_request(
    remote: &SshRemote,
    request: &RemoteRmuxRequest,
) -> Result<CommandOutput> {
    remote.ensure_daemon()?;
    let (program, args) = remote_rmux_argv(remote, request)?;
    let output = SystemCommandRunner.run(&program, &args)?;
    if output.success {
        return Ok(output);
    }
    Err(remote_operation_failure(remote.host(), &output.stderr))
}

#[cfg(feature = "app")]
impl MuxBackend for RemoteRmuxBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        let output = self.run(&RemoteRmuxRequest::Snapshot)?;
        serde_json::from_str(&output.stdout).context("decode remote Space snapshot")
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        self.completion = None;
        if let MuxCommand::CreateSession { plan } = &command {
            plan.validate()?;
            if !supports_rmux_session_launch_plan(plan) {
                return Err(MuxBackendOperationError::unsupported(
                    "remote rmux backend cannot preserve this recursive session launch plan",
                )
                .into());
            }
        }
        let output = self.run(&RemoteRmuxRequest::Execute { command })?;
        self.completion = decode_remote_operation_completion(&output.stdout)
            .context("decode remote rmux command completion")?;
        Ok(())
    }

    fn execute_checked(
        &mut self,
        scope: MuxScope,
        command: MuxCommand,
        precondition: Option<&MuxScopedExecutionPrecondition>,
    ) -> BindingOperationOutcome<Result<()>> {
        let descriptor = self.capabilities(scope);
        descriptor.invoke(
            descriptor.request(command.operation()),
            BindingOperationAvailability::Available,
            || {
                if let Some(precondition) = precondition {
                    if precondition.scope != scope {
                        return Err(MuxBackendOperationError::stale(
                            "remote rmux binding scope changed",
                        )
                        .into());
                    }
                    let snapshot = self.snapshot()?;
                    if !precondition.matches_snapshot(&snapshot) {
                        return Err(MuxBackendOperationError::stale(
                            "remote rmux command target changed before mutation",
                        )
                        .into());
                    }
                }
                self.execute(command)
            },
        )
    }

    fn execute_session_launch(
        &mut self,
        plan: MuxSessionLaunchPlan,
    ) -> BindingOperationOutcome<Result<()>> {
        self.completion = None;
        if plan.validate().is_err() || !supports_rmux_session_launch_plan(&plan) {
            return BindingOperationOutcome::Unsupported;
        }
        BindingOperationOutcome::Supported(self.execute(MuxCommand::CreateSession { plan }))
    }

    fn session_launch_capability(
        &self,
        plan: &MuxSessionLaunchPlan,
    ) -> BindingOperationOutcome<()> {
        (plan.validate().is_ok() && supports_rmux_session_launch_plan(plan))
            .then_some(())
            .map_or(
                BindingOperationOutcome::Unsupported,
                BindingOperationOutcome::Supported,
            )
    }

    fn take_authoritative_completion(&mut self) -> Option<MuxBackendCommandCompletion> {
        self.completion.take()
    }

    fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor {
        rmux_capabilities(scope)
    }

    fn event_capabilities(&self) -> Vec<MuxEventCapability> {
        crate::rmux_events::event_capabilities()
    }

    fn start_event_stream(&mut self) {
        self.start_remote_event_worker();
    }

    fn drain_events(&mut self, scope: MuxScope, maximum: usize) -> Vec<MuxEvent> {
        self.events.drain(scope, maximum)
    }
    fn release_event_scope(&mut self, scope: MuxScope) {
        self.events.remove_scope(scope);
    }
}

#[cfg(feature = "app")]
fn resolve_remote_rmux_pane_target(
    remote: &SshRemote,
    target: &RmuxPaneTarget,
) -> Result<RmuxPaneTarget> {
    let request = RemoteRmuxRequest::ResolvePane {
        session: target.session_selector().to_owned(),
        pane: target.pane_selector().map(str::to_owned),
    };
    let output = run_remote_rmux_request(remote, &request)?;
    decode_remote_pane_resolution(target.session_selector(), &output.stdout)
}

#[cfg(feature = "app")]
pub(crate) fn open_remote_rmux_pane_io(
    remote: &SshRemote,
    target: &RmuxPaneTarget,
    max_scrollback: usize,
) -> Result<RmuxPaneIo> {
    let target = resolve_remote_rmux_pane_target(remote, target)?;
    let session = target.session_selector().to_owned();
    let pane = target.pane_selector().map(str::to_owned).ok_or_else(|| {
        MuxBackendOperationError::Failed("remote rmux pane resolver returned no pane".to_owned())
    })?;
    let (output_tx, output_rx) = mpsc::channel();
    let (input_tx, input_rx) = tokio_mpsc::unbounded_channel();
    let (resize_tx, resize_rx) = tokio_mpsc::unbounded_channel();
    let (result_tx, result_rx) = mpsc::channel();

    spawn_output(
        remote,
        session.clone(),
        pane.clone(),
        max_scrollback,
        output_tx,
        result_tx.clone(),
    )?;
    spawn_input(
        remote,
        session.clone(),
        pane.clone(),
        input_rx,
        result_tx.clone(),
    )?;
    spawn_resize(remote, session, pane, resize_rx, result_tx);

    Ok(RmuxPaneIo {
        output_rx,
        input_tx,
        resize_tx,
        result_rx,
    })
}

#[cfg(feature = "app")]
fn remote_rmux_argv(
    remote: &SshRemote,
    request: &RemoteRmuxRequest,
) -> Result<(String, Vec<String>)> {
    let payload = encode_request(request)?;
    remote.proxy_command(
        crate::REMOTE_DAEMON_PROGRAM,
        &[REMOTE_RMUX_SUBCOMMAND.to_owned(), payload],
    )
}

#[cfg(feature = "app")]
fn encode_request(request: &RemoteRmuxRequest) -> Result<String> {
    let json = serde_json::to_vec(request).context("encode remote terminal request")?;
    if json.len() > MAX_REMOTE_RMUX_PAYLOAD {
        bail!("remote terminal request is too large")
    }
    Ok(URL_SAFE_NO_PAD.encode(json))
}

fn decode_request(payload: &str) -> Result<RemoteRmuxRequest> {
    if payload.len() > MAX_REMOTE_RMUX_PAYLOAD * 2 {
        bail!("remote terminal request is too large")
    }
    let json = URL_SAFE_NO_PAD
        .decode(payload)
        .context("decode remote terminal request")?;
    serde_json::from_slice(&json).context("parse remote terminal request")
}

#[cfg(feature = "app")]
fn read_remote_rmux_frame<T: DeserializeOwned>(
    reader: &mut impl BufRead,
    maximum: usize,
    description: &str,
) -> Result<Option<T>> {
    let mut line = Vec::with_capacity(maximum.min(8 * 1024));
    loop {
        let (take, complete) = {
            let bytes = reader.fill_buf().context("read remote rmux frame")?;
            if bytes.is_empty() {
                if line.is_empty() {
                    return Ok(None);
                }
                bail!("{description} frame ended without a newline");
            }
            let take = bytes
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |position| position + 1);
            if line.len().saturating_add(take) > maximum {
                bail!("{description} frame exceeds its protocol limit");
            }
            line.extend_from_slice(&bytes[..take]);
            (take, bytes[take - 1] == b'\n')
        };
        reader.consume(take);
        if complete {
            break;
        }
    }
    line.pop();
    serde_json::from_slice(&line).with_context(|| format!("decode {description} frame"))
}

#[cfg(feature = "app")]
fn decode_remote_pane_resolution(expected_session: &str, payload: &str) -> Result<RmuxPaneTarget> {
    let resolution =
        serde_json::from_str(payload).context("decode remote terminal pane resolution")?;
    match resolution {
        RemoteRmuxPaneResolution::Resolved { session, pane } => {
            if session != expected_session {
                return Err(MuxBackendOperationError::stale(format!(
                    "remote rmux pane resolver returned session {session:?} for binding session {expected_session:?}"
                ))
                .into());
            }
            Ok(RmuxPaneTarget::new(session, Some(pane)))
        }
        RemoteRmuxPaneResolution::Error(error) => Err(error.into_error()),
    }
}

pub fn run_remote_rmux_command(payload: &str) -> Result<i32> {
    let result = (|| -> Result<()> {
        match decode_request(payload)? {
            RemoteRmuxRequest::Snapshot => {
                println!("{}", serde_json::to_string(&rmux_snapshot()?)?);
            }
            RemoteRmuxRequest::Execute { command } => {
                write_remote_operation_completion(execute_remote_rmux(command, None)?)?;
            }
            RemoteRmuxRequest::ExecuteChecked {
                command,
                precondition,
            } => {
                write_remote_operation_completion(execute_remote_rmux(
                    command,
                    Some(*precondition),
                )?)?;
            }
            RemoteRmuxRequest::ResolvePane { session, pane } => {
                let resolution = resolve_remote_pane(session, pane, resolve_rmux_pane_target);
                println!("{}", serde_json::to_string(&resolution)?);
            }
            RemoteRmuxRequest::EventStream => {
                #[cfg(feature = "app")]
                stream_remote_rmux_events()?;
                #[cfg(not(feature = "app"))]
                bail!("remote rmux event streaming requires the app feature");
            }
            RemoteRmuxRequest::PaneStream {
                session,
                pane,
                max_scrollback,
            } => stream_pane(session, pane, max_scrollback)?,
            RemoteRmuxRequest::PaneInput { session, pane } => input_pane(session, pane)?,
            RemoteRmuxRequest::Resize {
                session,
                pane,
                cols,
                rows,
            } => resize_rmux_pane(
                RmuxPaneTarget::new(session, Some(pane)),
                TerminalSizeSpec::new(cols, rows),
            )?,
        }
        Ok(())
    })();

    match result {
        Ok(()) => Ok(0),
        Err(error) => {
            write_remote_operation_error(&error)?;
            Ok(1)
        }
    }
}

fn execute_remote_rmux(
    command: MuxCommand,
    precondition: Option<MuxScopedExecutionPrecondition>,
) -> Result<Option<MuxBackendCommandCompletion>> {
    let completion = match command {
        MuxCommand::CreateSession { plan } => {
            plan.validate()?;
            if !supports_rmux_session_launch_plan(&plan) {
                return Err(MuxBackendOperationError::unsupported(
                    "rmux cannot preserve this recursive session launch plan",
                )
                .into());
            }
            let allocated = rmux_launch_session(plan)?;
            let target = MuxEventTarget::session(allocated.session_id.clone());
            Some(MuxBackendCommandCompletion {
                allocated: Some(allocated),
                target: Some(target),
            })
        }
        command => {
            if precondition.is_some() {
                return Err(MuxBackendOperationError::unsupported(
                    "remote rmux lacks an atomic checked mutation protocol",
                )
                .into());
            }
            rmux_execute(command)?;
            None
        }
    };
    #[cfg(feature = "app")]
    crate::rmux_events::topology_invalidated();
    Ok(completion)
}

#[cfg(feature = "app")]
fn stream_remote_rmux_events() -> Result<()> {
    crate::rmux_events::start();
    let scope = MuxScope::new(
        SpaceId::from_persistence(-1),
        BindingId::from_persistence(-1),
    );
    let mut stdout = BufWriter::new(std::io::stdout().lock());
    let mut last_heartbeat = Instant::now();
    loop {
        for event in crate::rmux_events::drain_events(scope, REMOTE_RMUX_EVENT_BATCH) {
            write_remote_rmux_event_frame(&mut stdout, event_to_remote_draft(event))?;
        }
        // An idle stream still writes periodically so a disconnected SSH peer tears this daemon
        // process (and its local SDK observers) down promptly.
        if last_heartbeat.elapsed() >= REMOTE_RMUX_EVENT_HEARTBEAT_INTERVAL {
            write_remote_rmux_heartbeat(&mut stdout)?;
            last_heartbeat = Instant::now();
        }
        thread::sleep(REMOTE_RMUX_EVENT_POLL_INTERVAL);
    }
}

#[cfg(feature = "app")]
fn event_to_remote_draft(event: MuxEvent) -> MuxEventDraft {
    // Scope, revision, and backend identity are assigned by the receiving binding's queue; the
    // daemon transfers only the authoritative rmux observation facts.
    MuxEventDraft::new(
        event.topic,
        event.provenance,
        event.target,
        event.cursor,
        event.payload,
    )
}

#[cfg(feature = "app")]
fn write_remote_rmux_event_frame(writer: &mut impl Write, event: MuxEventDraft) -> Result<()> {
    let provenance = event.provenance;
    let payload = serde_json::to_vec(&RemoteRmuxEventFrame::Event {
        event: Box::new(event),
    })
    .context("encode remote rmux event frame")?;
    if payload.len() <= MAX_REMOTE_RMUX_EVENT_FRAME {
        return write_remote_rmux_event_payload(writer, payload);
    }
    let rebase = serde_json::to_vec(&RemoteRmuxEventFrame::Event {
        event: Box::new(MuxEventDraft::rebase(
            provenance,
            MuxRebaseReason::SequenceGap,
        )),
    })
    .context("encode bounded remote rmux rebase frame")?;
    write_remote_rmux_event_payload(writer, rebase)
}

#[cfg(feature = "app")]
fn write_remote_rmux_heartbeat(writer: &mut impl Write) -> Result<()> {
    let payload =
        serde_json::to_vec(&RemoteRmuxEventFrame::Heartbeat).context("encode rmux heartbeat")?;
    write_remote_rmux_event_payload(writer, payload)
}

#[cfg(feature = "app")]
fn write_remote_rmux_event_payload(writer: &mut impl Write, payload: Vec<u8>) -> Result<()> {
    if payload.len() > MAX_REMOTE_RMUX_EVENT_FRAME {
        bail!("remote rmux event frame exceeds its protocol limit");
    }
    writer
        .write_all(&payload)
        .context("write remote rmux event frame")?;
    writer
        .write_all(b"\n")
        .context("terminate remote rmux event frame")?;
    writer.flush().context("flush remote rmux event frame")
}

fn stream_pane(session: String, pane: String, max_scrollback: usize) -> Result<()> {
    if let Err(error) = stream_pane_inner(session, pane, max_scrollback) {
        let mut stdout = BufWriter::new(std::io::stdout().lock());
        write_remote_pane_frame(
            &mut stdout,
            RemotePaneFrame::Error(RemoteRmuxOperationError::from_error(&error)),
        )?;
    }
    Ok(())
}

fn stream_pane_inner(session: String, pane: String, max_scrollback: usize) -> Result<()> {
    let io = open_rmux_pane_io(RmuxPaneTarget::new(session, Some(pane)), max_scrollback)?;
    let mut stdout = BufWriter::new(std::io::stdout().lock());
    for event in io.output_rx {
        let frame = pane_frame(event);
        let terminal_error = matches!(frame, RemotePaneFrame::Error(_));
        write_remote_pane_frame(&mut stdout, frame)?;
        if terminal_error {
            break;
        }
    }
    Ok(())
}

fn write_remote_pane_frame(writer: &mut impl Write, frame: RemotePaneFrame) -> Result<()> {
    let payload = serde_json::to_vec(&frame).context("encode remote terminal frame")?;
    if payload.len() > MAX_REMOTE_RMUX_EVENT_FRAME {
        bail!("remote terminal frame exceeds its protocol limit");
    }
    writer
        .write_all(&payload)
        .context("write remote terminal frame")?;
    writer
        .write_all(b"\n")
        .context("terminate remote terminal frame")?;
    writer.flush().context("flush remote terminal frame")
}

fn pane_frame(event: RmuxPaneEvent) -> RemotePaneFrame {
    match event {
        RmuxPaneEvent::Restore {
            buffered_chunks,
            capture,
        } => RemotePaneFrame::Restore {
            capture: URL_SAFE_NO_PAD.encode(capture),
            buffered_chunks: encode_chunks(buffered_chunks),
        },
        RmuxPaneEvent::Chunks(chunks) => RemotePaneFrame::Chunks(encode_chunks(chunks)),
        RmuxPaneEvent::KeyboardProtocol(bytes) => {
            RemotePaneFrame::KeyboardProtocol(URL_SAFE_NO_PAD.encode(bytes))
        }
        RmuxPaneEvent::Error(error) => {
            RemotePaneFrame::Error(RemoteRmuxOperationError::from_error(&error))
        }
    }
}

fn encode_chunks(chunks: Vec<PaneOutputChunk>) -> Vec<String> {
    chunks
        .into_iter()
        .filter_map(|chunk| match chunk {
            PaneOutputChunk::Bytes { bytes, .. } => Some(URL_SAFE_NO_PAD.encode(bytes)),
            PaneOutputChunk::Lag(lag) if !lag.recent.bytes.is_empty() => {
                Some(URL_SAFE_NO_PAD.encode(lag.recent.bytes))
            }
            _ => None,
        })
        .collect()
}

#[cfg(feature = "app")]
fn decode_chunks(chunks: Vec<String>) -> Result<Vec<PaneOutputChunk>> {
    chunks
        .into_iter()
        .enumerate()
        .map(|(sequence, bytes)| {
            Ok(PaneOutputChunk::Bytes {
                sequence: sequence as u64,
                bytes: URL_SAFE_NO_PAD
                    .decode(bytes)
                    .context("decode remote terminal output")?,
            })
        })
        .collect()
}

fn input_pane(session: String, pane: String) -> Result<()> {
    if let Err(error) = input_pane_inner(session, pane) {
        let mut stdout = BufWriter::new(std::io::stdout().lock());
        write_remote_pane_input_frame(
            &mut stdout,
            RemotePaneInputFrame::Error(RemoteRmuxOperationError::from_error(&error)),
        )?;
    }
    Ok(())
}

fn input_pane_inner(session: String, pane: String) -> Result<()> {
    let io = open_rmux_pane_io(RmuxPaneTarget::new(session, Some(pane)), 0)?;
    thread::spawn(move || for _ in io.output_rx {});
    let stdin = std::io::stdin();
    let mut stdout = BufWriter::new(std::io::stdout().lock());
    for line in stdin.lock().lines() {
        let bytes = decode_input_line(&line?)?;
        io.input_tx
            .send(bytes)
            .map_err(|_| anyhow::anyhow!("remote terminal input stopped"))?;
        io.result_rx
            .recv()
            .context("remote terminal input worker stopped")??;
        write_remote_pane_input_frame(&mut stdout, RemotePaneInputFrame::Ack)?;
    }
    Ok(())
}

fn write_remote_pane_input_frame(
    writer: &mut impl Write,
    frame: RemotePaneInputFrame,
) -> Result<()> {
    let payload = serde_json::to_vec(&frame).context("encode remote terminal input frame")?;
    if payload.len() > MAX_REMOTE_RMUX_INPUT_FRAME {
        bail!("remote terminal input frame exceeds its protocol limit");
    }
    writer
        .write_all(&payload)
        .context("write remote terminal input frame")?;
    writer
        .write_all(b"\n")
        .context("terminate remote terminal input frame")?;
    writer.flush().context("flush remote terminal input frame")
}

fn decode_input_line(line: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(line)
        .context("decode remote terminal input")
}

#[cfg(feature = "app")]
fn spawn_output(
    remote: &SshRemote,
    session: String,
    pane: String,
    max_scrollback: usize,
    output_tx: mpsc::Sender<RmuxPaneEvent>,
    result_tx: mpsc::Sender<Result<()>>,
) -> Result<()> {
    let request = RemoteRmuxRequest::PaneStream {
        session,
        pane,
        max_scrollback,
    };
    let (program, args) = remote_rmux_argv(remote, &request)?;
    let mut child = Command::new(&program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("stream remote terminal pane")?;
    let stdout = child
        .stdout
        .take()
        .context("remote terminal output stream has no stdout")?;

    thread::spawn(move || {
        let _guard = ChildGuard(child);
        let mut reader = BufReader::new(stdout);
        loop {
            let result = read_remote_rmux_frame::<RemotePaneFrame>(
                &mut reader,
                MAX_REMOTE_RMUX_EVENT_FRAME,
                "remote terminal output",
            )
            .and_then(|frame| frame.ok_or_else(|| anyhow::anyhow!("remote terminal output ended")))
            .and_then(decode_frame);
            match result {
                Ok(event) => {
                    let terminal_error = matches!(&event, RmuxPaneEvent::Error(_));
                    if output_tx.send(event).is_err() || terminal_error {
                        return;
                    }
                }
                Err(error) => {
                    let _ = result_tx.send(Err(error.context("remote terminal output stopped")));
                    return;
                }
            }
        }
    });
    Ok(())
}

#[cfg(feature = "app")]
fn decode_frame(frame: RemotePaneFrame) -> Result<RmuxPaneEvent> {
    Ok(match frame {
        RemotePaneFrame::Restore {
            capture,
            buffered_chunks,
        } => RmuxPaneEvent::Restore {
            capture: URL_SAFE_NO_PAD
                .decode(capture)
                .context("decode remote terminal restore")?,
            buffered_chunks: decode_chunks(buffered_chunks)?,
        },
        RemotePaneFrame::Chunks(chunks) => RmuxPaneEvent::Chunks(decode_chunks(chunks)?),
        RemotePaneFrame::KeyboardProtocol(bytes) => RmuxPaneEvent::KeyboardProtocol(
            URL_SAFE_NO_PAD
                .decode(bytes)
                .context("decode remote terminal protocol")?,
        ),
        RemotePaneFrame::Error(error) => RmuxPaneEvent::Error(error.into_error()),
    })
}

#[cfg(feature = "app")]
fn spawn_input(
    remote: &SshRemote,
    session: String,
    pane: String,
    mut input_rx: tokio_mpsc::UnboundedReceiver<Vec<u8>>,
    result_tx: mpsc::Sender<Result<()>>,
) -> Result<()> {
    let request = RemoteRmuxRequest::PaneInput { session, pane };
    let (program, args) = remote_rmux_argv(remote, &request)?;
    let mut child = Command::new(&program)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("open remote terminal input")?;
    let stdin = child
        .stdin
        .take()
        .context("remote terminal input has no stdin")?;
    let stdout = child
        .stdout
        .take()
        .context("remote terminal input has no stdout")?;
    let stop = Arc::new(AtomicBool::new(false));
    monitor_remote_input_child(child, Arc::clone(&stop));

    let reader_stop = Arc::clone(&stop);
    let reader_results = result_tx.clone();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let result = read_remote_rmux_frame::<RemotePaneInputFrame>(
                &mut reader,
                MAX_REMOTE_RMUX_INPUT_FRAME,
                "remote terminal input",
            )
            .and_then(|frame| {
                frame.ok_or_else(|| anyhow::anyhow!("remote terminal input acknowledgement ended"))
            })
            .and_then(decode_remote_pane_input_frame);
            match result {
                Ok(()) => {
                    let _ = reader_results.send(Ok(()));
                }
                Err(error) => {
                    if !reader_stop.swap(true, Ordering::AcqRel) {
                        let _ = reader_results
                            .send(Err(error.context("remote terminal input stopped")));
                    }
                    return;
                }
            }
        }
    });

    thread::spawn(move || {
        let mut writer = BufWriter::new(stdin);
        while let Some(bytes) = input_rx.blocking_recv() {
            if stop.load(Ordering::Acquire) {
                return;
            }
            if let Err(error) = write_input_line(&mut writer, &bytes) {
                if !stop.swap(true, Ordering::AcqRel) {
                    let _ = result_tx.send(Err(
                        anyhow::Error::from(error).context("remote terminal input stopped")
                    ));
                }
                return;
            }
        }
        stop.store(true, Ordering::Release);
    });
    Ok(())
}

#[cfg(feature = "app")]
fn decode_remote_pane_input_frame(frame: RemotePaneInputFrame) -> Result<()> {
    match frame {
        RemotePaneInputFrame::Ack => Ok(()),
        RemotePaneInputFrame::Error(error) => Err(error.into_error()),
    }
}

#[cfg(feature = "app")]
fn monitor_remote_input_child(child: Child, stop: Arc<AtomicBool>) {
    thread::spawn(move || {
        let mut child = ChildGuard(child);
        while !stop.load(Ordering::Acquire) {
            match child.0.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => thread::sleep(Duration::from_millis(20)),
            }
        }
    });
}

#[cfg(feature = "app")]
fn write_input_line(writer: &mut BufWriter<ChildStdin>, bytes: &[u8]) -> std::io::Result<()> {
    writer.write_all(URL_SAFE_NO_PAD.encode(bytes).as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(feature = "app")]
fn spawn_resize(
    remote: &SshRemote,
    session: String,
    pane: String,
    mut resize_rx: tokio_mpsc::UnboundedReceiver<TerminalSizeSpec>,
    result_tx: mpsc::Sender<Result<()>>,
) {
    let remote = remote.clone();
    thread::spawn(move || {
        while let Some(size) = resize_rx.blocking_recv() {
            let request = RemoteRmuxRequest::Resize {
                session: session.clone(),
                pane: pane.clone(),
                cols: size.cols,
                rows: size.rows,
            };
            let result = remote
                .ensure_daemon()
                .and_then(|_| remote_rmux_argv(&remote, &request))
                .and_then(|(program, args)| {
                    let output = SystemCommandRunner.run(&program, &args)?;
                    if output.success {
                        Ok(())
                    } else {
                        Err(remote_operation_failure(remote.host(), &output.stderr))
                    }
                });
            let _ = result_tx.send(result);
        }
    });
}

#[cfg(feature = "app")]
struct ChildGuard(Child);

#[cfg(feature = "app")]
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "app")]
    use crate::controller::{BindingId, SpaceId};
    #[cfg(feature = "app")]
    use bootty_config::config::SshRemoteConfig;

    #[cfg(feature = "app")]
    fn remote() -> SshRemote {
        SshRemote::new(SshRemoteConfig::for_host("devbox"))
    }

    #[cfg(feature = "app")]
    fn decode_pane_resolution(
        expected_session: &str,
        resolution: RemoteRmuxPaneResolution,
    ) -> Result<RmuxPaneTarget> {
        let payload = serde_json::to_string(&resolution).expect("encode pane resolution");
        decode_remote_pane_resolution(expected_session, &payload)
    }

    #[cfg(feature = "app")]
    #[test]
    fn remote_commands_use_boottys_embedded_protocol() {
        let (_program, argv) =
            remote_rmux_argv(&remote(), &RemoteRmuxRequest::Snapshot).expect("remote command");
        let (remote_program, args, _) =
            crate::ssh::decode_proxy_command_line(argv.last().expect("command"))
                .expect("decode command");

        assert_eq!(remote_program, crate::ssh::REMOTE_DAEMON_PROGRAM);
        assert_eq!(
            args.first().map(String::as_str),
            Some(REMOTE_RMUX_SUBCOMMAND)
        );
    }

    #[cfg(feature = "app")]
    #[test]
    fn remote_request_round_trips_hostile_command_arguments() {
        let request = RemoteRmuxRequest::Execute {
            command: MuxCommand::RenameSession {
                session_id: "space ; $HOME".to_owned(),
                name: "work & play\"".to_owned(),
            },
        };

        let decoded = decode_request(&encode_request(&request).unwrap()).unwrap();

        assert!(matches!(
            decoded,
            RemoteRmuxRequest::Execute {
                command: MuxCommand::RenameSession { session_id, name }
            } if session_id == "space ; $HOME" && name == "work & play\""
        ));
    }

    #[cfg(feature = "app")]
    #[test]
    fn remote_event_request_round_trips_through_boottys_protocol() {
        let decoded = decode_request(&encode_request(&RemoteRmuxRequest::EventStream).unwrap())
            .expect("decode remote event request");

        assert!(matches!(decoded, RemoteRmuxRequest::EventStream));
    }

    #[cfg(feature = "app")]
    #[test]
    fn remote_event_frames_round_trip_exact_rmux_observations() {
        let target = MuxEventTarget::pane("session", "@4", "%7", "t7", None);
        let event = MuxEventDraft::new(
            MuxEventTopic::TerminalOutput,
            MuxEventProvenance::RmuxSdk,
            Some(target.clone()),
            Some(crate::backend::MuxEventCursor::new(
                "rmux-output:%7:gen",
                19,
            )),
            MuxEventPayload::Output {
                bytes: vec![0, 0x1b, 0xff],
            },
        );
        let payload = serde_json::to_vec(&RemoteRmuxEventFrame::Event {
            event: Box::new(event.clone()),
        })
        .expect("encode event frame");
        let decoded = match serde_json::from_slice(&payload).expect("decode event frame") {
            RemoteRmuxEventFrame::Event { event } => *event,
            RemoteRmuxEventFrame::Heartbeat => panic!("decoded event frame as heartbeat"),
        };

        assert!(decoded == event);
        assert!(decoded.target == Some(target));
        let heartbeat =
            serde_json::to_vec(&RemoteRmuxEventFrame::Heartbeat).expect("encode event heartbeat");
        match serde_json::from_slice::<RemoteRmuxEventFrame>(&heartbeat)
            .expect("decode event heartbeat")
        {
            RemoteRmuxEventFrame::Heartbeat => {}
            RemoteRmuxEventFrame::Event { .. } => panic!("decoded heartbeat as event frame"),
        }
    }

    #[cfg(feature = "app")]
    #[test]
    fn remote_event_frames_preserve_topology_and_pane_state_facts() {
        let target = MuxEventTarget::pane("session", "@4", "%7", "t7", None);
        let drafts = [
            MuxEventDraft::new(
                MuxEventTopic::TopologyChanged,
                MuxEventProvenance::RmuxSdk,
                None,
                None,
                MuxEventPayload::Topology {
                    change: crate::backend::MuxTopologyChange::Invalidated,
                },
            ),
            MuxEventDraft::new(
                MuxEventTopic::PaneStateChanged,
                MuxEventProvenance::RmuxSdk,
                Some(target.clone()),
                Some(crate::backend::MuxEventCursor::new("rmux-state:%7:gen", 23)),
                MuxEventPayload::PaneState {
                    state: crate::backend::MuxPaneState::default(),
                },
            ),
        ];

        for draft in drafts {
            let payload = serde_json::to_vec(&RemoteRmuxEventFrame::Event {
                event: Box::new(draft.clone()),
            })
            .expect("encode event frame");
            let decoded = match serde_json::from_slice(&payload).expect("decode event frame") {
                RemoteRmuxEventFrame::Event { event } => *event,
                RemoteRmuxEventFrame::Heartbeat => panic!("decoded event frame as heartbeat"),
            };
            assert!(decoded == draft);
        }
    }

    #[cfg(feature = "app")]
    #[test]
    fn remote_event_reader_rejects_an_oversized_protocol_frame() {
        let mut reader = BufReader::new(std::io::Cursor::new(b"abcdef\n".to_vec()));

        let error =
            read_remote_rmux_frame::<RemoteRmuxEventFrame>(&mut reader, 4, "remote rmux event")
                .unwrap_err();

        assert!(error.to_string().contains("exceeds its protocol limit"));
    }

    #[cfg(feature = "app")]
    #[test]
    fn remote_event_writer_rejects_an_oversized_protocol_frame() {
        let mut output = Vec::new();
        let error = write_remote_rmux_event_payload(
            &mut output,
            vec![0; MAX_REMOTE_RMUX_EVENT_FRAME.saturating_add(1)],
        )
        .unwrap_err();

        assert!(error.to_string().contains("exceeds its protocol limit"));
        assert!(output.is_empty());
    }

    #[cfg(feature = "app")]
    #[test]
    fn reconnect_turns_a_new_daemon_bootstrap_into_a_reconnect_rebase() {
        let event = MuxEventDraft::rebase(MuxEventProvenance::RmuxSdk, MuxRebaseReason::Bootstrap);

        let normalized =
            normalize_remote_event_stream_rebase(event, MuxRebaseReason::Reconnect).unwrap();

        assert!(matches!(
            normalized.as_slice(),
            [MuxEventDraft {
                topic: MuxEventTopic::SnapshotRebased,
                payload: MuxEventPayload::Rebase {
                    reason: MuxRebaseReason::Reconnect
                },
                ..
            }]
        ));
    }

    #[cfg(feature = "app")]
    #[test]
    fn reconnect_keeps_a_remote_gap_rebase_before_its_transport_rebase() {
        let event =
            MuxEventDraft::rebase(MuxEventProvenance::RmuxSdk, MuxRebaseReason::SequenceGap);

        let normalized =
            normalize_remote_event_stream_rebase(event, MuxRebaseReason::Reconnect).unwrap();

        assert!(matches!(
            normalized.as_slice(),
            [
                MuxEventDraft {
                    payload: MuxEventPayload::Rebase {
                        reason: MuxRebaseReason::SequenceGap
                    },
                    ..
                },
                MuxEventDraft {
                    payload: MuxEventPayload::Rebase {
                        reason: MuxRebaseReason::Reconnect
                    },
                    ..
                }
            ]
        ));
    }

    #[cfg(feature = "app")]
    #[test]
    fn dropping_a_remote_event_worker_stops_its_transport() {
        let stop = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));
        let worker = RemoteRmuxEventWorker {
            stop: Arc::clone(&stop),
            child: Arc::clone(&child),
        };

        drop(worker);

        assert!(stop.load(Ordering::Acquire));
        assert!(
            child
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_none()
        );
    }

    #[cfg(feature = "app")]
    #[test]
    fn pane_frames_preserve_binary_output() {
        let event = RmuxPaneEvent::Restore {
            capture: vec![0, 0xff, b'\n'],
            buffered_chunks: vec![PaneOutputChunk::Bytes {
                sequence: 7,
                bytes: vec![0x1b, b'[', b'm'],
            }],
        };

        let decoded = decode_frame(pane_frame(event)).unwrap();

        assert!(matches!(
            decoded,
            RmuxPaneEvent::Restore { capture, buffered_chunks }
                if capture == vec![0, 0xff, b'\n']
                    && matches!(buffered_chunks.as_slice(), [PaneOutputChunk::Bytes { bytes, .. }] if bytes == &vec![0x1b, b'[', b'm'])
        ));
    }

    #[cfg(feature = "app")]
    #[test]
    fn pane_frames_preserve_typed_stale_errors() {
        let event = RmuxPaneEvent::Error(
            MuxBackendOperationError::Stale("pane vanished before stream open".to_owned()).into(),
        );
        let frame = pane_frame(event);

        assert!(matches!(
            &frame,
            RemotePaneFrame::Error(RemoteRmuxOperationError::Stale(message))
                if message == "pane vanished before stream open"
        ));

        let decoded = decode_frame(frame).expect("decode stale pane frame");
        let RmuxPaneEvent::Error(error) = decoded else {
            panic!("expected stale pane error event");
        };
        assert_eq!(
            error.downcast_ref::<MuxBackendOperationError>(),
            Some(&MuxBackendOperationError::Stale(
                "pane vanished before stream open".to_owned()
            ))
        );
    }

    #[cfg(feature = "app")]
    #[test]
    fn pane_frames_preserve_lag_recovery_bytes() {
        let bytes = vec![0x1b, b'[', b'm'];
        let event = RmuxPaneEvent::Chunks(vec![PaneOutputChunk::Lag(rmux_sdk::PaneLagNotice {
            expected_sequence: 1,
            resume_sequence: 2,
            missed_events: 1,
            newest_sequence: 2,
            recent: rmux_sdk::PaneRecentOutput {
                bytes: bytes.clone(),
                oldest_sequence: Some(2),
                newest_sequence: Some(2),
            },
        })]);

        let decoded = decode_frame(pane_frame(event)).unwrap();

        assert!(matches!(
            decoded,
            RmuxPaneEvent::Chunks(chunks)
                if matches!(chunks.as_slice(), [PaneOutputChunk::Bytes { bytes: recovered, .. }] if recovered == &bytes)
        ));
    }
    #[test]
    fn input_lines_preserve_exact_bytes() {
        let bytes = [0x00, 0x1b, 0xff];
        assert_eq!(
            decode_input_line(&URL_SAFE_NO_PAD.encode(bytes)).unwrap(),
            bytes
        );
    }

    #[cfg(feature = "app")]
    #[test]
    fn a_remote_rmux_binding_claims_rmux_operations() {
        let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(2));
        let backend = RemoteRmuxBackend::new(remote());

        assert_eq!(
            backend.capabilities(scope).operations().collect::<Vec<_>>(),
            rmux_capabilities(scope).operations().collect::<Vec<_>>()
        );
    }

    #[cfg(feature = "app")]
    #[test]
    fn remote_rmux_event_capabilities_match_local_rmux_authority() {
        let backend = RemoteRmuxBackend::new(remote());

        assert_eq!(
            backend.event_capabilities(),
            crate::rmux_events::event_capabilities()
        );
    }

    #[cfg(feature = "app")]
    #[test]
    fn remote_backend_drains_protocol_events_with_binding_scoped_revisions() {
        let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(2));
        let other_scope =
            MuxScope::new(SpaceId::from_persistence(3), BindingId::from_persistence(4));
        let mut backend = RemoteRmuxBackend::new(remote());
        let target = MuxEventTarget::pane("session", "@1", "%2", "t2", None);
        let first_draft = MuxEventDraft::new(
            MuxEventTopic::TerminalOutput,
            MuxEventProvenance::RmuxSdk,
            Some(target.clone()),
            Some(crate::backend::MuxEventCursor::new("rmux-output:%2:gen", 4)),
            MuxEventPayload::Output {
                bytes: b"first".to_vec(),
            },
        );
        let second_draft = MuxEventDraft::new(
            MuxEventTopic::TerminalOutput,
            MuxEventProvenance::RmuxSdk,
            Some(target.clone()),
            Some(crate::backend::MuxEventCursor::new("rmux-output:%2:gen", 5)),
            MuxEventPayload::Output {
                bytes: b"second".to_vec(),
            },
        );
        let bootstrap_draft =
            MuxEventDraft::rebase(MuxEventProvenance::RmuxSdk, MuxRebaseReason::Bootstrap);
        backend.events.publish(bootstrap_draft.clone());

        backend.events.publish(first_draft.clone());
        backend.events.publish(second_draft.clone());

        let bootstrap = backend.drain_events(scope, 1);
        let first = backend.drain_events(scope, 1);
        let second = backend.drain_events(scope, 1);
        let other_bootstrap = backend.drain_events(other_scope, 1);
        let other_binding = backend.drain_events(other_scope, 2);

        assert_eq!(bootstrap.len(), 1);
        assert_eq!(bootstrap[0].revision, 1);
        assert!(matches!(
            &bootstrap[0].payload,
            MuxEventPayload::Rebase {
                reason: MuxRebaseReason::Bootstrap
            }
        ));
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].revision, 2);
        assert_eq!(first[0].cursor, first_draft.cursor);
        assert_eq!(first[0].target, Some(target.clone()));
        assert_eq!(first[0].provenance, MuxEventProvenance::RmuxSdk);
        assert!(first[0].backend_identity.starts_with("rmux:remote:"));
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].revision, 3);
        assert_eq!(second[0].cursor, second_draft.cursor);
        assert_eq!(
            other_bootstrap
                .iter()
                .map(|event| event.revision)
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(
            other_binding
                .iter()
                .map(|event| event.revision)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        backend.release_event_scope(other_scope);
        backend.release_event_scope(scope);
        backend.events.publish(bootstrap_draft);
        let recreated = backend.drain_events(scope, 1);
        assert_eq!(recreated.len(), 1);
        assert_eq!(recreated[0].revision, 1);
        assert!(matches!(
            &recreated[0].payload,
            MuxEventPayload::Rebase {
                reason: MuxRebaseReason::Bootstrap
            }
        ));
    }

    #[cfg(feature = "app")]
    #[test]
    fn remote_pane_frames_preserve_typed_operation_errors() {
        let output = match decode_frame(RemotePaneFrame::Error(
            RemoteRmuxOperationError::Unavailable("daemon unavailable".to_owned()),
        )) {
            Ok(RmuxPaneEvent::Error(error)) => error,
            Ok(_) => panic!("expected an operation error event"),
            Err(error) => panic!("error frame failed to decode: {error:#}"),
        };
        let input = match decode_remote_pane_input_frame(RemotePaneInputFrame::Error(
            RemoteRmuxOperationError::Stale("pane changed".to_owned()),
        )) {
            Err(error) => error,
            Ok(_) => panic!("error frame must decode as an operation error"),
        };

        assert!(matches!(
            output.downcast_ref::<MuxBackendOperationError>(),
            Some(MuxBackendOperationError::Unavailable(message)) if message == "daemon unavailable"
        ));
        assert!(matches!(
            input.downcast_ref::<MuxBackendOperationError>(),
            Some(MuxBackendOperationError::Stale(message)) if message == "pane changed"
        ));
    }

    #[cfg(feature = "app")]
    #[test]
    fn remote_pane_resolution_maps_an_absent_selector_to_the_active_pane() {
        let expected = RmuxPaneTarget::new("binding-session", Some("%17".to_owned()));
        let resolution = resolve_remote_pane("binding-session".to_owned(), None, |target| {
            assert_eq!(target.session_selector(), "binding-session");
            assert_eq!(target.pane_selector(), None);
            Ok(expected.clone())
        });

        assert_eq!(
            decode_pane_resolution("binding-session", resolution).unwrap(),
            expected
        );
    }

    #[cfg(feature = "app")]
    #[test]
    fn remote_pane_resolution_preserves_an_explicit_exact_target() {
        let expected = RmuxPaneTarget::new("binding-session", Some("%23".to_owned()));
        let resolution = resolve_remote_pane(
            "binding-session".to_owned(),
            Some("%23".to_owned()),
            |target| {
                assert_eq!(target, expected);
                Ok(expected.clone())
            },
        );

        assert_eq!(
            decode_pane_resolution("binding-session", resolution).unwrap(),
            expected
        );
    }

    #[cfg(feature = "app")]
    #[test]
    fn remote_pane_resolution_preserves_a_malformed_explicit_selector_as_stale() {
        let malformed = "%not-a-pane";
        let resolution = resolve_remote_pane(
            "binding-session".to_owned(),
            Some(malformed.to_owned()),
            |target| {
                assert_eq!(target.pane_selector(), Some(malformed));
                Err(MuxBackendOperationError::stale(format!(
                    "rmux pane target {malformed:?} is malformed"
                ))
                .into())
            },
        );

        let error = decode_pane_resolution("binding-session", resolution).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<MuxBackendOperationError>(),
            Some(MuxBackendOperationError::Stale(message)) if message.contains("malformed")
        ));
    }

    #[cfg(feature = "app")]
    #[test]
    fn remote_pane_resolution_without_an_active_pane_never_yields_a_target() {
        let resolution = resolve_remote_pane("binding-session".to_owned(), None, |target| {
            assert_eq!(target.pane_selector(), None);
            Err(MuxBackendOperationError::stale("rmux window has no active pane").into())
        });
        let invoked = std::cell::Cell::new(false);

        let error = decode_pane_resolution("binding-session", resolution)
            .inspect(|_| invoked.set(true))
            .unwrap_err();

        assert!(matches!(
            error.downcast_ref::<MuxBackendOperationError>(),
            Some(MuxBackendOperationError::Stale(message)) if message.contains("no active pane")
        ));
        assert!(
            !invoked.get(),
            "a pane must not be opened after resolution fails"
        );
    }
}
