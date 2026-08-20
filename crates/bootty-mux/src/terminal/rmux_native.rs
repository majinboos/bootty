use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use bootty_runtime::{
    DrainStats, TerminalSessionConfig,
    frame_source::TerminalFrameSource,
    terminal_session::{should_publish_frame_after_work, sync_output_suppresses_publish},
};
use bootty_surface::geometry::{CellMetrics, TerminalGeometry};
use bootty_terminal::{
    terminal_engine::{
        NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE, TerminalCopyModeAction, TerminalCopyModeOutcome,
        TerminalEngine, TerminalLiveConfig, TerminalSearchDirection, TerminalSelectionEvent,
        TerminalSelectionFormat, TerminalSideEffectEvent,
    },
    terminal_frame::RenderFrame,
    terminal_input_model::{KeyInput, MouseInput},
    terminal_side_effect::deliver_terminal_side_effects,
};
use rmux_sdk::{PaneOutputChunk, TerminalSizeSpec};

use crate::rmux_bridge::{RmuxPaneEvent, RmuxPaneIo, RmuxPaneTarget, open_rmux_pane_io};
use crate::rmux_remote::open_remote_rmux_pane_io;
use crate::ssh::SshRemote;

use super::pane::{MuxPaneTarget, TerminalRuntime};

const RMUX_MAX_DRAIN_BYTES_PER_TICK: usize = 4 * 1024 * 1024;
const RMUX_MAX_DRAIN_CHUNKS_PER_TICK: usize = 32;
const RMUX_MAX_DRAIN_SLICE_BYTES: usize = 8 * 1024;
const RMUX_MAX_DRAIN_TIME_US: u128 = 20_000;
const RMUX_INPUT_FAST_PATH_DRAIN_BYTES: usize = 64 * 1024;
const RMUX_INPUT_FAST_PATH_DRAIN_CHUNKS: usize = 8;
const RMUX_INPUT_FAST_PATH_DRAIN_TIME_US: u128 = 2_000;
const RMUX_RESTORE_DRAIN_BYTES_PER_TICK: usize = 64 * 1024;
const RMUX_RESTORE_DRAIN_CHUNKS_PER_TICK: usize = 4;
const RMUX_RESTORE_DRAIN_TIME_US: u128 = 2_000;
const RMUX_MAX_COLLECT_BYTES_PER_TICK: usize = 4 * 1024 * 1024;
const RMUX_MAX_COLLECT_CHUNKS_PER_TICK: usize = 256;
const RMUX_WORKER_IDLE_WAIT: Duration = Duration::from_millis(16);
const RMUX_INITIAL_FRAME_AGE: Duration = Duration::from_millis(16);

pub(super) struct RmuxNativeTerminal {
    command_tx: mpsc::Sender<RmuxTerminalCommand>,
    latest_frame: Arc<RmuxPublishedFrame>,
    latest_drain: Arc<Mutex<DrainStats>>,
    pending_output_len: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
    error_rx: mpsc::Receiver<String>,
    geometry: TerminalGeometry,
    display_scale: f32,
    render_cell: CellMetrics,
    needs_initial_resize: bool,
}

struct RmuxPublishedFrame {
    latest: Mutex<Arc<RenderFrame>>,
}

impl RmuxPublishedFrame {
    fn new() -> Self {
        Self {
            latest: Mutex::new(Arc::new(RenderFrame::default())),
        }
    }

    fn load(&self) -> Result<Arc<RenderFrame>> {
        self.latest
            .lock()
            .map(|latest| Arc::clone(&latest))
            .map_err(|_| anyhow::anyhow!("rmux frame cache lock poisoned"))
    }

    fn publish(&self, frame: RenderFrame) -> Result<()> {
        let mut latest = self
            .latest
            .lock()
            .map_err(|_| anyhow::anyhow!("rmux frame cache lock poisoned"))?;
        *latest = Arc::new(frame);
        Ok(())
    }
}

enum RmuxTerminalCommand {
    DisplayScale(f32),
    RenderCellMetrics(CellMetrics),
    Resize(TerminalGeometry),
    ForceResize,
    ApplyLiveConfig(TerminalLiveConfig),
    Key(KeyInput),
    Focus(bool),
    Mouse(MouseInput),
    MouseWheel {
        input: MouseInput,
        scroll_delta: isize,
    },
    Paste(String),
    InputBytes(Vec<u8>),
    MouseViewportScroll {
        delta: isize,
    },
    EnterCopyMode,
    SelectionBegin(TerminalSelectionEvent),
    SelectionUpdate(TerminalSelectionEvent),
    SelectionEnd(Option<TerminalSelectionEvent>),
    FormatSelection {
        format: TerminalSelectionFormat,
        done: mpsc::Sender<std::result::Result<Option<Vec<u8>>, String>>,
    },
    CopyModeActive {
        done: mpsc::Sender<std::result::Result<bool, String>>,
    },
    CopyModeAction {
        action: TerminalCopyModeAction,
        done: mpsc::Sender<std::result::Result<TerminalCopyModeOutcome, String>>,
    },
    SearchViewport {
        query: String,
        direction: TerminalSearchDirection,
        done: mpsc::Sender<std::result::Result<bool, String>>,
    },
    IsMouseTracking {
        done: mpsc::Sender<std::result::Result<bool, String>>,
    },
    DiscardPendingOutput {
        done: mpsc::Sender<std::result::Result<(), String>>,
    },
    Stop,
}

