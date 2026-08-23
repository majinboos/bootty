use std::{
    fmt::{self, Display, Formatter},
    io::{Read, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use crate::benchmark_trace::{BenchmarkTrace, TraceValue};
pub use crate::pty_backlog::DrainStats;
use crate::pty_backlog::{PtyBacklog, drain_pty_backlog, drain_pty_backlog_with_limits};
use anyhow::{Context, Result};
use portable_pty::{MasterPty, PtySize};

use bootty_surface::geometry::{CellMetrics, TerminalGeometry};
use bootty_terminal::{
    terminal_engine::{
        TERMINAL_TERM, TerminalColorConfig, TerminalCopyModeAction, TerminalCopyModeOutcome,
        TerminalCursorConfig, TerminalEngine, TerminalFeatureConfig, TerminalLiveConfig,
        TerminalSearchDirection, TerminalSelectionEvent, TerminalSelectionFormat,
        TerminalSideEffectEvent,
    },
    terminal_frame::RenderFrame,
    terminal_input_model::{KeyInput, MacosOptionAsAlt, MouseInput},
    terminal_side_effect::deliver_terminal_side_effects,
};

const INPUT_FAST_PATH_DRAIN_BYTES: usize = 64 * 1024;
const INPUT_FAST_PATH_DRAIN_CHUNKS: usize = 8;
const INPUT_FAST_PATH_DRAIN_TIME_US: u128 = 2_000;
const MAX_COLLECT_BYTES_PER_TICK: usize = 4 * 1024 * 1024;
const MAX_COLLECT_CHUNKS_PER_TICK: usize = 256;
const MAX_READER_QUEUE_CHUNKS: usize = MAX_COLLECT_CHUNKS_PER_TICK * 2;
pub use crate::terminal_launch::{BOOTTY_SHELL_ENV, configured_user_shell};
pub(crate) const WORKER_READY_FRAME_INTERVAL: Duration = Duration::from_millis(16);
pub(crate) const WORKER_BACKLOG_FRAME_INTERVAL: Duration = Duration::from_millis(64);
/// Pending PTY bytes past which output counts as a flood rather than an interactive redraw, and
/// publishing backs off to [`WORKER_BACKLOG_FRAME_INTERVAL`] to keep the drain moving. A full-screen
/// repaint of a large grid runs tens of kilobytes, so this sits well above one.
pub(crate) const WORKER_FLOOD_BACKLOG_BYTES: usize = 256 * 1024;
pub(crate) const WORKER_IDLE_WAIT: Duration = Duration::from_millis(16);
pub(crate) const WORKER_SETTLED_FRAME_DELAY: Duration = Duration::from_millis(16);
pub(crate) const SYNC_OUTPUT_MAX_SUPPRESS: Duration = Duration::from_secs(1);
const WORKER_RESPONSE_TIMEOUT: Duration = Duration::from_millis(50);
const WORKER_RESPONSE_COMPLETION_TIMEOUT: Duration = Duration::from_millis(100);
const REQUEST_PENDING: u8 = 0;
const REQUEST_RUNNING: u8 = 1;
const REQUEST_CANCELLED: u8 = 2;

#[derive(Clone, Debug, Default)]
pub struct TerminalSessionConfig {
    pub launch: SessionLaunchConfig,
    pub colors: TerminalColorConfig,
    pub cursor: TerminalCursorConfig,
    pub features: TerminalFeatureConfig,
    pub max_scrollback: usize,
    pub macos_option_as_alt: MacosOptionAsAlt,
    pub side_effect_tx: Option<Sender<TerminalSideEffectEvent>>,
    pub side_effect_pane_id: Option<String>,
    pub benchmark_trace: Option<BenchmarkTrace>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionLaunchConfig {
    pub shell: Option<String>,
    pub args: Vec<String>,
    pub working_directory: Option<PathBuf>,
    /// The mux pane this terminal is the front end for, exported as `BOOTTY_PANE`. Only backends
    /// that spawn the pane's own PTY know it, so it stays unset for a tmux attach, where tmux
    /// exports the same id as `$TMUX_PANE`.
    pub pane_id: Option<String>,
    pub env: Vec<(String, String)>,
    pub env_remove: Vec<String>,
    pub term: String,
    pub colorterm: String,
}

impl Default for SessionLaunchConfig {
    fn default() -> Self {
        Self {
            shell: None,
            args: Vec::new(),
            working_directory: None,
            pane_id: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            term: TERMINAL_TERM.to_owned(),
            colorterm: "truecolor".to_owned(),
        }
    }
}

pub struct TerminalSession {
    command_tx: Sender<TerminalCommand>,
    latest_frame: Arc<PublishedFrame>,
    latest_drain: Arc<Mutex<DrainStats>>,
    pending_pty_len: Arc<AtomicUsize>,
    worker_health: Arc<WorkerHealth>,
    current_working_directory: Arc<Mutex<Option<String>>>,
    geometry: TerminalGeometry,
    display_scale: f32,
    render_cell: CellMetrics,
    child: crate::terminal_launch::OwnedChild,
    tty_name: Option<String>,
}

type RepaintWakeup = Arc<dyn Fn() + Send + Sync + 'static>;

pub(crate) struct PublishedFrame {
    latest: Mutex<Arc<RenderFrame>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalWorkerFailure {
    operation: &'static str,
    source: String,
}

impl Display for TerminalWorkerFailure {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "terminal worker {}: {}",
            self.operation, self.source
        )
    }
}

#[derive(Debug, Default)]
struct WorkerHealth {
    latest: Mutex<Option<TerminalWorkerFailure>>,
}

impl WorkerHealth {
    fn record(&self, operation: &'static str, source: impl Display) {
        let mut latest = self
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *latest = Some(TerminalWorkerFailure {
            operation,
            source: source.to_string(),
        });
    }

    fn take(&self) -> Result<Option<TerminalWorkerFailure>> {
        self.latest
            .lock()
            .map(|mut latest| latest.take())
            .map_err(|_| anyhow::anyhow!("terminal worker health lock poisoned"))
    }
}

impl PublishedFrame {
    pub(crate) fn new() -> Self {
        Self {
            latest: Mutex::new(Arc::new(RenderFrame::default())),
        }
    }

    pub(crate) fn load(&self) -> Result<Arc<RenderFrame>> {
        self.latest
            .lock()
            .map(|frame| Arc::clone(&frame))
            .map_err(|_| anyhow::anyhow!("terminal render frame lock poisoned"))
    }

    pub(crate) fn publish(&self, frame: &RenderFrame) -> Result<()> {
        let mut latest = self
            .latest
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal render frame lock poisoned"))?;
        *latest = Arc::new(frame.clone());
        Ok(())
    }
}

type SelectionFormatResponse = std::result::Result<Option<Vec<u8>>, String>;
type MouseTrackingResponse = std::result::Result<bool, String>;
type SearchViewportResponse = std::result::Result<bool, String>;
type CopyModeActiveResponse = std::result::Result<bool, String>;
type CopyModeActionResponse = std::result::Result<TerminalCopyModeOutcome, String>;

/// Worker-side half of a single-response request, claimed with [`WorkerRequest::try_claim`] before
/// the work runs so a caller that already timed out is not served.
pub struct WorkerRequest<T> {
    state: Arc<AtomicU8>,
    sender: Sender<T>,
}

/// Caller-side half of a single-response request.
pub struct PendingWorkerResponse<T> {
    state: Arc<AtomicU8>,
    receiver: Receiver<T>,
}

/// Creates a request/response pair for one worker round trip.
pub fn worker_request<T>() -> (WorkerRequest<T>, PendingWorkerResponse<T>) {
    let state = Arc::new(AtomicU8::new(REQUEST_PENDING));
    let (sender, receiver) = mpsc::channel();
    (
        WorkerRequest {
            state: Arc::clone(&state),
            sender,
        },
        PendingWorkerResponse { state, receiver },
    )
}

impl<T> WorkerRequest<T> {
    /// Takes ownership of the request. Returns `false` when the caller already gave up.
    pub fn try_claim(&self) -> bool {
        self.state
            .compare_exchange(
                REQUEST_PENDING,
                REQUEST_RUNNING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Delivers the response to the waiting caller.
    pub fn send(self, response: T) {
        if self.state.load(Ordering::Acquire) == REQUEST_RUNNING {
            let _ = self.sender.send(response);
        }
    }
}

impl<T> PendingWorkerResponse<T> {
    /// Waits for the worker response, naming `operation` in any error.
    pub fn receive(self, operation: &'static str) -> Result<T> {
        match self.receiver.recv_timeout(WORKER_RESPONSE_TIMEOUT) {
            Ok(response) => Ok(response),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(anyhow::anyhow!(
                "terminal worker stopped before {operation}"
            )),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if self
                    .state
                    .compare_exchange(
                        REQUEST_PENDING,
                        REQUEST_CANCELLED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    return Err(anyhow::anyhow!(
                        "terminal worker timed out before {operation}"
                    ));
                }

                match self
                    .receiver
                    .recv_timeout(WORKER_RESPONSE_COMPLETION_TIMEOUT)
                {
                    Ok(response) => Ok(response),
                    Err(mpsc::RecvTimeoutError::Disconnected) => Err(anyhow::anyhow!(
                        "terminal worker stopped before {operation}"
                    )),
                    Err(mpsc::RecvTimeoutError::Timeout) => Err(anyhow::anyhow!(
                        "terminal worker completion unknown after {operation}; the operation may have completed"
                    )),
                }
            }
        }
    }
}

enum TerminalCommand {
    DisplayScale(f32),
    RenderCellMetrics {
        cell: CellMetrics,
        done: WorkerRequest<()>,
    },
    ApplyLiveConfig(TerminalLiveConfig),
    Resize {
        geometry: TerminalGeometry,
        done: Option<WorkerRequest<()>>,
    },
    Key(KeyInput),
    Focus(bool),
    Mouse(MouseInput),
    MouseWheel {
        input: MouseInput,
        scroll_delta: isize,
    },
    Paste(String),
    RawInput(Vec<u8>),
    MouseViewportScroll {
        delta: isize,
    },
    EnterCopyMode,
    SelectionBegin(TerminalSelectionEvent),
    SelectionUpdate(TerminalSelectionEvent),
    SelectionEnd(Option<TerminalSelectionEvent>),
    FormatSelection {
        format: TerminalSelectionFormat,
        done: WorkerRequest<SelectionFormatResponse>,
    },
    CopyModeActive(WorkerRequest<CopyModeActiveResponse>),
    CopyModeAction {
        action: TerminalCopyModeAction,
        done: WorkerRequest<CopyModeActionResponse>,
    },
    SearchViewport {
        query: String,
        direction: TerminalSearchDirection,
        done: WorkerRequest<SearchViewportResponse>,
    },
    IsMouseTracking(WorkerRequest<MouseTrackingResponse>),
    DiscardPendingOutput(WorkerRequest<()>),
}
impl TerminalSession {
    pub fn new(geometry: TerminalGeometry) -> Result<Self> {
        Self::new_with_repaint_wakeup(geometry, Arc::new(|| {}))
    }

    pub fn new_with_repaint_wakeup(
        geometry: TerminalGeometry,
        repaint_wakeup: RepaintWakeup,
    ) -> Result<Self> {
        Self::new_with_config(geometry, TerminalSessionConfig::default(), repaint_wakeup)
    }

    pub fn new_with_config(
        geometry: TerminalGeometry,
        config: TerminalSessionConfig,
        repaint_wakeup: RepaintWakeup,
    ) -> Result<Self> {
        let (pty_master, child, tty_name) =
            crate::terminal_launch::spawn(geometry, &config.launch)?.into_parts();
        let mut reader = pty_master.try_clone_reader()?;
        let pty_writer = Arc::new(Mutex::new(pty_master.take_writer()?));
        let (pty_tx, pty_rx) = mpsc::sync_channel(MAX_READER_QUEUE_CHUNKS);
        thread::spawn(move || {
            let mut buf = [0_u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || pty_tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        let (command_tx, command_rx) = mpsc::channel();
        let latest_frame = Arc::new(PublishedFrame::new());
        let latest_drain = Arc::new(Mutex::new(DrainStats::default()));
        let pending_pty_len = Arc::new(AtomicUsize::new(0));
        let worker_health = Arc::new(WorkerHealth::default());
        let current_working_directory = Arc::new(Mutex::new(None));
        let benchmark_trace = match config.benchmark_trace.clone() {
            Some(trace) => Some(trace),
            None => BenchmarkTrace::from_env().context("open benchmark trace")?,
        };
        spawn_terminal_worker(TerminalWorkerConfig {
            geometry,
            colors: config.colors,
            cursor: config.cursor,
            features: config.features,
            max_scrollback: config.max_scrollback,
            macos_option_as_alt: config.macos_option_as_alt,
            pty_master,
            pty_rx,
            pty_writer,
            command_rx,
            latest_frame: latest_frame.clone(),
            latest_drain: latest_drain.clone(),
            pending_pty_len: pending_pty_len.clone(),
            worker_health: Arc::clone(&worker_health),
            current_working_directory: current_working_directory.clone(),
            repaint_wakeup,
            side_effect_tx: config.side_effect_tx,
            side_effect_pane_id: config.side_effect_pane_id,
            benchmark_trace,
        })?;

        Ok(Self {
            command_tx,
            latest_frame,
            latest_drain,
            pending_pty_len,
            worker_health,
            current_working_directory,
            geometry,
            display_scale: 1.0,
            render_cell: CellMetrics::new(geometry.cell_width as f32, geometry.cell_height as f32),
            child,
            tty_name,
        })
    }

    pub fn grid_size(&self) -> (u16, u16) {
        (self.geometry.cols, self.geometry.rows)
    }

    pub fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        if geometry == self.geometry {
            return Ok(());
        }

        let (done, response) = worker_request();
        self.send_command(TerminalCommand::Resize {
            geometry,
            done: Some(done),
        })?;
        response.receive("resizing")?;
        self.check_worker_error()?;
        self.geometry = geometry;

        Ok(())
    }

    /// Queue a resize without waiting for the worker to publish it.
    pub fn queue_resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        if geometry == self.geometry {
            return Ok(());
        }
        self.send_command(TerminalCommand::Resize {
            geometry,
            done: None,
        })?;
        self.geometry = geometry;
        Ok(())
    }

    pub fn set_display_scale(&mut self, display_scale: f32) -> Result<()> {
        let display_scale = if display_scale.is_finite() && display_scale > 0.0 {
            display_scale
        } else {
            1.0
        };
        if (self.display_scale - display_scale).abs() <= f32::EPSILON {
            return Ok(());
        }
        self.send_command(TerminalCommand::DisplayScale(display_scale))?;
        self.display_scale = display_scale;
        Ok(())
    }

    pub fn set_render_cell_metrics(&mut self, cell: CellMetrics) -> Result<()> {
        if self.render_cell == cell {
            return Ok(());
        }
        let (done, response) = worker_request();
        self.send_command(TerminalCommand::RenderCellMetrics { cell, done })?;
        response.receive("setting render cell metrics")?;
        self.render_cell = cell;
        Ok(())
    }

    pub fn apply_live_config(&mut self, config: TerminalLiveConfig) -> Result<()> {
        // The worker applies the aggregate in colors, cursor, features order.
        self.send_command(TerminalCommand::ApplyLiveConfig(config))
    }

    pub fn drain_pty(&mut self) -> DrainStats {
        let Ok(mut stats) = self.latest_drain.lock() else {
            return DrainStats::default();
        };
        let drained = *stats;
        *stats = DrainStats::default();
        drained
    }

    pub fn pending_pty_len(&self) -> usize {
        self.pending_pty_len.load(Ordering::Relaxed)
    }

    pub fn child_exited(&mut self) -> Result<bool> {
        self.check_worker_error()?;
        self.child.exited()
    }

    pub fn tty_name(&self) -> Option<&str> {
        self.tty_name.as_deref()
    }

    pub fn write_input(&self, bytes: &[u8]) -> Result<()> {
        self.send_command(TerminalCommand::RawInput(bytes.to_vec()))
    }

    pub fn write_paste(&mut self, text: &str) -> Result<()> {
        self.send_command(TerminalCommand::Paste(text.to_owned()))
    }

    pub fn encode_key(&mut self, input: KeyInput) -> Result<()> {
        self.send_command(TerminalCommand::Key(input))
    }

    pub fn encode_focus(&mut self, gained: bool) -> Result<()> {
        self.send_command(TerminalCommand::Focus(gained))
    }

    pub fn encode_mouse(&mut self, input: MouseInput) -> Result<()> {
        self.send_command(TerminalCommand::Mouse(input))
    }

    pub fn handle_mouse_wheel(&mut self, input: MouseInput, scroll_delta: isize) -> Result<()> {
        self.send_command(TerminalCommand::MouseWheel {
            input,
            scroll_delta,
        })
    }

    pub fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        self.send_command(TerminalCommand::MouseViewportScroll { delta })
    }

    pub fn enter_copy_mode(&mut self) -> Result<()> {
        self.send_command(TerminalCommand::EnterCopyMode)
    }

    pub fn copy_mode_active(&mut self) -> Result<bool> {
        let (done, response) = worker_request();
        self.send_command(TerminalCommand::CopyModeActive(done))?;
        response
            .receive("reporting copy mode")?
            .map_err(anyhow::Error::msg)
    }

    pub fn handle_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome> {
        let (done, response) = worker_request();
        self.send_command(TerminalCommand::CopyModeAction { action, done })?;
        response
            .receive("handling copy mode action")?
            .map_err(anyhow::Error::msg)
    }

    pub fn begin_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        self.send_command(TerminalCommand::SelectionBegin(event))
    }

    pub fn update_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        self.send_command(TerminalCommand::SelectionUpdate(event))
    }

    pub fn end_selection(&mut self, event: Option<TerminalSelectionEvent>) -> Result<()> {
        self.send_command(TerminalCommand::SelectionEnd(event))
    }

    pub fn format_selection(&mut self, format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
        let (done, response) = worker_request();
        self.send_command(TerminalCommand::FormatSelection { format, done })?;
        response
            .receive("formatting selection")?
            .map_err(anyhow::Error::msg)
    }

    pub fn search_viewport(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
    ) -> Result<bool> {
        let (done, response) = worker_request();
        self.send_command(TerminalCommand::SearchViewport {
            query: query.to_owned(),
            direction,
            done,
        })?;
        response
            .receive("searching scrollback")?
            .map_err(anyhow::Error::msg)
    }

    pub fn is_mouse_tracking(&mut self) -> Result<bool> {
        let (done, response) = worker_request();
        self.send_command(TerminalCommand::IsMouseTracking(done))?;
        response
            .receive("reporting mouse tracking")?
            .map_err(anyhow::Error::msg)
    }

    pub fn current_working_directory(&self) -> Option<String> {
        self.current_working_directory
            .lock()
            .ok()
            .and_then(|cwd| cwd.clone())
    }

    pub fn discard_pending_output(&mut self) -> Result<()> {
        let (done, response) = worker_request();
        self.send_command(TerminalCommand::DiscardPendingOutput(done))?;
        response.receive("discarding output")
    }

    pub fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        self.check_worker_error()?;
        self.latest_frame.load()
    }

    fn send_command(&self, command: TerminalCommand) -> Result<()> {
        self.check_worker_error()?;
        self.command_tx
            .send(command)
            .map_err(|_| anyhow::anyhow!("terminal worker stopped"))
    }

    fn check_worker_error(&self) -> Result<()> {
        if let Some(failure) = self.worker_health.take()? {
            anyhow::bail!(failure);
        }
        Ok(())
    }
}

