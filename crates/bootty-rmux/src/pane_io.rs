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
use crate::bridge::{connect_bootty_rmux, rmux_missing_target_text, rmux_stale_target_text};

pub(crate) const RMUX_OUTPUT_CHANNEL_CAPACITY: usize = 64;
const RMUX_OUTPUT_EVENT_MAX_BYTES: usize = 16 * 1024;
// Kitty keyboard sequences are a handful of bytes; anything longer is some
// other CSI the scanner can skip instead of buffering.
const RMUX_KEYBOARD_PROTOCOL_MAX_SEQUENCE_BYTES: usize = 64;
const RMUX_KEYBOARD_PROTOCOL_OPTION: &str = "@bootty-keyboard-protocol";
// `list_pane_rows` enumerates the session, so a pane it does not name is gone
// rather than momentarily unresolvable. Worded distinctly from rmux's own
// wording so the classifiers match these on purpose, not by substring.
pub(crate) const RMUX_PANE_NOT_LISTED: &str = "rmux pane is absent from its session";
pub(crate) const RMUX_PANE_WINDOW_NOT_LISTED: &str = "rmux pane window is absent from its session";

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

enum RmuxPaneRequest {
    Open(RmuxOpenPaneRequest),
    /// A one-shot resize. It needs the pane handle and the worker runtime, but
    /// none of the recovery stream an open pays for.
    Resize {
        target: RmuxPaneTarget,
        size: TerminalSizeSpec,
        result_tx: mpsc::Sender<std::result::Result<(), String>>,
    },
}

struct RmuxOpenPaneRequest {
    target: RmuxPaneTarget,
    output_tx: tokio_mpsc::Sender<RmuxPaneEvent>,
    input_rx: tokio_mpsc::UnboundedReceiver<Vec<u8>>,
    resize_rx: tokio_mpsc::UnboundedReceiver<TerminalSizeSpec>,
    result_tx: mpsc::Sender<std::result::Result<(), String>>,
}

fn pane_sender() -> &'static mpsc::Sender<RmuxPaneRequest> {
    static PANE_TX: OnceLock<mpsc::Sender<RmuxPaneRequest>> = OnceLock::new();
    PANE_TX.get_or_init(|| {
        let (pane_tx, pane_rx) = mpsc::channel();
        thread::spawn(move || run_pane_worker(pane_rx));
        pane_tx
    })
}

pub(crate) fn resize_rmux_pane(target: RmuxPaneTarget, size: TerminalSizeSpec) -> Result<()> {
    let (result_tx, result_rx) = mpsc::channel();
    pane_sender()
        .send(RmuxPaneRequest::Resize {
            target,
            size,
            result_tx,
        })
        .map_err(|_| anyhow::anyhow!("rmux pane worker stopped"))?;
    result_rx
        .recv()
        .map_err(|_| anyhow::anyhow!("rmux pane resize worker stopped"))?
        .map_err(anyhow::Error::msg)
}

async fn run_pane_resize(
    target: RmuxPaneTarget,
    size: TerminalSizeSpec,
    result_tx: mpsc::Sender<std::result::Result<(), String>>,
) {
    let result = async {
        let rmux = connect_bootty_rmux().await?;
        let pane = pane_for_target(&rmux, &target).await?;
        resize_rmux_pane_with_retry(&rmux, &pane, &target, size).await
    }
    .await;
    finish_pane_operation(&result_tx, result);
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
        .send(RmuxPaneRequest::Open(RmuxOpenPaneRequest {
            target,
            output_tx,
            input_rx,
            resize_rx,
            result_tx,
        }))
        .map_err(|_| anyhow::anyhow!("rmux pane worker stopped"))?;
    Ok(RmuxPaneIo {
        output_rx,
        input_tx,
        resize_tx,
        result_rx,
    })
}

