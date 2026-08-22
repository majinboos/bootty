use std::{
    collections::VecDeque,
    sync::{OnceLock, mpsc},
    thread,
};

use anyhow::{Context, Result};
use rmux_proto::{PaneTarget, Request, Response};
use rmux_sdk::{
    Pane, PaneId, PaneOutputChunk, PaneOutputStart, PaneRecoveryEvent, PaneStreamEndReason, Rmux,
    SessionName, TerminalSizeSpec,
};
use tokio::runtime::Builder;
use tokio::sync::mpsc as tokio_mpsc;

use crate::backend::{list_pane_rows, list_window_rows, rmux_request};
use crate::bridge::connect_bootty_rmux;

pub(crate) const RMUX_OUTPUT_CHANNEL_CAPACITY: usize = 64;
const RMUX_OUTPUT_EVENT_MAX_BYTES: usize = 16 * 1024;
const RMUX_KEYBOARD_PROTOCOL_SCAN_TAIL_BYTES: usize = 64;
const RMUX_KEYBOARD_PROTOCOL_OPTION: &str = "@bootty-keyboard-protocol";

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RmuxPaneTarget {
    session_name: String,
    pane_id: Option<String>,
}

impl RmuxPaneTarget {
    pub(crate) fn new(session_name: impl Into<String>, pane_id: Option<String>) -> Self {
        Self {
            session_name: session_name.into(),
            pane_id,
        }
    }

    fn session_name(&self) -> Result<SessionName> {
        SessionName::new(&self.session_name).context("invalid rmux session name")
    }

    #[cfg(feature = "app")]
    /// The stable session selector shared by local and remote Bootty protocols.
    pub(crate) fn session_selector(&self) -> &str {
        &self.session_name
    }

    #[cfg(feature = "app")]
    pub(crate) fn pane_selector(&self) -> Option<&str> {
        self.pane_id.as_deref()
    }

    fn pane_id(&self) -> Option<PaneId> {
        self.pane_id
            .as_deref()
            .and_then(|pane_id| pane_id.strip_prefix('%'))
            .and_then(|pane_id| pane_id.parse::<u32>().ok())
            .map(PaneId::from)
    }
}

pub(crate) enum RmuxPaneEvent {
    Rebase(Vec<u8>),
    Bytes(Vec<u8>),
    ProcessExited,
    End(Option<String>),
    Error(String),
}

pub(crate) struct RmuxPaneIo {
    pub(crate) output_rx: tokio_mpsc::Receiver<RmuxPaneEvent>,
    pub(crate) input_tx: tokio_mpsc::UnboundedSender<Vec<u8>>,
    pub(crate) resize_tx: tokio_mpsc::UnboundedSender<TerminalSizeSpec>,
    pub(crate) result_rx: mpsc::Receiver<std::result::Result<(), String>>,
}

struct RmuxOpenPaneRequest {
    target: RmuxPaneTarget,
    output_tx: tokio_mpsc::Sender<RmuxPaneEvent>,
    input_rx: tokio_mpsc::UnboundedReceiver<Vec<u8>>,
    resize_rx: tokio_mpsc::UnboundedReceiver<TerminalSizeSpec>,
    result_tx: mpsc::Sender<std::result::Result<(), String>>,
}

fn pane_sender() -> &'static mpsc::Sender<RmuxOpenPaneRequest> {
    static PANE_TX: OnceLock<mpsc::Sender<RmuxOpenPaneRequest>> = OnceLock::new();
    PANE_TX.get_or_init(|| {
        let (pane_tx, pane_rx) = mpsc::channel();
        thread::spawn(move || run_pane_worker(pane_rx));
        pane_tx
    })
}

pub(crate) fn resize_rmux_pane(target: RmuxPaneTarget, size: TerminalSizeSpec) -> Result<()> {
    let io = open_rmux_pane_io(target)?;
    io.resize_tx
        .send(size)
        .map_err(|_| anyhow::anyhow!("rmux pane resize worker stopped"))?;
    io.result_rx
        .recv()
        .map_err(|_| anyhow::anyhow!("rmux pane resize worker stopped"))?
        .map_err(anyhow::Error::msg)
}

pub(crate) fn open_rmux_pane_io(target: RmuxPaneTarget) -> Result<RmuxPaneIo> {
    // Live byte events are split to 16 KiB, so a full channel bounds the
    // producer backlog to roughly one MiB. Recovery keyframes are atomic and
    // may use the SDK's larger cold-path frame budget.
    let (output_tx, output_rx) = tokio_mpsc::channel(RMUX_OUTPUT_CHANNEL_CAPACITY);
    let (input_tx, input_rx) = tokio_mpsc::unbounded_channel();
    let (resize_tx, resize_rx) = tokio_mpsc::unbounded_channel();
    let (result_tx, result_rx) = mpsc::channel();
    pane_sender()
        .send(RmuxOpenPaneRequest {
            target,
            output_tx,
            input_rx,
            resize_rx,
            result_tx,
        })
        .map_err(|_| anyhow::anyhow!("rmux pane worker stopped"))?;
    Ok(RmuxPaneIo {
        output_rx,
        input_tx,
        resize_tx,
        result_rx,
    })
}