struct TerminalWorkerConfig {
    geometry: TerminalGeometry,
    colors: TerminalColorConfig,
    cursor: TerminalCursorConfig,
    features: TerminalFeatureConfig,
    max_scrollback: usize,
    macos_option_as_alt: MacosOptionAsAlt,
    pty_master: Box<dyn MasterPty + Send>,
    pty_rx: Receiver<Vec<u8>>,
    pty_writer: Arc<Mutex<Box<dyn Write + Send>>>,
    command_rx: Receiver<TerminalCommand>,
    latest_frame: Arc<PublishedFrame>,
    latest_drain: Arc<Mutex<DrainStats>>,
    pending_pty_len: Arc<AtomicUsize>,
    worker_health: Arc<WorkerHealth>,
    current_working_directory: Arc<Mutex<Option<String>>>,
    repaint_wakeup: RepaintWakeup,
    side_effect_tx: Option<Sender<TerminalSideEffectEvent>>,
    side_effect_pane_id: Option<String>,
    benchmark_trace: Option<BenchmarkTrace>,
}

fn pty_size(geometry: TerminalGeometry) -> PtySize {
    PtySize {
        rows: geometry.rows,
        cols: geometry.cols,
        pixel_width: geometry.pixel_width(),
        pixel_height: geometry.pixel_height(),
    }
}

