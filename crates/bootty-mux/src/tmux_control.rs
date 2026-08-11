//! A persistent tmux control-mode client, so the session poll stops spawning a process.
//!
//! Reading the session list is the one tmux call bootty makes on a timer, and every call used to
//! fork a `tmux` client that connected, printed two lists and exited. Control mode keeps one client
//! attached and answers the same queries over its pipe. Anything that changes tmux state still runs
//! as its own process: mutations happen when someone acts, not several times a second.

use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};

use super::{
    backend::{
        MuxEvent, MuxEventCapability, MuxEventCursor, MuxEventDraft, MuxEventPayload,
        MuxEventProvenance, MuxEventQueue, MuxEventTarget, MuxEventTopic, MuxForegroundState,
        MuxOccupantIdentity, MuxRebaseReason, MuxTopologyChange,
    },
    controller::MuxScope,
    process::{CommandOutput, CommandRunner, SystemCommandRunner},
    ssh::SshRemote,
    tmux_protocol::{TmuxControlNotification, TmuxControlParser, TmuxParseError},
};

/// tmux commands that only read state, and so can be answered by a client shared with every other
/// reader. Everything else keeps its own process, where its exit status and stderr stand alone.
const CONTROL_QUERIES: [&str; 2] = ["list-sessions", "list-panes"];
/// How long a query waits for its reply. A client that misses this is treated as wedged rather than
/// waited on again: the caller forks instead, and the next poll starts a fresh client.
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);
/// How long queries keep forking after a client fails to start or dies. A tmux without control mode,
/// or without a running server, should not be asked for a new client on every poll.
const RESTART_BACKOFF: Duration = Duration::from_secs(10);
/// Handshake answer proving commands reach tmux and replies come back, before any real query trusts
/// the client.
const READY_TOKEN: &str = "bootty-control-ready";

/// An all-pane inventory is taken immediately after the control handshake, then refreshed from
/// bootty's normal chained snapshot replies. Its cwd and current-command fields describe
/// foreground state; only the pane PID can confirm a lifecycle change.
const PANE_INVENTORY_QUERY: &str = "list-panes -a -F 'p\x1f#{session_id}\x1f#{window_id}\x1f#{pane_id}\x1f#{pane_tty}\x1f#{pane_pid}\x1f#{pane_current_path}\x1f#{pane_current_command}'";

/// Runs read-only tmux queries through a shared control-mode client, and everything else as its own
/// process. Falls back to a process whenever the client is unavailable, so tmux versions without
/// control mode behave exactly as they did before.
#[derive(Clone)]
pub struct TmuxControlRunner {
    clients: Arc<Mutex<HashMap<String, ClientSlot>>>,
    events: MuxEventQueue,
    /// Set when the tmux server lives on another host. The control client is then a long-lived SSH
    /// process, which is the one place a remote snapshot poll can be as cheap as a local one.
    remote: Option<SshRemote>,
}

impl Default for TmuxControlRunner {
    fn default() -> Self {
        Self {
            clients: Arc::default(),
            events: MuxEventQueue::for_backend("tmux:local"),
            remote: None,
        }
    }
}

impl TmuxControlRunner {
    pub fn for_remote(remote: SshRemote) -> Self {
        let identity = format!("tmux:{}", remote.transport_identity());
        Self {
            clients: Arc::default(),
            events: MuxEventQueue::for_backend(identity),
            remote: Some(remote),
        }
    }
}

impl std::fmt::Debug for TmuxControlRunner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TmuxControlRunner")
            .finish_non_exhaustive()
    }
}

impl CommandRunner for TmuxControlRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        match self.control_query(program, args) {
            Some(output) => Ok(output),
            None => {
                let (program, spawned_args) = self.spawned(program, args);
                let output = SystemCommandRunner.run(&program, &spawned_args)?;
                self.record_spawned_inventory_fallback(args, &output);
                Ok(output)
            }
        }
    }

    fn run_disowned(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        let (program, args) = self.spawned(program, args);
        SystemCommandRunner.run_disowned(&program, &args)
    }

    fn mux_event_capabilities(&self) -> Vec<MuxEventCapability> {
        MuxEventTopic::ALL
            .into_iter()
            .map(|topic| match topic {
                MuxEventTopic::TopologyChanged
                | MuxEventTopic::BackendLagged
                | MuxEventTopic::SnapshotRebased => MuxEventCapability::available(topic),
                MuxEventTopic::TerminalOutput => MuxEventCapability::best_effort(
                    topic,
                    "tmux routes control-mode output through this attached client rather than guaranteeing every server pane; known output is authoritative and attributed from the all-pane inventory",
                ),
                MuxEventTopic::PaneCwdChanged
                | MuxEventTopic::PaneForegroundChanged
                | MuxEventTopic::PaneOccupantReplaced
                | MuxEventTopic::PaneClosed => MuxEventCapability::best_effort(
                    topic,
                    "tmux exposes this through authoritative all-pane inventory snapshots, not a dedicated control notification",
                ),
                MuxEventTopic::PaneStateChanged
                | MuxEventTopic::PaneTitleChanged
                | MuxEventTopic::PaneOptionsChanged
                | MuxEventTopic::BackendDisconnected => MuxEventCapability::unsupported(
                    topic,
                    "tmux control-mode transport degradation does not authoritatively prove the tmux backend is disconnected",
                ),
            })
            .collect()
    }

    fn start_mux_event_stream(&self, program: &str) {
        let _ = self.ensure_control_client(program);
    }

    fn drain_mux_events(&self, scope: MuxScope, maximum: usize) -> Vec<MuxEvent> {
        self.events
            .drain_with_initial_rebase(scope, maximum, MuxEventProvenance::TmuxControl)
    }
}

struct Reply {
    body: String,
    error: bool,
    acknowledge: SyncSender<()>,
}

struct TmuxControlClient {
    child: Child,
    stdin: ChildStdin,
    replies: Receiver<Reply>,
    mapper: Arc<Mutex<TmuxEventMapper>>,
    events: MuxEventQueue,
}

impl TmuxControlClient {
    #[cfg(test)]
    /// `prefix_args` go before `-C`, which is where tmux wants `-L`/`-S`. Only tests pass any.
    fn start_with(program: &str, prefix_args: &[&str], remote: Option<&SshRemote>) -> Result<Self> {
        Self::start_with_events(program, prefix_args, remote, MuxEventQueue::default())
    }

    fn start_with_events(
        program: &str,
        prefix_args: &[&str],
        remote: Option<&SshRemote>,
        events: MuxEventQueue,
    ) -> Result<Self> {
        // No `-t`: the most recently used session is as good as any, since the client is only ever
        // asked about the server as a whole. `ignore-size` keeps a client with no terminal from
        // having an opinion about window size. Do not set `no-output`: `%output` is the
        // authoritative control-mode notification this backend drains into the bounded event queue.
        let tmux_args = prefix_args
            .iter()
            .copied()
            .chain(["-C", "attach-session", "-f", "ignore-size"])
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let (program, args) = spawn_argv(program, &tmux_args, remote);
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("tmux control client has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("tmux control client has no stdout"))?;
        let (replies_tx, replies) = channel();
        let mapper = Arc::new(Mutex::new(TmuxEventMapper::default()));
        let reader_mapper = Arc::clone(&mapper);
        let reader_events = events.clone();
        std::thread::spawn(move || read_replies(stdout, replies_tx, reader_events, reader_mapper));

        let mut client = Self {
            child,
            stdin,
            replies,
            mapper,
            events,
        };
        // Attaching answers with a block of its own; the handshake below is what the first real
        // query's reply must line up behind.
        let attached = client.take_reply()?;
        let _ = attached.acknowledge.send(());
        let ready = client.query(&format!("display-message -p {READY_TOKEN}"), 1, None)?;
        if ready.trim() != READY_TOKEN {
            return Err(anyhow!("tmux control client answered {ready:?}"));
        }
        client.query(PANE_INVENTORY_QUERY, 1, Some(0))?;
        Ok(client)
    }

    fn take_reply(&self) -> Result<Reply> {
        self.replies
            .recv_timeout(QUERY_TIMEOUT)
            .map_err(|error| anyhow!("tmux control client stopped answering: {error}"))
    }

    /// Submit one command line and join the bodies of the `blocks` replies it produces. tmux answers
    /// commands in order, one block each, and the caller holds the client's lock for the whole
    /// exchange, so replies cannot be read out of turn.
    ///
    /// The reader waits for each acknowledgement before it maps later notifications. Installing an
    /// authoritative all-pane block before that acknowledgement makes the snapshot a true ordering
    /// barrier for output and close notifications.
    fn query(
        &mut self,
        line: &str,
        blocks: usize,
        authoritative_pane_inventory_block: Option<usize>,
    ) -> Result<String> {
        writeln!(self.stdin, "{line}")?;
        self.stdin.flush()?;
        let mut body = String::new();
        for block in 0..blocks {
            let reply = self.take_reply()?;
            let Reply {
                body: reply_body,
                error,
                acknowledge,
            } = reply;
            if error {
                let _ = acknowledge.send(());
                return Err(anyhow!(
                    "tmux control client rejected {line:?}: {reply_body}",
                ));
            }
            if authoritative_pane_inventory_block == Some(block) {
                self.record_snapshot(&reply_body, true);
            }
            if !reply_body.is_empty() {
                if !body.is_empty() {
                    body.push('\n');
                }
                body.push_str(&reply_body);
            }
            acknowledge
                .send(())
                .map_err(|_| anyhow!("tmux control reply reader stopped waiting"))?;
        }
        Ok(body)
    }

    fn record_snapshot(&self, snapshot: &str, authoritative_pane_inventory: bool) {
        if !authoritative_pane_inventory {
            return;
        }
        self.mapper
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_snapshot(snapshot, &self.events);
    }
}

impl Drop for TmuxControlClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_replies(
    stdout: ChildStdout,
    replies: Sender<Reply>,
    events: MuxEventQueue,
    mapper: Arc<Mutex<TmuxEventMapper>>,
) {
    let mut parser = TmuxControlParser::default();
    let mut reader = BufReader::new(stdout);
    let mut bytes = [0; 4096];
    loop {
        match reader.read(&mut bytes) {
            Ok(0) => {
                mapper
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .control_degraded(&events, MuxRebaseReason::Reconnect);
                return;
            }
            Err(_) => {
                mapper
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .control_degraded(&events, MuxRebaseReason::Reconnect);
                return;
            }
            Ok(read) => {
                for &byte in &bytes[..read] {
                    let notification = match next_control_notification(&mut parser, byte) {
                        Ok(Some(notification)) => notification,
                        Ok(None) => continue,
                        Err(reason) => {
                            mapper
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .control_degraded(&events, reason);
                            return;
                        }
                    };
                    match notification {
                        TmuxControlNotification::BlockEnd(body) => {
                            if !send_reply(&replies, body, false) {
                                return;
                            }
                        }
                        TmuxControlNotification::BlockError(body) => {
                            if !send_reply(&replies, body, true) {
                                return;
                            }
                        }
                        notification => mapper
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .publish(notification, &events),
                    }
                }
            }
        }
    }
}

fn next_control_notification(
    parser: &mut TmuxControlParser,
    byte: u8,
) -> std::result::Result<Option<TmuxControlNotification>, MuxRebaseReason> {
    match parser.put(byte) {
        Err(TmuxParseError::UnknownNotification) => Ok(None),
        Err(_) => Err(MuxRebaseReason::SequenceGap),
        Ok(notification) => Ok(notification),
    }
}

fn send_reply(replies: &Sender<Reply>, body: String, error: bool) -> bool {
    let (acknowledge, acknowledged) = sync_channel(0);
    replies
        .send(Reply {
            body,
            error,
            acknowledge,
        })
        .is_ok()
        && acknowledged.recv().is_ok()
}

#[derive(Default)]
struct TmuxEventMapper {
    output_sequences: HashMap<usize, u64>,
    pane_lifecycle_epochs: HashMap<usize, u64>,
    pane_targets: HashMap<usize, MuxEventTarget>,
    pane_placements: HashMap<usize, Vec<MuxEventTarget>>,
    pane_cwds: HashMap<usize, Option<String>>,
    pane_foregrounds: HashMap<usize, Option<MuxForegroundState>>,
    unknown_output_targets: HashSet<usize>,
    control_degraded: bool,
}

#[derive(Clone, Copy)]
struct TmuxPaneObservation<'a> {
    target: &'a MuxEventTarget,
    cwd: Option<&'a str>,
    foreground: Option<&'a MuxForegroundState>,
}