fn run_pane_worker(request_rx: mpsc::Receiver<RmuxPaneRequest>) {
    let runtime = Builder::new_multi_thread()
        .enable_all()
        .thread_name("bootty-rmux-pane")
        .worker_threads(2)
        .build()
        .expect("rmux pane runtime should initialize");
    while let Ok(request) = request_rx.recv() {
        match request {
            RmuxPaneRequest::Open(request) => {
                runtime.spawn(run_pane_io(request));
            }
            RmuxPaneRequest::Resize {
                target,
                size,
                result_tx,
            } => {
                runtime.spawn(run_pane_resize(target, size, result_tx));
            }
        }
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
    // Deliberate limit: a stored empty value skips the transcript replay, so
    // flags a pane negotiated with no worker attached are not restored and it
    // falls back to legacy encoding, which programs still accept. The reverse —
    // replaying flags a pane has since dropped — would encode keys a plain shell
    // cannot read at all, so any other stored value pays for the scan. Drop both
    // branches once the SDK reports the flags with the rest of the keyframe.
    let stored_keyboard_flags = pane
        .option(RMUX_KEYBOARD_PROTOCOL_OPTION)
        .await
        .ok()
        .flatten();
    let mut keyboard_protocol = if stored_keyboard_flags.as_deref() == Some("") {
        KittyKeyboardProtocol::default()
    } else {
        let mut protocol = retained_kitty_keyboard_protocol(&pane).await?;
        if !protocol.observed {
            // The retained transcript is a bounded ring. A negotiation that has
            // scrolled out of it is not evidence the flags were cleared, so
            // keep what the pane last stored.
            protocol.restore(stored_keyboard_flags.as_deref().unwrap_or_default());
        }
        let _ = pane
            .set_option(RMUX_KEYBOARD_PROTOCOL_OPTION, &protocol.flags_text())
            .await;
        protocol
    };
    // Programs push and pop these flags around every prompt, so the daemon
    // round-trip must not sit in the byte path. The writer coalesces bursts and
    // keeps the writes ordered; it stops when this worker drops the sender.
    let (keyboard_option_tx, mut keyboard_option_rx) =
        tokio::sync::watch::channel(keyboard_protocol.flags_text());
    let keyboard_option_writer = tokio::spawn({
        let pane = pane.clone();
        async move {
            while keyboard_option_rx.changed().await.is_ok() {
                let flags = keyboard_option_rx.borrow_and_update().clone();
                let _ = pane
                    .set_option(RMUX_KEYBOARD_PROTOCOL_OPTION, &flags)
                    .await;
            }
        }
    });

    let mut recovery = pane.recover_output().await?;
    let mut pending_events = VecDeque::new();
    let mut recovery_ended = false;
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
                        append_kitty_keyboard_protocol(&mut rebase.keyframe, &keyboard_protocol);
                        pending_events.push_back(RmuxPaneEvent::Rebase(rebase.keyframe));
                    }
                    Some(PaneRecoveryEvent::Bytes { bytes, .. }) => {
                        if let Some(flags) = keyboard_protocol.observe(&bytes) {
                            let _ = keyboard_option_tx.send(flags);
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
                if finish_pane_operation(
                    &result_tx,
                    send_rmux_pane_input(&rmux, &pane, &target, &bytes).await,
                ) {
                    // Drain what is already queued before the worker leaves.
                    recovery_ended = true;
                }
            }
            Some(mut size) = resize_rx.recv() => {
                while let Ok(next) = resize_rx.try_recv() {
                    size = next;
                }
                if finish_pane_operation(
                    &result_tx,
                    resize_rmux_pane_with_retry(&rmux, &pane, &target, size).await,
                ) {
                    recovery_ended = true;
                }
            }
            else => break,
        }
    }
    // The next open reads this option, and reopening can follow the worker's
    // exit immediately. Let the writer finish, then make the final state
    // durable before this worker goes away.
    drop(keyboard_option_tx);
    let _ = keyboard_option_writer.await;
    let _ = pane
        .set_option(RMUX_KEYBOARD_PROTOCOL_OPTION, &keyboard_protocol.flags_text())
        .await;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        if pane_gone_error(&error) {
            let _ = result_tx.send(Ok(()));
        } else {
            let text = error.to_string();
            let _ = result_tx.send(Err(text.clone()));
            let _ = output_tx.send(RmuxPaneEvent::Error(text)).await;
        }
    }
}

