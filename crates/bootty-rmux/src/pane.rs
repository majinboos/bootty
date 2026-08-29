use std::{
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
    DrainStats, OutputBacklog, TerminalSessionConfig, drain_output_backlog,
    drain_output_backlog_with_limits,
    frame_source::TerminalFrameSource,
    terminal_session::{
        WorkerRequest, should_publish_frame_after_work, sync_output_suppresses_publish,
        worker_request,
    },
};
use bootty_surface::geometry::{CellMetrics, TerminalGeometry};
use bootty_terminal::{
    terminal_engine::{
        TerminalCopyModeAction, TerminalCopyModeOutcome, TerminalEngine, TerminalLiveConfig,
        TerminalSearchDirection, TerminalSelectionEvent, TerminalSelectionFormat,
        TerminalSideEffectEvent,
    },
    terminal_frame::RenderFrame,
    terminal_input_model::{KeyInput, MouseInput},
    terminal_side_effect::deliver_terminal_side_effects,
};
use rmux_sdk::TerminalSizeSpec;

use crate::bridge::{rmux_missing_target_text, rmux_stale_target_text};
use crate::pane_io::{RmuxPaneEvent, RmuxPaneIo, RmuxPaneTarget, open_rmux_pane_io};
use crate::remote::open_remote_rmux_pane_io;
use bootty_remote::ssh::SshRemote;

use bootty_mux::terminal::{
    BackendPanePolicy, MuxPaneTarget, PaneLayoutResizeRequest, PaneStartRequest,
    ScopedMuxPaneTarget, TerminalRuntime,
};

const RMUX_MAX_PENDING_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const RMUX_INPUT_FAST_PATH_DRAIN_BYTES: usize = 64 * 1024;
const RMUX_INPUT_FAST_PATH_DRAIN_CHUNKS: usize = 8;
const RMUX_INPUT_FAST_PATH_DRAIN_TIME_US: u128 = 2_000;
const RMUX_MAX_COLLECT_BYTES_PER_TICK: usize = 4 * 1024 * 1024;
const RMUX_MAX_COLLECT_CHUNKS_PER_TICK: usize = 256;
const RMUX_WORKER_IDLE_WAIT: Duration = Duration::from_millis(16);
const RMUX_INITIAL_FRAME_AGE: Duration = Duration::from_millis(16);

struct RmuxNativeTerminal {
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
        done: WorkerRequest<std::result::Result<Option<Vec<u8>>, String>>,
    },
    CopyModeActive {
        done: WorkerRequest<std::result::Result<bool, String>>,
    },
    CopyModeAction {
        action: TerminalCopyModeAction,
        done: WorkerRequest<std::result::Result<TerminalCopyModeOutcome, String>>,
    },
    SearchViewport {
        query: String,
        direction: TerminalSearchDirection,
        done: WorkerRequest<std::result::Result<bool, String>>,
    },
    IsMouseTracking {
        done: WorkerRequest<std::result::Result<bool, String>>,
    },
    DiscardPendingOutput {
        done: WorkerRequest<std::result::Result<(), String>>,
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
    pending_output: OutputBacklog,
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
    command_disconnected: bool,
    output_closed: bool,
}

impl RmuxNativeTerminal {
    fn new(
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
        // A remote pane uses the other host's embedded Bootty rmux protocol.
        let pane_io = match remote {
            Some(remote) => open_remote_rmux_pane_io(remote, &pane_target)?,
            None => open_rmux_pane_io(pane_target)?,
        };
        let (command_tx, command_rx) = mpsc::channel();
        let (error_tx, error_rx) = mpsc::channel();
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
            waiting_initial_remote_frame: true,
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
        operation: &'static str,
        build: impl FnOnce(WorkerRequest<std::result::Result<T, String>>) -> RmuxTerminalCommand,
    ) -> Result<T> {
        self.check_worker_error()?;
        let (done, response) = worker_request();
        self.command_tx
            .send(build(done))
            .map_err(|_| anyhow::anyhow!("rmux terminal worker stopped"))?;
        response
            .receive(operation)?
            .map_err(|error| anyhow::anyhow!(error))
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
}

struct RmuxWindowResizeRequest {
    window_id: String,
    cols: u16,
    rows: u16,
}

struct RmuxWindowResizeWorker {
    tx: mpsc::Sender<RmuxWindowResizeRequest>,
    result_rx: mpsc::Receiver<std::result::Result<(), String>>,
}

/// How long to wait before re-driving a resize at a window the daemon will not
/// resolve. Re-driving on every paint would enumerate every session at frame
/// rate, and giving up entirely would leave a window that comes back listed at
/// the wrong size forever, so back off to this instead.
const UNRESOLVED_WINDOW_RESIZE_RETRY: Duration = Duration::from_millis(500);

pub struct RmuxPanePolicy {
    remote: Option<SshRemote>,
    window_id: Option<String>,
    last_window_size: Option<(String, u16, u16)>,
    unresolved_window_resize_at: Option<Instant>,
    resize_worker: Option<RmuxWindowResizeWorker>,
}

impl RmuxPanePolicy {
    pub fn new(remote: Option<SshRemote>) -> Self {
        Self {
            remote,
            window_id: None,
            last_window_size: None,
            unresolved_window_resize_at: None,
            resize_worker: None,
        }
    }