impl TmuxEventMapper {
    fn publish(&mut self, notification: TmuxControlNotification, events: &MuxEventQueue) {
        match notification {
            TmuxControlNotification::Output(output) => {
                // A binding-level fallback would misattribute these bytes to an arbitrary pane.
                // Drop them with an explicit rebase until inventory supplies the exact target.
                let Some(targets) = self.pane_placements.get(&output.pane_id).cloned() else {
                    if self.unknown_output_targets.insert(output.pane_id) {
                        self.topology(events, None);
                        events.publish(MuxEventDraft::rebase(
                            MuxEventProvenance::TmuxControl,
                            MuxRebaseReason::SequenceGap,
                        ));
                    }
                    return;
                };
                let epoch = self
                    .pane_lifecycle_epochs
                    .get(&output.pane_id)
                    .copied()
                    .unwrap_or_default();
                let sequence = {
                    let sequence = self
                        .output_sequences
                        .entry(output.pane_id)
                        .and_modify(|sequence| *sequence = sequence.saturating_add(1))
                        .or_insert(1);
                    *sequence
                };
                let bytes = decode_tmux_control_output(&output.data);
                for target in targets {
                    events.publish(MuxEventDraft::new(
                        MuxEventTopic::TerminalOutput,
                        MuxEventProvenance::TmuxControl,
                        Some(target),
                        Some(MuxEventCursor::new(
                            format!("tmux-output:{}:lifecycle:{epoch}", output.pane_id),
                            sequence,
                        )),
                        MuxEventPayload::Output {
                            bytes: bytes.clone(),
                        },
                    ));
                }
            }
            TmuxControlNotification::SessionChanged(session) => {
                self.topology(
                    events,
                    Some(MuxEventTarget::session(format!("${}", session.id))),
                );
            }
            TmuxControlNotification::SessionsChanged => self.topology(events, None),
            TmuxControlNotification::SessionRenamed(session) => {
                self.topology(
                    events,
                    Some(MuxEventTarget::session(format!("${}", session.id))),
                );
            }
            TmuxControlNotification::SessionWindowChanged(change) => {
                self.topology(
                    events,
                    Some(MuxEventTarget {
                        session_id: Some(format!("${}", change.session_id)),
                        window_id: Some(format!("@{}", change.window_id)),
                        ..Default::default()
                    }),
                );
            }
            TmuxControlNotification::LayoutChange(_)
            | TmuxControlNotification::WindowAdd { .. }
            | TmuxControlNotification::UnlinkedWindowAdd { .. }
            | TmuxControlNotification::WindowClose { .. }
            | TmuxControlNotification::UnlinkedWindowClose { .. }
            | TmuxControlNotification::WindowRenamed(_)
            | TmuxControlNotification::UnlinkedWindowRenamed(_)
            | TmuxControlNotification::WindowPaneChanged(_) => {
                self.topology(events, None);
            }
            TmuxControlNotification::PaneModeChanged { .. } => {
                self.topology(events, None);
            }
            TmuxControlNotification::ClientSessionChanged(change) => {
                self.topology(
                    events,
                    Some(MuxEventTarget::session(format!("${}", change.session_id))),
                );
            }
            TmuxControlNotification::ClientDetached { .. } => self.topology(events, None),
            TmuxControlNotification::Exit => {
                self.control_degraded(events, MuxRebaseReason::Reconnect);
            }
            TmuxControlNotification::BlockEnd(_) | TmuxControlNotification::BlockError(_) => {}
        }
    }

