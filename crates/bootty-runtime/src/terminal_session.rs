use std::{
    collections::VecDeque,
    env,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(target_os = "macos")]
use std::process::Command as ProcessCommand;

use crate::benchmark_trace::{BenchmarkTrace, TraceValue};
use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use bootty_surface::geometry::{CellMetrics, TerminalGeometry};
use bootty_terminal::{
    terminal_engine::{
        TERMINAL_PROGRAM, TERMINAL_PROGRAM_VERSION, TERMINAL_TERM, TerminalColorConfig,
        TerminalCopyModeAction, TerminalCopyModeOutcome, TerminalCursorConfig, TerminalEngine,
        TerminalFeatureConfig, TerminalSearchDirection, TerminalSelectionEvent,
        TerminalSelectionFormat, TerminalSideEffectEvent,
    },
    terminal_frame::RenderFrame,
    terminal_input_model::{KeyInput, MacosOptionAsAlt, MouseInput},
};

pub(crate) const MAX_DRAIN_BYTES_PER_FRAME: usize = 4 * 1024 * 1024;
pub(crate) const MAX_DRAIN_CHUNKS_PER_FRAME: usize = 32;
pub(crate) const MAX_DRAIN_SLICE_BYTES: usize = 8 * 1024;
pub(crate) const MAX_DRAIN_TIME_US: u128 = 20_000;
const INPUT_FAST_PATH_DRAIN_BYTES: usize = 64 * 1024;
const INPUT_FAST_PATH_DRAIN_CHUNKS: usize = 8;
const INPUT_FAST_PATH_DRAIN_TIME_US: u128 = 2_000;
const MAX_COLLECT_BYTES_PER_TICK: usize = 4 * 1024 * 1024;
const MAX_COLLECT_CHUNKS_PER_TICK: usize = 256;
const MAX_READER_QUEUE_CHUNKS: usize = MAX_COLLECT_CHUNKS_PER_TICK * 2;
const CURSOR_HOME: &[u8; 3] = b"\x1b[H";
pub const BOOTTY_SHELL_ENV: &str = "BOOTTY_SHELL";
const TERM_ENV: &str = "TERM";
const COLORTERM_ENV: &str = "COLORTERM";
const TERMINFO_ENV: &str = "TERMINFO";
const TERM_PROGRAM_ENV: &str = "TERM_PROGRAM";
const TERM_PROGRAM_VERSION_ENV: &str = "TERM_PROGRAM_VERSION";
#[cfg(windows)]
const DEFAULT_SHELL: &str = "powershell.exe";
#[cfg(not(windows))]
const DEFAULT_SHELL: &str = "/bin/sh";
pub(crate) const WORKER_READY_FRAME_INTERVAL: Duration = Duration::from_millis(16);
pub(crate) const WORKER_BACKLOG_FRAME_INTERVAL: Duration = Duration::from_millis(64);
/// Pending PTY bytes past which output counts as a flood rather than an interactive redraw, and
/// publishing backs off to [`WORKER_BACKLOG_FRAME_INTERVAL`] to keep the drain moving. A full-screen
/// repaint of a large grid runs tens of kilobytes, so this sits well above one.
pub(crate) const WORKER_FLOOD_BACKLOG_BYTES: usize = 256 * 1024;
pub(crate) const WORKER_IDLE_WAIT: Duration = Duration::from_millis(16);
pub(crate) const WORKER_SETTLED_FRAME_DELAY: Duration = Duration::from_millis(16);
pub(crate) const SYNC_OUTPUT_MAX_SUPPRESS: Duration = Duration::from_secs(1);

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
    current_working_directory: Arc<Mutex<Option<String>>>,
    geometry: TerminalGeometry,
    display_scale: f32,
    render_cell: CellMetrics,
    pty_master: Box<dyn MasterPty + Send>,
    child: Option<Box<dyn Child + Send + Sync>>,
    tty_name: Option<String>,
}

impl Drop for TerminalSession {
    // portable-pty does not kill the child on drop, and the master fd stays open
    // through the writer shared with the worker thread, so dropping a session never
    // delivers SIGHUP to the child. For the tmux/zellij backends the child is an
    // `attach-session` client; leaking it leaves a phantom client attached to the
    // session at its last size, and under tmux's `window-size smallest` the
    // smallest stale client pins the window so the live terminal can no longer grow
    // it. Switching sessions drops a fresh session every time, so these accumulate.
    // Kill the child on drop to detach the client (the tmux session itself
    // survives) and to reap the native shell when a terminal is closed. Reap on
    // a background thread so a slow child wait cannot beachball app shutdown.
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            terminate_child_without_blocking(child);
        }
    }
}

fn terminate_child_without_blocking(mut child: Box<dyn Child + Send + Sync>) {
    let _ = child.kill();
    thread::spawn(move || {
        let _ = child.wait();
    });
}

type SpawnedPty = (
    Box<dyn MasterPty + Send>,
    Arc<Mutex<Box<dyn Write + Send>>>,
    Receiver<Vec<u8>>,
    Box<dyn Child + Send + Sync>,
    Option<String>,
);

type RepaintWakeup = Arc<dyn Fn() + Send + Sync + 'static>;

pub(crate) struct PublishedFrame {
    latest: Mutex<Arc<RenderFrame>>,
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

fn search_and_publish_frame(
    engine: &mut TerminalEngine,
    latest_frame: &PublishedFrame,
    query: &str,
    direction: TerminalSearchDirection,
) -> Result<bool> {
    let found = engine.search_viewport(query, direction)?;
    let frame = engine.extract_frame()?;
    latest_frame.publish(frame)?;
    Ok(found)
}

type SelectionFormatResponse = std::result::Result<Option<Vec<u8>>, String>;
type MouseTrackingResponse = std::result::Result<bool, String>;
type SearchViewportResponse = std::result::Result<bool, String>;
type CopyModeActiveResponse = std::result::Result<bool, String>;
type CopyModeActionResponse = std::result::Result<TerminalCopyModeOutcome, String>;
enum TerminalCommand {
    DisplayScale(f32),
    RenderCellMetrics(CellMetrics),
    Cursor(TerminalCursorConfig),
    Features(TerminalFeatureConfig),
    Resize(TerminalGeometry),
    Colors(TerminalColorConfig),
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
        done: SyncSender<SelectionFormatResponse>,
    },
    CopyModeActive(SyncSender<CopyModeActiveResponse>),
    CopyModeAction {
        action: TerminalCopyModeAction,
        done: SyncSender<CopyModeActionResponse>,
    },
    SearchViewport {
        query: String,
        direction: TerminalSearchDirection,
        done: SyncSender<SearchViewportResponse>,
    },
    IsMouseTracking(SyncSender<MouseTrackingResponse>),
    DiscardPendingOutput(SyncSender<()>),
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
        let (pty_master, pty_writer, pty_rx, child, tty_name) =
            spawn_shell(geometry, &config.launch)?;
        let (command_tx, command_rx) = mpsc::channel();
        let latest_frame = Arc::new(PublishedFrame::new());
        let latest_drain = Arc::new(Mutex::new(DrainStats::default()));
        let pending_pty_len = Arc::new(AtomicUsize::new(0));
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
            pty_rx,
            pty_writer,
            command_rx,
            latest_frame: latest_frame.clone(),
            latest_drain: latest_drain.clone(),
            pending_pty_len: pending_pty_len.clone(),
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
            current_working_directory,
            geometry,
            display_scale: 1.0,
            render_cell: CellMetrics::new(geometry.cell_width as f32, geometry.cell_height as f32),
            pty_master,
            child: Some(child),
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

