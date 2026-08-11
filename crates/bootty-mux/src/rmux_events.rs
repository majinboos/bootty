use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::Result;
use rmux_sdk::{
    ForegroundState, Pane, PaneId, PaneOutputChunk, PaneProcessState, PaneStateEvent,
    PaneStateEventsOptions, SessionName,
};
use tokio::{
    runtime::Builder,
    sync::{mpsc, watch},
    time::{MissedTickBehavior, interval, sleep},
};

use crate::{
    backend::{
        MuxEvent, MuxEventCapability, MuxEventCursor, MuxEventDraft, MuxEventPayload,
        MuxEventProvenance, MuxEventQueue, MuxEventTarget, MuxEventTopic, MuxForegroundState,
        MuxOccupantIdentity, MuxPaneOption, MuxPaneState, MuxRebaseReason, MuxTopologyChange,
    },
    controller::MuxScope,
    rmux::{RmuxPaneRow, list_pane_rows},
    rmux_bridge::connect_bootty_rmux,
};

const RMUX_RECONNECT_DELAY: Duration = Duration::from_millis(250);
/// The pinned SDK exposes authoritative per-pane streams but no server-wide topology stream.
/// Reconciliation is therefore explicitly best-effort and only establishes/tears down the
/// authoritative pane watchers; it never fabricates a pane lifecycle observation.
const RMUX_TOPOLOGY_RESCAN_INTERVAL: Duration = Duration::from_millis(500);

pub(crate) fn event_capabilities() -> Vec<MuxEventCapability> {
    MuxEventTopic::ALL
        .into_iter()
        .map(|topic| match topic {
            MuxEventTopic::TopologyChanged => MuxEventCapability::best_effort(
                topic,
                "rmux SDK exposes authoritative per-pane lifecycle streams but no server-wide topology stream; periodic authoritative inventory reconciliation discovers external panes",
            ),
            MuxEventTopic::TerminalOutput
            | MuxEventTopic::PaneStateChanged
            | MuxEventTopic::PaneTitleChanged
            | MuxEventTopic::PaneOptionsChanged
            | MuxEventTopic::PaneForegroundChanged
            | MuxEventTopic::PaneCwdChanged
            | MuxEventTopic::PaneOccupantReplaced
            | MuxEventTopic::PaneClosed
            | MuxEventTopic::BackendDisconnected
            | MuxEventTopic::BackendLagged
            | MuxEventTopic::SnapshotRebased => MuxEventCapability::available(topic),
        })
        .collect()
}

pub(crate) fn start() {
    event_hub().start();
}

pub(crate) fn drain_events(scope: MuxScope, maximum: usize) -> Vec<MuxEvent> {
    event_hub().events.drain(scope, maximum)
}

pub(crate) fn topology_invalidated() {
    let hub = event_hub();
    hub.start();
    hub.events.publish(MuxEventDraft::new(
        MuxEventTopic::TopologyChanged,
        MuxEventProvenance::RmuxSdk,
        None,
        None,
        MuxEventPayload::Topology {
            change: MuxTopologyChange::Invalidated,
        },
    ));
    let control = hub
        .controls
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(control) = control.as_ref() {
        let _ = control.try_send(());
    }
}

type PaneWatchKey = (String, String);
type SharedPaneWatchTarget = Arc<Mutex<PaneWatchTarget>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PaneWatchLifecycle {
    Active,
    Retired,
    Closed,
}

struct PaneWatchTarget {
    target: MuxEventTarget,
    generation: u64,
    lifecycle: PaneWatchLifecycle,
}

impl PaneWatchTarget {
    fn new(target: MuxEventTarget) -> Self {
        Self {
            target,
            generation: 0,
            lifecycle: PaneWatchLifecycle::Active,
        }
    }

    fn snapshot(&self) -> Option<(u64, MuxEventTarget)> {
        if self.lifecycle != PaneWatchLifecycle::Active {
            return None;
        }
        Some((self.generation, self.target.clone()))
    }

    fn replace(&mut self, target: MuxEventTarget) -> Option<(MuxEventTarget, MuxEventTarget)> {
        if self.lifecycle != PaneWatchLifecycle::Active || self.target == target {
            return None;
        }
        let previous = std::mem::replace(&mut self.target, target);
        self.generation = self.generation.wrapping_add(1);
        Some((previous, self.target.clone()))
    }

    fn replace_authoritative(
        &mut self,
        mut target: MuxEventTarget,
    ) -> Option<(MuxEventTarget, MuxEventTarget)> {
        if occupant_generation(&target) == occupant_generation(&self.target) {
            target.occupant = self.target.occupant.clone();
        }
        self.replace(target)
    }

    fn replace_live_occupant(
        &mut self,
        expected_generation: u64,
        occupant: Option<MuxOccupantIdentity>,
    ) -> Option<(MuxEventTarget, MuxEventTarget)> {
        if self.generation != expected_generation {
            return None;
        }
        let mut target = self.target.clone();
        target.occupant = occupant;
        self.replace(target)
    }

    fn retire(&mut self) -> bool {
        if self.lifecycle != PaneWatchLifecycle::Active {
            return false;
        }
        self.lifecycle = PaneWatchLifecycle::Retired;
        self.generation = self.generation.wrapping_add(1);
        true
    }

    fn close(&mut self) -> Option<MuxEventTarget> {
        if self.lifecycle == PaneWatchLifecycle::Closed {
            return None;
        }
        self.lifecycle = PaneWatchLifecycle::Closed;
        self.generation = self.generation.wrapping_add(1);
        Some(self.target.clone())
    }

    fn needs_reconciliation(&self) -> bool {
        self.lifecycle != PaneWatchLifecycle::Active
    }
}

struct RmuxEventHub {
    events: MuxEventQueue,
    started: Arc<AtomicBool>,
    controls: Arc<Mutex<Option<mpsc::Sender<()>>>>,
    connection: RmuxConnectionState,
}