struct RmuxWorkerConfig {
    pane_io: RmuxPaneIo,
    geometry: TerminalGeometry,
    terminal_config: TerminalSessionConfig,
    command_rx: mpsc::Receiver<RmuxTerminalCommand>,
    latest_frame: Arc<RmuxPublishedFrame>,
    latest_drain: Arc<Mutex<DrainStats>>,
    pending_output_len: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
    error_tx: mpsc::Sender<String>,
    repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    waiting_initial_remote_frame: bool,
}
struct RmuxWorkerClosedGuard(Arc<AtomicBool>);

impl Drop for RmuxWorkerClosedGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

struct RmuxWorker {
    pane_io: RmuxPaneIo,
    geometry: TerminalGeometry,
    engine: TerminalEngine,
    command_rx: mpsc::Receiver<RmuxTerminalCommand>,
    pending_command: Option<RmuxTerminalCommand>,
    latest_frame: Arc<RmuxPublishedFrame>,
    latest_drain: Arc<Mutex<DrainStats>>,
    pending_output: RmuxOutputBacklog,
    pending_pre_restore_output: RmuxOutputBacklog,
    pending_restore_output: RmuxOutputBacklog,
    pending_output_len: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
    error_tx: mpsc::Sender<String>,
    repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    side_effect_tx: Option<mpsc::Sender<TerminalSideEffectEvent>>,
    side_effect_pane_id: Option<String>,
    output_buf: Vec<u8>,
    last_frame_publish: Instant,
    has_unpublished_frame: bool,
    force_next_frame_publish: bool,
    sync_output_since: Option<Instant>,
    last_terminal_change: Option<Instant>,
    waiting_initial_remote_frame: bool,
    scroll_bottom_after_output: bool,
    command_disconnected: bool,
    output_closed: bool,
}

struct RmuxOutputBacklog {
    chunks: VecDeque<RmuxPendingOutput>,
    len: usize,
}

struct RmuxPendingOutput {
    bytes: Vec<u8>,
    offset: usize,
}

impl RmuxOutputBacklog {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            chunks: VecDeque::with_capacity(capacity),
            len: 0,
        }
    }

    fn push_back(&mut self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        self.len += bytes.len();
        self.chunks
            .push_back(RmuxPendingOutput { bytes, offset: 0 });
    }

    fn len(&self) -> usize {
        self.len
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn clear(&mut self) {
        self.chunks.clear();
        self.len = 0;
    }

    fn front_len(&self) -> Option<usize> {
        self.chunks
            .front()
            .map(|front| front.bytes.len().saturating_sub(front.offset))
    }

    fn consume_front(&mut self, len: usize, mut consume: impl FnMut(&[u8])) {
        if len == 0 {
            return;
        }
        let mut consumed = 0;
        let mut remove_front = false;
        if let Some(front) = self.chunks.front_mut() {
            let available = front.bytes.len().saturating_sub(front.offset);
            let amount = len.min(available);
            let start = front.offset;
            let end = start + amount;
            consume(&front.bytes[start..end]);
            front.offset = end;
            consumed = amount;
            remove_front = front.offset >= front.bytes.len();
        }
        if remove_front {
            self.chunks.pop_front();
        }
        self.len = self.len.saturating_sub(consumed);
    }
}

#[derive(Default)]
struct RmuxCommandStats {
    did_work: bool,
    terminal_changed: bool,
}