        self.geometry = geometry;
        self.send_command(TerminalCommand::Resize(geometry))?;
        self.pty_master.resize(PtySize {
            rows: geometry.rows,
            cols: geometry.cols,
            pixel_width: geometry.pixel_width(),
            pixel_height: geometry.pixel_height(),
        })?;

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
        self.display_scale = display_scale;
        self.send_command(TerminalCommand::DisplayScale(display_scale))
    }

    pub fn set_render_cell_metrics(&mut self, cell: CellMetrics) -> Result<()> {
        if self.render_cell == cell {
            return Ok(());
        }
        self.render_cell = cell;
        self.send_command(TerminalCommand::RenderCellMetrics(cell))
    }

    pub fn set_colors(&mut self, colors: TerminalColorConfig) -> Result<()> {
        self.send_command(TerminalCommand::Colors(colors))
    }

    pub fn set_cursor_config(&mut self, cursor: TerminalCursorConfig) -> Result<()> {
        self.send_command(TerminalCommand::Cursor(cursor))
    }

    pub fn set_feature_config(&mut self, features: TerminalFeatureConfig) -> Result<()> {
        self.send_command(TerminalCommand::Features(features))
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
        match self.child.as_mut() {
            Some(child) => child
                .try_wait()
                .map(|status| status.is_some())
                .context("poll shell child process"),
            None => Ok(true),
        }
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
        let (done_tx, done_rx) = mpsc::sync_channel(0);
        self.send_command(TerminalCommand::CopyModeActive(done_tx))?;
        done_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("terminal worker stopped before reporting copy mode"))?
            .map_err(anyhow::Error::msg)
    }

    pub fn handle_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome> {
        let (done_tx, done_rx) = mpsc::sync_channel(0);
        self.send_command(TerminalCommand::CopyModeAction {
            action,
            done: done_tx,
        })?;
        done_rx
            .recv()
            .map_err(|_| {
                anyhow::anyhow!("terminal worker stopped before handling copy mode action")
            })?
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
        let (done_tx, done_rx) = mpsc::sync_channel(0);
        self.send_command(TerminalCommand::FormatSelection {
            format,
            done: done_tx,
        })?;
        done_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("terminal worker stopped before formatting selection"))?
            .map_err(anyhow::Error::msg)
    }

    pub fn search_viewport(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
    ) -> Result<bool> {
        let (done_tx, done_rx) = mpsc::sync_channel(0);
        self.send_command(TerminalCommand::SearchViewport {
            query: query.to_owned(),
            direction,
            done: done_tx,
        })?;
        done_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("terminal worker stopped before searching scrollback"))?
            .map_err(anyhow::Error::msg)
    }

    pub fn is_mouse_tracking(&mut self) -> Result<bool> {
        let (done_tx, done_rx) = mpsc::sync_channel(0);
        self.send_command(TerminalCommand::IsMouseTracking(done_tx))?;
        done_rx
            .recv()
            .map_err(|_| {
                anyhow::anyhow!("terminal worker stopped before reporting mouse tracking")
            })?
            .map_err(anyhow::Error::msg)
    }

    pub fn current_working_directory(&self) -> Option<String> {
        self.current_working_directory
            .lock()
            .ok()
            .and_then(|cwd| cwd.clone())
    }

    pub fn discard_pending_output(&mut self) -> Result<()> {
        let (done_tx, done_rx) = mpsc::sync_channel(0);
        self.send_command(TerminalCommand::DiscardPendingOutput(done_tx))?;
        done_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("terminal worker stopped before discarding output"))
    }

    pub fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        self.latest_frame.load()
    }

    fn send_command(&self, command: TerminalCommand) -> Result<()> {
        self.command_tx
            .send(command)
            .map_err(|_| anyhow::anyhow!("terminal worker stopped"))
    }
}