impl RmuxEventHub {
    fn start(&self) {
        if self.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let events = self.events.clone();
        let controls = Arc::clone(&self.controls);
        let control_slot = Arc::clone(&controls);
        let connection = self.connection.clone();
        let started = Arc::clone(&self.started);
        let spawn = thread::Builder::new()
            .name("bootty-rmux-events".to_owned())
            .spawn(move || {
                let runtime = match Builder::new_multi_thread()
                    .enable_all()
                    .worker_threads(2)
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        connection.disconnected(
                            &events,
                            format!("rmux event runtime failed to start: {error}"),
                        );
                        started.store(false, Ordering::Release);
                        return;
                    }
                };
                runtime.block_on(run_event_hub(events, controls, connection));
                *control_slot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                started.store(false, Ordering::Release);
            });
        if let Err(error) = spawn {
            self.started.store(false, Ordering::Release);
            self.connection.disconnected(
                &self.events,
                format!("rmux event worker failed to start: {error}"),
            );
        }
    }
}

fn event_hub() -> &'static RmuxEventHub {
    static HUB: LazyLock<RmuxEventHub> = LazyLock::new(|| RmuxEventHub {
        events: MuxEventQueue::for_backend("rmux:local-sdk"),
        started: Arc::new(AtomicBool::new(false)),
        controls: Arc::new(Mutex::new(None)),
        connection: RmuxConnectionState::default(),
    });
    &HUB
}

#[derive(Default)]
struct RmuxReconnectEpoch {
    current: u64,
    pending: Option<u64>,
}

#[derive(Clone, Default)]
struct RmuxConnectionState {
    disconnected: Arc<AtomicBool>,
    reconnect: Arc<Mutex<RmuxReconnectEpoch>>,
}

impl RmuxConnectionState {
    fn connected(&self) {
        self.disconnected.store(false, Ordering::Release);
    }

    fn inventory_epoch(&self) -> u64 {
        self.reconnect
            .lock()
            .expect("rmux reconnect epoch lock")
            .current
    }

    /// Emits the rebase only after the inventory captured at `inventory_epoch` succeeded.
    /// Holding the epoch lock through publication keeps a new disconnect ordered after this
    /// rebase rather than allowing stale inventory to reset newer connection state.
    fn publish_inventory_rebase(
        &self,
        events: &MuxEventQueue,
        inventory_epoch: u64,
        bootstrap: bool,
    ) {
        let mut reconnect = self.reconnect.lock().expect("rmux reconnect epoch lock");
        let inventory_current =
            !matches!(reconnect.pending, Some(epoch) if epoch > inventory_epoch);
        if inventory_current {
            self.connected();
        }
        let reconnected = inventory_current
            && matches!(reconnect.pending, Some(epoch) if epoch <= inventory_epoch);
        if reconnected {
            reconnect.pending = None;
        }
        let reason = if bootstrap {
            Some(MuxRebaseReason::Bootstrap)
        } else if reconnected {
            Some(MuxRebaseReason::Reconnect)
        } else {
            None
        };
        if let Some(reason) = reason {
            events.publish(MuxEventDraft::rebase(MuxEventProvenance::RmuxSdk, reason));
        }
    }

    fn disconnected(&self, events: &MuxEventQueue, reason: String) {
        if self.disconnected.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut reconnect = self.reconnect.lock().expect("rmux reconnect epoch lock");
        reconnect.current = reconnect.current.wrapping_add(1);
        reconnect.pending = Some(reconnect.current);
        events.publish(MuxEventDraft::new(
            MuxEventTopic::BackendDisconnected,
            MuxEventProvenance::RmuxSdk,
            None,
            None,
            MuxEventPayload::Disconnected { reason },
        ));
    }
}

async fn run_event_hub(
    events: MuxEventQueue,
    controls: Arc<Mutex<Option<mpsc::Sender<()>>>>,
    connection: RmuxConnectionState,
) {
    let (control_tx, mut control_rx) = mpsc::channel(1);
    *controls
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(control_tx);
    let mut watched = BTreeMap::new();
    let mut topology_tick = interval(RMUX_TOPOLOGY_RESCAN_INTERVAL);
    topology_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut bootstrapped = false;

    loop {
        let inventory_epoch = connection.inventory_epoch();
        let bootstrap = !bootstrapped;
        match reconcile_panes(
            &events,
            &connection,
            &mut watched,
            inventory_epoch,
            bootstrap,
        )
        .await
        {
            Ok(changed) => {
                if bootstrap {
                    bootstrapped = true;
                } else if changed {
                    events.publish(MuxEventDraft::new(
                        MuxEventTopic::TopologyChanged,
                        MuxEventProvenance::RmuxSdk,
                        None,
                        None,
                        MuxEventPayload::Topology {
                            change: MuxTopologyChange::Mutation,
                        },
                    ));
                }
            }
            Err(error) => {
                connection.disconnected(
                    &events,
                    format!("rmux topology reconciliation failed: {error}"),
                );
                sleep(RMUX_RECONNECT_DELAY).await;
                continue;
            }
        }

        tokio::select! {
            control = control_rx.recv() => {
                if control.is_none() {
                    stop_watchers(&mut watched);
                    return;
                }
            }
            _ = topology_tick.tick() => {}
        }
    }
}

struct PaneWatcher {
    stop: watch::Sender<bool>,
    target: SharedPaneWatchTarget,
}

fn occupant_generation(target: &MuxEventTarget) -> Option<&str> {
    target
        .occupant
        .as_ref()
        .map(|occupant| occupant.backend_identity.as_str())
}

fn update_watcher_target(
    events: &MuxEventQueue,
    watcher: &PaneWatcher,
    target: MuxEventTarget,
) -> bool {
    let mut watched_target = watcher.target.lock().expect("pane watcher target lock");
    let Some((previous, current)) = watched_target.replace_authoritative(target) else {
        return false;
    };
    publish_occupant_replacement(events, &previous, &current);
    true
}

async fn refresh_watcher_target(
    pane: &Pane,
    shared_target: &SharedPaneWatchTarget,
    events: &MuxEventQueue,
) {
    let snapshot = shared_target
        .lock()
        .expect("pane watcher target lock")
        .snapshot();
    let Some((generation, target)) = snapshot else {
        return;
    };
    let target = target_with_live_occupant(pane, target).await;
    let mut watched_target = shared_target.lock().expect("pane watcher target lock");
    let Some((previous, current)) =
        watched_target.replace_live_occupant(generation, target.occupant)
    else {
        return;
    };
    publish_occupant_replacement(events, &previous, &current);
}

