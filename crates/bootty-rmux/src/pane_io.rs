use std::{
    collections::VecDeque,
    future::Future,
    sync::{OnceLock, mpsc},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use rmux_proto::{PaneTarget, Request, Response};
use rmux_sdk::{
    Pane, PaneAttributes, PaneCell, PaneColor, PaneCursor, PaneId, PaneOutputChunk,
    PaneOutputStart, PaneSnapshot, Rmux, SessionName, TerminalSizeSpec,
};
use tokio::runtime::Builder;
use tokio::sync::{mpsc as tokio_mpsc, oneshot};

use crate::backend::{list_pane_rows, list_window_rows, rmux_request};
use crate::bridge::connect_bootty_rmux;

const RMUX_OUTPUT_POLL_MIN_DELAY: Duration = Duration::from_millis(1);
const RMUX_OUTPUT_POLL_MAX_DELAY: Duration = Duration::from_millis(16);
pub(crate) const RMUX_OUTPUT_CHANNEL_CAPACITY: usize = 64;
const RMUX_OUTPUT_EVENT_MAX_BYTES: usize = 16 * 1024;
const RMUX_RESTORE_RAW_CHUNK_BYTES: usize = RMUX_OUTPUT_EVENT_MAX_BYTES / 2;
const RMUX_OUTPUT_BACKLOG_MAX_BYTES: usize =
    RMUX_OUTPUT_CHANNEL_CAPACITY * RMUX_OUTPUT_EVENT_MAX_BYTES;
const RMUX_RESTORE_CAPTURE_TIMEOUT: Duration = Duration::from_millis(500);
const RMUX_KEYBOARD_PROTOCOL_SCAN_TAIL_BYTES: usize = 64;
const RMUX_KEYBOARD_PROTOCOL_OPTION: &str = "@bootty-keyboard-protocol";
const RMUX_BRACKETED_PASTE_OPTION: &str = "@bootty-bracketed-paste";
const RMUX_MOUSE_MODE_FORMAT: &str = "#{pane_id}\x1f#{mouse_all_flag}\x1f#{mouse_button_flag}\x1f#{mouse_standard_flag}\x1f#{mouse_utf8_flag}\x1f#{mouse_sgr_flag}";

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
    RestoreStart,
    RestoreChunk(Vec<u8>),
    RestoreEnd { has_capture: bool },
    Chunks(Vec<PaneOutputChunk>),
    KeyboardProtocol(Vec<u8>),
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
    max_scrollback: usize,
    output_tx: tokio_mpsc::Sender<RmuxPaneEvent>,
    input_rx: tokio_mpsc::UnboundedReceiver<Vec<u8>>,
    resize_rx: tokio_mpsc::UnboundedReceiver<TerminalSizeSpec>,
    result_tx: mpsc::Sender<std::result::Result<(), String>>,
}

struct RmuxPaneBridge {
    pane_tx: mpsc::Sender<RmuxOpenPaneRequest>,
}

fn pane_bridge() -> &'static RmuxPaneBridge {
    static BRIDGE: OnceLock<RmuxPaneBridge> = OnceLock::new();
    BRIDGE.get_or_init(RmuxPaneBridge::start)
}

impl RmuxPaneBridge {
    fn start() -> Self {
        let (pane_tx, pane_rx) = mpsc::channel();
        thread::spawn(move || run_pane_worker(pane_rx));
        Self { pane_tx }
    }
}

pub(crate) fn resize_rmux_pane(target: RmuxPaneTarget, size: TerminalSizeSpec) -> Result<()> {
    let io = open_rmux_pane_io(target, 0)?;
    io.resize_tx
        .send(size)
        .map_err(|_| anyhow::anyhow!("rmux pane resize worker stopped"))?;
    recv_pane_result(io.result_rx, "rmux pane resize worker")
}