fn spawn_terminal_worker(config: TerminalWorkerConfig) -> Result<()> {
    let (startup_tx, startup_rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut engine = match TerminalEngine::new_with_terminal_options(
            config.geometry,
            config.colors,
            config.cursor,
            config.features,
            config.max_scrollback,
            config.macos_option_as_alt,
        ) {
            Ok(engine) => engine,
            Err(error) => {
                let _ = startup_tx.send(Err(error.to_string()));
                return;
            }
        };
        let callback_writer = config.pty_writer.clone();
        let callback_health = Arc::clone(&config.worker_health);
        if let Err(error) = engine.on_pty_write(move |_terminal, bytes| {
            write_pty(&callback_writer, bytes, &callback_health);
        }) {
            let _ = startup_tx.send(Err(error.to_string()));
            return;
        }
        let _ = startup_tx.send(Ok(()));
        let mut worker = TerminalWorker {
            engine,
            pty_master: config.pty_master,
            pty_rx: config.pty_rx,
            pty_writer: config.pty_writer,
            command_rx: config.command_rx,
            latest_frame: config.latest_frame,
            latest_drain: config.latest_drain,
            pending_pty_len: config.pending_pty_len,
            worker_health: config.worker_health,
            current_working_directory: config.current_working_directory,
            repaint_wakeup: config.repaint_wakeup,
            side_effect_tx: config.side_effect_tx,
            side_effect_pane_id: config.side_effect_pane_id,
            benchmark_trace: config.benchmark_trace,
            output_buf: Vec::with_capacity(1024),
            pending_pty: PtyBacklog::with_capacity(MAX_COLLECT_CHUNKS_PER_TICK),
            pending_resize_ack: None,
            pending_render_cell_ack: None,
            last_frame_publish: Instant::now() - WORKER_READY_FRAME_INTERVAL,
            has_unpublished_frame: false,
            sync_output_since: None,
            sync_output_batch_pending: false,
            last_terminal_change: None,
            force_next_frame_publish: false,
            command_disconnected: false,
            pending_command: None,
            pty_disconnected: false,
        };
        worker.trace_event(
            "worker_start",
            &[
                ("cols", TraceValue::U64(u64::from(config.geometry.cols))),
                ("rows", TraceValue::U64(u64::from(config.geometry.rows))),
            ],
        );
        worker.run();
    });

    startup_rx
        .recv()
        .map_err(|_| anyhow::anyhow!("terminal worker failed to start"))?
        .map_err(|error| anyhow::anyhow!(error))
}