fn watcher_is_active(shared_target: &SharedPaneWatchTarget) -> bool {
    shared_target
        .lock()
        .expect("pane watcher target lock")
        .lifecycle
        == PaneWatchLifecycle::Active
}

fn watcher_needs_reconciliation(watcher: &PaneWatcher) -> bool {
    watcher
        .target
        .lock()
        .expect("pane watcher target lock")
        .needs_reconciliation()
}

fn retire_watcher(shared_target: &SharedPaneWatchTarget) -> bool {
    shared_target
        .lock()
        .expect("pane watcher target lock")
        .retire()
}

fn publish_for_active_watcher_target(
    shared_target: &SharedPaneWatchTarget,
    publish: impl FnOnce(&MuxEventTarget),
) -> bool {
    let watched_target = shared_target.lock().expect("pane watcher target lock");
    if watched_target.lifecycle != PaneWatchLifecycle::Active {
        return false;
    }
    publish(&watched_target.target);
    true
}

fn publish_output_for_target(
    events: &MuxEventQueue,
    target: &MuxEventTarget,
    sequence: u64,
    bytes: Vec<u8>,
) {
    events.publish(MuxEventDraft::new(
        MuxEventTopic::TerminalOutput,
        MuxEventProvenance::RmuxSdk,
        Some(target.clone()),
        Some(output_cursor(target, sequence)),
        MuxEventPayload::Output { bytes },
    ));
}

fn publish_output_gap_for_target(
    events: &MuxEventQueue,
    target: &MuxEventTarget,
    expected_sequence: u64,
    resume_sequence: u64,
    missed_events: u64,
) {
    events.publish_gap(
        MuxEventProvenance::RmuxSdk,
        Some(target.clone()),
        Some(output_cursor(target, resume_sequence)),
        expected_sequence,
        resume_sequence,
        missed_events,
    );
}

#[cfg(test)]
fn publish_output_observation(
    events: &MuxEventQueue,
    shared_target: &SharedPaneWatchTarget,
    sequence: u64,
    bytes: Vec<u8>,
) -> bool {
    publish_for_active_watcher_target(shared_target, |target| {
        publish_output_for_target(events, target, sequence, bytes);
    })
}

#[cfg(test)]
fn publish_output_gap_observation(
    events: &MuxEventQueue,
    shared_target: &SharedPaneWatchTarget,
    expected_sequence: u64,
    resume_sequence: u64,
    missed_events: u64,
) -> bool {
    publish_for_active_watcher_target(shared_target, |target| {
        publish_output_gap_for_target(
            events,
            target,
            expected_sequence,
            resume_sequence,
            missed_events,
        );
    })
}

fn close_watcher(
    shared_target: &SharedPaneWatchTarget,
    publish: impl FnOnce(&MuxEventTarget),
) -> bool {
    let mut watched_target = shared_target.lock().expect("pane watcher target lock");
    let Some(target) = watched_target.close() else {
        return false;
    };
    publish(&target);
    true
}

fn publish_stream_closed(
    events: &MuxEventQueue,
    shared_target: &SharedPaneWatchTarget,
    revision: u64,
    reason: String,
) -> bool {
    close_watcher(shared_target, |target| {
        events.publish(MuxEventDraft::new(
            MuxEventTopic::PaneClosed,
            MuxEventProvenance::RmuxSdk,
            Some(target.clone()),
            Some(state_cursor(target, revision)),
            MuxEventPayload::Closed { reason },
        ));
    })
}

fn publish_inventory_closed(events: &MuxEventQueue, shared_target: &SharedPaneWatchTarget) -> bool {
    close_watcher(shared_target, |target| {
        events.publish(MuxEventDraft::new(
            MuxEventTopic::PaneClosed,
            MuxEventProvenance::RmuxSdk,
            Some(target.clone()),
            None,
            MuxEventPayload::Closed {
                reason: "pane absent from authoritative rmux inventory".to_owned(),
            },
        ));
    })
}

async fn reconcile_panes(
    events: &MuxEventQueue,
    connection: &RmuxConnectionState,
    watched: &mut BTreeMap<PaneWatchKey, PaneWatcher>,
    inventory_epoch: u64,
    bootstrap: bool,
) -> Result<bool> {
    let rmux = connect_bootty_rmux().await?;
    let mut inventory = Vec::new();
    for session in rmux.list_sessions().await? {
        for row in list_pane_rows(&rmux, &session).await? {
            if parse_pane_id(&row.pane_id).is_some() {
                inventory.push((session.clone(), row));
            }
        }
    }

    let mut discovered = BTreeSet::new();
    let mut additions = Vec::new();
    let mut replacements = BTreeSet::new();
    for (session, row) in &inventory {
        let key = (row.session_name.clone(), row.pane_id.clone());
        discovered.insert(key.clone());
        if let Some(watcher) = watched.get(&key) {
            if watcher_needs_reconciliation(watcher) {
                replacements.insert(key.clone());
            } else {
                continue;
            }
        }
        let pane_id = parse_pane_id(&row.pane_id).expect("pane id was filtered from inventory");
        let pane = rmux.pane_by_id(session.clone(), pane_id).await?;
        let target = target_with_live_occupant(&pane, target_from_row(row)).await;
        additions.push((key, session.clone(), pane_id, target));
    }

    // List/session/pane acquisition above is complete before this rebase. The following
    // reconciliation may publish pane drafts, so the controller sees the reset first.
    connection.publish_inventory_rebase(events, inventory_epoch, bootstrap);

    let mut changed = false;
    for (_, row) in &inventory {
        let key = (row.session_name.clone(), row.pane_id.clone());
        let Some(watcher) = watched.get(&key) else {
            continue;
        };
        if replacements.contains(&key) {
            continue;
        }
        changed |= update_watcher_target(events, watcher, target_from_row(row));
    }

    let removed = watched
        .keys()
        .filter(|key| !discovered.contains(*key) || replacements.contains(*key))
        .cloned()
        .collect::<Vec<_>>();
    changed |= !additions.is_empty() || !removed.is_empty();
    for key in removed {
        let absent = !discovered.contains(&key);
        let watcher = watched
            .remove(&key)
            .expect("key was collected from the watch map");
        let _ = watcher.stop.send(true);
        if absent {
            let _ = publish_inventory_closed(events, &watcher.target);
        }
    }
    for (key, session, pane_id, target) in additions {
        let target = Arc::new(Mutex::new(PaneWatchTarget::new(target)));
        let (stop, _) = watch::channel(false);
        tokio::spawn(watch_output(
            session.clone(),
            pane_id,
            target.clone(),
            events.clone(),
            connection.clone(),
            stop.clone(),
        ));
        tokio::spawn(watch_state(
            session,
            pane_id,
            target.clone(),
            events.clone(),
            connection.clone(),
            stop.clone(),
        ));
        watched.insert(key, PaneWatcher { stop, target });
    }
    Ok(changed)
}