    fn record_snapshot(&mut self, snapshot: &str, events: &MuxEventQueue) {
        let mut refreshed: HashMap<usize, MuxEventTarget> = HashMap::new();
        let mut refreshed_placements: HashMap<usize, Vec<MuxEventTarget>> = HashMap::new();
        let mut refreshed_cwds = HashMap::new();
        let mut refreshed_foregrounds = HashMap::new();
        let mut malformed_pane_row = false;
        for line in snapshot.lines() {
            let Some(line) = line.strip_prefix("p\x1f") else {
                continue;
            };
            let fields = line.split('\x1f').collect::<Vec<_>>();
            let parsed = match fields.as_slice() {
                [session, window, pane, terminal, pid, cwd, process] => pane_target(
                    session,
                    window,
                    pane,
                    (Some(terminal), Some(pid)),
                    cwd,
                    process,
                    &self.pane_lifecycle_epochs,
                ),
                [session, window, pane, pid, cwd, process] => pane_target(
                    session,
                    window,
                    pane,
                    (None, Some(pid)),
                    cwd,
                    process,
                    &self.pane_lifecycle_epochs,
                ),
                fields if fields.len() >= 13 => pane_target(
                    fields[0],
                    fields[1],
                    fields[6],
                    (Some(fields[7]), Some(fields[8])),
                    fields[11],
                    fields[12],
                    &self.pane_lifecycle_epochs,
                ),
                fields if fields.len() >= 12 => pane_target(
                    fields[0],
                    fields[1],
                    fields[6],
                    (None, Some(fields[7])),
                    fields[10],
                    fields[11],
                    &self.pane_lifecycle_epochs,
                ),
                fields if fields.len() >= 11 => pane_target(
                    fields[0],
                    fields[1],
                    fields[6],
                    (None, None),
                    fields[9],
                    fields[10],
                    &self.pane_lifecycle_epochs,
                ),
                _ => None,
            };
            let Some((pane_id, target, cwd)) = parsed else {
                malformed_pane_row = true;
                continue;
            };
            refreshed_placements
                .entry(pane_id)
                .or_default()
                .push(target.clone());
            let replace = refreshed.get(&pane_id).is_none_or(|existing| {
                (
                    target.session_id.as_deref(),
                    target.window_id.as_deref(),
                    target.pane_id.as_deref(),
                ) < (
                    existing.session_id.as_deref(),
                    existing.window_id.as_deref(),
                    existing.pane_id.as_deref(),
                )
            });
            if replace {
                refreshed.insert(pane_id, target);
                refreshed_cwds.insert(pane_id, cwd);
            }
        }
        if malformed_pane_row {
            return;
        }
        for (pane_id, target) in &mut refreshed {
            if target.terminal_id.is_none()
                && let Some(previous) = self.pane_targets.get(pane_id)
            {
                target.terminal_id.clone_from(&previous.terminal_id);
            }
            let previous_pid = self
                .pane_targets
                .get(pane_id)
                .and_then(|previous| previous.occupant.as_ref())
                .and_then(|occupant| occupant.pid);
            let target_pid = target.occupant.as_ref().and_then(|occupant| occupant.pid);
            if confirmed_tmux_occupant_change(previous_pid, target_pid) {
                let epoch = self.advance_pane_lifecycle_epoch(*pane_id);
                self.output_sequences.remove(pane_id);
                set_tmux_target_lifecycle_epoch(target, epoch);
            } else if let (Some(previous), Some(occupant)) = (
                self.pane_targets
                    .get(pane_id)
                    .and_then(|target| target.occupant.as_ref()),
                target.occupant.as_mut(),
            ) {
                occupant
                    .backend_identity
                    .clone_from(&previous.backend_identity);
                if occupant.pid.is_none() {
                    occupant.pid = previous.pid;
                }
            }
            if let Some(placements) = refreshed_placements.get_mut(pane_id) {
                for placement in placements {
                    placement.terminal_id.clone_from(&target.terminal_id);
                    placement.occupant.clone_from(&target.occupant);
                }
            }
        }
        for (pane_id, target) in &refreshed {
            refreshed_foregrounds.insert(
                *pane_id,
                tmux_foreground_state(
                    target,
                    refreshed_cwds.get(pane_id).and_then(|cwd| cwd.as_deref()),
                ),
            );
        }

        let previous_placements = std::mem::take(&mut self.pane_placements);
        let topology_changed = previous_placements.len() != refreshed_placements.len()
            || refreshed_placements.iter().any(|(pane_id, current)| {
                previous_placements
                    .get(pane_id)
                    .is_none_or(|previous| previous.len() != current.len())
            });
        let previous_cwds = std::mem::take(&mut self.pane_cwds);
        let previous_foregrounds = std::mem::take(&mut self.pane_foregrounds);
        for (pane_id, previous_targets) in &previous_placements {
            let Some(current_targets) = refreshed_placements.get(pane_id) else {
                continue;
            };
            for target in previous_targets {
                if current_targets
                    .iter()
                    .any(|current| same_tmux_placement(target, current))
                {
                    continue;
                }
                events.publish(MuxEventDraft::new(
                    MuxEventTopic::PaneClosed,
                    MuxEventProvenance::TmuxSnapshotFallback,
                    Some(tmux_placement_close_target(target)),
                    None,
                    MuxEventPayload::Closed {
                        reason: "pane placement absent from authoritative tmux inventory"
                            .to_owned(),
                    },
                ));
            }
        }
        for (pane_id, placements) in &refreshed_placements {
            for target in placements {
                let previous_target = previous_placements.get(pane_id).and_then(|targets| {
                    targets
                        .iter()
                        .find(|previous| same_tmux_placement(previous, target))
                        .or_else(|| {
                            (targets.len() == placements.len())
                                .then(|| targets.first())
                                .flatten()
                        })
                });
                self.publish_target_delta(
                    events,
                    previous_target.map(|target| TmuxPaneObservation {
                        target,
                        cwd: previous_cwds.get(pane_id).and_then(|cwd| cwd.as_deref()),
                        foreground: previous_foregrounds
                            .get(pane_id)
                            .and_then(|foreground| foreground.as_ref()),
                    }),
                    TmuxPaneObservation {
                        target,
                        cwd: refreshed_cwds.get(pane_id).and_then(|cwd| cwd.as_deref()),
                        foreground: refreshed_foregrounds
                            .get(pane_id)
                            .and_then(|foreground| foreground.as_ref()),
                    },
                );
            }
        }
        for (pane_id, targets) in previous_placements {
            if refreshed.contains_key(&pane_id) {
                continue;
            }
            self.output_sequences.remove(&pane_id);
            self.pane_lifecycle_epochs.remove(&pane_id);
            self.unknown_output_targets.remove(&pane_id);
            for target in targets {
                events.publish(MuxEventDraft::new(
                    MuxEventTopic::PaneClosed,
                    MuxEventProvenance::TmuxSnapshotFallback,
                    Some(target),
                    None,
                    MuxEventPayload::Closed {
                        reason: "pane absent from authoritative tmux inventory".to_owned(),
                    },
                ));
            }
        }
        self.unknown_output_targets.clear();
        self.pane_targets = refreshed;
        self.pane_placements = refreshed_placements;
        self.pane_cwds = refreshed_cwds;
        self.pane_foregrounds = refreshed_foregrounds;
        if topology_changed {
            self.topology(events, None);
        }
    }

    fn publish_target_delta(
        &self,
        events: &MuxEventQueue,
        previous: Option<TmuxPaneObservation<'_>>,
        current: TmuxPaneObservation<'_>,
    ) {
        let TmuxPaneObservation {
            target,
            cwd: new_cwd,
            foreground: new_foreground,
        } = current;
        let (old_cwd, old_foreground) = previous
            .map(|previous| (previous.cwd, previous.foreground))
            .unwrap_or_default();
        if previous.is_some_and(|previous| !same_tmux_occupant(previous.target, target)) {
            events.publish(MuxEventDraft::new(
                MuxEventTopic::PaneOccupantReplaced,
                MuxEventProvenance::TmuxSnapshotFallback,
                Some(target.clone()),
                None,
                MuxEventPayload::OccupantReplaced {
                    old_occupant: previous.and_then(|previous| previous.target.occupant.clone()),
                    new_occupant: target.occupant.clone(),
                },
            ));
        }
        if previous.is_some_and(|previous| {
            previous.target.session_id != target.session_id
                || previous.target.window_id != target.window_id
                || previous.target.pane_id != target.pane_id
                || previous.target.terminal_id != target.terminal_id
        }) {
            self.topology(events, Some(target.clone()));
        }
        if previous.is_none() || old_cwd != new_cwd {
            events.publish(MuxEventDraft::new(
                MuxEventTopic::PaneCwdChanged,
                MuxEventProvenance::TmuxSnapshotFallback,
                Some(target.clone()),
                None,
                MuxEventPayload::Cwd {
                    old_cwd: old_cwd.map(str::to_owned),
                    new_cwd: new_cwd.map(str::to_owned),
                },
            ));
        }
        if (previous.is_none() && new_foreground.is_some()) || old_foreground != new_foreground {
            events.publish(MuxEventDraft::new(
                MuxEventTopic::PaneForegroundChanged,
                MuxEventProvenance::TmuxSnapshotFallback,
                Some(target.clone()),
                None,
                MuxEventPayload::Foreground {
                    old_state: old_foreground.cloned(),
                    new_state: new_foreground.cloned(),
                },
            ));
        }
    }

    fn advance_pane_lifecycle_epoch(&mut self, pane_id: usize) -> u64 {
        let epoch = self.pane_lifecycle_epochs.entry(pane_id).or_insert(0);
        *epoch = epoch.saturating_add(1);
        *epoch
    }

    fn topology(&self, events: &MuxEventQueue, target: Option<MuxEventTarget>) {
        events.publish(MuxEventDraft::new(
            MuxEventTopic::TopologyChanged,
            MuxEventProvenance::TmuxControl,
            target,
            None,
            MuxEventPayload::Topology {
                change: MuxTopologyChange::Invalidated,
            },
        ));
    }

    fn control_degraded(&mut self, events: &MuxEventQueue, reason: MuxRebaseReason) {
        if self.control_degraded {
            return;
        }
        self.control_degraded = true;
        self.topology(events, None);
        events.publish(MuxEventDraft::rebase(
            MuxEventProvenance::TmuxControl,
            reason,
        ));
    }
}

fn pane_target(
    session_id: &str,
    window_id: &str,
    pane_id: &str,
    identity: (Option<&str>, Option<&str>),
    cwd: &str,
    process: &str,
    lifecycle_epochs: &HashMap<usize, u64>,
) -> Option<(usize, MuxEventTarget, Option<String>)> {
    let (terminal_id, pid) = identity;
    let pane_number = pane_id.strip_prefix('%').unwrap_or(pane_id).parse().ok()?;
    let pid = pid.and_then(|value| value.parse::<u32>().ok());
    let cwd = (!cwd.is_empty()).then(|| cwd.to_owned());
    let process = (!process.is_empty()).then(|| process.to_owned());
    let lifecycle_epoch = lifecycle_epochs
        .get(&pane_number)
        .copied()
        .unwrap_or_default();
    let occupant = tmux_occupant_identity(pane_id, pid, process.as_deref(), lifecycle_epoch);
    Some((
        pane_number,
        MuxEventTarget {
            session_id: Some(session_id.to_owned()),
            window_id: Some(window_id.to_owned()),
            pane_id: Some(pane_id.to_owned()),
            terminal_id: terminal_id
                .filter(|terminal_id| !terminal_id.is_empty())
                .map(str::to_owned),
            occupant: Some(occupant),
        },
        cwd,
    ))
}

fn set_tmux_target_lifecycle_epoch(target: &mut MuxEventTarget, lifecycle_epoch: u64) {
    let Some(pane_id) = target.pane_id.as_deref() else {
        return;
    };
    let (pid, process) = target.occupant.as_ref().map_or((None, None), |occupant| {
        (occupant.pid, occupant.process.clone())
    });
    target.occupant = Some(tmux_occupant_identity(
        pane_id,
        pid,
        process.as_deref(),
        lifecycle_epoch,
    ));
}

fn same_tmux_placement(left: &MuxEventTarget, right: &MuxEventTarget) -> bool {
    left.session_id == right.session_id
        && left.window_id == right.window_id
        && left.pane_id == right.pane_id
}
/// Retire one linked placement without retiring the pane's shared watcher identity.
fn tmux_placement_close_target(target: &MuxEventTarget) -> MuxEventTarget {
    MuxEventTarget {
        session_id: target.session_id.clone(),
        window_id: target.window_id.clone(),
        pane_id: target.pane_id.clone(),
        ..MuxEventTarget::default()
    }
}