struct TerminalWorker {
    engine: TerminalEngine,
    pty_master: Box<dyn MasterPty + Send>,
    pty_rx: Receiver<Vec<u8>>,
    pty_writer: Arc<Mutex<Box<dyn Write + Send>>>,
    command_rx: Receiver<TerminalCommand>,
    pending_command: Option<TerminalCommand>,
    pending_resize_ack: Option<WorkerRequest<()>>,
    pending_render_cell_ack: Option<WorkerRequest<()>>,
    latest_frame: Arc<PublishedFrame>,
    latest_drain: Arc<Mutex<DrainStats>>,
    pending_pty_len: Arc<AtomicUsize>,
    worker_health: Arc<WorkerHealth>,
    current_working_directory: Arc<Mutex<Option<String>>>,
    repaint_wakeup: RepaintWakeup,
    side_effect_tx: Option<Sender<TerminalSideEffectEvent>>,
    side_effect_pane_id: Option<String>,
    output_buf: Vec<u8>,
    pending_pty: PtyBacklog,
    last_frame_publish: Instant,
    has_unpublished_frame: bool,
    sync_output_since: Option<Instant>,
    sync_output_batch_pending: bool,
    last_terminal_change: Option<Instant>,
    force_next_frame_publish: bool,
    command_disconnected: bool,
    pty_disconnected: bool,
    benchmark_trace: Option<BenchmarkTrace>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct WorkerCommandStats {
    did_work: bool,
    terminal_changed: bool,
    commands: usize,
}

impl TerminalWorker {
    fn run(&mut self) {
        loop {
            let command_stats = self.process_commands();
            let mut did_work = command_stats.did_work;
            let mut terminal_changed = command_stats.terminal_changed;
            did_work |= self.collect_pty();
            let stats = self.drain_pty();
            terminal_changed |= stats.bytes > 0;
            did_work |= stats.bytes > 0;
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
            if !did_work {
                if self.should_stop() {
                    break;
                }
                match self.command_rx.recv_timeout(WORKER_IDLE_WAIT) {
                    Ok(command) => self.pending_command = Some(command),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        self.command_disconnected = true;
                    }
                }
            }
        }
        self.trace_event("worker_stop", &[]);
    }