fn stop_watchers(watched: &mut BTreeMap<PaneWatchKey, PaneWatcher>) {
    for (_, watcher) in std::mem::take(watched) {
        let _ = watcher.stop.send(true);
    }
}

async fn watch_output(
    session: SessionName,
    pane_id: PaneId,
    shared_target: SharedPaneWatchTarget,
    events: MuxEventQueue,
    connection: RmuxConnectionState,
    stop: watch::Sender<bool>,
) {
    let mut stop_rx = stop.subscribe();
    'reconnect: loop {
        if *stop_rx.borrow() {
            return;
        }
        let pane = match open_pane(&session, pane_id).await {
            Ok(pane) => pane,
            Err(error) => {
                if !watcher_is_active(&shared_target) {
                    return;
                }
                connection.disconnected(&events, format!("rmux output connection failed: {error}"));
                if wait_for_reconnect_or_stop(&mut stop_rx).await {
                    return;
                }
                continue;
            }
        };
        refresh_watcher_target(&pane, &shared_target, &events).await;
        if *stop_rx.borrow() || !watcher_is_active(&shared_target) {
            return;
        }
        let mut stream = match pane.output_stream().await {
            Ok(stream) => stream,
            Err(error) => {
                if !watcher_is_active(&shared_target) {
                    return;
                }
                connection
                    .disconnected(&events, format!("rmux output subscription failed: {error}"));
                if wait_for_reconnect_or_stop(&mut stop_rx).await {
                    return;
                }
                continue;
            }
        };
        connection.connected();
        loop {
            refresh_watcher_target(&pane, &shared_target, &events).await;
            if *stop_rx.borrow() || !watcher_is_active(&shared_target) {
                return;
            }
            let Some((_, observed_target)) = shared_target
                .lock()
                .expect("pane watcher target lock")
                .snapshot()
            else {
                return;
            };
            let item = tokio::select! {
                changed = stop_rx.changed() => {
                    if changed.is_err() || *stop_rx.borrow() {
                        return;
                    }
                    continue;
                }
                item = stream.next() => item,
            };
            match item {
                Ok(Some(PaneOutputChunk::Bytes { sequence, bytes })) => {
                    publish_output_for_target(&events, &observed_target, sequence, bytes);
                }
                Ok(Some(PaneOutputChunk::Lag(lag))) => {
                    publish_output_gap_for_target(
                        &events,
                        &observed_target,
                        lag.expected_sequence,
                        lag.resume_sequence,
                        lag.missed_events,
                    );
                }
                Ok(Some(_)) => {
                    let _ = publish_for_active_watcher_target(&shared_target, |_| {
                        events.publish(MuxEventDraft::rebase(
                            MuxEventProvenance::RmuxSdk,
                            MuxRebaseReason::SequenceGap,
                        ));
                    });
                }
                Ok(None) => {
                    if retire_watcher(&shared_target) {
                        topology_invalidated();
                    }
                    let _ = stop.send(true);
                    return;
                }
                Err(error) => {
                    if !watcher_is_active(&shared_target) {
                        return;
                    }
                    connection.disconnected(&events, format!("rmux output stream lost: {error}"));
                    if wait_for_reconnect_or_stop(&mut stop_rx).await {
                        return;
                    }
                    continue 'reconnect;
                }
            }
        }
    }
}