impl RmuxNativeTerminal {
    pub(super) fn new(
        target: MuxPaneTarget,
        remote: Option<&SshRemote>,
        geometry: TerminalGeometry,
        config: TerminalSessionConfig,
        repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    ) -> Result<Self> {
        let pane_target = RmuxPaneTarget::new(
            target.session_id().to_owned(),
            match &target {
                MuxPaneTarget::Pane { pane_id, .. } => Some(pane_id.clone()),
                MuxPaneTarget::Session { .. } => None,
            },
        );
        let restore_lines = rmux_restore_lines(config.max_scrollback, geometry.rows);
        // A remote pane uses the other host's embedded Bootty rmux protocol.
        let pane_io = match remote {
            Some(remote) => open_remote_rmux_pane_io(remote, &pane_target, restore_lines)?,
            None => open_rmux_pane_io(pane_target, restore_lines)?,
        };
        let (command_tx, command_rx) = mpsc::channel();
        let (error_tx, error_rx) = mpsc::channel();
        let waiting_initial_remote_frame = true;
        let latest_frame = Arc::new(RmuxPublishedFrame::new());
        let latest_drain = Arc::new(Mutex::new(DrainStats::default()));
        let pending_output_len = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicBool::new(false));
        spawn_rmux_terminal_worker(RmuxWorkerConfig {
            pane_io,
            geometry,
            terminal_config: config,
            command_rx,
            latest_frame: Arc::clone(&latest_frame),
            latest_drain: Arc::clone(&latest_drain),
            pending_output_len: Arc::clone(&pending_output_len),
            closed: Arc::clone(&closed),
            error_tx,
            repaint_wakeup,
            waiting_initial_remote_frame,
        })?;
        Ok(Self {
            command_tx,
            latest_frame,
            latest_drain,
            pending_output_len,
            closed,
            error_rx,
            geometry,
            display_scale: 1.0,
            render_cell: CellMetrics::new(geometry.cell_width as f32, geometry.cell_height as f32),
            needs_initial_resize: true,
        })
    }

    fn send_command(&mut self, command: RmuxTerminalCommand) -> Result<()> {
        self.check_worker_error()?;
        self.command_tx
            .send(command)
            .map_err(|_| anyhow::anyhow!("rmux terminal worker stopped"))
    }

    fn request<T>(
        &mut self,
        build: impl FnOnce(mpsc::Sender<std::result::Result<T, String>>) -> RmuxTerminalCommand,
    ) -> Result<T> {
        self.check_worker_error()?;
        let (done, response_rx) = mpsc::channel();
        self.command_tx
            .send(build(done))
            .map_err(|_| anyhow::anyhow!("rmux terminal worker stopped"))?;
        response_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("rmux terminal worker stopped"))?
            .map_err(|error| anyhow::anyhow!(error))
    }

    fn queue_resize(&mut self) -> Result<()> {
        self.send_command(RmuxTerminalCommand::Resize(self.geometry))
    }

    fn queue_input(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.send_command(RmuxTerminalCommand::InputBytes(bytes.to_vec()))
    }

    fn check_worker_error(&mut self) -> Result<()> {
        let mut error = None;
        while let Ok(next) = self.error_rx.try_recv() {
            error = Some(next);
        }
        if let Some(error) = error {
            anyhow::bail!(error);
        }
        Ok(())
    }

    fn take_drain_stats(&self) -> DrainStats {
        let Ok(mut stats) = self.latest_drain.lock() else {
            return DrainStats::default();
        };
        let drained = *stats;
        *stats = DrainStats::default();
        drained
    }

    fn write_literal_input(&mut self, bytes: &[u8]) -> Result<()> {
        self.queue_input(bytes)
    }
}

impl TerminalFrameSource for RmuxNativeTerminal {
    fn set_display_scale(&mut self, display_scale: f32) -> Result<()> {
        let display_scale = if display_scale.is_finite() && display_scale > 0.0 {
            display_scale
        } else {
            1.0
        };
        if (self.display_scale - display_scale).abs() <= f32::EPSILON {
            return Ok(());
        }
        self.display_scale = display_scale;
        self.send_command(RmuxTerminalCommand::DisplayScale(display_scale))
    }

    fn set_render_cell_metrics(&mut self, cell: CellMetrics) -> Result<()> {
        if self.render_cell == cell {
            return Ok(());
        }
        self.render_cell = cell;
        self.send_command(RmuxTerminalCommand::RenderCellMetrics(cell))
    }

    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        if self.needs_initial_resize || self.geometry != geometry {
            self.geometry = geometry;
            self.needs_initial_resize = false;
            self.queue_resize()?;
        }
        self.check_worker_error()
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        self.check_worker_error()?;
        self.latest_frame.load()
    }
}

impl TerminalRuntime for RmuxNativeTerminal {
    fn drain_pty(&mut self) -> DrainStats {
        // Keep worker errors for extract_frame, child_exited, or the next command to report.
        self.take_drain_stats()
    }

    fn pending_pty_len(&self) -> usize {
        self.pending_output_len.load(Ordering::Relaxed)
    }

    fn child_exited(&mut self) -> Result<bool> {
        self.check_worker_error()?;
        Ok(self.closed.load(Ordering::Relaxed))
    }

    fn tty_name(&self) -> Option<&str> {
        None
    }

    fn discard_pending_output(&mut self) -> Result<()> {
        self.request(|done| RmuxTerminalCommand::DiscardPendingOutput { done })
    }