fn same_tmux_occupant(left: &MuxEventTarget, right: &MuxEventTarget) -> bool {
    left.occupant
        .as_ref()
        .map(|occupant| occupant.backend_identity.as_str())
        == right
            .occupant
            .as_ref()
            .map(|occupant| occupant.backend_identity.as_str())
}

/// tmux only proves a process replacement when two inventories report different pane PIDs. A
/// missing PID is incomplete inventory, not lifecycle evidence.
fn confirmed_tmux_occupant_change(previous_pid: Option<u32>, target_pid: Option<u32>) -> bool {
    matches!(
        (previous_pid, target_pid),
        (Some(previous_pid), Some(target_pid)) if previous_pid != target_pid
    )
}

fn tmux_foreground_state(target: &MuxEventTarget, cwd: Option<&str>) -> Option<MuxForegroundState> {
    let occupant = target.occupant.as_ref()?;
    let foreground = MuxForegroundState {
        pid: occupant.pid,
        command: occupant.process.clone(),
        cwd: cwd.map(str::to_owned),
        executable: None,
    };
    if foreground.pid.is_none() && foreground.command.is_none() && foreground.cwd.is_none() {
        None
    } else {
        Some(foreground)
    }
}

/// tmux pane IDs are server-global. Window and session placement plus foreground metadata do not
/// identify an occupant and must not retire its terminal stream.
fn tmux_occupant_identity(
    pane_id: &str,
    pid: Option<u32>,
    process: Option<&str>,
    lifecycle_epoch: u64,
) -> MuxOccupantIdentity {
    let backend_identity = match (pid, lifecycle_epoch) {
        (Some(pid), 0) => format!("tmux:{pane_id}:pid={pid}"),
        (Some(pid), lifecycle_epoch) => {
            format!("tmux:{pane_id}:lifecycle={lifecycle_epoch}:pid={pid}")
        }
        (None, lifecycle_epoch) => format!("tmux:{pane_id}:lifecycle={lifecycle_epoch}"),
    };
    MuxOccupantIdentity {
        backend_identity,
        pid,
        process: process.map(str::to_owned),
    }
}

fn decode_tmux_control_output(value: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut index = 0;
    while index < value.len() {
        if value[index] == b'\\'
            && value
                .get(index + 1..index + 4)
                .is_some_and(|digits| digits.iter().all(|digit| (b'0'..=b'7').contains(digit)))
        {
            let digits = &value[index + 1..index + 4];
            decoded
                .push(((digits[0] - b'0') << 6) | ((digits[1] - b'0') << 3) | (digits[2] - b'0'));
            index += 4;
        } else if value[index] == b'\\' && value.get(index + 1) == Some(&b'\\') {
            decoded.push(b'\\');
            index += 2;
        } else {
            decoded.push(value[index]);
            index += 1;
        }
    }
    decoded
}

#[derive(Default)]
struct ClientSlot {
    client: Option<TmuxControlClient>,
    retry_after: Option<Instant>,
}

impl TmuxControlRunner {
    /// argv for running `program args...` as its own process: an SSH invocation for a remote
    /// server, and the command itself for a local one.
    fn spawned(&self, program: &str, args: &[String]) -> (String, Vec<String>) {
        spawn_argv(program, args, self.remote.as_ref())
    }

    /// Answer `args` from this backend's control client, or `None` to let the caller run its own
    /// process. Cloned backends share the client; dropping the last clone drops the registry and
    /// tears every attached client down.
    fn control_query(&self, program: &str, args: &[String]) -> Option<CommandOutput> {
        let line = control_command_line(args)?;
        let pane_inventory_block = authoritative_pane_inventory_block(args);
        if !self.ensure_control_client(program) {
            return None;
        }
        let blocks = expected_blocks(args);
        let mut clients = self.clients.lock().ok()?;
        let slot = clients.get_mut(&self.client_key(program))?;
        match slot
            .client
            .as_mut()?
            .query(&line, blocks, pane_inventory_block)
        {
            Ok(stdout) => Some(CommandOutput {
                success: true,
                stdout,
                stderr: String::new(),
            }),
            // A client that timed out or errored cannot be trusted to still be in step with its
            // replies, so it goes rather than risk answering the next query with this one's output.
            Err(_) => {
                slot.client = None;
                slot.retry_after = Some(Instant::now() + RESTART_BACKOFF);
                None
            }
        }
    }

    fn ensure_control_client(&self, program: &str) -> bool {
        let mut clients = match self.clients.lock() {
            Ok(clients) => clients,
            Err(_) => return false,
        };
        let slot = clients.entry(self.client_key(program)).or_default();
        if slot.client.is_some() {
            return true;
        }
        if slot.retry_after.is_some_and(|at| Instant::now() < at) {
            return false;
        }
        match TmuxControlClient::start_with_events(
            program,
            &[],
            self.remote.as_ref(),
            self.events.clone(),
        ) {
            Ok(client) => {
                slot.client = Some(client);
                slot.retry_after = None;
                true
            }
            Err(_) => {
                slot.retry_after = Some(Instant::now() + RESTART_BACKOFF);
                false
            }
        }
    }
    fn record_spawned_inventory_fallback(&self, args: &[String], output: &CommandOutput) {
        if output.success && authoritative_pane_inventory_block(args).is_some() {
            Self::publish_snapshot_fallback(
                &self.events,
                "successful spawned authoritative pane inventory".to_owned(),
            );
        }
    }

    fn publish_snapshot_fallback(events: &MuxEventQueue, reason: String) {
        let _ = reason;
        events.publish(MuxEventDraft::new(
            MuxEventTopic::TopologyChanged,
            MuxEventProvenance::TmuxSnapshotFallback,
            None,
            None,
            MuxEventPayload::Topology {
                change: MuxTopologyChange::Invalidated,
            },
        ));
        events.publish(MuxEventDraft::rebase(
            MuxEventProvenance::TmuxSnapshotFallback,
            MuxRebaseReason::Reconnect,
        ));
    }
}

impl TmuxControlRunner {
    /// A client answers for one tmux server, so the host it runs on is part of its identity.
    fn client_key(&self, program: &str) -> String {
        match &self.remote {
            Some(remote) => format!("{}@{program}", remote.destination()),
            None => program.to_owned(),
        }
    }
}

fn spawn_argv(program: &str, args: &[String], remote: Option<&SshRemote>) -> (String, Vec<String>) {
    match remote {
        Some(remote) => remote.command(program, args),
        None => (program.to_owned(), args.to_vec()),
    }
}

/// How many reply blocks `args` produces: tmux answers each `;`-separated command with its own.
fn expected_blocks(args: &[String]) -> usize {
    1 + args.iter().filter(|arg| *arg == ";").count()
}

/// Only an all-pane query in the format this mapper parses can remove a remembered pane. A
/// successful empty response to that query is authoritative; a `list-sessions` response is not.
fn authoritative_pane_inventory_block(args: &[String]) -> Option<usize> {
    args.split(|arg| arg == ";")
        .enumerate()
        .find_map(|(index, command)| {
            (command.first().is_some_and(|name| name == "list-panes")
                && command.iter().any(|arg| arg == "-a")
                && command
                    .windows(2)
                    .any(|pair| pair[0] == "-F" && pair[1].starts_with("p\x1f")))
            .then_some(index)
        })
}

#[cfg(test)]
fn is_authoritative_pane_inventory_query(args: &[String]) -> bool {
    authoritative_pane_inventory_block(args).is_some()
}