fn finish_pane_operation(
    result_tx: &mpsc::Sender<std::result::Result<(), String>>,
    result: Result<()>,
) -> bool {
    match result {
        Ok(()) => {
            let _ = result_tx.send(Ok(()));
            false
        }
        Err(error) if pane_gone_error(&error) => {
            // A close can win after the UI queued input or resize. Complete the
            // in-flight request so synchronous callers do not hang, then let the
            // worker disappear without turning normal teardown into a toast.
            let _ = result_tx.send(Ok(()));
            true
        }
        Err(error) => {
            let _ = result_tx.send(Err(error.to_string()));
            false
        }
    }
}

// rmux 0.10's recovery keyframe restores every tracked terminal mode except
// Kitty's exact enhancement flags. Keep this cold-path transcript scan until
// the SDK exposes the flags and push/pop stack losslessly.
async fn retained_kitty_keyboard_protocol(pane: &Pane) -> Result<KittyKeyboardProtocol> {
    let mut output_stream = pane
        .output_stream_starting_at(PaneOutputStart::Oldest)
        .await?;
    let mut protocol = KittyKeyboardProtocol::default();
    loop {
        let chunks = output_stream.poll_once().await?;
        if chunks.is_empty() {
            break;
        }
        for chunk in chunks {
            let PaneOutputChunk::Bytes { bytes, .. } = chunk else {
                continue;
            };
            protocol.observe(&bytes);
        }
    }
    Ok(protocol)
}

/// Kitty keyboard protocol state reconstructed from a pane transcript.
///
/// The daemon replays every other terminal mode, so Bootty only tracks the
/// enhancement flags and their push/pop stack.
#[derive(Default)]
struct KittyKeyboardProtocol {
    pending: Vec<u8>,
    stack: Vec<u32>,
    current: u32,
    /// Whether the scanned bytes carried any kitty negotiation at all. A
    /// transcript that never mentions the protocol cannot contradict what the
    /// pane already stored.
    observed: bool,
}

/// Kitty caps its own stack and drops the oldest entry past the limit.
const KITTY_KEYBOARD_STACK_LIMIT: usize = 16;

enum KittyKeyboardUpdate {
    /// `CSI > flags u` pushes the current flags and installs new ones.
    Push(u32),
    /// `CSI = flags ; mode u` replaces, sets, or clears bits in place.
    Set { flags: u32, mode: u32 },
    /// `CSI < number u` pops that many entries.
    Pop(u32),
}

enum CsiScan {
    Complete(usize),
    Incomplete,
    Invalid,
}

impl KittyKeyboardProtocol {
    /// Reads back what [`Self::flags_text`] stored.
    fn restore(&mut self, flags: &str) {
        let mut entries = flags
            .split(';')
            .filter(|entry| !entry.is_empty())
            .filter_map(|entry| entry.parse().ok())
            .collect::<Vec<u32>>();
        self.current = entries.pop().unwrap_or_default();
        self.stack = entries;
    }

    /// The push stack under the active flags, so a pane sitting on a stack it
    /// has popped back to zero is not mistaken for one that never negotiated.
    fn flags_text(&self) -> String {
        if self.stack.is_empty() && self.current == 0 {
            return String::new();
        }
        self.stack
            .iter()
            .copied()
            .chain([self.current])
            .map(|flags| flags.to_string())
            .collect::<Vec<_>>()
            .join(";")
    }