async fn watch_state(
    session: SessionName,
    pane_id: PaneId,
    shared_target: SharedPaneWatchTarget,
    events: MuxEventQueue,
    connection: RmuxConnectionState,
    stop: watch::Sender<bool>,
) {
    let mut stop_rx = stop.subscribe();
    'reconnect: loop {
        if *stop_rx.borrow() {
            return;
        }
        let pane = match open_pane(&session, pane_id).await {
            Ok(pane) => pane,
            Err(error) => {
                if !watcher_is_active(&shared_target) {
                    return;
                }
                connection.disconnected(&events, format!("rmux state connection failed: {error}"));
                if wait_for_reconnect_or_stop(&mut stop_rx).await {
                    return;
                }
                continue;
            }
        };
        refresh_watcher_target(&pane, &shared_target, &events).await;
        if *stop_rx.borrow() || !watcher_is_active(&shared_target) {
            return;
        }
        let mut stream = match pane
            .state_events(PaneStateEventsOptions {
                include_title: true,
                include_options: true,
                include_foreground: true,
            })
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                if !watcher_is_active(&shared_target) {
                    return;
                }
                connection
                    .disconnected(&events, format!("rmux state subscription failed: {error}"));
                if wait_for_reconnect_or_stop(&mut stop_rx).await {
                    return;
                }
                continue;
            }
        };
        connection.connected();
        loop {
            refresh_watcher_target(&pane, &shared_target, &events).await;
            if *stop_rx.borrow() || !watcher_is_active(&shared_target) {
                return;
            }
            let event = tokio::select! {
                changed = stop_rx.changed() => {
                    if changed.is_err() || *stop_rx.borrow() {
                        return;
                    }
                    continue;
                }
                event = stream.next() => match event {
                    Ok(Some(event)) => event,
                    Ok(None) => {
                        if retire_watcher(&shared_target) {
                            topology_invalidated();
                        }
                        let _ = stop.send(true);
                        return;
                    }
                    Err(error) => {
                        if !watcher_is_active(&shared_target) {
                            return;
                        }
                        connection.disconnected(&events, format!("rmux state stream lost: {error}"));
                        if wait_for_reconnect_or_stop(&mut stop_rx).await {
                            return;
                        }
                        continue 'reconnect;
                    }
                },
            };
            if !watcher_is_active(&shared_target) {
                return;
            }
            match event {
                PaneStateEvent::Snapshot {
                    revision,
                    title,
                    options,
                    foreground,
                    ..
                } => {
                    let foreground = foreground.map(foreground_state);
                    let _ = publish_for_active_watcher_target(&shared_target, |target| {
                        let cursor = state_cursor(target, revision);
                        if let Some(cwd) = foreground.as_ref().and_then(|state| state.cwd.clone()) {
                            events.publish(MuxEventDraft::new(
                                MuxEventTopic::PaneCwdChanged,
                                MuxEventProvenance::RmuxSdk,
                                Some(target.clone()),
                                Some(cursor.clone()),
                                MuxEventPayload::Cwd {
                                    old_cwd: None,
                                    new_cwd: Some(cwd),
                                },
                            ));
                        }
                        events.publish(MuxEventDraft::new(
                            MuxEventTopic::PaneStateChanged,
                            MuxEventProvenance::RmuxSdk,
                            Some(target.clone()),
                            Some(cursor),
                            MuxEventPayload::PaneState {
                                state: MuxPaneState {
                                    title,
                                    options: options
                                        .into_iter()
                                        .map(|option| MuxPaneOption {
                                            name: option.name,
                                            value: option.value,
                                        })
                                        .collect(),
                                    foreground,
                                },
                            },
                        ));
                    });
                }
                PaneStateEvent::TitleChanged {
                    revision,
                    old_title,
                    new_title,
                    ..
                } => {
                    let _ = publish_for_active_watcher_target(&shared_target, |target| {
                        events.publish(MuxEventDraft::new(
                            MuxEventTopic::PaneTitleChanged,
                            MuxEventProvenance::RmuxSdk,
                            Some(target.clone()),
                            Some(state_cursor(target, revision)),
                            MuxEventPayload::Title {
                                old_title: Some(old_title),
                                new_title: Some(new_title),
                            },
                        ));
                    });
                }
                PaneStateEvent::OptionSet {
                    revision,
                    name,
                    old_value,
                    new_value,
                    ..
                } => {
                    let _ = publish_for_active_watcher_target(&shared_target, |target| {
                        events.publish(MuxEventDraft::new(
                            MuxEventTopic::PaneOptionsChanged,
                            MuxEventProvenance::RmuxSdk,
                            Some(target.clone()),
                            Some(state_cursor(target, revision)),
                            MuxEventPayload::Option {
                                name,
                                old_value,
                                new_value: Some(new_value),
                            },
                        ));
                    });
                }
                PaneStateEvent::OptionUnset {
                    revision,
                    name,
                    old_value,
                    ..
                } => {
                    let _ = publish_for_active_watcher_target(&shared_target, |target| {
                        events.publish(MuxEventDraft::new(
                            MuxEventTopic::PaneOptionsChanged,
                            MuxEventProvenance::RmuxSdk,
                            Some(target.clone()),
                            Some(state_cursor(target, revision)),
                            MuxEventPayload::Option {
                                name,
                                old_value,
                                new_value: None,
                            },
                        ));
                    });
                }
                PaneStateEvent::ForegroundChanged {
                    revision,
                    old_state,
                    new_state,
                    ..
                } => {
                    let old_state = foreground_state(old_state);
                    let new_state = foreground_state(new_state);
                    let _ = publish_for_active_watcher_target(&shared_target, |target| {
                        let cursor = state_cursor(target, revision);
                        if old_state.cwd != new_state.cwd {
                            events.publish(MuxEventDraft::new(
                                MuxEventTopic::PaneCwdChanged,
                                MuxEventProvenance::RmuxSdk,
                                Some(target.clone()),
                                Some(cursor.clone()),
                                MuxEventPayload::Cwd {
                                    old_cwd: old_state.cwd.clone(),
                                    new_cwd: new_state.cwd.clone(),
                                },
                            ));
                        }
                        events.publish(MuxEventDraft::new(
                            MuxEventTopic::PaneForegroundChanged,
                            MuxEventProvenance::RmuxSdk,
                            Some(target.clone()),
                            Some(cursor),
                            MuxEventPayload::Foreground {
                                old_state: Some(old_state),
                                new_state: Some(new_state),
                            },
                        ));
                    });
                }
                PaneStateEvent::Lagged {
                    missed_from_revision,
                    resume_revision,
                } => {
                    let _ = publish_for_active_watcher_target(&shared_target, |target| {
                        events.publish_gap(
                            MuxEventProvenance::RmuxSdk,
                            Some(target.clone()),
                            Some(state_cursor(target, resume_revision)),
                            missed_from_revision,
                            resume_revision,
                            resume_revision.saturating_sub(missed_from_revision),
                        );
                    });
                }
                PaneStateEvent::Closed {
                    revision, reason, ..
                } => {
                    let _ = publish_stream_closed(
                        &events,
                        &shared_target,
                        revision,
                        format!("{reason:?}"),
                    );
                    topology_invalidated();
                    let _ = stop.send(true);
                    return;
                }
                _ => {
                    let _ = publish_for_active_watcher_target(&shared_target, |_| {
                        events.publish(MuxEventDraft::rebase(
                            MuxEventProvenance::RmuxSdk,
                            MuxRebaseReason::SequenceGap,
                        ));
                    });
                }
            }
        }
    }
}

async fn wait_for_reconnect_or_stop(stop: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        _ = sleep(RMUX_RECONNECT_DELAY) => false,
        changed = stop.changed() => changed.is_err() || *stop.borrow(),
    }
}

fn publish_occupant_replacement(
    events: &MuxEventQueue,
    previous: &MuxEventTarget,
    target: &MuxEventTarget,
) {
    if previous.occupant == target.occupant {
        return;
    }
    events.publish(MuxEventDraft::new(
        MuxEventTopic::PaneOccupantReplaced,
        MuxEventProvenance::RmuxSdk,
        Some(target.clone()),
        None,
        MuxEventPayload::OccupantReplaced {
            old_occupant: previous.occupant.clone(),
            new_occupant: target.occupant.clone(),
        },
    ));
}