    fn ensure_resize_worker(&mut self, repaint_wakeup: &Arc<dyn Fn() + Send + Sync + 'static>) {
        if self.resize_worker.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel::<RmuxWindowResizeRequest>();
        let (result_tx, result_rx) = mpsc::channel::<std::result::Result<(), String>>();
        let repaint = Arc::clone(repaint_wakeup);
        thread::spawn(move || {
            while let Ok(mut request) = rx.recv() {
                while let Ok(next) = rx.try_recv() {
                    request = next;
                }
                let result = crate::backend::resize_bootty_rmux_window(
                    &request.window_id,
                    request.cols,
                    request.rows,
                )
                .map_err(|error| error.to_string());
                let _ = result_tx.send(result);
                repaint();
            }
        });
        self.resize_worker = Some(RmuxWindowResizeWorker { tx, result_rx });
    }

    fn drain_resize_results(&mut self) -> Result<bool> {
        let mut completed = false;
        let mut error = None;
        if let Some(worker) = &self.resize_worker {
            while let Ok(result) = worker.result_rx.try_recv() {
                // Results arrive in order, so a later success retires an
                // earlier rejection rather than leaving it to be acted on.
                match result {
                    Ok(()) => {
                        completed = true;
                        error = None;
                        self.unresolved_window_resize_at = None;
                    }
                    Err(result_error) => error = Some(result_error),
                }
            }
        }
        if let Some(error) = error {
            if rmux_stale_target_text(&error) {
                // The window still exists, its index or listing moved under the
                // request. Forget the requested size so a later paint re-drives
                // it, rate limited so a window that never resolves again does
                // not enumerate every session on every frame.
                let now = Instant::now();
                if self
                    .unresolved_window_resize_at
                    .is_none_or(|at| now.duration_since(at) >= UNRESOLVED_WINDOW_RESIZE_RETRY)
                {
                    self.unresolved_window_resize_at = Some(now);
                    self.last_window_size = None;
                }
                return Ok(completed);
            }
            if rmux_missing_target_text(&error) {
                // Teardown, not a terminal error. Keep the requested size so the
                // paint path does not re-send a resize the daemon cannot route.
                // The window stays at its old size until the layout asks for
                // different dimensions.
                return Ok(completed);
            }
            self.last_window_size = None;
            anyhow::bail!(error);
        }
        Ok(completed)
    }
}

impl BackendPanePolicy for RmuxPanePolicy {
    fn remote_target(&self) -> Option<&bootty_mux_model::SshTarget> {
        self.remote.as_ref().map(SshRemote::target)
    }

    fn start_terminal(
        &mut self,
        request: PaneStartRequest<'_>,
    ) -> Result<Option<Box<dyn TerminalRuntime>>> {
        let mut config = request.terminal_config.clone();
        config.side_effect_pane_id = request.target.side_effect_pane_id();
        Ok(Some(Box::new(RmuxNativeTerminal::new(
            request.target.mux_target().clone(),
            self.remote.as_ref(),
            request.spawn_geometry,
            config,
            Arc::clone(request.repaint_wakeup),
        )?)))
    }

    fn sync_target(&mut self, _target: Option<&ScopedMuxPaneTarget>, _hide_tmux_status: bool) {}

    fn set_layout_window(&mut self, window_id: Option<&str>) {
        if self.window_id.as_deref() != window_id {
            self.window_id = window_id.map(str::to_owned);
            self.last_window_size = None;
            // The backoff is per window: a window that stopped resolving must not
            // delay the next one's first retry.
            self.unresolved_window_resize_at = None;
        }
    }