    /// Feeds transcript bytes through the scanner, returning the new flags text
    /// whenever a sequence changed them.
    fn observe(&mut self, bytes: &[u8]) -> Option<String> {
        self.pending.extend_from_slice(bytes);
        // A push onto flags that already match leaves `current` alone but still
        // deepens the stack, and the stored state has to carry that.
        let mut changed = false;
        let mut consumed = 0;
        loop {
            let Some(offset) = self.pending[consumed..]
                .iter()
                .position(|byte| *byte == b'\x1b')
            else {
                consumed = self.pending.len();
                break;
            };
            let start = consumed + offset;
            // A split write can leave the introducer astride two chunks.
            if self.pending.len() == start + 1 {
                consumed = start;
                break;
            }
            if self.pending[start + 1] != b'[' {
                consumed = start + 1;
                continue;
            }
            match scan_csi(&self.pending[start..]) {
                CsiScan::Complete(len) => {
                    if let Some(update) = parse_kitty_keyboard(&self.pending[start..start + len]) {
                        self.apply(update);
                        changed = true;
                    }
                    consumed = start + len;
                }
                CsiScan::Incomplete => {
                    consumed = start;
                    break;
                }
                CsiScan::Invalid => consumed = start + 1,
            }
        }
        self.pending.drain(..consumed);
        changed.then(|| self.flags_text())
    }

    fn apply(&mut self, update: KittyKeyboardUpdate) {
        match update {
            KittyKeyboardUpdate::Push(flags) => {
                if self.stack.len() >= KITTY_KEYBOARD_STACK_LIMIT {
                    self.stack.remove(0);
                }
                self.stack.push(self.current);
                self.current = flags;
            }
            KittyKeyboardUpdate::Set { flags, mode } => match mode {
                2 => self.current |= flags,
                3 => self.current &= !flags,
                _ => self.current = flags,
            },
            KittyKeyboardUpdate::Pop(count) => {
                // A pane can ask to pop past the bottom of the stack. One pop
                // beyond it already clears the flags, so the rest are no-ops.
                let depth = (count as usize).min(self.stack.len().saturating_add(1));
                for _ in 0..depth {
                    self.current = self.stack.pop().unwrap_or_default();
                }
            }
        }
        self.observed = true;
    }
}

/// Measures the CSI sequence starting at `bytes`, which begins with `ESC [`.
fn scan_csi(bytes: &[u8]) -> CsiScan {
    for (offset, byte) in bytes.iter().enumerate().skip(2) {
        if offset >= RMUX_KEYBOARD_PROTOCOL_MAX_SEQUENCE_BYTES {
            return CsiScan::Invalid;
        }
        match byte {
            // Parameter and intermediate bytes.
            0x20..=0x3f => continue,
            // Final byte.
            0x40..=0x7e => return CsiScan::Complete(offset + 1),
            _ => return CsiScan::Invalid,
        }
    }
    CsiScan::Incomplete
}

fn parse_kitty_keyboard(sequence: &[u8]) -> Option<KittyKeyboardUpdate> {
    let body = sequence.strip_prefix(b"\x1b[")?.strip_suffix(b"u")?;
    let (kind, parameters) = body.split_first()?;
    let mut parameters = parameters.split(|byte| *byte == b';');
    let first = parse_kitty_parameter(parameters.next()?)?;
    let second = match parameters.next() {
        Some(parameter) => Some(parse_kitty_parameter(parameter)?),
        None => None,
    };
    if parameters.next().is_some() {
        return None;
    }
    match kind {
        b'>' if second.is_none() => Some(KittyKeyboardUpdate::Push(first)),
        b'=' => Some(KittyKeyboardUpdate::Set {
            flags: first,
            mode: second.unwrap_or(1),
        }),
        // `CSI < u` pops a single entry.
        b'<' if second.is_none() => Some(KittyKeyboardUpdate::Pop(first.max(1))),
        _ => None,
    }
}

fn parse_kitty_parameter(parameter: &[u8]) -> Option<u32> {
    if parameter.is_empty() {
        return Some(0);
    }
    std::str::from_utf8(parameter).ok()?.parse().ok()
}