async fn open_pane(session: &SessionName, pane_id: PaneId) -> Result<Pane> {
    let rmux = connect_bootty_rmux().await?;
    Ok(rmux.pane_by_id(session.clone(), pane_id).await?)
}

fn target_from_row(row: &RmuxPaneRow) -> MuxEventTarget {
    let mut target = MuxEventTarget {
        session_id: Some(row.session_name.clone()),
        window_id: Some(row.window_id.clone()),
        pane_id: Some(row.pane_id.clone()),
        terminal_id: row.terminal_id.clone(),
        occupant: None,
    };
    target.occupant = row
        .occupant_id
        .as_ref()
        .map(|backend_identity| MuxOccupantIdentity {
            backend_identity: backend_identity.clone(),
            pid: None,
            process: row.process.clone(),
        });
    target
}

async fn target_with_live_occupant(pane: &Pane, mut target: MuxEventTarget) -> MuxEventTarget {
    let Some(pane_id) = target.pane_id.as_deref().and_then(parse_pane_id) else {
        return target;
    };
    let Ok(info) = pane.info().await else {
        return target;
    };
    let Some(info) = info.panes.iter().find(|info| info.id == pane_id) else {
        return target;
    };
    let pid = match &info.process {
        PaneProcessState::Running { pid } => *pid,
        PaneProcessState::Exited | PaneProcessState::Unknown => None,
        _ => None,
    };
    target.occupant = Some(MuxOccupantIdentity {
        backend_identity: format!(
            "rmux:{}:generation:{}",
            target.pane_id.as_deref().unwrap_or_default(),
            info.generation
        ),
        pid,
        process: info.command.as_ref().map(|command| command.join(" ")),
    });
    target
}

fn parse_pane_id(value: &str) -> Option<PaneId> {
    value
        .strip_prefix('%')
        .unwrap_or(value)
        .parse::<u32>()
        .ok()
        .map(PaneId::from)
}

fn output_cursor(target: &MuxEventTarget, sequence: u64) -> MuxEventCursor {
    MuxEventCursor::new(
        format!(
            "rmux-output:{}:{}",
            target.pane_id.as_deref().unwrap_or_default(),
            target
                .occupant
                .as_ref()
                .map_or("", |occupant| occupant.backend_identity.as_str())
        ),
        sequence,
    )
}

fn state_cursor(target: &MuxEventTarget, revision: u64) -> MuxEventCursor {
    MuxEventCursor::new(
        format!(
            "rmux-state:{}:{}",
            target.pane_id.as_deref().unwrap_or_default(),
            target
                .occupant
                .as_ref()
                .map_or("", |occupant| occupant.backend_identity.as_str())
        ),
        revision,
    )
}