    fn force_resize(&mut self) -> Result<()> {
        self.send_command(RmuxTerminalCommand::ForceResize)
    }

    fn format_selection(&mut self, format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
        self.request(|done| RmuxTerminalCommand::FormatSelection { format, done })
    }

    fn current_working_directory(&mut self) -> Result<Option<String>> {
        Ok(None)
    }

    fn apply_live_config(&mut self, config: TerminalLiveConfig) -> Result<()> {
        self.send_command(RmuxTerminalCommand::ApplyLiveConfig(config))
    }

    fn is_mouse_tracking(&mut self) -> Result<bool> {
        self.request(|done| RmuxTerminalCommand::IsMouseTracking { done })
    }

    fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        self.send_command(RmuxTerminalCommand::MouseViewportScroll { delta })
    }

    fn enter_copy_mode(&mut self) -> Result<()> {
        self.send_command(RmuxTerminalCommand::EnterCopyMode)
    }

    fn copy_mode_active(&mut self) -> Result<bool> {
        self.request(|done| RmuxTerminalCommand::CopyModeActive { done })
    }

    fn handle_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome> {
        self.request(|done| RmuxTerminalCommand::CopyModeAction { action, done })
    }

    fn search_viewport(&mut self, query: &str, direction: TerminalSearchDirection) -> Result<bool> {
        self.request(|done| RmuxTerminalCommand::SearchViewport {
            query: query.to_owned(),
            direction,
            done,
        })
    }

    fn begin_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        self.send_command(RmuxTerminalCommand::SelectionBegin(event))
    }

    fn update_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        self.send_command(RmuxTerminalCommand::SelectionUpdate(event))
    }

    fn end_selection(&mut self, event: Option<TerminalSelectionEvent>) -> Result<()> {
        self.send_command(RmuxTerminalCommand::SelectionEnd(event))
    }

    fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        self.write_literal_input(bytes)
    }

    fn write_paste(&mut self, text: &str) -> Result<()> {
        self.send_command(RmuxTerminalCommand::Paste(text.to_owned()))
    }

    fn encode_key(&mut self, input: KeyInput) -> Result<()> {
        self.send_command(RmuxTerminalCommand::Key(input))
    }

    fn encode_focus(&mut self, gained: bool) -> Result<()> {
        self.send_command(RmuxTerminalCommand::Focus(gained))
    }

    fn encode_mouse(&mut self, input: MouseInput) -> Result<()> {
        self.send_command(RmuxTerminalCommand::Mouse(input))
    }

    fn handle_mouse_wheel(&mut self, input: MouseInput, scroll_delta: isize) -> Result<()> {
        self.send_command(RmuxTerminalCommand::MouseWheel {
            input,
            scroll_delta,
        })
    }
}

impl Drop for RmuxNativeTerminal {
    fn drop(&mut self) {
        let _ = self.command_tx.send(RmuxTerminalCommand::Stop);
    }
}

fn spawn_rmux_terminal_worker(config: RmuxWorkerConfig) -> Result<()> {
    let (startup_tx, startup_rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let _closed_guard = RmuxWorkerClosedGuard(Arc::clone(&config.closed));
        let engine = match TerminalEngine::new_with_terminal_options(
            config.geometry,
            config.terminal_config.colors,
            config.terminal_config.cursor,
            config.terminal_config.features,
            config.terminal_config.max_scrollback,
            config.terminal_config.macos_option_as_alt,
        ) {
            Ok(engine) => engine,
            Err(error) => {
                let _ = startup_tx.send(Err(error.to_string()));
                return;
            }
        };
        let worker = RmuxWorker {
            pane_io: config.pane_io,
            geometry: config.geometry,
            engine,
            command_rx: config.command_rx,
            latest_frame: config.latest_frame,
            latest_drain: config.latest_drain,
            pending_output: RmuxOutputBacklog::with_capacity(RMUX_MAX_COLLECT_CHUNKS_PER_TICK),
            pending_pre_restore_output: RmuxOutputBacklog::with_capacity(
                RMUX_MAX_COLLECT_CHUNKS_PER_TICK,
            ),
            pending_restore_output: RmuxOutputBacklog::with_capacity(1),
            pending_output_len: config.pending_output_len,
            closed: config.closed,
            error_tx: config.error_tx,
            repaint_wakeup: config.repaint_wakeup,
            side_effect_tx: config.terminal_config.side_effect_tx,
            side_effect_pane_id: config.terminal_config.side_effect_pane_id,
            output_buf: Vec::with_capacity(1024),
            last_frame_publish: Instant::now() - RMUX_INITIAL_FRAME_AGE,
            has_unpublished_frame: false,
            force_next_frame_publish: false,
            sync_output_since: None,
            last_terminal_change: None,
            waiting_initial_remote_frame: config.waiting_initial_remote_frame,
            scroll_bottom_after_output: false,
            command_disconnected: false,
            pending_command: None,
            output_closed: false,
        };
        let _ = startup_tx.send(Ok(()));
        worker.run();
    });

    startup_rx
        .recv()
        .map_err(|_| anyhow::anyhow!("rmux terminal worker failed to start"))?
        .map_err(|error| anyhow::anyhow!(error))
}