struct TerminalWorkerConfig {
    geometry: TerminalGeometry,
    colors: TerminalColorConfig,
    cursor: TerminalCursorConfig,
    features: TerminalFeatureConfig,
    max_scrollback: usize,
    macos_option_as_alt: MacosOptionAsAlt,
    pty_rx: Receiver<Vec<u8>>,
    pty_writer: Arc<Mutex<Box<dyn Write + Send>>>,
    command_rx: Receiver<TerminalCommand>,
    latest_frame: Arc<PublishedFrame>,
    latest_drain: Arc<Mutex<DrainStats>>,
    pending_pty_len: Arc<AtomicUsize>,
    current_working_directory: Arc<Mutex<Option<String>>>,
    repaint_wakeup: RepaintWakeup,
    side_effect_tx: Option<Sender<TerminalSideEffectEvent>>,
    side_effect_pane_id: Option<String>,
    benchmark_trace: Option<BenchmarkTrace>,
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
        if let Err(error) = engine.on_pty_write(move |_terminal, bytes| {
            write_pty(&callback_writer, bytes);
        }) {
            let _ = startup_tx.send(Err(error.to_string()));
            return;
        }
        let _ = startup_tx.send(Ok(()));
        let mut worker = TerminalWorker {
            engine,
            pty_rx: config.pty_rx,
            pty_writer: config.pty_writer,
            command_rx: config.command_rx,
            latest_frame: config.latest_frame,
            latest_drain: config.latest_drain,
            pending_pty_len: config.pending_pty_len,
            current_working_directory: config.current_working_directory,
            repaint_wakeup: config.repaint_wakeup,
            side_effect_tx: config.side_effect_tx,
            side_effect_pane_id: config.side_effect_pane_id,
            benchmark_trace: config.benchmark_trace,
            output_buf: Vec::with_capacity(1024),
            pending_pty: PtyBacklog::with_capacity(MAX_COLLECT_CHUNKS_PER_TICK),
            last_frame_publish: Instant::now() - WORKER_READY_FRAME_INTERVAL,
            has_unpublished_frame: false,
            sync_output_since: None,
            sync_output_batch_pending: false,
            sync_output_escape_prefix_len: 0,
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
    pty_rx: Receiver<Vec<u8>>,
    pty_writer: Arc<Mutex<Box<dyn Write + Send>>>,
    command_rx: Receiver<TerminalCommand>,
    pending_command: Option<TerminalCommand>,
    latest_frame: Arc<PublishedFrame>,
    latest_drain: Arc<Mutex<DrainStats>>,
    pending_pty_len: Arc<AtomicUsize>,
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
    sync_output_escape_prefix_len: usize,
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
        let active = self.engine.is_synchronized_output().unwrap_or(false);
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
                TerminalCommand::RenderCellMetrics(cell) => {
                    self.engine.set_render_cell_metrics(cell);
                    stats.terminal_changed = true;
                }
                TerminalCommand::Resize(geometry) => {
                    if self.engine.resize(geometry).is_ok() {
                        stats.terminal_changed = true;
                    }
                }
                TerminalCommand::Colors(colors) => {
                    if self.engine.set_colors(colors).is_ok() {
                        stats.terminal_changed = true;
                    }
                }
                TerminalCommand::Cursor(cursor) => {
                    if self.engine.set_cursor_config(cursor).is_ok() {
                        stats.terminal_changed = true;
                    }
                }
                TerminalCommand::Features(features) => {
                    if self.engine.set_feature_config(features).is_ok() {
                        stats.terminal_changed = true;
                    }
                }
                TerminalCommand::Key(input) => {
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
                TerminalCommand::Focus(gained) => {
                    self.mark_input_fast_path();
                    if self
                        .engine
                        .encode_focus_to_vec(gained, &mut self.output_buf)
                        .is_ok()
                    {
                        self.write_output_buf();
                    }
                }
                TerminalCommand::Mouse(input) => {
                    self.mark_input_fast_path();
                    if self
                        .engine
                        .encode_mouse_to_vec(input, &mut self.output_buf)
                        .is_ok()
                    {
                        self.write_output_buf();
                    }
                }
                TerminalCommand::MouseWheel {
                    input,
                    scroll_delta,
                } => match self.engine.is_mouse_tracking() {
                    Ok(true) => {
                        self.mark_input_fast_path();
                        if self
                            .engine
                            .encode_mouse_to_vec(input, &mut self.output_buf)
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
                    Err(_) => {}
                },
                TerminalCommand::Paste(text) => {
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
                TerminalCommand::DiscardPendingOutput(done) => {
                    self.discard_pending_output_queue();
                    let _ = done.send(());
                }
                TerminalCommand::RawInput(bytes) => {
                    self.mark_input_fast_path();
                    self.engine.scroll_viewport_bottom();
                    stats.terminal_changed = true;
                    write_pty(&self.pty_writer, &bytes);
                }
                TerminalCommand::MouseViewportScroll { delta } => {
                    self.mark_input_fast_path();
                    self.engine.scroll_viewport_delta(delta);
                    stats.terminal_changed = true;
                }
                TerminalCommand::EnterCopyMode => {
                    self.mark_input_fast_path();
                    if self.engine.enter_copy_mode().is_ok() {
                        stats.terminal_changed = true;
                    }
                }
                TerminalCommand::SelectionBegin(event) => {
                    self.mark_input_fast_path();
                    if self.engine.begin_selection(event).is_ok() {
                        stats.terminal_changed = true;
                    }
                }
                TerminalCommand::SelectionUpdate(event) => {
                    self.mark_input_fast_path();
                    if self.engine.update_selection(event).is_ok() {
                        stats.terminal_changed = true;
                    }
                }
                TerminalCommand::SelectionEnd(event) => {
                    self.mark_input_fast_path();
                    if self.engine.end_selection(event).is_ok() {
                        stats.terminal_changed = true;
                    }
                }
                TerminalCommand::FormatSelection { format, done } => {
                    let response = self
                        .engine
                        .format_selection(format)
                        .map_err(|error| error.to_string());
                    let _ = done.send(response);
                }
                TerminalCommand::CopyModeActive(done) => {
                    let _ = done.send(Ok(self.engine.copy_mode_active()));
                }
                TerminalCommand::CopyModeAction { action, done } => {
                    self.mark_input_fast_path();
                    let response = self
                        .engine
                        .handle_copy_mode_action(action)
                        .map_err(|error| error.to_string());
                    stats.terminal_changed = true;
                    let _ = done.send(response);
                }
                TerminalCommand::SearchViewport {
                    query,
                    direction,
                    done,
                } => {
                    let response = search_and_publish_frame(
                        &mut self.engine,
                        &self.latest_frame,
                        &query,
                        direction,
                    )
                    .map_err(|error| error.to_string());
                    if response.is_ok() {
                        self.force_next_frame_publish = false;
                        self.has_unpublished_frame = false;
                        (self.repaint_wakeup)();
                    }
                    let _ = done.send(response);
                }
                TerminalCommand::IsMouseTracking(done) => {
                    let response = self
                        .engine
                        .is_mouse_tracking()
                        .map_err(|error| error.to_string());
                    let _ = done.send(response);
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
        self.sync_output_escape_prefix_len = 0;
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
        let mut observed_sync_output = engine.is_synchronized_output().unwrap_or(false);
        let mut sync_output_escape_prefix_len = self.sync_output_escape_prefix_len;
        let mut write = |bytes: &[u8]| {
            observed_sync_output |=
                observe_sync_output_start(bytes, &mut sync_output_escape_prefix_len);
            engine.write_vt(bytes);
            observed_sync_output |= engine.is_synchronized_output().unwrap_or(false);
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
        self.sync_output_escape_prefix_len = sync_output_escape_prefix_len;
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
        let Some(tx) = self.side_effect_tx.as_ref() else {
            return;
        };
        let mut disconnected = false;
        for effect in self.engine.drain_side_effects() {
            let event = TerminalSideEffectEvent::new(self.side_effect_pane_id.clone(), effect);
            if tx.send(event).is_err() {
                disconnected = true;
                break;
            }
        }
        if disconnected {
            self.side_effect_tx = None;
        }
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
        let Ok(frame) = self.engine.extract_frame() else {
            return;
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
        if self.latest_frame.publish(frame).is_ok() {
            if let Some(trace) = &trace {
                trace.emit(
                    "frame_presented",
                    &[("presenter", TraceValue::Str("published_frame"))],
                );
            }
            self.force_next_frame_publish = false;
            self.has_unpublished_frame = false;
            (self.repaint_wakeup)();
        }
    }

    fn trace_event(&self, event: &str, fields: &[(&str, TraceValue<'_>)]) {
        if let Some(trace) = &self.benchmark_trace {
            trace.emit(event, fields);
        }
    }

    fn write_output_buf(&self) {
        if !self.output_buf.is_empty() {
            write_pty(&self.pty_writer, &self.output_buf);
        }
    }
}

#[derive(Debug, Default)]
struct CursorHomeFloodCompactor {
    pending_len: usize,
    active: bool,
}

impl CursorHomeFloodCompactor {
    fn compact(&mut self, bytes: Vec<u8>) -> Option<Vec<u8>> {
        let mut state = self.pending_len;
        let mut complete = 0;

        for (index, byte) in bytes.iter().enumerate() {
            if *byte == CURSOR_HOME[state] {
                state += 1;
                if state == CURSOR_HOME.len() {
                    complete += 1;
                    state = 0;
                }
                continue;
            }

            if !self.active && complete <= 1 {
                if self.pending_len == 0 {
                    return Some(bytes);
                }
                let mut out = Vec::with_capacity(self.pending_len + bytes.len());
                out.extend_from_slice(&CURSOR_HOME[..self.pending_len]);
                out.extend_from_slice(&bytes);
                self.pending_len = 0;
                return Some(out);
            }

            let mut out =
                Vec::with_capacity(CURSOR_HOME.len() + self.pending_len + bytes.len() - index);
            if !self.active && complete > 0 {
                out.extend_from_slice(CURSOR_HOME);
            }
            if self.pending_len > 0 {
                out.extend_from_slice(&CURSOR_HOME[..self.pending_len]);
            }
            out.extend_from_slice(&bytes[index..]);
            self.pending_len = 0;
            self.active = false;
            return Some(out);
        }

        self.pending_len = state;
        if complete > 0 && !self.active {
            self.active = true;
            return Some(CURSOR_HOME.to_vec());
        }
        if complete > 0 {
            self.active = true;
        }
        None
    }

    fn finish(&mut self) -> Option<Vec<u8>> {
        if self.pending_len == 0 {
            return None;
        }
        let bytes = CURSOR_HOME[..self.pending_len].to_vec();
        self.pending_len = 0;
        self.active = false;
        Some(bytes)
    }
}

#[derive(Clone, Debug, Default)]
pub struct PtyBacklog {
    chunks: VecDeque<Vec<u8>>,
    bytes: usize,
    front_offset: usize,
}

impl PtyBacklog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            chunks: VecDeque::with_capacity(capacity),
            bytes: 0,
            front_offset: 0,
        }
    }

    pub fn push_back(&mut self, bytes: Vec<u8>) {
        self.bytes = self.bytes.saturating_add(bytes.len());
        self.chunks.push_back(bytes);
    }

    pub fn len(&self) -> usize {
        self.bytes
    }

    pub fn is_empty(&self) -> bool {
        self.bytes == 0
    }

    pub(crate) fn clear(&mut self) {
        self.chunks.clear();
        self.bytes = 0;
        self.front_offset = 0;
    }

    pub(crate) fn front_len(&self) -> Option<usize> {
        self.chunks
            .front()
            .map(|front| front.len().saturating_sub(self.front_offset))
    }

    pub(crate) fn consume_front(&mut self, len: usize, mut consume: impl FnMut(&[u8])) {
        let end = self.front_offset + len;
        if let Some(front) = self.chunks.front() {
            consume(&front[self.front_offset..end]);
        }

        self.front_offset = end;
        self.bytes = self.bytes.saturating_sub(len);
        if self
            .chunks
            .front()
            .is_some_and(|front| self.front_offset >= front.len())
        {
            self.chunks.pop_front();
            self.front_offset = 0;
        }
    }
}

pub fn drain_pty_backlog(backlog: &mut PtyBacklog, write: impl FnMut(&[u8])) -> DrainStats {
    drain_pty_backlog_with_limits(
        backlog,
        MAX_DRAIN_BYTES_PER_FRAME,
        MAX_DRAIN_CHUNKS_PER_FRAME,
        MAX_DRAIN_TIME_US,
        write,
    )
}

fn drain_pty_backlog_with_limits(
    backlog: &mut PtyBacklog,
    max_bytes: usize,
    max_chunks: usize,
    max_time_us: u128,
    mut write: impl FnMut(&[u8]),
) -> DrainStats {
    let start = Instant::now();
    let mut stats = DrainStats::default();

    while !backlog.is_empty()
        && !drain_budget_exhausted_with_limits(stats, max_bytes, max_chunks)
        && !drain_time_exhausted(start, max_time_us)
    {
        let Some(available) = backlog.front_len() else {
            backlog.clear();
            break;
        };
        let consumed = drain_slice_len_with_limit(stats, max_bytes, available);
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

fn write_pty(writer: &Arc<Mutex<Box<dyn Write + Send>>>, bytes: &[u8]) {
    if let Ok(mut writer) = writer.lock() {
        let _ = writer.write_all(bytes);
        let _ = writer.flush();
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrainStats {
    pub chunks: usize,
    pub bytes: usize,
    pub elapsed_us: u64,
}

#[cfg(test)]
pub(crate) fn drain_bytes_remaining(stats: DrainStats) -> usize {
    MAX_DRAIN_BYTES_PER_FRAME.saturating_sub(stats.bytes)
}

fn drain_bytes_remaining_with_limit(stats: DrainStats, max_bytes: usize) -> usize {
    max_bytes.saturating_sub(stats.bytes)
}

#[cfg(test)]
pub(crate) fn drain_slice_len(stats: DrainStats, available: usize) -> usize {
    drain_slice_len_with_limit(stats, MAX_DRAIN_BYTES_PER_FRAME, available)
}

fn drain_slice_len_with_limit(stats: DrainStats, max_bytes: usize, available: usize) -> usize {
    drain_bytes_remaining_with_limit(stats, max_bytes)
        .min(MAX_DRAIN_SLICE_BYTES)
        .min(available)
}

fn drain_time_exhausted(start: Instant, max_time_us: u128) -> bool {
    start.elapsed().as_micros() >= max_time_us
}

#[cfg(test)]
pub(crate) fn drain_budget_exhausted(stats: DrainStats) -> bool {
    drain_budget_exhausted_with_limits(stats, MAX_DRAIN_BYTES_PER_FRAME, MAX_DRAIN_CHUNKS_PER_FRAME)
}

fn drain_budget_exhausted_with_limits(
    stats: DrainStats,
    max_bytes: usize,
    max_chunks: usize,
) -> bool {
    stats.bytes >= max_bytes || stats.chunks >= max_chunks
}

fn observe_sync_output_start(bytes: &[u8], matched_prefix_len: &mut usize) -> bool {
    const START: &[u8] = b"\x1b[?2026h";
    let mut observed = false;
    for byte in bytes {
        if *byte == START[*matched_prefix_len] {
            *matched_prefix_len += 1;
            if *matched_prefix_len == START.len() {
                observed = true;
                *matched_prefix_len = 0;
            }
        } else {
            *matched_prefix_len = usize::from(*byte == START[0]);
        }
    }
    observed
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedLaunchEnvironment {
    term: String,
    colorterm: String,
    terminfo: Option<PathBuf>,
    term_program: String,
    term_program_version: String,
    env: Vec<(String, String)>,
}

fn resolve_launch_environment(
    config: &SessionLaunchConfig,
    bootty_terminfo_dir: Option<&Path>,
) -> ResolvedLaunchEnvironment {
    let (term, terminfo) = if config.term == crate::terminfo::XTERM_BOOTTY {
        match bootty_terminfo_dir {
            Some(dir) => (config.term.clone(), Some(dir.to_path_buf())),
            None => ("xterm-256color".to_owned(), None),
        }
    } else {
        (config.term.clone(), None)
    };
    let env = config
        .env
        .iter()
        .filter(|(name, _)| !is_managed_launch_env(name))
        .cloned()
        .collect();

    ResolvedLaunchEnvironment {
        term,
        colorterm: config.colorterm.clone(),
        terminfo,
        term_program: TERMINAL_PROGRAM.to_owned(),
        term_program_version: TERMINAL_PROGRAM_VERSION.to_owned(),
        env,
    }
}

fn is_managed_launch_env(name: &str) -> bool {
    matches!(
        name,
        TERM_ENV | COLORTERM_ENV | TERMINFO_ENV | TERM_PROGRAM_ENV | TERM_PROGRAM_VERSION_ENV
    )
}

fn spawn_shell(geometry: TerminalGeometry, config: &SessionLaunchConfig) -> Result<SpawnedPty> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows: geometry.rows,
        cols: geometry.cols,
        pixel_width: geometry.pixel_width(),
        pixel_height: geometry.pixel_height(),
    })?;

    let shell = shell_command_path(config.shell.clone());
    let launch_env = resolve_launch_environment(config, crate::terminfo::vendored_terminfo_dir());
    let mut command = CommandBuilder::new(shell);
    command.args(&config.args);
    for (name, value) in locale_env_entries() {
        command.env(name, value);
    }
    for (name, value) in &launch_env.env {
        command.env(name, value);
    }
    for name in &config.env_remove {
        command.env_remove(name);
    }
    command.env(TERM_ENV, &launch_env.term);
    command.env(COLORTERM_ENV, &launch_env.colorterm);
    command.env(TERM_PROGRAM_ENV, &launch_env.term_program);
    command.env(TERM_PROGRAM_VERSION_ENV, &launch_env.term_program_version);
    if let Some(terminfo) = &launch_env.terminfo {
        command.env(TERMINFO_ENV, terminfo.to_string_lossy().into_owned());
    }
    if let Some(cwd) = &config.working_directory {
        command.cwd(cwd);
    }

    // portable-pty only exposes tty_name() on Unix; ConPTY has no tty path.
    #[cfg(unix)]
    let tty_name = pair
        .master
        .tty_name()
        .map(|path| path.to_string_lossy().into_owned());
    #[cfg(not(unix))]
    let tty_name: Option<String> = None;

    let child = pair
        .slave
        .spawn_command(command)
        .context("spawn shell in PTY")?;

    let mut reader = pair.master.try_clone_reader()?;
    let writer = Arc::new(Mutex::new(pair.master.take_writer()?));
    let (tx, rx) = mpsc::sync_channel(MAX_READER_QUEUE_CHUNKS);

    thread::spawn(move || {
        let mut buf = [0_u8; 8192];
        let mut compactor = CursorHomeFloodCompactor::default();
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    if let Some(bytes) = compactor.finish() {
                        let _ = tx.send(bytes);
                    }
                    break;
                }
                Ok(n) => {
                    if let Some(bytes) = compactor.compact(buf[..n].to_vec())
                        && tx.send(bytes).is_err()
                    {
                        break;
                    }
                }
                Err(_) => {
                    if let Some(bytes) = compactor.finish() {
                        let _ = tx.send(bytes);
                    }
                    break;
                }
            }
        }
    });

    Ok((pair.master, writer, rx, child, tty_name))
}

pub fn configured_user_shell() -> Option<String> {
    configured_login_shell()
}

fn locale_env_entries() -> Vec<(String, String)> {
    let mut entries = Vec::new();
    for key in ["LANG", "LC_ALL", "LC_CTYPE"] {
        push_env_if_present(&mut entries, key);
    }
    for (key, value) in env::vars() {
        if key.starts_with("LC_") && !entries.iter().any(|(existing, _)| existing == &key) {
            entries.push((key, value));
        }
    }
    if !entries.iter().any(|(key, _)| key == "LC_CTYPE")
        && let Some((_, lang)) = entries.iter().find(|(key, _)| key == "LANG")
    {
        entries.push(("LC_CTYPE".to_owned(), lang.clone()));
    }
    normalize_locale_entries(&mut entries);
    entries
}

fn push_env_if_present(entries: &mut Vec<(String, String)>, key: &str) {
    if let Ok(value) = env::var(key) {
        entries.push((key.to_owned(), value));
    }
}

#[cfg(target_os = "macos")]
fn normalize_locale_entries(entries: &mut Vec<(String, String)>) {
    for (_, value) in entries.iter_mut() {
        if is_macos_c_locale(value) {
            *value = "en_US.UTF-8".to_owned();
        }
    }
    if !entries.iter().any(|(key, _)| key == "LANG") {
        entries.push(("LANG".to_owned(), "en_US.UTF-8".to_owned()));
    }
    if !entries.iter().any(|(key, _)| key == "LC_CTYPE") {
        entries.push(("LC_CTYPE".to_owned(), "en_US.UTF-8".to_owned()));
    }
}

#[cfg(target_os = "macos")]
fn is_macos_c_locale(value: &str) -> bool {
    matches!(value, "C" | "POSIX" | "C.UTF-8" | "C.utf8")
}

#[cfg(not(target_os = "macos"))]
fn normalize_locale_entries(_entries: &mut Vec<(String, String)>) {}

fn shell_command_path(configured: Option<String>) -> String {
    select_shell_path(
        env::var(BOOTTY_SHELL_ENV).ok(),
        configured,
        configured_user_shell(),
        env::var("SHELL").ok(),
    )
}

fn select_shell_path(
    explicit: Option<String>,
    configured: Option<String>,
    login: Option<String>,
    inherited: Option<String>,
) -> String {
    [explicit, configured, login, inherited]
        .into_iter()
        .flatten()
        .find_map(normalize_shell_path)
        .unwrap_or_else(|| DEFAULT_SHELL.to_string())
}

fn normalize_shell_path(shell: String) -> Option<String> {
    let shell = shell.trim();
    if shell.is_empty() || !Path::new(shell).is_absolute() {
        return None;
    }
    Some(shell.to_string())
}

#[cfg(target_os = "macos")]
fn configured_login_shell() -> Option<String> {
    configured_login_shell_with(
        env::var("USER").ok(),
        env::var("LOGNAME").ok(),
        current_username(),
        read_login_shell_for_user,
    )
}

#[cfg(target_os = "macos")]
fn configured_login_shell_with(
    user: Option<String>,
    logname: Option<String>,
    current: Option<String>,
    mut read_shell: impl FnMut(&str) -> Option<String>,
) -> Option<String> {
    let user = select_configured_shell_username(user, logname, current)?;
    read_shell(&user)
}

#[cfg(target_os = "macos")]
fn select_configured_shell_username(
    user: Option<String>,
    logname: Option<String>,
    current: Option<String>,
) -> Option<String> {
    [user, logname, current]
        .into_iter()
        .flatten()
        .find_map(normalize_username)
}

#[cfg(target_os = "macos")]
fn normalize_username(user: String) -> Option<String> {
    let user = user.trim();
    if user.is_empty() || user.contains('/') {
        return None;
    }
    Some(user.to_string())
}

#[cfg(target_os = "macos")]
fn current_username() -> Option<String> {
    let output = ProcessCommand::new("/usr/bin/id")
        .arg("-un")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    normalize_username(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(target_os = "macos")]
fn read_login_shell_for_user(user: &str) -> Option<String> {
    let user_record = format!("/Users/{user}");
    let output = ProcessCommand::new("/usr/bin/dscl")
        .args([".", "-read", user_record.as_str(), "UserShell"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_user_shell_output(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(target_os = "macos"))]
fn configured_login_shell() -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
fn parse_user_shell_output(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let (_, shell) = line.split_once(':')?;
        normalize_shell_path(shell.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn search_response_publishes_matching_frame_before_returning() -> Result<()> {
        let mut engine = TerminalEngine::new(TerminalGeometry {
            cols: 20,
            rows: 8,
            cell_width: 10,
            cell_height: 20,
        })?;
        engine.write_vt(b"RIGHTONLY\r\nRIGHTONLY");
        let published = PublishedFrame::new();
        published.publish(engine.extract_frame()?)?;

        assert!(search_and_publish_frame(
            &mut engine,
            &published,
            "RIGHTONLY",
            TerminalSearchDirection::Current,
        )?);

        let frame = published.load()?;
        assert_eq!(frame.search_match_count, 2);
        assert_eq!(frame.active_search_match_index, Some(1));
        assert_eq!(frame.search_matches.len(), 2);
        Ok(())
    }

    #[test]
    fn drain_budget_limits_bytes_per_frame() {
        let stats = DrainStats {
            chunks: 0,
            bytes: MAX_DRAIN_BYTES_PER_FRAME - 1,
            elapsed_us: 0,
        };

        assert_eq!(drain_bytes_remaining(stats), 1);
    }

    #[test]
    fn drain_slice_never_exceeds_slice_or_frame_budget() {
        assert_eq!(
            drain_slice_len(DrainStats::default(), MAX_DRAIN_SLICE_BYTES * 4),
            MAX_DRAIN_SLICE_BYTES
        );

        let nearly_full = DrainStats {
            chunks: 0,
            bytes: MAX_DRAIN_BYTES_PER_FRAME - 7,
            elapsed_us: 0,
        };
        assert_eq!(drain_slice_len(nearly_full, MAX_DRAIN_SLICE_BYTES), 7);
    }

    #[test]
    fn input_fast_path_drain_limit_is_smaller_than_backlog_catchup_budget() {
        let input_bytes = std::hint::black_box(INPUT_FAST_PATH_DRAIN_BYTES);
        let input_chunks = std::hint::black_box(INPUT_FAST_PATH_DRAIN_CHUNKS);
        let input_time = std::hint::black_box(INPUT_FAST_PATH_DRAIN_TIME_US);
        let max_bytes = std::hint::black_box(MAX_DRAIN_BYTES_PER_FRAME);
        let max_chunks = std::hint::black_box(MAX_DRAIN_CHUNKS_PER_FRAME);
        let max_time = std::hint::black_box(MAX_DRAIN_TIME_US);
        let slice = std::hint::black_box(MAX_DRAIN_SLICE_BYTES);

        assert!(input_bytes < max_bytes);
        assert!(input_chunks < max_chunks);
        assert!(input_time < max_time);
        assert!(input_bytes >= slice);
    }

    #[test]
    fn limited_drain_stops_at_input_fast_path_budget() {
        let mut backlog = PtyBacklog::with_capacity(32);
        for _ in 0..32 {
            backlog.push_back(vec![b'x'; MAX_DRAIN_SLICE_BYTES]);
        }

        let stats = drain_pty_backlog_with_limits(
            &mut backlog,
            INPUT_FAST_PATH_DRAIN_BYTES,
            INPUT_FAST_PATH_DRAIN_CHUNKS,
            INPUT_FAST_PATH_DRAIN_TIME_US,
            |_| {},
        );

        assert_eq!(stats.bytes, INPUT_FAST_PATH_DRAIN_BYTES);
        assert_eq!(stats.chunks, INPUT_FAST_PATH_DRAIN_CHUNKS);
        assert!(!backlog.is_empty());
    }

    #[test]
    fn backlog_catchup_budget_is_large_enough_for_history_bursts() {
        let max_bytes = std::hint::black_box(MAX_DRAIN_BYTES_PER_FRAME);
        let max_slice = std::hint::black_box(MAX_DRAIN_SLICE_BYTES);
        let max_chunks = std::hint::black_box(MAX_DRAIN_CHUNKS_PER_FRAME);
        let max_time = std::hint::black_box(MAX_DRAIN_TIME_US);

        assert!(max_bytes >= 4 * 1024 * 1024);
        assert!(max_slice >= 8 * 1024);
        assert!(max_chunks >= 32);
        assert!(max_time >= 20_000);
    }

    #[test]
    fn input_wakeup_does_not_publish_stale_pre_echo_frame() {
        assert!(!should_publish_frame_after_work(
            false,
            true,
            false,
            0,
            Duration::ZERO,
            Duration::ZERO,
        ));
    }

    #[test]
    fn input_echo_publishes_immediately_after_terminal_changes() {
        assert!(should_publish_frame_after_work(
            true,
            true,
            false,
            0,
            Duration::ZERO,
            Duration::ZERO,
        ));
    }

    #[test]
    fn backlog_catchup_batches_within_ready_interval() {
        assert!(!should_publish_frame_after_work(
            true,
            false,
            false,
            4096,
            Duration::ZERO,
            WORKER_READY_FRAME_INTERVAL / 2,
        ));
    }

    #[test]
    fn flood_backlog_waits_for_backlog_interval_before_publishing_partial_frame() {
        assert!(!should_publish_frame_after_work(
            true,
            false,
            false,
            WORKER_FLOOD_BACKLOG_BYTES,
            Duration::ZERO,
            WORKER_READY_FRAME_INTERVAL,
        ));

        assert!(should_publish_frame_after_work(
            true,
            false,
            false,
            WORKER_FLOOD_BACKLOG_BYTES,
            Duration::ZERO,
            WORKER_BACKLOG_FRAME_INTERVAL,
        ));
    }

    #[test]
    fn non_input_output_publishes_after_quiet_settle() {
        assert!(should_publish_frame_after_work(
            true,
            false,
            false,
            0,
            WORKER_SETTLED_FRAME_DELAY,
            Duration::ZERO,
        ));
    }

    #[test]
    fn non_input_output_waits_for_quiet_settle_even_when_display_interval_elapsed() {
        assert!(!should_publish_frame_after_work(
            true,
            false,
            false,
            0,
            Duration::ZERO,
            WORKER_READY_FRAME_INTERVAL,
        ));
    }

    #[test]
    fn cursor_home_flood_compactor_suppresses_repeated_complete_sequences() {
        let mut compactor = CursorHomeFloodCompactor::default();

        assert_eq!(
            compactor.compact(b"\x1b[H\x1b[H".to_vec()),
            Some(b"\x1b[H".to_vec())
        );
        assert_eq!(compactor.compact(b"\x1b[H\x1b[H".to_vec()), None);
        assert_eq!(compactor.finish(), None);
    }

    #[test]
    fn cursor_home_flood_compactor_preserves_split_sequences() {
        let mut compactor = CursorHomeFloodCompactor::default();

        assert_eq!(
            compactor.compact(b"\x1b[H\x1b".to_vec()),
            Some(b"\x1b[H".to_vec())
        );
        assert_eq!(compactor.compact(b"[H\x1b[".to_vec()), None);
        assert_eq!(compactor.compact(b"H".to_vec()), None);
        assert_eq!(compactor.finish(), None);
    }
    #[test]
    fn cursor_home_flood_compactor_leaves_single_home_before_content_intact() {
        let mut compactor = CursorHomeFloodCompactor::default();

        assert_eq!(
            compactor.compact(b"\x1b[H\x1b[38;5;1mA".to_vec()),
            Some(b"\x1b[H\x1b[38;5;1mA".to_vec())
        );
    }

    #[test]
    fn cursor_home_flood_compactor_flushes_partial_before_other_input() {
        let mut compactor = CursorHomeFloodCompactor::default();

        assert_eq!(compactor.compact(b"\x1b".to_vec()), None);
        assert_eq!(compactor.compact(b"x".to_vec()), Some(b"\x1bx".to_vec()));
        assert_eq!(compactor.compact(b"abc".to_vec()), Some(b"abc".to_vec()));
    }

    #[test]
    fn sync_output_holds_back_mid_redraw_frames_even_for_input_echo() {
        assert!(!should_publish_frame_after_work(
            true,
            true,
            true,
            0,
            WORKER_SETTLED_FRAME_DELAY,
            WORKER_READY_FRAME_INTERVAL,
        ));
    }

    #[test]
    fn synchronized_output_start_is_detected_with_end_in_same_slice() {
        let mut matched = 0;
        assert!(observe_sync_output_start(
            b"\x1b[?2026hredraw\x1b[?2026l",
            &mut matched
        ));
        assert_eq!(matched, 0);
    }

    #[test]
    fn synchronized_output_start_is_detected_across_slices() {
        let mut matched = 0;
        assert!(!observe_sync_output_start(b"\x1b[?20", &mut matched));
        assert!(observe_sync_output_start(b"26hredraw", &mut matched));
        assert_eq!(matched, 0);
    }

    #[test]
    fn stuck_sync_output_mode_stops_suppressing_after_grace_period() {
        assert!(sync_output_suppresses_publish(
            true,
            false,
            SYNC_OUTPUT_MAX_SUPPRESS / 2
        ));
        assert!(!sync_output_suppresses_publish(
            true,
            false,
            SYNC_OUTPUT_MAX_SUPPRESS
        ));
        assert!(!sync_output_suppresses_publish(
            false,
            false,
            Duration::ZERO
        ));
    }

    #[test]
    fn completed_sync_output_batch_suppresses_its_intermediate_frame() {
        assert!(sync_output_suppresses_publish(false, true, Duration::ZERO));
    }

    #[test]
    fn published_frame_load_clones_only_arc_handle() -> Result<()> {
        let slot = PublishedFrame::new();
        let first = slot.load()?;
        let second = slot.load()?;

        assert!(Arc::ptr_eq(&first, &second));
        Ok(())
    }

    #[test]
    fn published_frame_publish_swaps_latest_arc() -> Result<()> {
        let slot = PublishedFrame::new();
        let first = slot.load()?;
        let mut frame = RenderFrame {
            cols: 123,
            ..Default::default()
        };
        frame.text.extend(['o', 'k']);

        slot.publish(&frame)?;
        let second = slot.load()?;

        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(second.cols, 123);
        assert_eq!(second.text, ['o', 'k']);
        Ok(())
    }

    #[test]
    fn drain_budget_exhausts_on_bytes_or_chunks() {
        assert!(drain_budget_exhausted(DrainStats {
            chunks: 0,
            bytes: MAX_DRAIN_BYTES_PER_FRAME,
            elapsed_us: 0,
        }));
        assert!(drain_budget_exhausted(DrainStats {
            chunks: MAX_DRAIN_CHUNKS_PER_FRAME,
            bytes: 0,
            elapsed_us: 0,
        }));
        assert!(!drain_budget_exhausted(DrainStats {
            chunks: MAX_DRAIN_CHUNKS_PER_FRAME - 1,
            bytes: MAX_DRAIN_BYTES_PER_FRAME - 1,
            elapsed_us: 0,
        }));
    }

    #[test]
    fn pty_reader_queue_is_bounded_before_worker_collection() {
        assert_eq!(MAX_READER_QUEUE_CHUNKS, MAX_COLLECT_CHUNKS_PER_TICK * 2);
    }

    #[test]
    fn launch_environment_ignores_managed_env_overrides() {
        let config = SessionLaunchConfig {
            env: vec![
                ("TERM".to_owned(), "xterm-256color".to_owned()),
                ("COLORTERM".to_owned(), "false".to_owned()),
                ("TERMINFO".to_owned(), "/wrong".to_owned()),
                ("TERM_PROGRAM".to_owned(), "WezTerm".to_owned()),
                ("TERM_PROGRAM_VERSION".to_owned(), "wrong".to_owned()),
                ("EDITOR".to_owned(), "nvim".to_owned()),
            ],
            ..Default::default()
        };

        let resolved = resolve_launch_environment(&config, Some(Path::new("/bootty/terminfo")));

        assert_eq!(resolved.term, TERMINAL_TERM);
        assert_eq!(resolved.colorterm, "truecolor");
        assert_eq!(
            resolved.terminfo.as_deref(),
            Some(Path::new("/bootty/terminfo"))
        );
        assert_eq!(resolved.term_program, TERMINAL_PROGRAM);
        assert_eq!(resolved.term_program_version, TERMINAL_PROGRAM_VERSION);
        assert_eq!(resolved.env, [("EDITOR".to_owned(), "nvim".to_owned())]);
    }

    #[test]
    fn launch_environment_falls_back_when_bootty_terminfo_is_unavailable() {
        let resolved = resolve_launch_environment(&SessionLaunchConfig::default(), None);

        assert_eq!(resolved.term, "xterm-256color");
        assert_eq!(resolved.colorterm, "truecolor");
        assert_eq!(resolved.terminfo, None);
        assert_eq!(resolved.term_program, TERMINAL_PROGRAM);
        assert_eq!(resolved.term_program_version, TERMINAL_PROGRAM_VERSION);
    }

    /// Absolute shell path fixture that passes `normalize_shell_path` on the
    /// running platform; `/custom/fish` is not absolute on Windows.
    fn shell_fixture_path(suffix: &str) -> String {
        if cfg!(windows) {
            format!("C:\\{}", suffix.replace('/', "\\"))
        } else {
            format!("/{suffix}")
        }
    }

    #[test]
    fn shell_selection_prefers_explicit_then_login_then_environment() {
        assert_eq!(
            select_shell_path(
                Some(shell_fixture_path("custom/fish")),
                Some(shell_fixture_path("configured/bash")),
                Some(shell_fixture_path("login/fish")),
                Some(shell_fixture_path("env/zsh")),
            ),
            shell_fixture_path("custom/fish"),
        );
        assert_eq!(
            select_shell_path(
                Some("relative".to_string()),
                Some(shell_fixture_path("configured/bash")),
                Some(shell_fixture_path("login/fish")),
                Some(shell_fixture_path("env/zsh")),
            ),
            shell_fixture_path("configured/bash"),
        );
        assert_eq!(
            select_shell_path(
                None,
                Some("relative".to_string()),
                Some(shell_fixture_path("login/fish")),
                Some(shell_fixture_path("env/zsh")),
            ),
            shell_fixture_path("login/fish"),
        );
        assert_eq!(
            select_shell_path(
                None,
                Some("".to_string()),
                None,
                Some(shell_fixture_path("env/zsh")),
            ),
            shell_fixture_path("env/zsh"),
        );
        assert_eq!(
            select_shell_path(None, None, None, None),
            DEFAULT_SHELL.to_owned()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn locale_entries_default_macos_pty_clients_to_utf8() {
        let mut entries = vec![("LANG".to_owned(), "C.UTF-8".to_owned())];

        normalize_locale_entries(&mut entries);

        assert!(entries.contains(&("LANG".to_owned(), "en_US.UTF-8".to_owned())));
        assert!(entries.contains(&("LC_CTYPE".to_owned(), "en_US.UTF-8".to_owned())));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn shell_selection_uses_configured_login_shell_without_user_env() {
        let login = configured_login_shell_with(None, None, Some("luan".to_string()), |user| {
            assert_eq!(user, "luan");
            Some("/opt/homebrew/bin/fish".to_string())
        });

        assert_eq!(
            select_shell_path(None, None, login, Some("/opt/homebrew/bin/zsh".to_string()),),
            "/opt/homebrew/bin/fish"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn configured_shell_username_falls_back_to_logname_and_current_account() {
        assert_eq!(
            select_configured_shell_username(None, Some(" luan ".to_string()), None),
            Some("luan".to_string())
        );
        assert_eq!(
            select_configured_shell_username(None, None, Some("luan\n".to_string())),
            Some("luan".to_string())
        );
        assert_eq!(
            select_configured_shell_username(
                Some("".to_string()),
                Some("/Users/luan".to_string()),
                Some("luan".to_string()),
            ),
            Some("luan".to_string())
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn user_shell_output_parser_accepts_macos_dscl_format() {
        assert_eq!(
            parse_user_shell_output("UserShell: /opt/homebrew/bin/fish\n"),
            Some("/opt/homebrew/bin/fish".to_string()),
        );
        assert_eq!(parse_user_shell_output("UserShell: fish\n"), None);
    }

    proptest! {
        #[test]
        fn property_drain_slice_respects_available_slice_and_frame_budget(
            bytes in 0_usize..(MAX_DRAIN_BYTES_PER_FRAME + MAX_DRAIN_SLICE_BYTES),
            chunks in 0_usize..(MAX_DRAIN_CHUNKS_PER_FRAME + 8),
            available in 0_usize..(MAX_DRAIN_SLICE_BYTES * 3),
        ) {
            let stats = DrainStats {
                chunks,
                bytes,
                elapsed_us: 0,
            };
            let slice = drain_slice_len(stats, available);
            let remaining = drain_bytes_remaining(stats);

            prop_assert!(slice <= available);
            prop_assert!(slice <= MAX_DRAIN_SLICE_BYTES);
            prop_assert!(slice <= remaining);
            if available == 0 || remaining == 0 {
                prop_assert_eq!(slice, 0);
            }
        }

        #[test]
        fn property_publish_policy_preserves_fast_path_cadence_and_settle_invariants(
            unpublished in any::<bool>(),
            force in any::<bool>(),
            sync_suppressed in any::<bool>(),
            pending_pty_bytes in 0_usize..8192,
            elapsed_change_ms in 0_u64..1000,
            elapsed_publish_ms in 0_u64..1000,
        ) {
            let elapsed_change = Duration::from_millis(elapsed_change_ms);
            let elapsed_publish = Duration::from_millis(elapsed_publish_ms);
            let should_publish = should_publish_frame_after_work(
                unpublished,
                force,
                sync_suppressed,
                pending_pty_bytes,
                elapsed_change,
                elapsed_publish,
            );

            if !unpublished || sync_suppressed {
                prop_assert!(!should_publish);
            }
            if unpublished && !sync_suppressed && force {
                prop_assert!(should_publish);
            }
            if unpublished
                && !sync_suppressed
                && !force
                && pending_pty_bytes == 0
                && elapsed_change < WORKER_SETTLED_FRAME_DELAY
            {
                prop_assert!(!should_publish);
            }
            if unpublished
                && !force
                && pending_pty_bytes >= WORKER_FLOOD_BACKLOG_BYTES
                && elapsed_publish < WORKER_BACKLOG_FRAME_INTERVAL
            {
                prop_assert!(!should_publish);
            }
            if unpublished
                && !sync_suppressed
                && !force
                && pending_pty_bytes > 0
                && elapsed_publish >= WORKER_BACKLOG_FRAME_INTERVAL
            {
                prop_assert!(should_publish);
            }
            if unpublished
                && !sync_suppressed
                && !force
                && (1..WORKER_FLOOD_BACKLOG_BYTES).contains(&pending_pty_bytes)
                && elapsed_publish >= WORKER_READY_FRAME_INTERVAL
            {
                prop_assert!(should_publish);
            }
            if unpublished
                && !sync_suppressed
                && !force
                && pending_pty_bytes == 0
                && elapsed_change >= WORKER_SETTLED_FRAME_DELAY
            {
                prop_assert!(should_publish);
            }
        }
    }

    /// A scrolling TUI keeps a small backlog pending continuously. Holding those frames for the
    /// flood interval capped visible content at ~15fps while the window painted at 120.
    #[test]
    fn interactive_backlog_publishes_at_the_ready_interval_and_floods_back_off() {
        let interactive = 8 * 1024;
        assert!(
            should_publish_frame_after_work(
                true,
                false,
                false,
                interactive,
                Duration::ZERO,
                WORKER_READY_FRAME_INTERVAL,
            ),
            "an interactive redraw must publish at the ready interval"
        );
        assert!(
            !should_publish_frame_after_work(
                true,
                false,
                false,
                WORKER_FLOOD_BACKLOG_BYTES,
                Duration::ZERO,
                WORKER_READY_FRAME_INTERVAL,
            ),
            "a flood must keep backing off so the drain is not starved"
        );
        assert!(should_publish_frame_after_work(
            true,
            false,
            false,
            WORKER_FLOOD_BACKLOG_BYTES,
            Duration::ZERO,
            WORKER_BACKLOG_FRAME_INTERVAL,
        ));
    }

    #[cfg(unix)]
    #[test]
    fn terminal_session_selection_commands_publish_frame_selections() {
        let geometry = TerminalGeometry {
            cols: 8,
            rows: 2,
            cell_width: 10,
            cell_height: 20,
        };
        let mut session = TerminalSession::new_with_config(
            geometry,
            TerminalSessionConfig {
                launch: SessionLaunchConfig {
                    shell: Some("/bin/sh".to_owned()),
                    args: vec!["-c".to_owned(), "printf abcdefgh; sleep 2".to_owned()],
                    ..Default::default()
                },
                ..Default::default()
            },
            Arc::new(|| {}),
        )
        .expect("spawn terminal session");
        let surface = bootty_surface::geometry::TerminalSurface::for_logical_size(
            80.0,
            40.0,
            bootty_surface::geometry::CellMetrics::new(10.0, 20.0),
            bootty_surface::geometry::TerminalPadding::default(),
        );
        let event = |x, y| TerminalSelectionEvent {
            surface,
            position: bootty_surface::geometry::SurfacePoint { x, y },
            rectangle: false,
        };

        for _ in 0..100 {
            session.drain_pty();
            let frame = session.extract_frame().expect("extract frame");
            if frame.text.iter().collect::<String>().contains("abcdefgh") {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        session
            .begin_selection(event(15.0, 10.0))
            .expect("begin selection");
        session
            .update_selection(event(45.0, 10.0))
            .expect("update selection");
        session
            .end_selection(Some(event(45.0, 10.0)))
            .expect("end selection");

        let mut selected = None;
        for _ in 0..100 {
            session.drain_pty();
            let frame = session.extract_frame().expect("extract frame");
            if !frame.selections.is_empty() {
                selected = Some(frame.selections.clone());
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(
            selected.expect("selection frame rows"),
            vec![bootty_terminal::terminal_frame::FrameSelection {
                row: 0,
                start_col: 1,
                end_col: 3,
            }]
        );
    }

    #[derive(Debug)]
    struct BlockingWaitChild {
        killed: Arc<AtomicUsize>,
        wait_started: mpsc::Sender<()>,
        wait_gate: Arc<(Mutex<bool>, std::sync::Condvar)>,
    }

    impl portable_pty::ChildKiller for BlockingWaitChild {
        fn kill(&mut self) -> std::io::Result<()> {
            self.killed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(BlockingWaitKiller {
                killed: self.killed.clone(),
            })
        }
    }

    impl Child for BlockingWaitChild {
        fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
            Ok(None)
        }

        fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
            let _ = self.wait_started.send(());
            let (lock, cvar) = &*self.wait_gate;
            let mut released = lock.lock().expect("wait gate lock");
            while !*released {
                released = cvar.wait(released).expect("wait gate condvar");
            }
            Ok(portable_pty::ExitStatus::with_exit_code(0))
        }

        fn process_id(&self) -> Option<u32> {
            None
        }

        #[cfg(windows)]
        fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
            None
        }
    }

    #[derive(Debug)]
    struct BlockingWaitKiller {
        killed: Arc<AtomicUsize>,
    }

    impl portable_pty::ChildKiller for BlockingWaitKiller {
        fn kill(&mut self) -> std::io::Result<()> {
            self.killed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(Self {
                killed: self.killed.clone(),
            })
        }
    }

    #[test]
    fn terminal_child_reap_runs_after_kill_on_background_thread() {
        let killed = Arc::new(AtomicUsize::new(0));
        let wait_gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let (wait_started_tx, wait_started_rx) = mpsc::channel();
        let child = Box::new(BlockingWaitChild {
            killed: killed.clone(),
            wait_started: wait_started_tx,
            wait_gate: wait_gate.clone(),
        });
        let (returned_tx, returned_rx) = mpsc::channel();

        thread::spawn(move || {
            terminate_child_without_blocking(child);
            returned_tx.send(()).expect("return signal");
        });

        let returned = returned_rx.recv_timeout(Duration::from_millis(100));
        {
            let (lock, cvar) = &*wait_gate;
            *lock.lock().expect("wait gate lock") = true;
            cvar.notify_all();
        }

        assert_eq!(killed.load(Ordering::SeqCst), 1);
        assert!(wait_started_rx.recv_timeout(Duration::from_secs(1)).is_ok());
        assert!(
            returned.is_ok(),
            "kill returns before the child reap completes"
        );
    }

    #[cfg(unix)]
    fn wait_until_process_exits(pid: u32) -> bool {
        for _ in 0..100 {
            if !process_alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    // A shell on a fresh PTY blocks waiting for input, so it stays alive until the
    // session is dropped. The drop must kill it: for the tmux/zellij backends the
    // child is an `attach-session` client, and a leaked client pins the window
    // under `window-size smallest` so later resizes are silently ignored.
    #[cfg(unix)]
    #[test]
    fn dropping_session_kills_pty_child() {
        let session = TerminalSession::new(TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        })
        .expect("spawn terminal session");
        let pid = session
            .child
            .as_ref()
            .and_then(|child| child.process_id())
            .expect("pty child pid");
        assert!(
            process_alive(pid),
            "child should run before the session is dropped"
        );

        drop(session);

        assert!(
            wait_until_process_exits(pid),
            "dropping the session must kill its PTY child to avoid leaking a mux client"
        );
    }

    #[cfg(unix)]
    fn process_alive(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|status| status.success())
    }
}