    fn resize_layout_window(&mut self, request: PaneLayoutResizeRequest<'_>) -> Result<bool> {
        let completed = self.drain_resize_results()?;
        let Some(window_id) = request.window_id else {
            return Ok(completed);
        };
        let requested = (window_id.to_owned(), request.cols, request.rows);
        if self.last_window_size.as_ref() == Some(&requested) {
            return Ok(completed);
        }
        self.ensure_resize_worker(request.repaint_wakeup);
        let Some(worker) = &self.resize_worker else {
            anyhow::bail!("rmux window resize worker did not start");
        };
        worker
            .tx
            .send(RmuxWindowResizeRequest {
                window_id: window_id.to_owned(),
                cols: request.cols,
                rows: request.rows,
            })
            .map_err(|_| anyhow::anyhow!("rmux window resize worker stopped"))?;
        self.last_window_size = Some(requested);
        Ok(completed)
    }

    fn deactivate(&mut self) {}
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
            self.send_command(RmuxTerminalCommand::Resize(self.geometry))?;
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
        self.request("discarding output", |done| {
            RmuxTerminalCommand::DiscardPendingOutput { done }
        })
    }

    fn force_resize(&mut self) -> Result<()> {
        if self.closed.load(Ordering::Relaxed) {
            return Ok(());
        }
        self.send_command(RmuxTerminalCommand::ForceResize)
    }

    fn format_selection(&mut self, format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
        self.request("formatting selection", |done| {
            RmuxTerminalCommand::FormatSelection { format, done }
        })
    }

    fn current_working_directory(&mut self) -> Result<Option<String>> {
        Ok(None)
    }

    fn apply_live_config(&mut self, config: TerminalLiveConfig) -> Result<()> {
        self.send_command(RmuxTerminalCommand::ApplyLiveConfig(config))
    }

    fn is_mouse_tracking(&mut self) -> Result<bool> {
        self.request("reporting mouse tracking", |done| {
            RmuxTerminalCommand::IsMouseTracking { done }
        })
    }

    fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        self.send_command(RmuxTerminalCommand::MouseViewportScroll { delta })
    }

    fn enter_copy_mode(&mut self) -> Result<()> {
        self.send_command(RmuxTerminalCommand::EnterCopyMode)
    }

    fn copy_mode_active(&mut self) -> Result<bool> {
        self.request("reporting copy mode", |done| {
            RmuxTerminalCommand::CopyModeActive { done }
        })
    }

    fn handle_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome> {
        self.request("handling copy mode action", |done| {
            RmuxTerminalCommand::CopyModeAction { action, done }
        })
    }

    fn search_viewport(&mut self, query: &str, direction: TerminalSearchDirection) -> Result<bool> {
        self.request("searching scrollback", |done| {
            RmuxTerminalCommand::SearchViewport {
                query: query.to_owned(),
                direction,
                done,
            }
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
        if bytes.is_empty() {
            Ok(())
        } else {
            self.send_command(RmuxTerminalCommand::InputBytes(bytes.to_vec()))
        }
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
        let mut engine = match TerminalEngine::new_with_terminal_options(
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
        let callback_input = config.pane_io.input_tx.clone();
        if let Err(error) = engine.on_pty_write(move |_terminal, bytes| {
            // RMUX answers terminal capability queries beside the pane PTY.
            // Bootty alone knows its configured palette, so return only those
            // replies instead of duplicating delayed CSI replies into the shell.
            if is_osc_default_color_response(bytes) {
                let _ = callback_input.send(bytes.to_vec());
            }
        }) {
            let _ = startup_tx.send(Err(error.to_string()));
            return;
        }
        let worker = RmuxWorker {
            pane_io: config.pane_io,
            geometry: config.geometry,
            engine,
            command_rx: config.command_rx,
            latest_frame: config.latest_frame,
            latest_drain: config.latest_drain,
            pending_output: OutputBacklog::with_capacity(RMUX_MAX_COLLECT_CHUNKS_PER_TICK),
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

fn is_osc_default_color_response(bytes: &[u8]) -> bool {
    [b"\x1b]10;rgb:", b"\x1b]11;rgb:", b"\x1b]12;rgb:"]
        .iter()
        .any(|prefix| bytes.starts_with(*prefix))
}

impl RmuxWorker {
    fn run(mut self) {
        loop {
            let (mut did_work, mut terminal_changed) = self.process_commands();
            did_work |= self.collect_pane_output();
            let stats = self.drain_pending_output();
            terminal_changed |= stats.bytes > 0;
            did_work |= stats.bytes > 0;
            self.drain_input_results();
            self.forward_side_effects();

            if terminal_changed {
                self.mark_unpublished_frame();
            }

            if did_work {
                self.publish_drain(stats);
            }
            if self.should_publish_frame() {
                self.publish_frame();
                self.last_frame_publish = Instant::now();
                if !did_work {
                    continue;
                }
            }
            if did_work {
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

    fn process_commands(&mut self) -> (bool, bool) {
        let mut did_work = false;
        let mut terminal_changed = false;
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
            did_work = true;
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
                        terminal_changed = true;
                    }
                }
                RmuxTerminalCommand::ForceResize => {
                    self.force_next_frame_publish = true;
                    self.queue_resize(self.geometry);
                    terminal_changed = true;
                }
                RmuxTerminalCommand::ApplyLiveConfig(config) => {
                    match self.engine.apply_live_config(config) {
                        Ok(()) => terminal_changed = true,
                        Err(error) => self.send_error(error),
                    }
                }
                RmuxTerminalCommand::Key(input) => {
                    self.mark_input_fast_path();
                    self.engine.scroll_viewport_bottom();
                    terminal_changed = true;
                    self.encode_output(|engine, out| engine.encode_key_to_vec(input, out));
                }
                RmuxTerminalCommand::Focus(gained) => {
                    self.mark_input_fast_path();
                    self.encode_output(|engine, out| engine.encode_focus_to_vec(gained, out));
                }
                RmuxTerminalCommand::Mouse(input) => {
                    self.mark_input_fast_path();
                    self.encode_output(|engine, out| engine.encode_mouse_to_vec(input, out));
                }
                RmuxTerminalCommand::MouseWheel {
                    input,
                    scroll_delta,
                } => match self.engine.is_mouse_tracking() {
                    Ok(true) => {
                        self.mark_input_fast_path();
                        self.encode_output(|engine, out| {
                            engine.encode_mouse_wheel_to_vec(
                                input,
                                scroll_delta.unsigned_abs().max(1),
                                out,
                            )
                        });
                    }
                    Ok(false) if scroll_delta != 0 => {
                        self.mark_input_fast_path();
                        self.engine.scroll_viewport_delta(scroll_delta);
                        terminal_changed = true;
                    }
                    Ok(false) => {}
                    Err(error) => self.send_error(error),
                },
                RmuxTerminalCommand::Paste(text) => {
                    self.mark_input_fast_path();
                    self.engine.scroll_viewport_bottom();
                    terminal_changed = true;
                    self.encode_output(|engine, out| engine.encode_paste_to_vec(&text, out));
                }
                RmuxTerminalCommand::InputBytes(bytes) => {
                    self.mark_input_fast_path();
                    self.engine.scroll_viewport_bottom();
                    terminal_changed = true;
                    self.queue_input(&bytes);
                }
                RmuxTerminalCommand::MouseViewportScroll { delta } => {
                    self.mark_input_fast_path();
                    self.engine.scroll_viewport_delta(delta);
                    terminal_changed = true;
                }
                RmuxTerminalCommand::EnterCopyMode => {
                    terminal_changed |= self.apply_terminal_change(TerminalEngine::enter_copy_mode);
                }
                RmuxTerminalCommand::SelectionBegin(event) => {
                    terminal_changed |=
                        self.apply_terminal_change(|engine| engine.begin_selection(event));
                }
                RmuxTerminalCommand::SelectionUpdate(event) => {
                    terminal_changed |=
                        self.apply_terminal_change(|engine| engine.update_selection(event));
                }
                RmuxTerminalCommand::SelectionEnd(event) => {
                    terminal_changed |=
                        self.apply_terminal_change(|engine| engine.end_selection(event));
                }
                RmuxTerminalCommand::FormatSelection { format, done } => {
                    self.respond(done, |worker| worker.engine.format_selection(format));
                }
                RmuxTerminalCommand::CopyModeActive { done } => {
                    self.respond(done, |worker| Ok(worker.engine.copy_mode_active()));
                }
                RmuxTerminalCommand::CopyModeAction { action, done } => {
                    if self.respond(done, |worker| {
                        worker.mark_input_fast_path();
                        worker.engine.handle_copy_mode_action(action)
                    }) {
                        terminal_changed = true;
                    }
                }
                RmuxTerminalCommand::SearchViewport {
                    query,
                    direction,
                    done,
                } => {
                    if self.respond(done, |worker| {
                        worker.mark_input_fast_path();
                        worker.engine.search_viewport(&query, direction)
                    }) {
                        terminal_changed = true;
                    }
                }
                RmuxTerminalCommand::IsMouseTracking { done } => {
                    self.respond(done, |worker| worker.engine.is_mouse_tracking());
                }
                RmuxTerminalCommand::DiscardPendingOutput { done } => {
                    self.respond(done, |worker| {
                        worker.pending_output.clear();
                        worker.pending_output_len.store(0, Ordering::Relaxed);
                        worker.has_unpublished_frame = false;
                        Ok(())
                    });
                }
                RmuxTerminalCommand::Stop => {
                    self.command_disconnected = true;
                    break;
                }
            }
        }
        (did_work, terminal_changed)
    }

    fn collect_pane_output(&mut self) -> bool {
        let mut did_work = false;
        let mut collected_bytes = 0;
        let mut collected_chunks = 0;
        while collected_chunks < RMUX_MAX_COLLECT_CHUNKS_PER_TICK
            && collected_bytes < RMUX_MAX_COLLECT_BYTES_PER_TICK
            && self.total_pending_output_len() < RMUX_MAX_PENDING_OUTPUT_BYTES
        {
            let event = match self.pane_io.output_rx.try_recv() {
                Ok(event) => event,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    self.output_closed = true;
                    self.closed.store(true, Ordering::Relaxed);
                    break;
                }
            };
            did_work = true;
            match event {
                RmuxPaneEvent::Rebase(keyframe) => {
                    // A rebase supersedes every byte queued from the previous
                    // epoch. Feed its reset-and-reconstruct keyframe as one
                    // authoritative emulator transition before accepting the
                    // following epoch's bytes.
                    self.pending_output.clear();
                    self.update_pending_output_len();
                    collected_chunks += 1;
                    collected_bytes += keyframe.len();
                    self.engine.write_vt_without_pty_responses(&keyframe);
                    self.engine.scroll_viewport_bottom();
                    self.waiting_initial_remote_frame = false;
                    self.force_next_frame_publish = true;
                    self.mark_unpublished_frame();
                }
                RmuxPaneEvent::Bytes(bytes) => {
                    collected_chunks += 1;
                    collected_bytes += bytes.len();
                    self.pending_output.push_back(bytes);
                    self.update_pending_output_len();
                }
                RmuxPaneEvent::ProcessExited => {}
                RmuxPaneEvent::End(error) => {
                    if let Some(reason) = error {
                        self.send_error(anyhow::anyhow!("rmux pane output ended: {reason}"));
                    }
                    self.output_closed = true;
                    self.closed.store(true, Ordering::Relaxed);
                    break;
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

    fn total_pending_output_len(&self) -> usize {
        self.pending_output.len()
    }

    fn update_pending_output_len(&self) {
        self.pending_output_len
            .store(self.total_pending_output_len(), Ordering::Relaxed);
    }

    fn drain_pending_output(&mut self) -> DrainStats {
        let engine = &mut self.engine;
        let stats = if self.force_next_frame_publish {
            drain_output_backlog_with_limits(
                &mut self.pending_output,
                RMUX_INPUT_FAST_PATH_DRAIN_BYTES,
                RMUX_INPUT_FAST_PATH_DRAIN_CHUNKS,
                RMUX_INPUT_FAST_PATH_DRAIN_TIME_US,
                |bytes| engine.write_vt(bytes),
            )
        } else {
            drain_output_backlog(&mut self.pending_output, |bytes| engine.write_vt(bytes))
        };
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
            // The pane stream can close before a queued layout resize reaches
            // this worker. That is normal pane teardown, not a terminal error.
            self.output_closed = true;
            self.closed.store(true, Ordering::Relaxed);
        }
    }

    fn queue_input(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if self.pane_io.input_tx.send(bytes.to_vec()).is_err() {
            // Input can race pane/session close in the same way as resize.
            self.output_closed = true;
            self.closed.store(true, Ordering::Relaxed);
        }
    }

    fn write_output_buf(&mut self) {
        if self.output_buf.is_empty() {
            return;
        }
        let bytes = std::mem::take(&mut self.output_buf);
        self.queue_input(&bytes);
    }

    fn encode_output(
        &mut self,
        encode: impl FnOnce(&mut TerminalEngine, &mut Vec<u8>) -> Result<()>,
    ) {
        if encode(&mut self.engine, &mut self.output_buf).is_ok() {
            self.write_output_buf();
        }
    }

    fn apply_terminal_change(
        &mut self,
        change: impl FnOnce(&mut TerminalEngine) -> Result<()>,
    ) -> bool {
        self.mark_input_fast_path();
        change(&mut self.engine).is_ok()
    }

    fn respond<T>(
        &mut self,
        done: WorkerRequest<std::result::Result<T, String>>,
        operation: impl FnOnce(&mut Self) -> Result<T>,
    ) -> bool {
        if !done.try_claim() {
            return false;
        }
        done.send(operation(self).map_err(|error| error.to_string()));
        true
    }

    fn send_error(&self, error: anyhow::Error) {
        let _ = self.error_tx.send(error.to_string());
    }
}