    fn should_stop(&self) -> bool {
        self.command_disconnected && self.pty_disconnected && self.pending_pty.is_empty()
    }

    fn should_publish_frame(&mut self) -> bool {
        let sync_output_suppressed = self.sync_output_suppressed();
        should_publish_frame_after_work(
            self.has_unpublished_frame,
            self.force_next_frame_publish,
            sync_output_suppressed,
            self.pending_pty.len(),
            self.last_terminal_change
                .map(|instant| instant.elapsed())
                .unwrap_or(Duration::ZERO),
            self.last_frame_publish.elapsed(),
        )
    }

    fn sync_output_suppressed(&mut self) -> bool {
        let active = synchronized_output_state(&self.engine, &self.worker_health);
        let elapsed = if active {
            self.sync_output_since
                .get_or_insert_with(Instant::now)
                .elapsed()
        } else {
            self.sync_output_since = None;
            Duration::ZERO
        };
        sync_output_suppresses_publish(
            active,
            std::mem::take(&mut self.sync_output_batch_pending),
            elapsed,
        )
    }

    fn mark_unpublished_frame(&mut self) {
        self.has_unpublished_frame = true;
        self.last_terminal_change = Some(Instant::now());
    }

    fn mark_input_fast_path(&mut self) {
        self.force_next_frame_publish = true;
    }