/// Replays the pane's push stack under its active flags. Restoring only the
/// active flags would leave the emulator one level deep, so the next pop the
/// program makes would clear the flags instead of uncovering what it pushed.
fn append_kitty_keyboard_protocol(keyframe: &mut Vec<u8>, protocol: &KittyKeyboardProtocol) {
    if protocol.stack.is_empty() && protocol.current == 0 {
        return;
    }
    for flags in protocol.stack.iter().copied().chain([protocol.current]) {
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

async fn send_rmux_pane_input(
    rmux: &Rmux,
    pane: &Pane,
    target: &RmuxPaneTarget,
    bytes: &[u8],
) -> Result<()> {
    // PaneInput resolves the stable pane id on the daemon side. Keep all valid
    // terminal byte sequences on that path, including Backspace, Escape, and
    // control-key sequences; only arbitrary non-UTF-8 bytes need the legacy
    // slot-based send-keys request, which addresses the pane by index.
    if let Ok(text) = std::str::from_utf8(bytes) {
        return pane.send_text(text).await.map_err(Into::into);
    }
    let input_target = pane_input_target(rmux, target).await?;
    match send_rmux_pane_input_once(&input_target, bytes).await {
        Ok(()) => Ok(()),
        Err(error) if retryable_pane_target_error(&error) => {
            let input_target = pane_input_target(rmux, target).await?;
            send_rmux_pane_input_once(&input_target, bytes).await
        }
        Err(error) => Err(error),
    }
}

async fn send_rmux_pane_input_once(target: &PaneTarget, bytes: &[u8]) -> Result<()> {
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

async fn resize_rmux_pane_with_retry(
    rmux: &Rmux,
    pane: &Pane,
    target: &RmuxPaneTarget,
    size: TerminalSizeSpec,
) -> Result<()> {
    match pane.resize(size).await {
        Ok(()) => Ok(()),
        Err(error) if retryable_pane_resize_error(&error) => pane_for_target(rmux, target)
            .await?
            .resize(size)
            .await
            .map_err(Into::into),
        Err(error) => Err(error.into()),
    }
}

fn retryable_pane_resize_error(error: &rmux_sdk::RmuxError) -> bool {
    rmux_stale_target_text(&error.to_string())
}

fn retryable_pane_target_error(error: &anyhow::Error) -> bool {
    rmux_stale_target_text(&error.to_string())
}

/// Deliberately narrower than `rmux_stale_target_text`: this decides that a
/// pane is dead, and a stale index says nothing about the process behind it.
fn pane_gone_error(error: &anyhow::Error) -> bool {
    rmux_missing_target_text(&error.to_string())
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
        .context(RMUX_PANE_NOT_LISTED)?;
    let window_index = list_window_rows(rmux, &session_name)
        .await?
        .into_iter()
        .find(|window| window.id == pane.window_id)
        .map(|window| window.index)
        .context(RMUX_PANE_WINDOW_NOT_LISTED)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The active flags after the chunks, ignoring the push stack under them.
    fn flags_after(chunks: &[&[u8]]) -> u32 {
        let mut protocol = KittyKeyboardProtocol::default();
        for chunk in chunks {
            protocol.observe(chunk);
        }
        protocol.current
    }

    #[test]
    fn push_is_seen_behind_an_unrelated_csi() {
        assert_eq!(flags_after(&[b"\x1b[?1049h\x1b[>1u"]), 1);
    }

    #[test]
    fn numbered_pop_restores_the_pushed_flags() {
        assert_eq!(flags_after(&[b"\x1b[>1u\x1b[>15u\x1b[<1u"]), 1);
        assert_eq!(flags_after(&[b"\x1b[>1u\x1b[<2u"]), 0);
    }

    #[test]
    fn mode_qualified_set_updates_bits_in_place() {
        assert_eq!(flags_after(&[b"\x1b[>1u\x1b[=6;2u"]), 7);
        assert_eq!(flags_after(&[b"\x1b[>7u\x1b[=2;3u"]), 5);
        assert_eq!(flags_after(&[b"\x1b[>7u\x1b[=2;1u"]), 2);
    }

    #[test]
    fn sequences_split_across_chunks_are_reassembled() {
        assert_eq!(flags_after(&[b"\x1b", b"[>", b"1", b"u"]), 1);
    }

    #[test]
    fn queries_and_responses_do_not_change_the_flags() {
        assert_eq!(flags_after(&[b"\x1b[>1u\x1b[?u\x1b[?1u"]), 1);
    }

    #[test]
    fn an_oversized_pop_clears_the_flags_without_spinning() {
        assert_eq!(flags_after(&[b"\x1b[>1u\x1b[<4294967295u"]), 0);
    }

    #[test]
    fn a_keyframe_restores_the_whole_push_stack() {
        let mut protocol = KittyKeyboardProtocol::default();
        protocol.observe(b"\x1b[>1u\x1b[>15u");
        let mut keyframe = Vec::new();
        append_kitty_keyboard_protocol(&mut keyframe, &protocol);
        // Without the stack the next `CSI < 1 u` would clear the flags instead
        // of uncovering the 1 the shell pushed.
        assert_eq!(keyframe, b"\x1b[>0u\x1b[>1u\x1b[>15u");

        let mut restored = KittyKeyboardProtocol::default();
        restored.observe(&keyframe);
        restored.observe(b"\x1b[<1u");
        assert_eq!(restored.current, 1);
    }

    #[test]
    fn a_keyframe_stays_empty_for_a_pane_without_flags() {
        let mut keyframe = Vec::new();
        append_kitty_keyboard_protocol(&mut keyframe, &KittyKeyboardProtocol::default());
        assert!(keyframe.is_empty());
    }

    #[test]
    fn a_transcript_without_kitty_sequences_stays_unobserved() {
        let mut protocol = KittyKeyboardProtocol::default();
        protocol.observe(b"plain output\x1b[0m\x1b[?1049h");
        // Nothing in the transcript mentions the protocol, so the stored flags
        // survive rather than being cleared by a scan that aged past them.
        assert!(!protocol.observed);
        protocol.restore("1");
        assert_eq!(protocol.flags_text(), "1");

        let mut negotiated = KittyKeyboardProtocol::default();
        negotiated.observe(b"\x1b[>1u\x1b[<1u");
        assert!(negotiated.observed);
        assert_eq!(negotiated.flags_text(), "");
    }

    #[test]
    fn the_stored_state_round_trips_through_the_option() {
        let mut protocol = KittyKeyboardProtocol::default();
        protocol.observe(b"\x1b[>1u\x1b[>15u\x1b[=0u");
        // Active flags are 0, but the pane is still sitting on a push stack, so
        // the stored value must not read as "never negotiated".
        assert_eq!(protocol.flags_text(), "0;1;0");

        let mut restored = KittyKeyboardProtocol::default();
        restored.restore(&protocol.flags_text());
        assert_eq!(restored.stack, protocol.stack);
        assert_eq!(restored.current, protocol.current);
    }

    #[test]
    fn a_push_onto_matching_flags_still_reports_the_deeper_stack() {
        let mut protocol = KittyKeyboardProtocol::default();
        protocol.observe(b"\x1b[>1u");
        // The active flags do not move, but the pop that follows now uncovers
        // 1 rather than 0, so the stored state has to record the push.
        assert_eq!(protocol.observe(b"\x1b[>1u").as_deref(), Some("0;1;1"));
    }

    #[test]
    fn the_stack_stays_bounded() {
        let mut protocol = KittyKeyboardProtocol::default();
        for _ in 0..KITTY_KEYBOARD_STACK_LIMIT * 4 {
            protocol.observe(b"\x1b[>1u");
        }
        assert_eq!(protocol.stack.len(), KITTY_KEYBOARD_STACK_LIMIT);
    }

    #[test]
    fn pending_stays_bounded_without_a_kitty_sequence() {
        let mut protocol = KittyKeyboardProtocol::default();
        for _ in 0..64 {
            protocol.observe(&vec![b'x'; 1024]);
        }
        assert!(protocol.pending.len() <= RMUX_KEYBOARD_PROTOCOL_MAX_SEQUENCE_BYTES);
    }
}