impl RmuxWorker {
    fn run(mut self) {
        loop {
            let command_stats = self.process_commands();
            let mut did_work = command_stats.did_work;
            let mut terminal_changed = command_stats.terminal_changed;
            did_work |= self.collect_pane_output();
            let stats = self.drain_pending_output();
            if stats.bytes > 0 && self.pending_restore_output.is_empty() {
                self.waiting_initial_remote_frame = false;
            }
            terminal_changed |= stats.bytes > 0;
            did_work |= stats.bytes > 0;
            self.drain_input_results();
            self.forward_side_effects();

            if terminal_changed {
                self.mark_unpublished_frame();
            }

            if did_work {
                self.publish_drain(stats);
                if self.should_publish_frame() {
                    self.publish_frame();
                    self.last_frame_publish = Instant::now();
                }
            } else {
                if self.should_publish_frame() {
                    self.publish_frame();
                    self.last_frame_publish = Instant::now();
                    continue;
                }
                if self.should_stop() {
                    break;
                }
                match self.command_rx.recv_timeout(RMUX_WORKER_IDLE_WAIT) {
                    Ok(command) => self.pending_command = Some(command),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        self.command_disconnected = true;
                    }
                }
            }
        }
    }

    fn process_commands(&mut self) -> RmuxCommandStats {
        let mut stats = RmuxCommandStats::default();
        loop {
            let command = if let Some(command) = self.pending_command.take() {
                command
            } else {
                match self.command_rx.try_recv() {
                    Ok(command) => command,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.command_disconnected = true;
                        break;
                    }
                }
            };
            stats.did_work = true;
            match command {
                RmuxTerminalCommand::DisplayScale(display_scale) => {
                    self.engine.set_display_scale(display_scale);
                    self.mark_unpublished_frame();
                }
                RmuxTerminalCommand::RenderCellMetrics(cell) => {
                    self.engine.set_render_cell_metrics(cell);
                    self.mark_unpublished_frame();
                }
                RmuxTerminalCommand::Resize(geometry) => {
                    self.force_next_frame_publish = true;
                    self.geometry = geometry;
                    self.queue_resize(geometry);
                    if self.engine.resize(geometry).is_ok() {
                        stats.terminal_changed = true;
                    }
                }
                RmuxTerminalCommand::ForceResize => {
                    self.force_next_frame_publish = true;
                    self.queue_resize(self.geometry);
                    stats.terminal_changed = true;
                }
                RmuxTerminalCommand::ApplyLiveConfig(config) => {
                    match self.engine.apply_live_config(config) {
                        Ok(()) => stats.terminal_changed = true,
                        Err(error) => self.send_error(error),
                    }
                }
                RmuxTerminalCommand::Key(input) => {
                    self.mark_input_fast_path();
                    self.engine.scroll_viewport_bottom();
                    stats.terminal_changed = true;
                    if self
                        .engine
                        .encode_key_to_vec(input, &mut self.output_buf)
                        .is_ok()
                    {
                        self.write_output_buf();
                    }
                }
                RmuxTerminalCommand::Focus(gained) => {
                    self.mark_input_fast_path();
                    if self
                        .engine
                        .encode_focus_to_vec(gained, &mut self.output_buf)
                        .is_ok()
                    {
                        self.write_output_buf();
                    }
                }
                RmuxTerminalCommand::Mouse(input) => {
                    self.mark_input_fast_path();
                    if self
                        .engine
                        .encode_mouse_to_vec(input, &mut self.output_buf)
                        .is_ok()
                    {
                        self.write_output_buf();
                    }
                }
                RmuxTerminalCommand::MouseWheel {
                    input,
                    scroll_delta,
                } => match self.engine.is_mouse_tracking() {
                    Ok(true) => {
                        self.mark_input_fast_path();
                        if self
                            .engine
                            .encode_mouse_wheel_to_vec(
                                input,
                                scroll_delta.unsigned_abs().max(1),
                                &mut self.output_buf,
                            )
                            .is_ok()
                        {
                            self.write_output_buf();
                        }
                    }
                    Ok(false) if scroll_delta != 0 => {
                        self.mark_input_fast_path();
                        self.engine.scroll_viewport_delta(scroll_delta);
                        stats.terminal_changed = true;
                    }
                    Ok(false) => {}
                    Err(error) => self.send_error(error),
                },
                RmuxTerminalCommand::Paste(text) => {
                    self.mark_input_fast_path();
                    self.engine.scroll_viewport_bottom();
                    stats.terminal_changed = true;
                    if self
                        .engine
                        .encode_paste_to_vec(&text, &mut self.output_buf)
                        .is_ok()
                    {
                        self.write_output_buf();
                    }
                }
                RmuxTerminalCommand::InputBytes(bytes) => {
                    self.mark_input_fast_path();
                    self.engine.scroll_viewport_bottom();
                    stats.terminal_changed = true;
                    self.queue_input(&bytes);
                }
                RmuxTerminalCommand::MouseViewportScroll { delta } => {
                    self.mark_input_fast_path();
                    self.engine.scroll_viewport_delta(delta);
                    stats.terminal_changed = true;
                }
                RmuxTerminalCommand::EnterCopyMode => {
                    self.mark_input_fast_path();
                    if self.engine.enter_copy_mode().is_ok() {
                        stats.terminal_changed = true;
                    }
                }
                RmuxTerminalCommand::SelectionBegin(event) => {
                    self.mark_input_fast_path();
                    if self.engine.begin_selection(event).is_ok() {
                        stats.terminal_changed = true;
                    }
                }
                RmuxTerminalCommand::SelectionUpdate(event) => {
                    self.mark_input_fast_path();
                    if self.engine.update_selection(event).is_ok() {
                        stats.terminal_changed = true;
                    }
                }
                RmuxTerminalCommand::SelectionEnd(event) => {
                    self.mark_input_fast_path();
                    if self.engine.end_selection(event).is_ok() {
                        stats.terminal_changed = true;
                    }
                }
                RmuxTerminalCommand::FormatSelection { format, done } => {
                    let response = self
                        .engine
                        .format_selection(format)
                        .map_err(|error| error.to_string());
                    let _ = done.send(response);
                }
                RmuxTerminalCommand::CopyModeActive { done } => {
                    let _ = done.send(Ok(self.engine.copy_mode_active()));
                }
                RmuxTerminalCommand::CopyModeAction { action, done } => {
                    self.mark_input_fast_path();
                    let response = self
                        .engine
                        .handle_copy_mode_action(action)
                        .map_err(|error| error.to_string());
                    stats.terminal_changed = true;
                    let _ = done.send(response);
                }
                RmuxTerminalCommand::SearchViewport {
                    query,
                    direction,
                    done,
                } => {
                    self.mark_input_fast_path();
                    let response = self
                        .engine
                        .search_viewport(&query, direction)
                        .map_err(|error| error.to_string());
                    stats.terminal_changed = true;
                    let _ = done.send(response);
                }
                RmuxTerminalCommand::IsMouseTracking { done } => {
                    let response = self
                        .engine
                        .is_mouse_tracking()
                        .map_err(|error| error.to_string());
                    let _ = done.send(response);
                }
                RmuxTerminalCommand::DiscardPendingOutput { done } => {
                    self.pending_output.clear();
                    self.pending_restore_output.clear();
                    self.pending_output_len.store(0, Ordering::Relaxed);
                    self.has_unpublished_frame = false;
                    let _ = done.send(Ok(()));
                }
                RmuxTerminalCommand::Stop => {
                    self.command_disconnected = true;
                    break;
                }
            }
        }
        stats
    }

    fn collect_pane_output(&mut self) -> bool {
        let mut did_work = false;
        let mut collected_bytes = 0;
        let mut collected_chunks = 0;
        while collected_chunks < RMUX_MAX_COLLECT_CHUNKS_PER_TICK
            && collected_bytes < RMUX_MAX_COLLECT_BYTES_PER_TICK
        {
            let event = match self.pane_io.output_rx.try_recv() {
                Ok(event) => event,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.output_closed = true;
                    self.closed.store(true, Ordering::Relaxed);
                    break;
                }
            };
            did_work = true;
            match event {
                RmuxPaneEvent::Restore {
                    buffered_chunks,
                    capture,
                } => {
                    for chunk in buffered_chunks {
                        collected_chunks += 1;
                        if let Some(bytes) = pane_output_chunk_bytes(chunk) {
                            collected_bytes += bytes.len();
                            self.pending_pre_restore_output.push_back(bytes);
                        }
                    }
                    if !capture.is_empty() {
                        collected_chunks += 1;
                        let mut bytes = normalize_capture_newlines(&capture);
                        // A captured snapshot is complete even when live output was cut mid-redraw.
                        bytes.extend_from_slice(b"\x1b[?2026l");
                        collected_bytes += bytes.len();
                        self.push_pending_restore_output(bytes);
                    }
                    self.scroll_bottom_after_output = true;
                }
                RmuxPaneEvent::Chunks(chunks) => {
                    for chunk in chunks {
                        collected_chunks += 1;
                        if let Some(bytes) = pane_output_chunk_bytes(chunk) {
                            collected_bytes += bytes.len();
                            self.push_pending_output(bytes);
                        }
                    }
                }
                RmuxPaneEvent::KeyboardProtocol(bytes) => {
                    self.engine.write_vt(&bytes);
                }
                RmuxPaneEvent::Error(error) => {
                    self.send_error(anyhow::anyhow!(error));
                    self.output_closed = true;
                    self.closed.store(true, Ordering::Relaxed);
                    break;
                }
            }
        }
        did_work
    }

    fn push_pending_output(&mut self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        self.pending_output.push_back(bytes);
        self.update_pending_output_len();
    }

    fn push_pending_restore_output(&mut self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        self.pending_restore_output.push_back(bytes);
        self.update_pending_output_len();
    }

    fn discard_pending_restore_output(&mut self) {
        if self.pending_restore_output.is_empty() {
            return;
        }
        self.pending_restore_output.clear();
        self.update_pending_output_len();
    }

    fn total_pending_output_len(&self) -> usize {
        self.pending_output
            .len()
            .saturating_add(self.pending_pre_restore_output.len())
            .saturating_add(self.pending_restore_output.len())
    }

    fn update_pending_output_len(&self) {
        self.pending_output_len
            .store(self.total_pending_output_len(), Ordering::Relaxed);
    }

    fn drain_pending_output(&mut self) -> DrainStats {
        let (max_bytes, max_chunks, max_time_us) = if self.force_next_frame_publish {
            (
                RMUX_INPUT_FAST_PATH_DRAIN_BYTES,
                RMUX_INPUT_FAST_PATH_DRAIN_CHUNKS,
                RMUX_INPUT_FAST_PATH_DRAIN_TIME_US,
            )
        } else {
            (
                RMUX_MAX_DRAIN_BYTES_PER_TICK,
                RMUX_MAX_DRAIN_CHUNKS_PER_TICK,
                RMUX_MAX_DRAIN_TIME_US,
            )
        };
        let stats = drain_rmux_pending_output(
            &mut self.engine,
            &mut self.pending_pre_restore_output,
            &mut self.pending_restore_output,
            &mut self.pending_output,
            max_bytes,
            max_chunks,
            max_time_us,
        );
        if stats.bytes > 0 && self.scroll_bottom_after_output {
            self.engine.scroll_viewport_bottom();
        }
        if self.pending_output.is_empty()
            && self.pending_pre_restore_output.is_empty()
            && self.pending_restore_output.is_empty()
        {
            self.scroll_bottom_after_output = false;
        }
        if stats.bytes > 0 {
            self.update_pending_output_len();
        }
        stats
    }

    fn drain_input_results(&mut self) {
        while let Ok(result) = self.pane_io.result_rx.try_recv() {
            if let Err(error) = result {
                self.send_error(anyhow::anyhow!(error));
            }
        }
    }

    fn forward_side_effects(&mut self) {
        deliver_terminal_side_effects(
            &mut self.side_effect_tx,
            &self.side_effect_pane_id,
            self.engine.drain_side_effects(),
        );
    }

    fn publish_drain(&self, stats: DrainStats) {
        if let Ok(mut latest) = self.latest_drain.lock() {
            latest.chunks = latest.chunks.saturating_add(stats.chunks);
            latest.bytes = latest.bytes.saturating_add(stats.bytes);
            latest.elapsed_us = latest.elapsed_us.saturating_add(stats.elapsed_us);
        }
    }

    fn should_publish_frame(&mut self) -> bool {
        if self.waiting_initial_remote_frame {
            return false;
        }
        let sync_output_suppressed = self.sync_output_suppressed();
        should_publish_frame_after_work(
            self.has_unpublished_frame,
            self.force_next_frame_publish,
            sync_output_suppressed,
            self.total_pending_output_len(),
            self.last_terminal_change
                .map(|instant| instant.elapsed())
                .unwrap_or(Duration::ZERO),
            self.last_frame_publish.elapsed(),
        )
    }

    fn sync_output_suppressed(&mut self) -> bool {
        if !self.engine.is_synchronized_output().unwrap_or(false) {
            self.sync_output_since = None;
            return false;
        }
        let since = *self.sync_output_since.get_or_insert_with(Instant::now);
        sync_output_suppresses_publish(true, false, since.elapsed())
    }

    fn publish_frame(&mut self) {
        let Ok(frame) = self.engine.extract_frame() else {
            return;
        };
        if self.latest_frame.publish(frame.clone()).is_ok() {
            self.force_next_frame_publish = false;
            self.has_unpublished_frame = false;
            (self.repaint_wakeup)();
        }
    }

    fn mark_unpublished_frame(&mut self) {
        self.has_unpublished_frame = true;
        self.last_terminal_change = Some(Instant::now());
    }

    fn mark_input_fast_path(&mut self) {
        self.discard_pending_restore_output();
        self.waiting_initial_remote_frame = false;
        self.force_next_frame_publish = true;
    }

    fn should_stop(&self) -> bool {
        self.command_disconnected || (self.output_closed && self.total_pending_output_len() == 0)
    }

    fn queue_resize(&mut self, geometry: TerminalGeometry) {
        if self
            .pane_io
            .resize_tx
            .send(TerminalSizeSpec::new(geometry.cols, geometry.rows))
            .is_err()
        {
            self.send_error(anyhow::anyhow!("rmux resize queue stopped"));
        }
    }

    fn queue_input(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if self.pane_io.input_tx.send(bytes.to_vec()).is_err() {
            self.send_error(anyhow::anyhow!("rmux input queue stopped"));
        }
    }

    fn write_literal_input(&mut self, bytes: &[u8]) {
        self.queue_input(bytes);
    }

    fn write_output_buf(&mut self) {
        if self.output_buf.is_empty() {
            return;
        }
        let bytes = std::mem::take(&mut self.output_buf);
        self.write_literal_input(&bytes);
    }

    fn send_error(&self, error: anyhow::Error) {
        let _ = self.error_tx.send(error.to_string());
    }
}