fn run_pane_worker(request_rx: mpsc::Receiver<RmuxOpenPaneRequest>) {
    let runtime = Builder::new_multi_thread()
        .enable_all()
        .thread_name("bootty-rmux-pane")
        .worker_threads(2)
        .build()
        .expect("rmux pane runtime should initialize");
    while let Ok(request) = request_rx.recv() {
        runtime.spawn(run_pane_io(request));
    }
}

async fn run_pane_io(request: RmuxOpenPaneRequest) {
    let RmuxOpenPaneRequest {
        target,
        output_tx,
        mut input_rx,
        mut resize_rx,
        result_tx,
    } = request;
    let result: Result<()> = async {
        target.session_name()?;
        let rmux = connect_bootty_rmux().await?;
        let pane = pane_for_target(&rmux, &target).await?;
    let mut keyboard_flags = pane
        .option(RMUX_KEYBOARD_PROTOCOL_OPTION)
        .await
        .ok()
        .flatten();
    if keyboard_flags.is_none() {
        keyboard_flags = Some(
            retained_kitty_keyboard_flags(&pane)
                .await?
                .unwrap_or_default(),
        );
        let _ = pane
            .set_option(
                RMUX_KEYBOARD_PROTOCOL_OPTION,
                keyboard_flags.as_deref().unwrap_or_default(),
            )
            .await;
    }

    let input_target = pane_input_target(&rmux, &target).await?;
    let mut recovery = pane.recover_output().await?;
    let mut pending_events = VecDeque::new();
    let mut recovery_ended = false;
    let mut terminal_protocol_tail = Vec::new();

    loop {
        if output_tx.is_closed() || (recovery_ended && pending_events.is_empty()) {
            break;
        }

        tokio::select! {
            permit = output_tx.reserve(), if !pending_events.is_empty() => {
                let Ok(permit) = permit else {
                    break;
                };
                permit.send(
                    pending_events
                        .pop_front()
                        .expect("output permit requires one pending event"),
                );
            }
            event = recovery.next(), if pending_events.is_empty() && !recovery_ended => {
                match event? {
                    Some(PaneRecoveryEvent::Rebase(mut rebase)) => {
                        append_kitty_keyboard_protocol(
                            &mut rebase.keyframe,
                            keyboard_flags.as_deref(),
                        );
                        pending_events.push_back(RmuxPaneEvent::Rebase(rebase.keyframe));
                    }
                    Some(PaneRecoveryEvent::Bytes { bytes, .. }) => {
                        if let Some(flags) =
                            observe_kitty_keyboard_flags(&mut terminal_protocol_tail, &bytes)
                        {
                            let _ = pane
                                .set_option(RMUX_KEYBOARD_PROTOCOL_OPTION, &flags)
                                .await;
                            keyboard_flags = Some(flags);
                        }
                        queue_bytes(&mut pending_events, bytes);
                    }
                    Some(PaneRecoveryEvent::Lifecycle(_)) => {
                        pending_events.push_back(RmuxPaneEvent::ProcessExited);
                    }
                    Some(PaneRecoveryEvent::End(reason)) => {
                        let error = (!matches!(reason, PaneStreamEndReason::PaneRemoved))
                            .then(|| format!("{reason:?}"));
                        pending_events.push_back(RmuxPaneEvent::End(error));
                        recovery_ended = true;
                    }
                    Some(_) => anyhow::bail!("rmux returned an unsupported pane recovery event"),
                    None => recovery_ended = true,
                }
            }
            Some(mut bytes) = input_rx.recv() => {
                while let Ok(next) = input_rx.try_recv() {
                    bytes.extend_from_slice(&next);
                }
                let result = send_rmux_pane_input(&pane, &input_target, &bytes)
                    .await
                    .map_err(|error| error.to_string());
                let _ = result_tx.send(result);
            }
            Some(mut size) = resize_rx.recv() => {
                while let Ok(next) = resize_rx.try_recv() {
                    size = next;
                }
                let result = pane.resize(size).await.map_err(|error| error.to_string());
                let _ = result_tx.send(result);
            }
            else => break,
        }
    }
        Ok(())
    }
    .await;
    if let Err(error) = result {
        let text = error.to_string();
        let _ = result_tx.send(Err(text.clone()));
        let _ = output_tx.send(RmuxPaneEvent::Error(text)).await;
    }
}