fn foreground_state(state: ForegroundState) -> MuxForegroundState {
    MuxForegroundState {
        pid: state.pid,
        command: state.command,
        cwd: state.cwd,
        executable: state.exe,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{BindingId, SpaceId};

    fn scope() -> MuxScope {
        MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(2))
    }

    fn pane_target(window_id: &str, generation: u64) -> MuxEventTarget {
        MuxEventTarget::pane(
            "$1",
            window_id,
            "%3",
            "t3",
            Some(MuxOccupantIdentity {
                backend_identity: format!("rmux:%3:generation:{generation}"),
                pid: Some(10),
                process: Some("shell".to_owned()),
            }),
        )
    }

    fn pane_row(window_id: &str, generation: u64) -> crate::rmux::RmuxPaneRow {
        crate::rmux::RmuxPaneRow {
            session_name: "$1".to_owned(),
            window_id: window_id.to_owned(),
            pane_id: "%3".to_owned(),
            terminal_id: Some("t3".to_owned()),
            index: 0,
            active: true,
            cwd: None,
            process: Some("shell".to_owned()),
            occupant_id: Some(format!("rmux:%3:generation:{generation}")),
        }
    }

    fn pane_watcher(target: MuxEventTarget) -> PaneWatcher {
        let (stop, _) = tokio::sync::watch::channel(false);
        PaneWatcher {
            stop,
            target: std::sync::Arc::new(std::sync::Mutex::new(PaneWatchTarget::new(target))),
        }
    }

    fn publish_state_observation(
        events: &MuxEventQueue,
        shared_target: &SharedPaneWatchTarget,
        revision: u64,
    ) -> bool {
        publish_for_active_watcher_target(shared_target, |target| {
            events.publish(MuxEventDraft::new(
                MuxEventTopic::PaneStateChanged,
                MuxEventProvenance::RmuxSdk,
                Some(target.clone()),
                Some(state_cursor(target, revision)),
                MuxEventPayload::PaneState {
                    state: MuxPaneState::default(),
                },
            ));
        })
    }

    #[test]
    fn reconnect_rebase_waits_for_authoritative_inventory_when_topology_is_unchanged() {
        let events = MuxEventQueue::with_backend_limits("rmux:test", 8, 1024);
        let connection = RmuxConnectionState::default();

        let stale_inventory_epoch = connection.inventory_epoch();
        connection.disconnected(&events, "rmux output transport lost".to_owned());
        connection.publish_inventory_rebase(&events, stale_inventory_epoch, false);

        let disconnected = events.drain(scope(), 8);
        assert_eq!(disconnected.len(), 1);
        assert!(matches!(
            &disconnected[0].payload,
            MuxEventPayload::Disconnected { reason } if reason.contains("transport lost")
        ));
        let inventory_epoch = connection.inventory_epoch();
        connection.connected();
        connection.publish_inventory_rebase(&events, inventory_epoch, false);
        connection.publish_inventory_rebase(&events, inventory_epoch, false);

        let reconnected = events.drain(scope(), 8);
        assert_eq!(reconnected.len(), 1);
        assert!(matches!(
            &reconnected[0].payload,
            MuxEventPayload::Rebase {
                reason: MuxRebaseReason::Reconnect
            }
        ));
    }

    #[test]
    fn cold_start_inventory_rebase_emits_bootstrap_and_keeps_event_continuity() {
        let events = MuxEventQueue::with_backend_limits("rmux:test", 8, 1024);
        let connection = RmuxConnectionState::default();
        let inventory_epoch = connection.inventory_epoch();

        connection.publish_inventory_rebase(&events, inventory_epoch, true);
        connection.publish_inventory_rebase(&events, inventory_epoch, false);

        let bootstrap = events.drain(scope(), 8);
        assert_eq!(bootstrap.len(), 1);
        assert_eq!(bootstrap[0].revision, 1);
        assert_eq!(bootstrap[0].topic, MuxEventTopic::SnapshotRebased);
        assert_eq!(bootstrap[0].provenance, MuxEventProvenance::RmuxSdk);
        assert!(matches!(
            &bootstrap[0].payload,
            MuxEventPayload::Rebase {
                reason: MuxRebaseReason::Bootstrap
            }
        ));
        assert!(!bootstrap.iter().any(|event| {
            matches!(
                &event.payload,
                MuxEventPayload::Disconnected { .. }
                    | MuxEventPayload::Gap { .. }
                    | MuxEventPayload::Rebase {
                        reason: MuxRebaseReason::Reconnect | MuxRebaseReason::SequenceGap
                    }
            )
        }));
        assert_eq!(connection.inventory_epoch(), inventory_epoch);

        events.publish(MuxEventDraft::new(
            MuxEventTopic::TopologyChanged,
            MuxEventProvenance::RmuxSdk,
            None,
            None,
            MuxEventPayload::Topology {
                change: MuxTopologyChange::Mutation,
            },
        ));
        let continuation = events.drain(scope(), 8);
        assert_eq!(continuation.len(), 1);
        assert_eq!(continuation[0].revision, 2);
        assert!(matches!(
            &continuation[0].payload,
            MuxEventPayload::Topology {
                change: MuxTopologyChange::Mutation
            }
        ));
    }

    #[test]
    fn bootstrap_rebase_takes_precedence_over_a_pending_reconnect() {
        let events = MuxEventQueue::with_backend_limits("rmux:test", 8, 1024);
        let connection = RmuxConnectionState::default();

        connection.disconnected(&events, "initial inventory failed".to_owned());
        let disconnected = events.drain(scope(), 8);
        assert_eq!(disconnected.len(), 1);
        assert!(matches!(
            &disconnected[0].payload,
            MuxEventPayload::Disconnected { reason } if reason.contains("initial inventory failed")
        ));

        let inventory_epoch = connection.inventory_epoch();
        connection.publish_inventory_rebase(&events, inventory_epoch, true);
        connection.publish_inventory_rebase(&events, inventory_epoch, false);

        let bootstrap = events.drain(scope(), 8);
        assert_eq!(bootstrap.len(), 1);
        assert_eq!(bootstrap[0].revision, 2);
        assert!(matches!(
            &bootstrap[0].payload,
            MuxEventPayload::Rebase {
                reason: MuxRebaseReason::Bootstrap
            }
        ));
        assert_eq!(connection.inventory_epoch(), inventory_epoch);
    }

    #[test]
    fn pane_stream_eof_retires_only_its_watcher_without_backend_rebase() {
        let events = MuxEventQueue::with_backend_limits("rmux:test", 8, 1024);
        let retired = pane_watcher(pane_target("@retired", 1));
        let active_target = pane_target("@active", 2);
        let active = pane_watcher(active_target.clone());

        assert!(retire_watcher(&retired.target));
        assert!(watcher_needs_reconciliation(&retired));
        assert!(!publish_output_observation(
            &events,
            &retired.target,
            7,
            b"retired".to_vec(),
        ));
        assert!(publish_output_observation(
            &events,
            &active.target,
            8,
            b"active".to_vec(),
        ));

        let observed = events.drain(scope(), 8);
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].target.as_ref(), Some(&active_target));
        assert!(matches!(
            &observed[0].payload,
            MuxEventPayload::Output { bytes } if bytes.as_slice() == b"active"
        ));
        assert!(!observed.iter().any(|event| {
            matches!(
                &event.payload,
                MuxEventPayload::Disconnected { .. }
                    | MuxEventPayload::Rebase {
                        reason: MuxRebaseReason::Reconnect
                    }
            )
        }));
    }

    #[test]
    fn occupant_replacement_keeps_the_complete_pane_target() {
        let events = MuxEventQueue::with_backend_limits("rmux:test", 8, 1024);
        let previous = MuxEventTarget::pane(
            "$1",
            "@2",
            "%3",
            "t3",
            Some(MuxOccupantIdentity {
                backend_identity: "rmux:%3:generation:1".to_owned(),
                pid: Some(10),
                process: Some("shell".to_owned()),
            }),
        );
        let current = MuxEventTarget::pane(
            "$1",
            "@2",
            "%3",
            "t3",
            Some(MuxOccupantIdentity {
                backend_identity: "rmux:%3:generation:2".to_owned(),
                pid: Some(11),
                process: Some("shell".to_owned()),
            }),
        );

        publish_occupant_replacement(&events, &previous, &current);

        let event = events
            .drain(scope(), 8)
            .pop()
            .expect("occupant replacement event");
        assert_eq!(event.topic, MuxEventTopic::PaneOccupantReplaced);
        assert_eq!(event.target, Some(current));
    }

    #[test]
    fn occupant_replacement_rebases_output_and_state_cursor_streams() {
        let generation = |identity: &str| {
            MuxEventTarget::pane(
                "$1",
                "@2",
                "%3",
                "t3",
                Some(MuxOccupantIdentity {
                    backend_identity: identity.to_owned(),
                    pid: None,
                    process: None,
                }),
            )
        };
        let first = generation("rmux:%3:generation:1");
        let second = generation("rmux:%3:generation:2");

        assert_ne!(
            output_cursor(&first, 7).stream,
            output_cursor(&second, 7).stream
        );
        assert_ne!(
            state_cursor(&first, 7).stream,
            state_cursor(&second, 7).stream
        );
    }

    #[test]
    fn cross_window_move_retargets_output_state_and_close_without_stale_target() {
        let events = MuxEventQueue::with_backend_limits("rmux:test", 8, 1024);
        let old_target = pane_target("@old", 1);
        let watcher = pane_watcher(old_target.clone());
        let moved_row = pane_row("@new", 1);
        let mut expected_target = target_from_row(&moved_row);
        expected_target.occupant = old_target.occupant.clone();

        assert!(update_watcher_target(
            &events,
            &watcher,
            target_from_row(&moved_row),
        ));
        assert!(publish_state_observation(&events, &watcher.target, 7));
        assert!(publish_output_observation(
            &events,
            &watcher.target,
            8,
            b"new window".to_vec(),
        ));
        assert!(publish_stream_closed(
            &events,
            &watcher.target,
            9,
            "closed after move".to_owned(),
        ));

        let observed = events.drain(scope(), 8);
        assert_eq!(observed.len(), 3);
        assert!(observed.iter().all(|event| {
            event.target.as_ref() == Some(&expected_target)
                && event
                    .target
                    .as_ref()
                    .and_then(|target| target.window_id.as_deref())
                    == Some("@new")
        }));
        assert!(
            observed
                .iter()
                .all(|event| event.target.as_ref() != Some(&old_target))
        );
        assert!(
            observed
                .iter()
                .any(|event| event.topic == MuxEventTopic::PaneStateChanged)
        );
        assert!(
            observed
                .iter()
                .any(|event| event.topic == MuxEventTopic::TerminalOutput)
        );
        assert!(
            observed
                .iter()
                .any(|event| event.topic == MuxEventTopic::PaneClosed)
        );
    }

    #[test]
    fn output_bytes_and_lag_bind_to_their_observed_occupant_generation() {
        let output_events = MuxEventQueue::with_backend_limits("rmux:test", 8, 1024);
        let old_target = pane_target("@2", 1);
        let output_watcher = pane_watcher(old_target.clone());
        let replacement_row = pane_row("@2", 2);
        let replacement_target = target_from_row(&replacement_row);

        let observed_before_poll = output_watcher
            .target
            .lock()
            .expect("pane watcher target lock")
            .snapshot()
            .expect("active watcher before poll")
            .1;
        assert!(update_watcher_target(
            &output_events,
            &output_watcher,
            target_from_row(&replacement_row),
        ));
        // The stream item belongs to the target captured before `next().await`, even when an
        // occupant replacement completes while that poll is pending.
        publish_output_for_target(&output_events, &observed_before_poll, 7, b"before".to_vec());
        assert!(publish_output_observation(
            &output_events,
            &output_watcher.target,
            10,
            b"after".to_vec(),
        ));

        let output_observed = output_events.drain(scope(), 8);
        let before_output = output_observed
            .iter()
            .find(|event| {
                event.topic == MuxEventTopic::TerminalOutput
                    && event.cursor.as_ref().map(|cursor| cursor.sequence) == Some(7)
            })
            .expect("output observed before replacement");
        let after_output = output_observed
            .iter()
            .find(|event| {
                event.topic == MuxEventTopic::TerminalOutput
                    && event.cursor.as_ref().map(|cursor| cursor.sequence) == Some(10)
            })
            .expect("output observed after replacement");
        let replacement = output_observed
            .iter()
            .find(|event| event.topic == MuxEventTopic::PaneOccupantReplaced)
            .expect("occupant replacement event");
        let before_cursor = output_cursor(&old_target, 7);
        let after_cursor = output_cursor(&replacement_target, 10);

        assert_eq!(before_output.target.as_ref(), Some(&old_target));
        assert_eq!(before_output.cursor.as_ref(), Some(&before_cursor));
        assert_eq!(replacement.target.as_ref(), Some(&replacement_target));
        assert_eq!(after_output.target.as_ref(), Some(&replacement_target));
        assert_eq!(after_output.cursor.as_ref(), Some(&after_cursor));

        let lag_events = MuxEventQueue::with_backend_limits("rmux:test", 8, 1024);
        let lag_watcher = pane_watcher(old_target.clone());
        assert!(publish_output_gap_observation(
            &lag_events,
            &lag_watcher.target,
            8,
            9,
            1,
        ));
        let before_lag = lag_events.drain(scope(), 8);
        let before_gap = before_lag
            .iter()
            .find(|event| {
                matches!(
                    &event.payload,
                    MuxEventPayload::Gap {
                        expected_sequence: 8,
                        resume_sequence: 9,
                        missed_events: 1,
                    }
                )
            })
            .expect("lag observed before replacement");
        let before_gap_cursor = output_cursor(&old_target, 9);
        assert_eq!(before_gap.target.as_ref(), Some(&old_target));
        assert_eq!(before_gap.cursor.as_ref(), Some(&before_gap_cursor));

        assert!(update_watcher_target(
            &lag_events,
            &lag_watcher,
            target_from_row(&replacement_row),
        ));
        assert!(publish_output_gap_observation(
            &lag_events,
            &lag_watcher.target,
            11,
            12,
            1,
        ));
        let after_lag = lag_events.drain(scope(), 8);
        let after_gap = after_lag
            .iter()
            .find(|event| {
                matches!(
                    &event.payload,
                    MuxEventPayload::Gap {
                        expected_sequence: 11,
                        resume_sequence: 12,
                        missed_events: 1,
                    }
                )
            })
            .expect("lag observed after replacement");
        let after_gap_cursor = output_cursor(&replacement_target, 12);
        assert_eq!(after_gap.target.as_ref(), Some(&replacement_target));
        assert_eq!(after_gap.cursor.as_ref(), Some(&after_gap_cursor));
    }

    #[test]
    fn stream_close_suppresses_inventory_duplicate_for_the_exact_occupant() {
        let events = MuxEventQueue::with_backend_limits("rmux:test", 8, 1024);
        let target = pane_target("@2", 3);
        let watcher = pane_watcher(target.clone());

        assert!(publish_stream_closed(
            &events,
            &watcher.target,
            7,
            "stream closed".to_owned(),
        ));
        assert!(!publish_inventory_closed(&events, &watcher.target));

        let observed = events.drain(scope(), 8);
        let closed = observed
            .iter()
            .filter(|event| event.topic == MuxEventTopic::PaneClosed)
            .collect::<Vec<_>>();
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].target.as_ref(), Some(&target));
        assert_eq!(closed[0].cursor.as_ref(), Some(&state_cursor(&target, 7)));
    }
}