fn normalize_capture_newlines(bytes: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut previous = None;
    for byte in bytes {
        if *byte == b'\n' && previous != Some(b'\r') {
            normalized.push(b'\r');
        }
        normalized.push(*byte);
        previous = Some(*byte);
    }
    normalized
}

fn pane_output_chunk_bytes(chunk: PaneOutputChunk) -> Option<Vec<u8>> {
    match chunk {
        PaneOutputChunk::Bytes { bytes, .. } => Some(bytes),
        PaneOutputChunk::Lag(lag) if !lag.recent.bytes.is_empty() => Some(lag.recent.bytes),
        PaneOutputChunk::Lag(_) => None,
        _ => None,
    }
}

fn rmux_restore_lines(max_scrollback_bytes: usize, viewport_rows: u16) -> usize {
    let viewport_rows = usize::from(viewport_rows);
    if max_scrollback_bytes == 0 {
        return viewport_rows;
    }
    let scrollback_rows = max_scrollback_bytes.div_ceil(NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE);
    viewport_rows.saturating_add(scrollback_rows)
}

fn drain_rmux_pending_output(
    engine: &mut TerminalEngine,
    pre_restore: &mut RmuxOutputBacklog,
    restore: &mut RmuxOutputBacklog,
    live: &mut RmuxOutputBacklog,
    max_bytes: usize,
    max_chunks: usize,
    max_time_us: u128,
) -> DrainStats {
    if !pre_restore.is_empty() {
        return drain_rmux_output_backlog_with_limits(
            pre_restore,
            max_bytes,
            max_chunks,
            max_time_us,
            |bytes| engine.write_vt(bytes),
        );
    }
    if !restore.is_empty() {
        return drain_rmux_output_backlog_with_limits(
            restore,
            RMUX_RESTORE_DRAIN_BYTES_PER_TICK,
            RMUX_RESTORE_DRAIN_CHUNKS_PER_TICK,
            RMUX_RESTORE_DRAIN_TIME_US,
            |bytes| engine.write_vt(bytes),
        );
    }
    drain_rmux_output_backlog_with_limits(live, max_bytes, max_chunks, max_time_us, |bytes| {
        engine.write_vt(bytes)
    })
}

fn drain_rmux_output_backlog_with_limits(
    backlog: &mut RmuxOutputBacklog,
    max_bytes: usize,
    max_chunks: usize,
    max_time_us: u128,
    mut write: impl FnMut(&[u8]),
) -> DrainStats {
    let start = Instant::now();
    let mut stats = DrainStats::default();

    while !backlog.is_empty()
        && stats.bytes < max_bytes
        && stats.chunks < max_chunks
        && start.elapsed().as_micros() < max_time_us
    {
        let Some(available) = backlog.front_len() else {
            backlog.clear();
            break;
        };
        let remaining_bytes = max_bytes.saturating_sub(stats.bytes);
        let consumed = available
            .min(remaining_bytes)
            .min(RMUX_MAX_DRAIN_SLICE_BYTES);
        if consumed == 0 {
            break;
        }

        stats.chunks += 1;
        backlog.consume_front(consumed, |bytes| write(bytes));
        stats.bytes += consumed;
    }

    stats.elapsed_us = start.elapsed().as_micros() as u64;
    stats
}