pub(crate) fn open_rmux_pane_io(
    target: RmuxPaneTarget,
    max_scrollback: usize,
) -> Result<RmuxPaneIo> {
    // One slot carries at most 16 KiB. A full queue makes the producer await
    // the reader. The rmux stream then applies its own bounded lag policy.
    let (output_tx, output_rx) = tokio_mpsc::channel(RMUX_OUTPUT_CHANNEL_CAPACITY);
    let (input_tx, input_rx) = tokio_mpsc::unbounded_channel();
    let (resize_tx, resize_rx) = tokio_mpsc::unbounded_channel();
    let (result_tx, result_rx) = mpsc::channel();
    pane_bridge()
        .pane_tx
        .send(RmuxOpenPaneRequest {
            target,
            max_scrollback,
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

fn recv_pane_result<T>(
    result_rx: mpsc::Receiver<std::result::Result<T, String>>,
    worker_name: &str,
) -> Result<T> {
    result_rx
        .recv()
        .map_err(|_| anyhow::anyhow!("{worker_name} stopped"))?
        .map_err(anyhow::Error::msg)
}

fn run_pane_worker(request_rx: mpsc::Receiver<RmuxOpenPaneRequest>) {
    let runtime = Builder::new_multi_thread()
        .enable_all()
        .thread_name("bootty-rmux-pane")
        .worker_threads(2)
        .build()
        .expect("rmux pane runtime should initialize");
    while let Ok(request) = request_rx.recv() {
        runtime.spawn(run_pane_io(
            request.target,
            request.max_scrollback,
            request.output_tx,
            request.input_rx,
            request.resize_rx,
            request.result_tx,
        ));
    }
}

async fn run_pane_io(
    target: RmuxPaneTarget,
    max_scrollback: usize,
    output_tx: tokio_mpsc::Sender<RmuxPaneEvent>,
    mut input_rx: tokio_mpsc::UnboundedReceiver<Vec<u8>>,
    mut resize_rx: tokio_mpsc::UnboundedReceiver<TerminalSizeSpec>,
    result_tx: mpsc::Sender<std::result::Result<(), String>>,
) {
    let result = run_pane_io_inner(
        target,
        max_scrollback,
        &output_tx,
        &mut input_rx,
        &mut resize_rx,
        &result_tx,
    )
    .await;
    if let Err(error) = result {
        let text = error.to_string();
        let _ = result_tx.send(Err(text.clone()));
        let _ = output_tx.send(RmuxPaneEvent::Error(text)).await;
    }
}

async fn replay_retained_terminal_protocol(pane: &Pane, mouse_modes: &[u16]) -> Result<Vec<u8>> {
    let mut output_stream = pane
        .output_stream_starting_at(PaneOutputStart::Oldest)
        .await?;
    let mut tail = Vec::new();
    let mut keyboard_protocol = None;
    let mut bracketed_paste = None;
    loop {
        let chunks = output_stream.poll_once().await?;
        if chunks.is_empty() {
            break;
        }
        for chunk in chunks {
            let PaneOutputChunk::Bytes { bytes, .. } = chunk else {
                continue;
            };
            tail.extend_from_slice(&bytes);
            keyboard_protocol = kitty_keyboard_protocol_query(&tail).or(keyboard_protocol);
            bracketed_paste = bracketed_paste_mode(&tail).or(bracketed_paste);
            if tail.len() > RMUX_KEYBOARD_PROTOCOL_SCAN_TAIL_BYTES {
                let start = tail.len() - RMUX_KEYBOARD_PROTOCOL_SCAN_TAIL_BYTES;
                tail.drain(..start);
            }
        }
    }
    if let Some(enabled) = bracketed_paste {
        let _ = pane
            .set_option(RMUX_BRACKETED_PASTE_OPTION, if enabled { "1" } else { "0" })
            .await;
    }
    let flags = keyboard_protocol
        .as_deref()
        .and_then(kitty_keyboard_protocol_flags);
    let _ = pane
        .set_option(
            RMUX_KEYBOARD_PROTOCOL_OPTION,
            flags.as_deref().unwrap_or(""),
        )
        .await;
    let protocol = restored_terminal_protocol(
        flags.as_deref(),
        bracketed_paste.unwrap_or(false),
        mouse_modes,
    );
    Ok(protocol)
}

fn kitty_keyboard_protocol_flags(sequence: &[u8]) -> Option<String> {
    let flags = sequence.strip_prefix(b"\x1b[>")?;
    let end = flags.iter().position(|byte| *byte == b'u')?;
    flags[..end]
        .iter()
        .all(u8::is_ascii_digit)
        .then(|| String::from_utf8_lossy(&flags[..end]).into_owned())
}

pub(crate) fn restored_terminal_protocol(
    flags: Option<&str>,
    bracketed_paste: bool,
    mouse_modes: &[u16],
) -> Vec<u8> {
    let mut protocol = Vec::new();
    if let Some(flags) = flags {
        protocol.extend_from_slice(format!("\x1b[>{flags}u").as_bytes());
    }
    if bracketed_paste {
        protocol.extend_from_slice(b"\x1b[?2004h");
    }
    for mode in mouse_modes {
        protocol.extend_from_slice(format!("\x1b[?{mode}h").as_bytes());
    }
    protocol
}

async fn rmux_mouse_protocol_modes(target: &RmuxPaneTarget) -> Result<Vec<u16>> {
    let session = target.session_name()?;
    let response = rmux_request(Request::ListPanes(Box::new(rmux_proto::ListPanesRequest {
        target: session,
        target_window_index: None,
        format: Some(RMUX_MOUSE_MODE_FORMAT.to_owned()),
        filter: None,
        sort_order: None,
        reversed: false,
    })))
    .await?;
    let Response::ListPanes(response) = response else {
        anyhow::bail!("rmux returned an unexpected list-panes response");
    };
    let target_pane = target.pane_id().map(|id| id.to_string());
    for row in String::from_utf8_lossy(&response.output.stdout).lines() {
        let Some((pane_id, modes)) = parse_rmux_mouse_protocol_modes(row) else {
            continue;
        };
        if target_pane
            .as_deref()
            .is_some_and(|target_pane| target_pane != pane_id)
        {
            continue;
        }
        return Ok(modes);
    }
    anyhow::bail!("rmux pane mouse modes not found")
}

fn parse_rmux_mouse_protocol_modes(row: &str) -> Option<(&str, Vec<u16>)> {
    let mut fields = row.split('\x1f');
    let pane_id = fields.next()?;
    let mouse_all = fields.next()? == "1";
    let mouse_button = fields.next()? == "1";
    let mouse_standard = fields.next()? == "1";
    let mouse_utf8 = fields.next()? == "1";
    let mouse_sgr = fields.next()? == "1";
    let mut modes = Vec::new();
    if mouse_all {
        modes.push(1003);
    } else if mouse_button {
        modes.push(1002);
    } else if mouse_standard {
        modes.push(1000);
    }
    if mouse_utf8 {
        modes.push(1005);
    }
    if mouse_sgr {
        modes.push(1006);
    }
    Some((pane_id, modes))
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

fn bracketed_paste_mode(bytes: &[u8]) -> Option<bool> {
    let enabled = bytes
        .windows(8)
        .rposition(|window| window == b"\x1b[?2004h")
        .map(|index| (index, true));
    let disabled = bytes
        .windows(8)
        .rposition(|window| window == b"\x1b[?2004l")
        .map(|index| (index, false));
    enabled
        .into_iter()
        .chain(disabled)
        .max_by_key(|(index, _)| *index)
        .map(|(_, enabled)| enabled)
}

fn kitty_keyboard_protocol_query(bytes: &[u8]) -> Option<Vec<u8>> {
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
            return Some(bytes[start..query_end].to_vec());
        }
        search_start = flags_end + 1;
    }
    None
}

fn pane_output_chunk_bytes(chunk: &PaneOutputChunk) -> usize {
    match chunk {
        PaneOutputChunk::Bytes { bytes, .. } => bytes.len(),
        PaneOutputChunk::Lag(lag) => lag.recent.bytes.len(),
        _ => 0,
    }
}

fn event_bytes(event: &RmuxPaneEvent) -> usize {
    match event {
        RmuxPaneEvent::RestoreChunk(bytes) | RmuxPaneEvent::KeyboardProtocol(bytes) => bytes.len(),
        RmuxPaneEvent::Chunks(chunks) => chunks.iter().map(pane_output_chunk_bytes).sum(),
        RmuxPaneEvent::Error(error) => error.len(),
        RmuxPaneEvent::RestoreStart | RmuxPaneEvent::RestoreEnd { .. } => 0,
    }
}

fn queue_event(
    pending: &mut VecDeque<RmuxPaneEvent>,
    pending_bytes: &mut usize,
    event: RmuxPaneEvent,
) -> bool {
    let bytes = event_bytes(&event);
    if bytes > RMUX_OUTPUT_EVENT_MAX_BYTES
        || pending_bytes.saturating_add(bytes) > RMUX_OUTPUT_BACKLOG_MAX_BYTES
    {
        return false;
    }
    pending.push_back(event);
    *pending_bytes = pending_bytes.saturating_add(bytes);
    true
}

async fn send_next_event(
    output_tx: &tokio_mpsc::Sender<RmuxPaneEvent>,
    pending: &mut VecDeque<RmuxPaneEvent>,
    pending_bytes: &mut usize,
) -> bool {
    let permit = match output_tx.reserve().await {
        Ok(permit) => permit,
        Err(_) => return false,
    };
    let Some(event) = pending.pop_front() else {
        return true;
    };
    *pending_bytes = pending_bytes.saturating_sub(event_bytes(&event));
    permit.send(event);
    true
}

struct RestoreOutput {
    capture: Option<Vec<u8>>,
    capture_offset: usize,
    capture_previous: Option<u8>,
    buffered_chunks: VecDeque<PaneOutputChunk>,
    buffered_bytes: usize,
    started: bool,
    ended: bool,
    has_capture: bool,
}

impl RestoreOutput {
    fn new(
        capture: Option<Vec<u8>>,
        buffered_chunks: VecDeque<PaneOutputChunk>,
        buffered_bytes: usize,
    ) -> Self {
        let has_capture = capture.as_ref().is_some_and(|capture| !capture.is_empty());
        Self {
            capture,
            capture_offset: 0,
            capture_previous: None,
            buffered_chunks,
            buffered_bytes,
            started: false,
            ended: false,
            has_capture,
        }
    }

    fn is_complete(&self) -> bool {
        self.ended && self.buffered_chunks.is_empty()
    }

    fn enqueue(&mut self, pending: &mut VecDeque<RmuxPaneEvent>, pending_bytes: &mut usize) {
        if !self.started {
            if queue_event(pending, pending_bytes, RmuxPaneEvent::RestoreStart) {
                self.started = true;
            }
            return;
        }

        if let Some(capture) = self.capture.as_ref() {
            if self.capture_offset < capture.len() {
                let end = (self.capture_offset + RMUX_RESTORE_RAW_CHUNK_BYTES).min(capture.len());
                let mut previous = self.capture_previous;
                let bytes =
                    normalize_capture_chunk(&capture[self.capture_offset..end], &mut previous);
                if queue_event(pending, pending_bytes, RmuxPaneEvent::RestoreChunk(bytes)) {
                    self.capture_previous = previous;
                    self.capture_offset = end;
                }
                return;
            }
            self.capture = None;
        }

        if !self.ended {
            if queue_event(
                pending,
                pending_bytes,
                RmuxPaneEvent::RestoreEnd {
                    has_capture: self.has_capture,
                },
            ) {
                self.ended = true;
            }
            return;
        }

        let Some(chunk) = self.buffered_chunks.front_mut() else {
            return;
        };
        let (sequence, bytes) = match chunk {
            PaneOutputChunk::Bytes { sequence, bytes } => (*sequence, bytes),
            PaneOutputChunk::Lag(lag) if !lag.recent.bytes.is_empty() => {
                (lag.resume_sequence, &mut lag.recent.bytes)
            }
            _ => {
                self.buffered_chunks.pop_front();
                return;
            }
        };
        let length = bytes.len().min(RMUX_OUTPUT_EVENT_MAX_BYTES);
        if pending_bytes.saturating_add(length) > RMUX_OUTPUT_BACKLOG_MAX_BYTES {
            return;
        }
        let remainder = bytes.split_off(length);
        let chunk_bytes = std::mem::replace(bytes, remainder);
        let chunk_len = chunk_bytes.len();
        if !queue_event(
            pending,
            pending_bytes,
            RmuxPaneEvent::Chunks(vec![PaneOutputChunk::Bytes {
                sequence,
                bytes: chunk_bytes,
            }]),
        ) {
            return;
        }
        self.buffered_bytes = self.buffered_bytes.saturating_sub(chunk_len);
        if pane_output_chunk_bytes(self.buffered_chunks.front().unwrap()) == 0 {
            self.buffered_chunks.pop_front();
        }
    }
}

fn normalize_capture_chunk(bytes: &[u8], previous: &mut Option<u8>) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(bytes.len());
    for byte in bytes {
        if *byte == b'\n' && *previous != Some(b'\r') {
            normalized.push(b'\r');
        }
        normalized.push(*byte);
        *previous = Some(*byte);
    }
    normalized
}

fn queue_live_chunks(
    pending: &mut VecDeque<RmuxPaneEvent>,
    pending_bytes: &mut usize,
    chunks: &mut VecDeque<PaneOutputChunk>,
    deferred_bytes: &mut usize,
) {
    loop {
        let Some(chunk) = chunks.front_mut() else {
            return;
        };
        let (sequence, bytes) = match chunk {
            PaneOutputChunk::Bytes { sequence, bytes } => (*sequence, bytes),
            PaneOutputChunk::Lag(lag) if !lag.recent.bytes.is_empty() => {
                (lag.resume_sequence, &mut lag.recent.bytes)
            }
            _ => {
                chunks.pop_front();
                continue;
            }
        };
        let length = bytes.len().min(RMUX_OUTPUT_EVENT_MAX_BYTES);
        if pending_bytes.saturating_add(length) > RMUX_OUTPUT_BACKLOG_MAX_BYTES {
            return;
        }
        let remainder = bytes.split_off(length);
        let chunk_bytes = std::mem::replace(bytes, remainder);
        let chunk_len = chunk_bytes.len();
        if !queue_event(
            pending,
            pending_bytes,
            RmuxPaneEvent::Chunks(vec![PaneOutputChunk::Bytes {
                sequence,
                bytes: chunk_bytes,
            }]),
        ) {
            return;
        }
        *deferred_bytes = deferred_bytes.saturating_sub(chunk_len);
        if pane_output_chunk_bytes(chunks.front().unwrap()) == 0 {
            chunks.pop_front();
        }
    }
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

async fn run_pane_io_inner(
    target: RmuxPaneTarget,
    max_scrollback: usize,
    output_tx: &tokio_mpsc::Sender<RmuxPaneEvent>,
    input_rx: &mut tokio_mpsc::UnboundedReceiver<Vec<u8>>,
    resize_rx: &mut tokio_mpsc::UnboundedReceiver<TerminalSizeSpec>,
    result_tx: &mpsc::Sender<std::result::Result<(), String>>,
) -> Result<()> {
    target.session_name()?;
    let rmux = connect_bootty_rmux().await?;
    let pane = pane_for_target(&rmux, &target).await?;
    let mouse_modes = rmux_mouse_protocol_modes(&target).await?;
    let keyboard_protocol = pane
        .option(RMUX_KEYBOARD_PROTOCOL_OPTION)
        .await
        .ok()
        .flatten();
    let mut pending_output = VecDeque::new();
    let mut pending_output_bytes = 0;
    if let Some(flags) = &keyboard_protocol {
        let bracketed_paste = pane
            .option(RMUX_BRACKETED_PASTE_OPTION)
            .await
            .ok()
            .flatten()
            .as_deref()
            == Some("1");
        let protocol = restored_terminal_protocol(
            (!flags.is_empty()).then_some(flags.as_str()),
            bracketed_paste,
            &mouse_modes,
        );
        if !protocol.is_empty() {
            queue_event(
                &mut pending_output,
                &mut pending_output_bytes,
                RmuxPaneEvent::KeyboardProtocol(protocol),
            );
        }
    } else {
        let protocol = replay_retained_terminal_protocol(&pane, &mouse_modes).await?;
        if !protocol.is_empty() {
            queue_event(
                &mut pending_output,
                &mut pending_output_bytes,
                RmuxPaneEvent::KeyboardProtocol(protocol),
            );
        }
    }
    let input_target = pane_input_target(&rmux, &target).await?;
    let mut live_output = pane.output_stream().await?;
    let mut restore_rx = start_restore_capture(target.clone(), max_scrollback);
    let mut restore_result = None;
    let mut restore_received = false;
    let mut restore_output: Option<RestoreOutput> = None;
    let mut restore_complete = false;
    let mut deferred_live_chunks = VecDeque::new();
    let mut deferred_live_bytes = 0;
    let mut output_poll_delay = RMUX_OUTPUT_POLL_MIN_DELAY;
    let mut terminal_protocol_tail = Vec::new();

    loop {
        if let Some(restore) = restore_output.as_mut() {
            while pending_output_bytes < RMUX_OUTPUT_BACKLOG_MAX_BYTES && !restore.is_complete() {
                let before = pending_output.len();
                restore.enqueue(&mut pending_output, &mut pending_output_bytes);
                if pending_output.len() == before {
                    break;
                }
            }
            if restore.is_complete() {
                restore_output = None;
                restore_complete = true;
            }
        }
        if restore_received
            && restore_result.is_some()
            && restore_output.is_none()
            && !restore_complete
        {
            restore_output = restore_result.take().map(|capture| {
                RestoreOutput::new(
                    capture,
                    std::mem::take(&mut deferred_live_chunks),
                    std::mem::take(&mut deferred_live_bytes),
                )
            });
        }
        if restore_complete && !deferred_live_chunks.is_empty() {
            queue_live_chunks(
                &mut pending_output,
                &mut pending_output_bytes,
                &mut deferred_live_chunks,
                &mut deferred_live_bytes,
            );
        }
        let can_poll_live = pending_output_bytes < RMUX_OUTPUT_BACKLOG_MAX_BYTES
            && if restore_complete {
                restore_output.is_none() && deferred_live_chunks.is_empty()
            } else {
                deferred_live_bytes < RMUX_OUTPUT_BACKLOG_MAX_BYTES
            };

        tokio::select! {
            sent = send_next_event(output_tx, &mut pending_output, &mut pending_output_bytes), if !pending_output.is_empty() => {
                if !sent {
                    break;
                }
            }
            restore = &mut restore_rx, if !restore_received => {
                restore_received = true;
                restore_result = Some(restore.ok().flatten());
            }
            _ = tokio::time::sleep(output_poll_delay), if can_poll_live => {
                let chunks = live_output.poll_once().await?;
                if chunks.is_empty() {
                    output_poll_delay = (output_poll_delay * 2).min(RMUX_OUTPUT_POLL_MAX_DELAY);
                } else {
                    output_poll_delay = RMUX_OUTPUT_POLL_MIN_DELAY;
                    for chunk in &chunks {
                        if let PaneOutputChunk::Bytes { bytes, .. } = chunk {
                            terminal_protocol_tail.extend_from_slice(bytes);
                            if let Some(sequence) =
                                kitty_keyboard_protocol_query(&terminal_protocol_tail)
                                && let Some(flags) = kitty_keyboard_protocol_flags(&sequence)
                            {
                                let _ = pane
                                    .set_option(RMUX_KEYBOARD_PROTOCOL_OPTION, flags)
                                    .await;
                            }
                            if let Some(enabled) = bracketed_paste_mode(&terminal_protocol_tail) {
                                let _ = pane
                                    .set_option(
                                        RMUX_BRACKETED_PASTE_OPTION,
                                        if enabled { "1" } else { "0" },
                                    )
                                    .await;
                            }
                            if terminal_protocol_tail.len()
                                > RMUX_KEYBOARD_PROTOCOL_SCAN_TAIL_BYTES
                            {
                                let start = terminal_protocol_tail.len()
                                    - RMUX_KEYBOARD_PROTOCOL_SCAN_TAIL_BYTES;
                                terminal_protocol_tail.drain(..start);
                            }
                        }
                    }
                    deferred_live_bytes = deferred_live_bytes.saturating_add(
                        chunks.iter().map(pane_output_chunk_bytes).sum::<usize>(),
                    );
                    deferred_live_chunks.extend(chunks);
                }
            }
            Some(mut bytes) = input_rx.recv() => {
                while let Ok(next) = input_rx.try_recv() {
                    bytes.extend_from_slice(&next);
                }
                let result = send_rmux_pane_input(&pane, &input_target, &bytes)
                    .await
                    .map_err(|error| error.to_string());
                let ok = result.is_ok();
                let _ = result_tx.send(result);
                if ok {
                    output_poll_delay = RMUX_OUTPUT_POLL_MIN_DELAY;
                }
            }
            Some(mut size) = resize_rx.recv() => {
                while let Ok(next) = resize_rx.try_recv() {
                    size = next;
                }
                let result = pane.resize(size).await.map_err(|error| error.to_string());
                let ok = result.is_ok();
                let _ = result_tx.send(result);
                if ok {
                    output_poll_delay = RMUX_OUTPUT_POLL_MIN_DELAY;
                }
            }
            else => break,
        }
    }
    Ok(())
}

fn start_restore_capture(
    target: RmuxPaneTarget,
    max_scrollback: usize,
) -> oneshot::Receiver<Option<Vec<u8>>> {
    let (result_tx, result_rx) = oneshot::channel();
    tokio::spawn(async move {
        let bytes = complete_restore_capture(RMUX_RESTORE_CAPTURE_TIMEOUT, async {
            let rmux = connect_bootty_rmux().await.ok()?;
            let pane = pane_for_target(&rmux, &target).await.ok()?;
            restore_capture(&pane, max_scrollback).await.ok()
        })
        .await;
        let _ = result_tx.send(bytes);
    });
    result_rx
}

async fn restore_capture(pane: &Pane, max_scrollback: usize) -> Result<Vec<u8>> {
    let restore_lines = max_scrollback.min(i64::MAX as usize) as i64;
    let capture = pane
        .capture_pane()
        .start(-restore_lines)
        .escape_ansi(true)
        .preserve_trailing_spaces(true)
        .await?;
    let mut stdout = capture.stdout;
    if let Ok(snapshot) = pane.snapshot().await {
        append_restore_snapshot(&mut stdout, &snapshot);
    }
    Ok(stdout)
}

fn append_restore_snapshot(bytes: &mut Vec<u8>, snapshot: &PaneSnapshot) {
    append_restore_snapshot_visible(bytes, snapshot);
}

fn append_restore_snapshot_visible(bytes: &mut Vec<u8>, snapshot: &PaneSnapshot) {
    bytes.extend_from_slice(b"\x1b[?25l\x1b[H\x1b[J");
    for row in 0..snapshot.rows {
        let Some(cells) = snapshot.row_cells(row) else {
            continue;
        };
        let terminal_row = row.saturating_add(1);
        for (col, cell) in cells.iter().enumerate() {
            if cell.is_padding() || !restore_cell_needs_render(cell) {
                continue;
            }
            let terminal_col = (col as u16).saturating_add(1);
            bytes.extend_from_slice(format!("\x1b[{terminal_row};{terminal_col}H").as_bytes());
            append_restore_cell_sgr(bytes, cell);
            bytes.extend_from_slice(cell.text().as_bytes());
        }
    }
    bytes.extend_from_slice(b"\x1b[0m");
    append_restore_cursor_position(bytes, snapshot.cursor);
}

fn restore_cell_needs_render(cell: &PaneCell) -> bool {
    cell.text() != " "
        || !cell.attributes.is_empty()
        || !matches!(cell.foreground, PaneColor::Default | PaneColor::Terminal)
        || !matches!(cell.background, PaneColor::Default | PaneColor::Terminal)
        || !matches!(cell.underline, PaneColor::Default | PaneColor::Terminal)
}

fn append_restore_cell_sgr(bytes: &mut Vec<u8>, cell: &PaneCell) {
    let mut params = vec!["0".to_owned()];
    append_restore_attribute_sgr(&mut params, cell.attributes);
    append_restore_color_sgr(&mut params, cell.foreground, 30, 90, 38, 39);
    append_restore_color_sgr(&mut params, cell.background, 40, 100, 48, 49);
    append_restore_underline_color_sgr(&mut params, cell.underline);
    bytes.extend_from_slice(b"\x1b[");
    bytes.extend_from_slice(params.join(";").as_bytes());
    bytes.push(b'm');
}

fn append_restore_attribute_sgr(params: &mut Vec<String>, attributes: PaneAttributes) {
    if attributes.contains(PaneAttributes::BOLD) {
        params.push("1".to_owned());
    }
    if attributes.contains(PaneAttributes::DIM) {
        params.push("2".to_owned());
    }
    if attributes.contains(PaneAttributes::ITALIC) {
        params.push("3".to_owned());
    }
    if attributes.contains(PaneAttributes::UNDERLINE) {
        params.push("4".to_owned());
    } else if attributes.contains(PaneAttributes::DOUBLE_UNDERLINE) {
        params.push("21".to_owned());
    } else if attributes.contains(PaneAttributes::CURLY_UNDERLINE) {
        params.push("4:3".to_owned());
    } else if attributes.contains(PaneAttributes::DOTTED_UNDERLINE) {
        params.push("4:4".to_owned());
    } else if attributes.contains(PaneAttributes::DASHED_UNDERLINE) {
        params.push("4:5".to_owned());
    }
    if attributes.contains(PaneAttributes::BLINK) {
        params.push("5".to_owned());
    }
    if attributes.contains(PaneAttributes::REVERSE) {
        params.push("7".to_owned());
    }
    if attributes.contains(PaneAttributes::HIDDEN) {
        params.push("8".to_owned());
    }
    if attributes.contains(PaneAttributes::STRIKETHROUGH) {
        params.push("9".to_owned());
    }
    if attributes.contains(PaneAttributes::OVERLINE) {
        params.push("53".to_owned());
    }
}

fn append_restore_color_sgr(
    params: &mut Vec<String>,
    color: PaneColor,
    ansi_base: u8,
    bright_base: u8,
    extended_prefix: u8,
    default_code: u8,
) {
    match color {
        PaneColor::Default | PaneColor::Terminal => {}
        PaneColor::None => params.push(default_code.to_string()),
        PaneColor::Ansi { index } => params.push((ansi_base + index.min(7)).to_string()),
        PaneColor::BrightAnsi { index } => params.push((bright_base + index.min(7)).to_string()),
        PaneColor::Indexed { index } => params.push(format!("{extended_prefix};5;{index}")),
        PaneColor::Rgb { red, green, blue } => {
            params.push(format!("{extended_prefix};2;{red};{green};{blue}"));
        }
        PaneColor::Encoded { value } => append_restore_color_sgr(
            params,
            PaneColor::from_encoded(value),
            ansi_base,
            bright_base,
            extended_prefix,
            default_code,
        ),
        _ => {}
    }
}

fn append_restore_underline_color_sgr(params: &mut Vec<String>, color: PaneColor) {
    match color {
        PaneColor::Default | PaneColor::Terminal => {}
        PaneColor::None => params.push("59".to_owned()),
        PaneColor::Ansi { index } => params.push(format!("58;5;{}", index.min(7))),
        PaneColor::BrightAnsi { index } => params.push(format!("58;5;{}", index.min(7) + 8)),
        PaneColor::Indexed { index } => params.push(format!("58;5;{index}")),
        PaneColor::Rgb { red, green, blue } => params.push(format!("58;2;{red};{green};{blue}")),
        PaneColor::Encoded { value } => {
            append_restore_underline_color_sgr(params, PaneColor::from_encoded(value));
        }
        _ => {}
    }
}

fn append_restore_cursor_position(bytes: &mut Vec<u8>, cursor: PaneCursor) {
    let row = cursor.row.saturating_add(1);
    let col = cursor.col.saturating_add(1);
    bytes.extend_from_slice(format!("\x1b[{row};{col}H").as_bytes());
    if cursor.visible {
        bytes.extend_from_slice(b"\x1b[?25h");
    } else {
        bytes.extend_from_slice(b"\x1b[?25l");
    }
}

async fn complete_restore_capture<F>(timeout: Duration, capture: F) -> Option<Vec<u8>>
where
    F: Future<Output = Option<Vec<u8>>>,
{
    tokio::time::timeout(timeout, capture).await.ok().flatten()
}

pub(crate) async fn pane_for_target(rmux: &Rmux, target: &RmuxPaneTarget) -> Result<Pane> {
    let session_name = target.session_name()?;
    if let Some(pane_id) = target.pane_id() {
        return Ok(rmux.pane_by_id(session_name, pane_id).await?);
    }
    Ok(rmux.session(session_name).await?.pane(0, 0))
}