/// The control-mode command line for `args`, or `None` when the control client should not run it.
///
/// Every command has to be a read-only query, and every argument has to survive tmux's parser
/// unchanged. Single quotes keep an argument literal — including the `#{...}` a format string
/// carries — and tmux offers no way to escape a quote inside them, so an argument holding one goes
/// back to being its own process rather than being mangled here.
fn control_command_line(args: &[String]) -> Option<String> {
    if args.is_empty() {
        return None;
    }
    let mut line = String::new();
    for command in args.split(|arg| arg == ";") {
        let (name, arguments) = command.split_first()?;
        if !CONTROL_QUERIES.contains(&name.as_str()) {
            return None;
        }
        if !line.is_empty() {
            line.push_str(" ; ");
        }
        line.push_str(name);
        for argument in arguments {
            if argument.contains('\'') || argument.contains('\n') {
                return None;
            }
            line.push_str(" '");
            line.push_str(argument);
            line.push('\'');
        }
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux_protocol::TmuxIdNameNotification;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn scope() -> MuxScope {
        MuxScope::new(
            crate::controller::SpaceId::from_persistence(1),
            crate::controller::BindingId::from_persistence(2),
        )
    }

    fn pane_inventory(
        pane_id: usize,
        window_id: usize,
        pid: u32,
        cwd: &str,
        command: &str,
    ) -> String {
        format!(
            "p\x1f$1\x1f@{window_id}\x1f%{pane_id}\x1f/dev/ttys{pane_id}\x1f{pid}\x1f{cwd}\x1f{command}\n"
        )
    }

    fn initialized_mapper(inventory: &str) -> (TmuxEventMapper, MuxEventQueue, MuxScope) {
        let queue = MuxEventQueue::with_backend_limits("tmux:test", 16, 1024);
        let mut mapper = TmuxEventMapper::default();
        let scope = scope();
        mapper.record_snapshot(inventory, &queue);
        let _ = queue.drain(scope, 16);
        (mapper, queue, scope)
    }

    #[test]
    fn control_parser_ignores_unknown_notifications_but_rebases_on_malformed_known_ones() {
        let mut parser = TmuxControlParser::default();
        for byte in b"%future-notification @42\n" {
            assert_eq!(next_control_notification(&mut parser, *byte), Ok(None));
        }

        let mut result = Ok(None);
        for byte in b"%window-add not-a-window\n" {
            result = next_control_notification(&mut parser, *byte);
        }
        assert_eq!(result, Err(MuxRebaseReason::SequenceGap));
    }

    fn output_cursor(
        mapper: &mut TmuxEventMapper,
        queue: &MuxEventQueue,
        scope: MuxScope,
        data: &str,
    ) -> MuxEventCursor {
        mapper.publish(
            TmuxControlNotification::Output(crate::tmux_protocol::TmuxOutputNotification {
                pane_id: 3,
                data: data.as_bytes().to_vec(),
            }),
            queue,
        );
        queue
            .drain(scope, 16)
            .into_iter()
            .find(|event| event.topic == MuxEventTopic::TerminalOutput)
            .and_then(|event| event.cursor)
            .expect("attributed pane output cursor")
    }

    fn foreground(pid: u32, command: &str, cwd: &str) -> MuxForegroundState {
        MuxForegroundState {
            pid: Some(pid),
            command: Some(command.to_owned()),
            cwd: Some(cwd.to_owned()),
            executable: None,
        }
    }

    #[test]
    fn linked_pane_output_is_published_for_every_session_placement() {
        let first = pane_inventory(3, 2, 42, "/repo", "zsh");
        let second = first.replacen("$1", "$2", 1);
        let (mut mapper, queue, scope) = initialized_mapper(&format!("{first}{second}"));

        mapper.publish(
            TmuxControlNotification::Output(crate::tmux_protocol::TmuxOutputNotification {
                pane_id: 3,
                data: b"linked".to_vec(),
            }),
            &queue,
        );

        let events = queue.drain(scope, 16);
        let sessions = events
            .iter()
            .filter(|event| event.topic == MuxEventTopic::TerminalOutput)
            .filter_map(|event| {
                event
                    .target
                    .as_ref()
                    .and_then(|target| target.session_id.clone())
            })
            .collect::<HashSet<_>>();
        assert_eq!(sessions, HashSet::from(["$1".to_owned(), "$2".to_owned()]));
    }
    #[test]
    fn unlinking_linked_pane_retires_only_removed_placement_and_keeps_output_alive() {
        let linked = format!(
            "{}{}",
            pane_inventory(3, 2, 42, "/repo", "zsh"),
            pane_inventory(3, 4, 42, "/repo", "zsh"),
        );
        let remaining = pane_inventory(3, 4, 42, "/repo", "zsh");
        let (mut mapper, queue, scope) = initialized_mapper(&linked);

        mapper.record_snapshot(&remaining, &queue);
        let events = queue.drain(scope, 16);
        let closed = events
            .iter()
            .find(|event| event.topic == MuxEventTopic::PaneClosed)
            .expect("removed linked placement close");
        let closed_target = closed.target.as_ref().expect("closed placement target");
        assert_eq!(closed_target.session_id.as_deref(), Some("$1"));
        assert_eq!(closed_target.window_id.as_deref(), Some("@2"));
        assert_eq!(closed_target.pane_id.as_deref(), Some("%3"));
        assert!(closed_target.terminal_id.is_none());
        assert!(closed_target.occupant.is_none());
        assert_eq!(
            mapper.pane_placements.get(&3).map(|targets| targets.len()),
            Some(1)
        );
        assert_eq!(
            mapper
                .pane_placements
                .get(&3)
                .and_then(|targets| targets[0].window_id.as_deref()),
            Some("@4")
        );

        mapper.publish(
            TmuxControlNotification::Output(crate::tmux_protocol::TmuxOutputNotification {
                pane_id: 3,
                data: b"remaining".to_vec(),
            }),
            &queue,
        );
        let output = queue
            .drain(scope, 16)
            .into_iter()
            .find(|event| event.topic == MuxEventTopic::TerminalOutput)
            .expect("remaining linked placement output");
        assert_eq!(
            output
                .target
                .as_ref()
                .and_then(|target| target.window_id.as_deref()),
            Some("@4")
        );
        assert_eq!(
            output
                .target
                .as_ref()
                .and_then(|target| target.terminal_id.as_deref()),
            Some("/dev/ttys3")
        );
        assert!(
            output
                .target
                .as_ref()
                .and_then(|target| target.occupant.as_ref())
                .is_some()
        );
    }

    #[test]
    fn control_command_line_quotes_a_chained_snapshot_query() {
        let line = control_command_line(&args(&[
            "list-sessions",
            "-F",
            "s\x1f#{session_id}",
            ";",
            "list-panes",
            "-a",
            "-F",
            "p\x1f#{pane_id}",
        ]))
        .expect("snapshot query");

        assert_eq!(
            line,
            "list-sessions '-F' 's\x1f#{session_id}' ; list-panes '-a' '-F' 'p\x1f#{pane_id}'"
        );
        assert_eq!(
            expected_blocks(&args(&["list-sessions", ";", "list-panes"])),
            2
        );
        assert!(is_authoritative_pane_inventory_query(&args(&[
            "list-sessions",
            ";",
            "list-panes",
            "-a",
            "-F",
            "p\x1f#{pane_id}",
        ])));
        assert_eq!(
            authoritative_pane_inventory_block(&args(&[
                "list-sessions",
                ";",
                "list-panes",
                "-a",
                "-F",
                "p\x1f#{pane_id}",
            ])),
            Some(1)
        );
        assert!(!is_authoritative_pane_inventory_query(&args(&[
            "list-sessions",
            "-F",
            "s\x1f#{session_id}",
        ])));
    }

    #[test]
    fn successful_spawned_inventory_fallback_rebases_lifecycle() {
        let runner = TmuxControlRunner::default();
        let output = CommandOutput {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
        };
        runner.record_spawned_inventory_fallback(
            &args(&["list-panes", "-a", "-F", "p\x1f#{pane_id}"]),
            &output,
        );

        let events = runner.events.drain(scope(), 16);
        assert_eq!(events.len(), 2);
        assert!(
            events
                .iter()
                .any(|event| event.topic == MuxEventTopic::TopologyChanged)
        );
        assert!(events.iter().any(MuxEvent::requires_rebase));
    }

    #[test]
    fn tmux_event_subscriptions_receive_one_ordered_bootstrap_each() {
        let runner = TmuxControlRunner::default();
        let first_scope = scope();
        let second_scope = MuxScope::new(
            crate::controller::SpaceId::from_persistence(1),
            crate::controller::BindingId::from_persistence(3),
        );

        let first = runner.drain_mux_events(first_scope, 16);
        let second = runner.drain_mux_events(second_scope, 16);
        for events in [first, second] {
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].topic, MuxEventTopic::SnapshotRebased);
            assert_eq!(
                events[0].payload,
                MuxEventPayload::Rebase {
                    reason: MuxRebaseReason::Bootstrap,
                }
            );
        }

        runner.events.publish(MuxEventDraft::new(
            MuxEventTopic::TopologyChanged,
            MuxEventProvenance::TmuxControl,
            None,
            None,
            MuxEventPayload::Topology {
                change: MuxTopologyChange::Invalidated,
            },
        ));
        for scope in [first_scope, second_scope] {
            let events = runner.drain_mux_events(scope, 16);
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].topic, MuxEventTopic::TopologyChanged);
        }
        assert!(runner.drain_mux_events(first_scope, 16).is_empty());
    }

    #[test]
    fn empty_authoritative_inventory_closes_and_clears_remembered_panes() {
        let queue = MuxEventQueue::with_backend_limits("tmux:test", 16, 1024);
        let mut mapper = TmuxEventMapper::default();
        let scope = MuxScope::new(
            crate::controller::SpaceId::from_persistence(1),
            crate::controller::BindingId::from_persistence(2),
        );
        mapper.record_snapshot(
            "p\x1f$1\x1f@2\x1f%3\x1f/dev/ttys3\x1f42\x1f/repo\x1fzsh\n",
            &queue,
        );
        let _ = queue.drain(scope, 8);

        mapper.record_snapshot("", &queue);

        let events = queue.drain(scope, 8);
        let closed = events
            .iter()
            .find(|event| event.topic == MuxEventTopic::PaneClosed)
            .expect("pane absent from an authoritative inventory must close");
        assert_eq!(
            closed
                .target
                .as_ref()
                .and_then(|target| target.pane_id.as_deref()),
            Some("%3")
        );
        assert_eq!(
            closed
                .target
                .as_ref()
                .and_then(|target| target.terminal_id.as_deref()),
            Some("/dev/ttys3")
        );
        assert!(
            closed
                .target
                .as_ref()
                .and_then(|target| target.occupant.as_ref())
                .is_some()
        );
        assert!(mapper.pane_targets.is_empty());
        assert!(mapper.pane_cwds.is_empty());
        assert!(mapper.output_sequences.is_empty());
        assert!(mapper.pane_lifecycle_epochs.is_empty());
    }

    #[test]
    fn authoritative_inventory_pane_set_changes_invalidate_topology_once() {
        let (mut mapper, queue, scope) =
            initialized_mapper(&pane_inventory(3, 2, 42, "/repo", "zsh"));

        mapper.record_snapshot(
            &format!(
                "{}{}",
                pane_inventory(3, 2, 42, "/repo", "zsh"),
                pane_inventory(5, 4, 43, "/other", "fish"),
            ),
            &queue,
        );
        let added = queue.drain(scope, 16);
        assert_eq!(
            added
                .iter()
                .filter(|event| event.topic == MuxEventTopic::TopologyChanged)
                .count(),
            1
        );

        mapper.record_snapshot(&pane_inventory(5, 4, 43, "/other", "fish"), &queue);
        let removed = queue.drain(scope, 16);
        assert_eq!(
            removed
                .iter()
                .filter(|event| event.topic == MuxEventTopic::TopologyChanged)
                .count(),
            1
        );
        assert!(
            removed
                .iter()
                .any(|event| event.topic == MuxEventTopic::PaneClosed)
        );
    }

    #[test]
    fn initial_inventory_does_not_claim_an_occupant_replacement() {
        let queue = MuxEventQueue::with_backend_limits("tmux:test", 16, 1024);
        let mut mapper = TmuxEventMapper::default();
        let scope = scope();

        mapper.record_snapshot(&pane_inventory(3, 2, 42, "/repo", "zsh"), &queue);
        let events = queue.drain(scope, 16);

        assert!(
            events
                .iter()
                .all(|event| event.topic != MuxEventTopic::PaneOccupantReplaced)
        );
    }
    #[test]
    fn pane_mode_change_preserves_occupant_generation_and_output_cursor() {
        let inventory = pane_inventory(3, 2, 42, "/repo", "zsh");
        let (mut mapper, queue, scope) = initialized_mapper(&inventory);
        let initial_occupant = mapper
            .pane_targets
            .get(&3)
            .and_then(|target| target.occupant.as_ref())
            .cloned()
            .expect("inventory target occupant");
        let first_cursor = output_cursor(&mut mapper, &queue, scope, "before");

        mapper.publish(
            TmuxControlNotification::PaneModeChanged { pane_id: 3 },
            &queue,
        );
        let mode_events = queue.drain(scope, 16);

        assert_eq!(mode_events.len(), 1);
        assert_eq!(mode_events[0].topic, MuxEventTopic::TopologyChanged);
        assert!(
            mode_events
                .iter()
                .all(|event| event.topic != MuxEventTopic::PaneOccupantReplaced)
        );
        assert_eq!(
            mapper
                .pane_targets
                .get(&3)
                .and_then(|target| target.occupant.as_ref()),
            Some(&initial_occupant)
        );
        assert_eq!(
            mapper
                .pane_lifecycle_epochs
                .get(&3)
                .copied()
                .unwrap_or_default(),
            0
        );

        let second_cursor = output_cursor(&mut mapper, &queue, scope, "after");
        assert_eq!(second_cursor.stream, first_cursor.stream);
        assert_eq!(second_cursor.sequence, first_cursor.sequence + 1);
    }

    #[test]
    fn inventory_pid_change_replaces_the_occupant_once_and_rebases_output_cursor() {
        let initial_inventory = pane_inventory(3, 2, 42, "/repo", "zsh");
        let replacement_inventory = pane_inventory(3, 2, 43, "/repo", "fish");
        let (mut mapper, queue, scope) = initialized_mapper(&initial_inventory);
        let first_cursor = output_cursor(&mut mapper, &queue, scope, "before");

        mapper.record_snapshot(&replacement_inventory, &queue);
        let events = queue.drain(scope, 16);
        let replacements = events
            .iter()
            .filter(|event| event.topic == MuxEventTopic::PaneOccupantReplaced)
            .collect::<Vec<_>>();

        assert_eq!(replacements.len(), 1);
        assert!(
            replacements[0]
                .target
                .as_ref()
                .and_then(|target| target.occupant.as_ref())
                .is_some_and(|occupant| {
                    occupant.pid == Some(43) && occupant.backend_identity.contains("lifecycle=1")
                })
        );
        assert_eq!(mapper.pane_lifecycle_epochs.get(&3), Some(&1));

        mapper.record_snapshot(&replacement_inventory, &queue);
        assert!(
            queue.drain(scope, 16).is_empty(),
            "the same inventory must not retire the replacement a second time"
        );

        let second_cursor = output_cursor(&mut mapper, &queue, scope, "after");
        assert_ne!(second_cursor.stream, first_cursor.stream);
        assert_eq!(first_cursor.sequence, 1);
        assert_eq!(second_cursor.sequence, 1);
    }

    #[test]
    fn incomplete_inventory_is_not_lifecycle_evidence() {
        let initial_inventory = pane_inventory(3, 2, 42, "/repo", "zsh");
        // Older tmux formats can omit pane_pid while still reporting cwd and the foreground
        // command. That makes the inventory incomplete, not a replacement.
        let incomplete_inventory =
            "p\x1f$1\x1f@2\x1f0\x1fmain\x1f1\x1f1\x1f%3\x1fhidden\x1f\x1f/repo\x1fzsh\n";
        let replacement_inventory = pane_inventory(3, 2, 43, "/repo", "fish");
        let (mut mapper, queue, scope) = initialized_mapper(&initial_inventory);
        let first_cursor = output_cursor(&mut mapper, &queue, scope, "before");

        mapper.record_snapshot(incomplete_inventory, &queue);
        let events = queue.drain(scope, 16);

        assert!(
            events.is_empty(),
            "an inventory missing only pane_pid must preserve the known foreground state"
        );
        assert_eq!(
            mapper
                .pane_targets
                .get(&3)
                .and_then(|target| target.occupant.as_ref())
                .and_then(|occupant| occupant.pid),
            Some(42)
        );
        assert_eq!(
            mapper
                .pane_lifecycle_epochs
                .get(&3)
                .copied()
                .unwrap_or_default(),
            0
        );

        let second_cursor = output_cursor(&mut mapper, &queue, scope, "after incomplete");
        assert_eq!(second_cursor.stream, first_cursor.stream);
        assert_eq!(second_cursor.sequence, first_cursor.sequence + 1);

        mapper.record_snapshot(&replacement_inventory, &queue);
        assert_eq!(
            queue
                .drain(scope, 16)
                .iter()
                .filter(|event| event.topic == MuxEventTopic::PaneOccupantReplaced)
                .count(),
            1
        );
        let third_cursor = output_cursor(&mut mapper, &queue, scope, "after replacement");
        assert_ne!(third_cursor.stream, second_cursor.stream);
        assert_eq!(third_cursor.sequence, 1);
    }
    #[test]
    fn inventory_cwd_change_emits_cwd_and_foreground_without_replacing_the_occupant() {
        let initial_inventory = pane_inventory(3, 2, 42, "/repo", "zsh");
        let changed_inventory = pane_inventory(3, 2, 42, "/other", "zsh");
        let (mut mapper, queue, scope) = initialized_mapper(&initial_inventory);
        let initial_identity = mapper
            .pane_targets
            .get(&3)
            .and_then(|target| target.occupant.as_ref())
            .map(|occupant| occupant.backend_identity.clone())
            .expect("initial occupant identity");
        let first_cursor = output_cursor(&mut mapper, &queue, scope, "before");

        mapper.record_snapshot(&changed_inventory, &queue);
        let events = queue.drain(scope, 16);

        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|event| matches!(
            event.topic,
            MuxEventTopic::PaneCwdChanged | MuxEventTopic::PaneForegroundChanged
        )));
        let cwd = events
            .iter()
            .find(|event| event.topic == MuxEventTopic::PaneCwdChanged)
            .expect("cwd delta");
        assert_eq!(
            &cwd.payload,
            &MuxEventPayload::Cwd {
                old_cwd: Some("/repo".to_owned()),
                new_cwd: Some("/other".to_owned()),
            }
        );
        let foreground_event = events
            .iter()
            .find(|event| event.topic == MuxEventTopic::PaneForegroundChanged)
            .expect("foreground delta");
        assert_eq!(
            &foreground_event.payload,
            &MuxEventPayload::Foreground {
                old_state: Some(foreground(42, "zsh", "/repo")),
                new_state: Some(foreground(42, "zsh", "/other")),
            }
        );
        assert_eq!(
            mapper
                .pane_targets
                .get(&3)
                .and_then(|target| target.occupant.as_ref())
                .map(|occupant| occupant.backend_identity.as_str()),
            Some(initial_identity.as_str())
        );
        assert_eq!(
            mapper
                .pane_lifecycle_epochs
                .get(&3)
                .copied()
                .unwrap_or_default(),
            0
        );

        let second_cursor = output_cursor(&mut mapper, &queue, scope, "after");
        assert_eq!(second_cursor.stream, first_cursor.stream);
        assert_eq!(second_cursor.sequence, first_cursor.sequence + 1);
    }

    #[test]
    fn inventory_foreground_change_preserves_occupant_generation_and_output_cursor() {
        let initial_inventory = pane_inventory(3, 2, 42, "/repo", "zsh");
        let changed_inventory = pane_inventory(3, 2, 42, "/repo", "vim");
        let (mut mapper, queue, scope) = initialized_mapper(&initial_inventory);
        let initial_identity = mapper
            .pane_targets
            .get(&3)
            .and_then(|target| target.occupant.as_ref())
            .map(|occupant| occupant.backend_identity.clone())
            .expect("initial occupant identity");
        let first_cursor = output_cursor(&mut mapper, &queue, scope, "before");

        mapper.record_snapshot(&changed_inventory, &queue);
        let events = queue.drain(scope, 16);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].topic, MuxEventTopic::PaneForegroundChanged);
        assert_eq!(
            &events[0].payload,
            &MuxEventPayload::Foreground {
                old_state: Some(foreground(42, "zsh", "/repo")),
                new_state: Some(foreground(42, "vim", "/repo")),
            }
        );
        assert_eq!(
            mapper
                .pane_targets
                .get(&3)
                .and_then(|target| target.occupant.as_ref())
                .map(|occupant| occupant.backend_identity.as_str()),
            Some(initial_identity.as_str())
        );
        assert_eq!(
            mapper
                .pane_lifecycle_epochs
                .get(&3)
                .copied()
                .unwrap_or_default(),
            0
        );

        let second_cursor = output_cursor(&mut mapper, &queue, scope, "after");
        assert_eq!(second_cursor.stream, first_cursor.stream);
        assert_eq!(second_cursor.sequence, first_cursor.sequence + 1);
    }

    #[test]
    fn active_pane_notification_defers_placement_changes_to_inventory() {
        let initial_inventory = pane_inventory(3, 2, 42, "/repo", "zsh");
        let moved_inventory = pane_inventory(3, 4, 42, "/repo", "zsh");
        let (mut mapper, queue, scope) = initialized_mapper(&initial_inventory);
        let first_cursor = output_cursor(&mut mapper, &queue, scope, "before");

        mapper.publish(
            TmuxControlNotification::WindowPaneChanged(
                crate::tmux_protocol::TmuxWindowPaneChangedNotification {
                    window_id: 4,
                    pane_id: 3,
                },
            ),
            &queue,
        );
        let move_events = queue.drain(scope, 16);

        assert_eq!(move_events.len(), 1);
        assert_eq!(move_events[0].topic, MuxEventTopic::TopologyChanged);
        assert!(move_events[0].target.is_none());
        assert_eq!(
            mapper
                .pane_targets
                .get(&3)
                .and_then(|target| target.window_id.as_deref()),
            Some("@2")
        );

        mapper.record_snapshot(&moved_inventory, &queue);
        let inventory_events = queue.drain(scope, 16);
        assert!(
            inventory_events
                .iter()
                .any(|event| event.topic == MuxEventTopic::TopologyChanged)
        );
        assert!(
            inventory_events
                .iter()
                .all(|event| event.topic != MuxEventTopic::PaneOccupantReplaced)
        );
        assert_eq!(
            mapper
                .pane_lifecycle_epochs
                .get(&3)
                .copied()
                .unwrap_or_default(),
            0
        );

        let second_cursor = output_cursor(&mut mapper, &queue, scope, "after");
        assert_eq!(second_cursor.stream, first_cursor.stream);
        assert_eq!(second_cursor.sequence, first_cursor.sequence + 1);
    }

    #[test]
    fn inventory_retarget_invalidates_topology_without_replacing_the_occupant() {
        let initial_inventory = pane_inventory(3, 2, 42, "/repo", "zsh");
        let moved_inventory = pane_inventory(3, 4, 42, "/repo", "zsh");
        let (mut mapper, queue, scope) = initialized_mapper(&initial_inventory);
        let first_cursor = output_cursor(&mut mapper, &queue, scope, "before");

        mapper.record_snapshot(&moved_inventory, &queue);
        let events = queue.drain(scope, 16);

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].topic, MuxEventTopic::PaneClosed);
        assert_eq!(
            events[0]
                .target
                .as_ref()
                .and_then(|target| target.window_id.as_deref()),
            Some("@2")
        );
        assert!(
            events[0]
                .target
                .as_ref()
                .is_some_and(|target| target.terminal_id.is_none() && target.occupant.is_none())
        );
        assert_eq!(events[1].topic, MuxEventTopic::TopologyChanged);
        assert_eq!(
            events[1]
                .target
                .as_ref()
                .and_then(|target| target.window_id.as_deref()),
            Some("@4")
        );
        assert!(
            events
                .iter()
                .all(|event| event.topic != MuxEventTopic::PaneOccupantReplaced)
        );
        assert_eq!(
            mapper
                .pane_lifecycle_epochs
                .get(&3)
                .copied()
                .unwrap_or_default(),
            0
        );

        let second_cursor = output_cursor(&mut mapper, &queue, scope, "after");
        assert_eq!(second_cursor.stream, first_cursor.stream);
        assert_eq!(second_cursor.sequence, first_cursor.sequence + 1);
    }
    #[test]
    fn window_close_notifications_defer_pane_retirement_to_inventory() {
        let inventory = format!(
            "{}{}",
            pane_inventory(3, 2, 42, "/repo", "zsh"),
            pane_inventory(5, 4, 43, "/other", "fish"),
        );
        let (mut mapper, queue, scope) = initialized_mapper(&inventory);

        mapper.publish(TmuxControlNotification::WindowClose { id: 2 }, &queue);
        let events = queue.drain(scope, 16);

        assert_eq!(events.len(), 1);
        assert!(
            events
                .iter()
                .all(|event| event.topic != MuxEventTopic::PaneClosed)
        );
        let topology = events
            .iter()
            .find(|event| event.topic == MuxEventTopic::TopologyChanged)
            .expect("topology invalidation");
        assert!(topology.target.is_none());
        assert!(mapper.pane_targets.contains_key(&3));
        assert!(mapper.pane_targets.contains_key(&5));

        mapper.record_snapshot(&pane_inventory(5, 4, 43, "/other", "fish"), &queue);
        let refresh_events = queue.drain(scope, 16);
        assert!(
            refresh_events
                .iter()
                .any(|event| event.topic == MuxEventTopic::PaneClosed)
        );
    }

    #[test]
    fn unlinking_a_window_alias_preserves_its_shared_panes() {
        let inventory = pane_inventory(3, 2, 42, "/repo", "zsh");
        let (mut mapper, queue, scope) = initialized_mapper(&inventory);

        mapper.publish(
            TmuxControlNotification::UnlinkedWindowClose { id: 2 },
            &queue,
        );
        let events = queue.drain(scope, 16);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].topic, MuxEventTopic::TopologyChanged);
        assert!(mapper.pane_targets.contains_key(&3));
    }

    #[test]
    fn unlinked_window_add_and_rename_invalidate_global_topology() {
        let (mut mapper, queue, scope) =
            initialized_mapper(&pane_inventory(3, 2, 42, "/repo", "zsh"));

        for notification in [
            TmuxControlNotification::UnlinkedWindowAdd { id: 9 },
            TmuxControlNotification::UnlinkedWindowRenamed(TmuxIdNameNotification {
                id: 9,
                name: "other-session".to_owned(),
            }),
        ] {
            mapper.publish(notification, &queue);
            let events = queue.drain(scope, 16);
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].topic, MuxEventTopic::TopologyChanged);
            assert!(events[0].target.is_none());
            assert_eq!(
                events[0].payload,
                MuxEventPayload::Topology {
                    change: MuxTopologyChange::Invalidated,
                }
            );
        }
    }

    #[test]
    fn output_before_inventory_rebases_without_publishing_targetless_terminal_output() {
        let queue = MuxEventQueue::with_backend_limits("tmux:test", 16, 1024);
        let mut mapper = TmuxEventMapper::default();
        let scope = scope();

        mapper.publish(
            TmuxControlNotification::Output(crate::tmux_protocol::TmuxOutputNotification {
                pane_id: 3,
                data: b"before".to_vec(),
            }),
            &queue,
        );
        let early_events = queue.drain(scope, 16);

        assert!(
            early_events
                .iter()
                .all(|event| event.topic != MuxEventTopic::TerminalOutput)
        );
        assert!(early_events.iter().any(MuxEvent::requires_rebase));
        assert!(mapper.output_sequences.is_empty());

        mapper.record_snapshot(&pane_inventory(3, 2, 42, "/repo", "zsh"), &queue);
        assert!(
            queue
                .drain(scope, 16)
                .iter()
                .all(|event| event.topic != MuxEventTopic::TerminalOutput)
        );
        mapper.publish(
            TmuxControlNotification::Output(crate::tmux_protocol::TmuxOutputNotification {
                pane_id: 3,
                data: b"after".to_vec(),
            }),
            &queue,
        );
        let output = queue
            .drain(scope, 16)
            .into_iter()
            .find(|event| event.topic == MuxEventTopic::TerminalOutput)
            .expect("known pane output");

        assert_eq!(
            output
                .target
                .as_ref()
                .and_then(|target| target.pane_id.as_deref()),
            Some("%3")
        );
        assert_eq!(
            output.cursor,
            Some(MuxEventCursor::new("tmux-output:3:lifecycle:0", 1))
        );
    }

    #[test]
    fn control_adapter_decodes_octal_bytes_and_preserves_exact_inventory_target() {
        let queue = MuxEventQueue::with_backend_limits("tmux:test", 16, 1024);
        let mut mapper = TmuxEventMapper::default();
        let scope = MuxScope::new(
            crate::controller::SpaceId::from_persistence(1),
            crate::controller::BindingId::from_persistence(2),
        );
        mapper.record_snapshot(
            "p\x1f$1\x1f@2\x1f%3\x1f/dev/ttys003\x1f42\x1f/repo\x1fzsh\n",
            &queue,
        );
        let _ = queue.drain(scope, 8);

        mapper.publish(
            TmuxControlNotification::Output(crate::tmux_protocol::TmuxOutputNotification {
                pane_id: 3,
                data: br"\033[31mhello\134".to_vec(),
            }),
            &queue,
        );
        let output = queue.drain(scope, 8);
        let event = output
            .iter()
            .find(|event| event.topic == MuxEventTopic::TerminalOutput)
            .expect("terminal output event");
        assert_eq!(
            &event.payload,
            &MuxEventPayload::Output {
                bytes: b"\x1b[31mhello\\".to_vec(),
            }
        );
        let target = event.target.as_ref().expect("exact inventory target");
        assert_eq!(target.session_id.as_deref(), Some("$1"));
        assert_eq!(target.window_id.as_deref(), Some("@2"));
        assert_eq!(target.pane_id.as_deref(), Some("%3"));
        assert_eq!(target.terminal_id.as_deref(), Some("/dev/ttys003"));
        assert_eq!(
            target.occupant.as_ref().and_then(|occupant| occupant.pid),
            Some(42)
        );
    }

    #[test]
    fn control_adapter_preserves_binary_and_non_ascii_output_bytes() {
        let (mut mapper, queue, scope) =
            initialized_mapper(&pane_inventory(3, 2, 42, "/repo", "zsh"));
        let mut parser = TmuxControlParser::default();
        let notification = parser
            .put_bytes(b"%output %3 \xff\xc3\xa9\xf0\x9f\x98\x80\n")
            .expect("raw output parses")
            .pop()
            .expect("output notification");

        mapper.publish(notification, &queue);
        let event = queue
            .drain(scope, 16)
            .into_iter()
            .find(|event| event.topic == MuxEventTopic::TerminalOutput)
            .expect("terminal output event");

        assert_eq!(
            event.payload,
            MuxEventPayload::Output {
                bytes: b"\xff\xc3\xa9\xf0\x9f\x98\x80".to_vec(),
            }
        );
    }

    #[test]
    fn control_degradation_rebases_without_claiming_backend_disconnect() {
        let queue = MuxEventQueue::with_limits(8, 1024);
        let mut mapper = TmuxEventMapper::default();
        let scope = MuxScope::new(
            crate::controller::SpaceId::from_persistence(1),
            crate::controller::BindingId::from_persistence(2),
        );
        mapper.publish(TmuxControlNotification::Exit, &queue);
        let events = queue.drain(scope, 8);

        assert!(events.iter().any(MuxEvent::requires_rebase));
        assert!(
            events
                .iter()
                .all(|event| event.topic != MuxEventTopic::BackendDisconnected)
        );
    }

    #[test]
    fn unrelated_client_detach_does_not_degrade_the_control_connection() {
        let queue = MuxEventQueue::with_limits(8, 1024);
        let mut mapper = TmuxEventMapper::default();
        let scope = scope();

        mapper.publish(
            TmuxControlNotification::ClientDetached {
                client: "/dev/pts/1".to_owned(),
            },
            &queue,
        );
        let detached_events = queue.drain(scope, 8);
        assert!(detached_events.iter().all(|event| !event.requires_rebase()));

        mapper.publish(TmuxControlNotification::Exit, &queue);
        assert!(
            queue.drain(scope, 8).iter().any(MuxEvent::requires_rebase),
            "a later control EOF must still surface its rebase"
        );
    }
    /// Mutations skip the control client and fork their own process. For a remote binding that fork
    /// has to be an SSH invocation: run here, a rename or a kill would land on this machine's tmux
    /// server, whose sessions bootty is not showing.
    #[test]
    fn a_remote_runner_forks_its_mutations_at_the_other_host() {
        let remote = SshRemote::new(bootty_config::config::SshRemoteConfig {
            host: "devbox".to_owned(),
            user: None,
            port: None,
            program: "ssh".to_owned(),
            args: Vec::new(),
        });
        let mutation = args(&["kill-session", "-t", "build"]);

        let (program, argv) = TmuxControlRunner::for_remote(remote).spawned("tmux", &mutation);
        assert_eq!(program, "ssh");
        assert_eq!(
            argv.last().map(String::as_str),
            Some("'tmux' 'kill-session' '-t' 'build'")
        );

        let (program, argv) = TmuxControlRunner::default().spawned("tmux", &mutation);
        assert_eq!(program, "tmux");
        assert_eq!(argv, mutation);
    }

    #[test]
    fn remote_transport_variants_have_distinct_event_producer_identities() {
        fn remote(port: u16, args: &[&str]) -> SshRemote {
            SshRemote::new(bootty_config::config::SshRemoteConfig {
                host: "devbox".to_owned(),
                user: Some("dev".to_owned()),
                port: Some(port),
                program: "ssh".to_owned(),
                args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            })
        }

        fn event_backend_identity(runner: &TmuxControlRunner) -> String {
            runner.events.publish(MuxEventDraft::new(
                MuxEventTopic::TopologyChanged,
                MuxEventProvenance::TmuxControl,
                None,
                None,
                MuxEventPayload::Topology {
                    change: MuxTopologyChange::Invalidated,
                },
            ));
            runner
                .events
                .drain(scope(), 1)
                .into_iter()
                .next()
                .expect("event producer identity")
                .backend_identity
        }

        let baseline = TmuxControlRunner::for_remote(remote(22, &["-o", "Compression=no"]));
        let different_port = TmuxControlRunner::for_remote(remote(2202, &["-o", "Compression=no"]));
        let different_args = TmuxControlRunner::for_remote(remote(22, &["-o", "Compression=yes"]));

        let baseline_identity = event_backend_identity(&baseline);
        let port_identity = event_backend_identity(&different_port);
        let args_identity = event_backend_identity(&different_args);

        assert!(baseline_identity.starts_with("tmux:dev@devbox:"));
        assert!(!baseline_identity.contains("Compression=no"));
        assert_ne!(baseline_identity, port_identity);
        assert_ne!(baseline_identity, args_identity);
        assert_ne!(port_identity, args_identity);
    }

    #[test]
    fn control_command_line_refuses_anything_it_would_change_or_mangle() {
        // A mutation answered by the shared client would be run out of band from its own exit
        // status, and a quote inside an argument has no escape in tmux's parser.
        assert_eq!(
            control_command_line(&args(&["rename-session", "-t", "$1", "release"])),
            None
        );
        assert_eq!(
            control_command_line(&args(&["list-sessions", ";", "kill-server"])),
            None
        );
        assert_eq!(
            control_command_line(&args(&["list-sessions", "-F", "it's"])),
            None
        );
        assert_eq!(control_command_line(&[]), None);
    }

    #[cfg(unix)]
    #[test]
    fn dropping_the_backend_ends_its_control_client() {
        use super::super::tmux::TmuxBackend;

        let runner = TmuxControlRunner::default();
        let registry = Arc::downgrade(&runner.clients);
        let mut child = Command::new("sh")
            .args(["-c", "cat >/dev/null"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("start stand-in control client");
        let pid = child.id();
        let stdin = child.stdin.take().expect("client stdin");
        let stdout = child.stdout.take().expect("client stdout");
        let (_replies_tx, replies) = channel();
        runner.clients.lock().expect("client registry").insert(
            "tmux".to_owned(),
            ClientSlot {
                client: Some(TmuxControlClient {
                    child,
                    stdin,
                    replies,
                    mapper: Arc::new(Mutex::new(TmuxEventMapper::default())),
                    events: runner.events.clone(),
                }),
                retry_after: None,
            },
        );
        let backend = TmuxBackend::with_runner("tmux", runner);

        drop(backend);

        assert!(
            registry.upgrade().is_none(),
            "backend owns the client registry"
        );
        assert!(
            !Command::new("ps")
                .args(["-p", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("inspect stand-in process")
                .success(),
            "dropping the registry must kill and wait for its client"
        );
        drop(stdout);
    }

    /// Both snapshot paths have to describe the same server, or control mode is a data change
    /// wearing a performance change's clothes. This reads the developer's own tmux server, which is
    /// why it is opt-in.
    #[cfg(unix)]
    #[test]
    #[ignore = "reads the running tmux server on the default socket"]
    fn control_mode_and_process_snapshots_describe_the_same_server() {
        use super::super::tmux::TmuxBackend;

        let forked = TmuxBackend::with_runner("tmux", SystemCommandRunner)
            .snapshot()
            .expect("forked snapshot");
        let controlled = TmuxBackend::new().snapshot().expect("control snapshot");

        assert_eq!(forked, controlled);
        assert!(
            !controlled.sessions.is_empty(),
            "start a tmux session before running this"
        );
    }

    /// Skipped where tmux is unavailable; the fallback path covers that case in production too.
    #[cfg(unix)]
    #[test]
    #[ignore = "requires a tmux binary"]
    fn control_client_answers_repeated_queries_from_one_process() {
        let socket = format!("bootty-control-test-{}", std::process::id());
        // Start the server from an empty config on a private socket: whoever runs this has their
        // own `~/.tmux.conf`, and a session hook there runs to completion before `new-session`
        // returns, so a hook that waits on something else would hang this test forever. The
        // server also inherits and holds whatever stdout it is handed, so capturing output would
        // block on an EOF that never comes — send its streams to /dev/null and read the status.
        let tmux = |args: &[&str]| {
            Command::new("tmux")
                .args(["-L", &socket, "-f", "/dev/null"])
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
        };
        assert!(
            tmux(&["new-session", "-d", "-s", "one"])
                .expect("start private tmux server")
                .success(),
            "tmux must start a private server"
        );
        struct KillServer(String);
        impl Drop for KillServer {
            fn drop(&mut self) {
                let _ = Command::new("tmux")
                    .args(["-L", &self.0, "kill-server"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
        let _guard = KillServer(socket.clone());

        let mut client =
            TmuxControlClient::start_with("tmux", &["-L", &socket, "-f", "/dev/null"], None)
                .expect("control client");
        let query = "list-sessions -F '#{session_name}'";

        assert_eq!(client.query(query, 1, None).expect("first query"), "one");
        assert!(
            tmux(&["new-session", "-d", "-s", "two"])
                .expect("second session")
                .success()
        );
        // The same client, still one process, reports state it was never told about at startup.
        assert_eq!(
            client.query(query, 1, None).expect("second query"),
            "one\ntwo",
            "a live client should report both sessions"
        );
        // A rejected command must not leave the client answering the next query with its output.
        assert!(client.query("bogus-command", 1, None).is_err());
        assert_eq!(
            client.query(query, 1, None).expect("query after error"),
            "one\ntwo"
        );
    }
}