    fn process_commands(&mut self) -> WorkerCommandStats {
        let mut stats = WorkerCommandStats::default();
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
            stats.commands += 1;
            match command {
                TerminalCommand::DisplayScale(display_scale) => {
                    self.engine.set_display_scale(display_scale);
                    stats.terminal_changed = true;
                }
                TerminalCommand::RenderCellMetrics { cell, done } => {
                    if !done.try_claim() {
                        continue;
                    }
                    self.engine.set_render_cell_metrics(cell);
                    stats.terminal_changed = true;
                    self.pending_render_cell_ack = Some(done);
                }
                TerminalCommand::Resize { geometry, done } => {
                    if let Some(done) = done.as_ref()
                        && !done.try_claim()
                    {
                        continue;
                    }
                    let result = self.resize(geometry);
                    if let Err(error) = result {
                        self.worker_health.record("resize", error);
                        if let Some(done) = done {
                            done.send(());
                        }
                    } else {
                        stats.terminal_changed = true;
                        self.pending_resize_ack = done;
                    }
                }
                TerminalCommand::ApplyLiveConfig(config) => {
                    match self.engine.apply_live_config(config) {
                        Ok(()) => stats.terminal_changed = true,
                        Err(error) => self.worker_health.record("apply_live_config", error),
                    }
                }
                TerminalCommand::Key(input) => {
                    self.mark_input_fast_path();
                    self.engine.scroll_viewport_bottom();
                    stats.terminal_changed = true;
                    match self.engine.encode_key_to_vec(input, &mut self.output_buf) {
                        Ok(()) => self.write_output_buf(),
                        Err(error) => self.worker_health.record("encode_key", error),
                    }
                }
                TerminalCommand::Focus(gained) => {
                    self.mark_input_fast_path();
                    match self
                        .engine
                        .encode_focus_to_vec(gained, &mut self.output_buf)
                    {
                        Ok(()) => self.write_output_buf(),
                        Err(error) => self.worker_health.record("encode_focus", error),
                    }
                }
                TerminalCommand::Mouse(input) => {
                    self.mark_input_fast_path();
                    match self.engine.encode_mouse_to_vec(input, &mut self.output_buf) {
                        Ok(()) => self.write_output_buf(),
                        Err(error) => self.worker_health.record("encode_mouse", error),
                    }
                }
                TerminalCommand::MouseWheel {
                    input,
                    scroll_delta,
                } => match self.engine.is_mouse_tracking() {
                    Ok(true) => {
                        self.mark_input_fast_path();
                        match self.engine.encode_mouse_wheel_to_vec(
                            input,
                            scroll_delta.unsigned_abs().max(1),
                            &mut self.output_buf,
                        ) {
                            Ok(()) => self.write_output_buf(),
                            Err(error) => self.worker_health.record("encode_mouse_wheel", error),
                        }
                    }
                    Ok(false) if scroll_delta != 0 => {
                        self.mark_input_fast_path();
                        self.engine.scroll_viewport_delta(scroll_delta);
                        stats.terminal_changed = true;
                    }
                    Ok(false) => {}
                    Err(error) => self.worker_health.record("mouse_tracking_for_wheel", error),
                },
                TerminalCommand::Paste(text) => {
                    self.mark_input_fast_path();
                    self.engine.scroll_viewport_bottom();
                    stats.terminal_changed = true;
                    match self.engine.encode_paste_to_vec(&text, &mut self.output_buf) {
                        Ok(()) => self.write_output_buf(),
                        Err(error) => self.worker_health.record("encode_paste", error),
                    }
                }
                TerminalCommand::DiscardPendingOutput(done) => {
                    if !done.try_claim() {
                        continue;
                    }
                    self.discard_pending_output_queue();
                    done.send(());
                }
                TerminalCommand::RawInput(bytes) => {
                    self.mark_input_fast_path();
                    self.engine.scroll_viewport_bottom();
                    stats.terminal_changed = true;
                    write_pty(&self.pty_writer, &bytes, &self.worker_health);
                }
                TerminalCommand::MouseViewportScroll { delta } => {
                    self.mark_input_fast_path();
                    self.engine.scroll_viewport_delta(delta);
                    stats.terminal_changed = true;
                }
                TerminalCommand::EnterCopyMode => {
                    self.mark_input_fast_path();
                    match self.engine.enter_copy_mode() {
                        Ok(()) => stats.terminal_changed = true,
                        Err(error) => self.worker_health.record("enter_copy_mode", error),
                    }
                }
                TerminalCommand::SelectionBegin(event) => {
                    self.mark_input_fast_path();
                    match self.engine.begin_selection(event) {
                        Ok(()) => stats.terminal_changed = true,
                        Err(error) => self.worker_health.record("selection_begin", error),
                    }
                }
                TerminalCommand::SelectionUpdate(event) => {
                    self.mark_input_fast_path();
                    match self.engine.update_selection(event) {
                        Ok(()) => stats.terminal_changed = true,
                        Err(error) => self.worker_health.record("selection_update", error),
                    }
                }
                TerminalCommand::SelectionEnd(event) => {
                    self.mark_input_fast_path();
                    match self.engine.end_selection(event) {
                        Ok(()) => stats.terminal_changed = true,
                        Err(error) => self.worker_health.record("selection_end", error),
                    }
                }
                TerminalCommand::FormatSelection { format, done } => {
                    if !done.try_claim() {
                        continue;
                    }
                    let response = self
                        .engine
                        .format_selection(format)
                        .map_err(|error| error.to_string());
                    done.send(response);
                }
                TerminalCommand::CopyModeActive(done) => {
                    if done.try_claim() {
                        done.send(Ok(self.engine.copy_mode_active()));
                    }
                }
                TerminalCommand::CopyModeAction { action, done } => {
                    if !done.try_claim() {
                        continue;
                    }
                    self.mark_input_fast_path();
                    let response = self
                        .engine
                        .handle_copy_mode_action(action)
                        .map_err(|error| error.to_string());
                    stats.terminal_changed = true;
                    done.send(response);
                }
                TerminalCommand::SearchViewport {
                    query,
                    direction,
                    done,
                } => {
                    if !done.try_claim() {
                        continue;
                    }
                    let response = self
                        .engine
                        .search_viewport(&query, direction)
                        .and_then(|found| {
                            let frame = self.engine.extract_frame()?;
                            self.latest_frame.publish(frame)?;
                            Ok(found)
                        })
                        .map_err(|error| error.to_string());
                    if response.is_ok() {
                        self.force_next_frame_publish = false;
                        self.has_unpublished_frame = false;
                        (self.repaint_wakeup)();
                    }
                    done.send(response);
                }
                TerminalCommand::IsMouseTracking(done) => {
                    if !done.try_claim() {
                        continue;
                    }
                    let response = self
                        .engine
                        .is_mouse_tracking()
                        .map_err(|error| error.to_string());
                    done.send(response);
                }
            }
        }
        if stats.commands > 0 {
            self.trace_event(
                "input_commands",
                &[
                    ("commands", TraceValue::Usize(stats.commands)),
                    ("terminal_changed", TraceValue::Bool(stats.terminal_changed)),
                ],
            );
        }
        stats
    }

    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        let previous = self.engine.geometry();
        self.pty_master.resize(pty_size(geometry))?;
        if let Err(error) = self.engine.resize(geometry) {
            let engine_rollback = self.engine.resize(previous).err();
            let pty_rollback = self.pty_master.resize(pty_size(previous)).err();
            if engine_rollback.is_none() && pty_rollback.is_none() {
                return Err(error);
            }

            let details = [
                engine_rollback.map(|error| format!("engine rollback: {error}")),
                pty_rollback.map(|error| format!("PTY rollback: {error}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ");
            return Err(error.context(format!("resize rollback failed: {details}")));
        }
        Ok(())
    }

    fn discard_pending_output_queue(&mut self) {
        self.pending_pty.clear();
        loop {
            match self.pty_rx.try_recv() {
                Ok(_) => {}
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pty_disconnected = true;
                    break;
                }
            }
        }
        self.pending_pty_len.store(0, Ordering::Relaxed);
        self.has_unpublished_frame = false;
        self.sync_output_batch_pending = false;
        self.last_terminal_change = None;
    }

    fn collect_pty(&mut self) -> bool {
        let mut did_work = false;
        let mut collected_bytes = 0;
        let mut collected_chunks = 0;
        while collected_chunks < MAX_COLLECT_CHUNKS_PER_TICK
            && collected_bytes < MAX_COLLECT_BYTES_PER_TICK
        {
            let bytes = match self.pty_rx.try_recv() {
                Ok(bytes) => bytes,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pty_disconnected = true;
                    break;
                }
            };
            let bytes_len = bytes.len();
            did_work = true;
            collected_bytes += bytes_len;
            collected_chunks += 1;
            self.pending_pty.push_back(bytes);
            self.trace_event(
                "pty_read",
                &[
                    ("bytes", TraceValue::Usize(bytes_len)),
                    (
                        "pending_pty_bytes",
                        TraceValue::Usize(self.pending_pty.len()),
                    ),
                ],
            );
        }
        if did_work {
            self.pending_pty_len
                .store(self.pending_pty.len(), Ordering::Relaxed);
            self.trace_event(
                "pty_collect_done",
                &[
                    ("bytes", TraceValue::Usize(collected_bytes)),
                    ("chunks", TraceValue::Usize(collected_chunks)),
                    (
                        "pending_pty_bytes",
                        TraceValue::Usize(self.pending_pty.len()),
                    ),
                ],
            );
        }
        did_work
    }

    fn drain_pty(&mut self) -> DrainStats {
        let pending_before = self.pending_pty.len();
        if pending_before > 0 {
            self.trace_event(
                "parse_start",
                &[("pending_pty_bytes", TraceValue::Usize(pending_before))],
            );
        }
        let engine = &mut self.engine;
        let worker_health = Arc::clone(&self.worker_health);
        let mut observed_sync_output = synchronized_output_state(engine, &worker_health);
        let mut write = |bytes: &[u8]| {
            engine.write_vt(bytes);
            observed_sync_output |= engine.take_synchronized_output_observed();
            observed_sync_output |= synchronized_output_state(engine, &worker_health);
        };
        let stats = if self.force_next_frame_publish {
            drain_pty_backlog_with_limits(
                &mut self.pending_pty,
                INPUT_FAST_PATH_DRAIN_BYTES,
                INPUT_FAST_PATH_DRAIN_CHUNKS,
                INPUT_FAST_PATH_DRAIN_TIME_US,
                &mut write,
            )
        } else {
            drain_pty_backlog(&mut self.pending_pty, &mut write)
        };
        self.sync_output_batch_pending |= observed_sync_output;
        if stats.bytes > 0 {
            self.publish_current_working_directory();
            self.trace_event(
                "parse_done",
                &[
                    ("bytes", TraceValue::Usize(stats.bytes)),
                    ("chunks", TraceValue::Usize(stats.chunks)),
                    ("elapsed_us", TraceValue::U64(stats.elapsed_us)),
                    (
                        "pending_pty_bytes",
                        TraceValue::Usize(self.pending_pty.len()),
                    ),
                ],
            );
        }
        self.forward_side_effects();

        if self.pending_pty.len() != pending_before {
            self.pending_pty_len
                .store(self.pending_pty.len(), Ordering::Relaxed);
        }
        stats
    }

    fn publish_current_working_directory(&mut self) {
        let cwd = self.engine.current_working_directory();
        let next = (!cwd.is_empty()).then(|| cwd.to_owned());
        if let Ok(mut current) = self.current_working_directory.lock()
            && *current != next
        {
            *current = next;
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

    fn publish_frame(&mut self) {
        let trace = self.benchmark_trace.clone();
        let extract_start = Instant::now();
        let frame = match self.engine.extract_frame() {
            Ok(frame) => frame,
            Err(error) => {
                self.worker_health.record("extract_frame", error);
                self.acknowledge_resize();
                return;
            }
        };
        let extract_elapsed_us = extract_start.elapsed().as_micros() as u64;
        if let Some(trace) = &trace {
            trace.emit(
                "frame_submitted",
                &[
                    ("cols", TraceValue::U64(u64::from(frame.cols))),
                    ("rows", TraceValue::U64(u64::from(frame.rows))),
                    ("cells", TraceValue::Usize(frame.stats.cells)),
                    ("chars", TraceValue::Usize(frame.stats.chars)),
                    ("dirty_rows", TraceValue::Usize(frame.stats.dirty_rows)),
                    ("extract_us", TraceValue::U64(extract_elapsed_us)),
                    (
                        "render_state_update_us",
                        TraceValue::U64(frame.stats.render_state_update_us),
                    ),
                    (
                        "frame_extraction_us",
                        TraceValue::U64(frame.stats.extraction_us),
                    ),
                    (
                        "image_placements",
                        TraceValue::Usize(frame.images.placements.len()),
                    ),
                    (
                        "virtual_placements",
                        TraceValue::Usize(frame.images.virtual_placements.len()),
                    ),
                ],
            );
        }
        if let Err(error) = self.latest_frame.publish(frame) {
            self.worker_health.record("publish_frame", error);
            self.acknowledge_resize();
            return;
        }
        if let Some(trace) = &trace {
            trace.emit(
                "frame_presented",
                &[("presenter", TraceValue::Str("published_frame"))],
            );
        }
        self.force_next_frame_publish = false;
        self.has_unpublished_frame = false;
        self.acknowledge_resize();
        (self.repaint_wakeup)();
    }

    fn acknowledge_resize(&mut self) {
        if let Some(done) = self.pending_resize_ack.take() {
            done.send(());
        }
        if let Some(done) = self.pending_render_cell_ack.take() {
            done.send(());
        }
    }

    fn trace_event(&self, event: &str, fields: &[(&str, TraceValue<'_>)]) {
        if let Some(trace) = &self.benchmark_trace {
            trace.emit(event, fields);
        }
    }

    fn write_output_buf(&self) {
        if !self.output_buf.is_empty() {
            write_pty(&self.pty_writer, &self.output_buf, &self.worker_health);
        }
    }
}

fn synchronized_output_state(engine: &TerminalEngine, health: &WorkerHealth) -> bool {
    match engine.is_synchronized_output() {
        Ok(active) => active,
        Err(error) => {
            health.record("synchronized_output", error);
            false
        }
    }
}

fn write_pty(writer: &Arc<Mutex<Box<dyn Write + Send>>>, bytes: &[u8], health: &WorkerHealth) {
    let mut writer = match writer.lock() {
        Ok(writer) => writer,
        Err(_) => {
            health.record("pty_write_lock", "writer lock poisoned");
            return;
        }
    };
    if let Err(error) = writer.write_all(bytes) {
        health.record("pty_write_all", error);
        return;
    }
    if let Err(error) = writer.flush() {
        health.record("pty_flush", error);
    }
}

// DEC mode 2026 (synchronized output): applications wrap multi-step redraws
// in BSU/ESU so intermediate states (e.g. a cleared screen before a tmux
// layout repaint) never reach the display. The grace period bounds a client
// that sets the mode and dies without clearing it.
pub fn sync_output_suppresses_publish(
    sync_output_active: bool,
    sync_output_observed_in_batch: bool,
    elapsed_since_sync_start: Duration,
) -> bool {
    sync_output_observed_in_batch
        || (sync_output_active && elapsed_since_sync_start < SYNC_OUTPUT_MAX_SUPPRESS)
}

pub fn should_publish_frame_after_work(
    unpublished_frame: bool,
    force_next_frame_publish: bool,
    sync_output_suppressed: bool,
    pending_pty_bytes: usize,
    elapsed_since_last_terminal_change: Duration,
    elapsed_since_last_publish: Duration,
) -> bool {
    if !unpublished_frame {
        return false;
    }
    if sync_output_suppressed {
        return false;
    }
    if force_next_frame_publish {
        return true;
    }
    if pending_pty_bytes > 0 {
        // A TUI repainting as it scrolls keeps a little output pending at all times, which pinned
        // publishing to the backlog interval and left content updating at ~15fps under a window
        // painting at 120. That interval is there to keep a flood (`cat` of a large file) from
        // starving the drain, so it applies once the backlog is actually flood-sized.
        let interval = if pending_pty_bytes >= WORKER_FLOOD_BACKLOG_BYTES {
            WORKER_BACKLOG_FRAME_INTERVAL
        } else {
            WORKER_READY_FRAME_INTERVAL
        };
        return elapsed_since_last_publish >= interval;
    }
    elapsed_since_last_terminal_change >= WORKER_SETTLED_FRAME_DELAY
}