// rmux 0.10's recovery keyframe restores every tracked terminal mode except
// Kitty's exact enhancement flags. The parser deliberately ignores that
// negotiation until it can implement the complete protocol, so retain this
// cold-path transcript scan until the SDK exposes the flags losslessly.
async fn retained_kitty_keyboard_flags(pane: &Pane) -> Result<Option<String>> {
    let mut output_stream = pane
        .output_stream_starting_at(PaneOutputStart::Oldest)
        .await?;
    let mut tail = Vec::new();
    let mut flags = None;
    loop {
        let chunks = output_stream.poll_once().await?;
        if chunks.is_empty() {
            break;
        }
        for chunk in chunks {
            let PaneOutputChunk::Bytes { bytes, .. } = chunk else {
                continue;
            };
            flags = observe_kitty_keyboard_flags(&mut tail, &bytes).or(flags);
        }
    }
    Ok(flags)
}

fn observe_kitty_keyboard_flags(tail: &mut Vec<u8>, bytes: &[u8]) -> Option<String> {
    tail.extend_from_slice(bytes);
    let flags = kitty_keyboard_protocol_flags(tail);
    if tail.len() > RMUX_KEYBOARD_PROTOCOL_SCAN_TAIL_BYTES {
        tail.drain(..tail.len() - RMUX_KEYBOARD_PROTOCOL_SCAN_TAIL_BYTES);
    }
    flags
}

fn kitty_keyboard_protocol_flags(bytes: &[u8]) -> Option<String> {
    let mut search_start = 0;
    while let Some(relative_start) = bytes[search_start..]
        .windows(3)
        .position(|window| window == b"\x1b[>")
    {
        let start = search_start + relative_start;
        let flags_start = start + 3;
        let relative_end = bytes[flags_start..].iter().position(|byte| *byte == b'u')?;
        let flags_end = flags_start + relative_end;
        if flags_end == flags_start || !bytes[flags_start..flags_end].iter().all(u8::is_ascii_digit)
        {
            search_start = flags_end + 1;
            continue;
        }
        let query_end = flags_end + 5;
        if bytes.get(flags_end + 1..query_end) == Some(b"\x1b[?u") {
            return Some(String::from_utf8_lossy(&bytes[flags_start..flags_end]).into_owned());
        }
        search_start = flags_end + 1;
    }
    None
}

fn append_kitty_keyboard_protocol(keyframe: &mut Vec<u8>, flags: Option<&str>) {
    if let Some(flags) = flags.filter(|flags| !flags.is_empty()) {
        keyframe.extend_from_slice(format!("\x1b[>{flags}u").as_bytes());
    }
}

fn queue_bytes(events: &mut VecDeque<RmuxPaneEvent>, bytes: Vec<u8>) {
    events.extend(
        bytes
            .chunks(RMUX_OUTPUT_EVENT_MAX_BYTES)
            .map(|chunk| RmuxPaneEvent::Bytes(chunk.to_vec())),
    );
}

async fn send_rmux_pane_input(pane: &Pane, target: &PaneTarget, bytes: &[u8]) -> Result<()> {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return pane.send_text(text).await.map_err(Into::into);
    }
    let response = rmux_request(Request::SendKeysExt(rmux_proto::SendKeysExtRequest {
        target: Some(target.clone()),
        keys: rmux_hex_keys(bytes),
        expand_formats: false,
        hex: true,
        literal: false,
        dispatch_key_table: false,
        copy_mode_command: false,
        forward_mouse_event: false,
        reset_terminal: false,
        repeat_count: None,
    }))
    .await?;
    let Response::SendKeys(_) = response else {
        anyhow::bail!("rmux returned an unexpected send-keys response");
    };
    Ok(())
}

fn rmux_hex_keys(bytes: &[u8]) -> Vec<String> {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn pane_input_target(rmux: &Rmux, target: &RmuxPaneTarget) -> Result<PaneTarget> {
    let session_name = target.session_name()?;
    let pane_id = target
        .pane_id()
        .context("rmux pane id required for output pipe")?;
    let pane = list_pane_rows(rmux, &session_name)
        .await?
        .into_iter()
        .find(|pane| pane.pane_id == pane_id.to_string())
        .context("rmux output pipe pane not found")?;
    let window_index = list_window_rows(rmux, &session_name)
        .await?
        .into_iter()
        .find(|window| window.id == pane.window_id)
        .map(|window| window.index)
        .context("rmux output pipe window not found")?;
    Ok(PaneTarget::with_window(
        session_name,
        window_index,
        pane.index,
    ))
}

pub(crate) async fn pane_for_target(rmux: &Rmux, target: &RmuxPaneTarget) -> Result<Pane> {
    let session_name = target.session_name()?;
    if let Some(pane_id) = target.pane_id() {
        return Ok(rmux.pane_by_id(session_name, pane_id).await?);
    }
    Ok(rmux.session(session_name).await?.pane(0, 0))
}
